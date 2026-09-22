#!/usr/bin/env bash
# P4a T3 (Lane C)：p4a 表组「自 roundtrip」门——Go 裁判不可用/违例形时的自证通道
# （借 Lane B 影子库思路独立成件；全绿时也跑一次作双保险，成本 ~2min）。
# 形态：单库内「前态 = 空表」——容器起后整灌 tools/gen-data-p4a.sql（seed 唯一
# 来源，与 run-difftest P4A 分支同一文件，禁双抄 DML 防漂移），FLUSH 封口，
# 窗口 = 全 p4a 库（尾 -2 取封口文件，run-difftest 同款手法）→ 我方 to-sql
# （在线 --uri schema，native 认证插件供 sqlx/旧驱动共用）→ 产物灌 `_clone`
# 库（schema 由 mysqldump --no-data 自 main 复制 = 前态空表）→ 逐表
# CHECKSUM TABLE == 主库 + 行级 mysqldump diff 空。不等即非零退出。
# 纪律（沿 P2 T6）：人工触发、数据只进自建容器（datadir 全在容器可写层，
# docker cp 取出 binlog）、EXIT trap 自清（DROP DATABASE + docker rm -f；
# KEEP=1 失败时保留容器排障）。产物快照落 out/p4a-roundtrip/。
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"
# 终审轮 FIX E（沿 edb2148 接线口径）：debug 二进制路径随 CARGO_TARGET_DIR
# 解析（并行 lane 各自独立 target 目录，spec §6），与 run-difftest.sh 的
# RSBIN="${CARGO_TARGET_DIR:-$ROOT/target}/debug/my2sql-rs" 同口径。
RSBIN="${CARGO_TARGET_DIR:-$ROOT/target}/debug/my2sql-rs"
NAME=my2sql-p4a-rt
VER=8.0
DB=p4a
CLONE=p4a_clone
TABLES="t_enum_wide t_geom t_blob"
OUT="$ROOT/out/p4a-roundtrip"
rm -rf "$OUT" && mkdir -p "$OUT/binlog" "$OUT/rs" "$OUT/apply"
trap 'rc=$?; docker exec "$NAME" mysql -uroot -e "DROP DATABASE IF EXISTS $CLONE; DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1 || true; if [ $rc -ne 0 ] && [ -n "${KEEP:-}" ]; then echo "FAILED(rc=$rc) — container $NAME kept for debug"; else docker rm -f "$NAME" >/dev/null 2>&1 || true; fi' EXIT

echo "== [1/6] mysql:$VER container (ephemeral datadir, binlog ROW/FULL, native auth)"
docker rm -f "$NAME" >/dev/null 2>&1 || true
docker run -d --name "$NAME" \
  -e MYSQL_ALLOW_EMPTY_PASSWORD=1 -e TZ=UTC --memory=2g \
  -p 127.0.0.1::3306 \
  mysql:"$VER" --log-bin=mysql-bin --binlog-format=row --binlog-row-image=full \
  --server-id=1 --default-authentication-plugin=mysql_native_password >/dev/null
# 就绪 TCP ping（docker-mysql.sh 实踩：首启 initialize 期 skip-networking）
for i in $(seq 1 180); do
  docker exec "$NAME" mysqladmin -uroot -h127.0.0.1 -P3306 ping >/dev/null 2>&1 && break
  [ "$i" = 180 ] && { echo "mysql not ready in 180s" >&2; docker logs "$NAME" >&2; exit 1; }
  sleep 1
done
PORT="$(docker port "$NAME" 3306/tcp | head -1 | sed 's/.*://')"
echo "   host port: $PORT"

echo "== [2/6] seed 整灌 tools/gen-data-p4a.sql + FLUSH 封口（窗口 = 全 $DB 库）"
docker exec -i "$NAME" mysql --default-character-set=utf8mb4 < tools/gen-data-p4a.sql
docker exec "$NAME" mysql -uroot -e "FLUSH BINARY LOGS"
BIN="$(docker exec "$NAME" mysql -uroot -N -e "SHOW BINARY LOGS" | awk '{print $1}' | tail -2 | head -1)"
echo "   data binlog: $BIN"
docker cp "$NAME:/var/lib/mysql/$BIN" "$OUT/binlog/$BIN"

echo "== [3/6] rust to-sql (online schema via --uri)"
cargo build --quiet
"$RSBIN" to-sql \
  --binlog-dir "$OUT/binlog" --start-file "$BIN" \
  --uri "mysql://root@127.0.0.1:$PORT" --time-zone +00:00 \
  --add-extra-info --threads 4 --output-dir "$OUT/rs" > "$OUT/rs.log" 2>&1 \
  || { tail -20 "$OUT/rs.log"; echo "rust to-sql FAILED"; exit 1; }
grep -q "^to-sql done" "$OUT/rs.log" || { echo "no to-sql summary line"; cat "$OUT/rs.log"; exit 1; }
ls "$OUT"/rs/*.sql >/dev/null || { echo "rust to-sql produced no sql"; exit 1; }

echo "== [4/6] clone 库 = 前态空表（schema 复制自 main）+ 产物灌回"
docker exec "$NAME" mysql -uroot -e "DROP DATABASE IF EXISTS $CLONE; CREATE DATABASE $CLONE CHARACTER SET utf8mb4"
docker exec "$NAME" mysqldump -uroot --no-data --skip-add-drop-table "$DB" \
  | docker exec -i "$NAME" mysql -uroot "$CLONE"
# 产物表引用改写 main→clone（形如 \`p4a\`.\`tbl\` 的限定名；语句其余逐字节原样）
for f in "$OUT"/rs/*.sql; do
  sed "s/\`$DB\`\./\`$CLONE\`./g" "$f" > "$OUT/apply/$(basename "$f")"
  docker exec -i "$NAME" mysql -uroot "$CLONE" < "$OUT/apply/$(basename "$f")"
done

echo "== [5/6] 逐表 CHECKSUM TABLE 门（clone == main）"
FAIL=0
for t in $TABLES; do
  CKM="$(docker exec "$NAME" mysql -uroot -N -e "CHECKSUM TABLE $DB.$t" | awk '{print $2}')"
  CCK="$(docker exec "$NAME" mysql -uroot -N -e "CHECKSUM TABLE $CLONE.$t" | awk '{print $2}')"
  echo "   $t: main=$CKM clone=$CCK"
  [ "$CKM" = "$CCK" ] || FAIL=1
done
[ "$FAIL" = 0 ] || { echo "ROUNDTRIP RED: CHECKSUM TABLE mismatch"; exit 1; }

echo "== [6/6] 行级 mysqldump diff 门（checksum 碰撞兜底，内容级终审）"
for t in $TABLES; do
  docker exec "$NAME" mysqldump -uroot --no-create-info --skip-extended-insert \
    --default-character-set=utf8mb4 "$DB" "$t" > "$OUT/$t.main.sql"
  docker exec "$NAME" mysqldump -uroot --no-create-info --skip-extended-insert \
    --default-character-set=utf8mb4 "$CLONE" "$t" > "$OUT/$t.clone.sql"
done
# -a 必带（P4A 实踩）：GEOMETRY/LONGBLOB dump 含未转义的非 NUL 控制字节
# （0x01/0xC0 等），GNU grep 二进制探测会把 .rows 置空 → 行级 diff 假绿。
# 修复轮 1：不再保留 `^--$` 注释行——mysqldump 恒带裸 `--` 样板行，留着会让
# 「非空硬闸」在 0 数据行时也假绿。.rows = 纯 INSERT 数据线（main/clone 同构）。
dump_rows() { grep -a -E "^[(]|^INSERT" "$1" | sed 's/[[:space:]]*$//'; }
for t in $TABLES; do
  dump_rows "$OUT/$t.main.sql" > "$OUT/$t.main.rows"
  dump_rows "$OUT/$t.clone.sql" > "$OUT/$t.clone.rows"
  # 数据线硬闸（grep -c 无匹配时退 1 且打印 0，|| true 规避 set -e）：
  # 两侧 .rows 必须含 >=1 条 INSERT 数据行，否则（全空/只剩样板/抽行失败）即红。
  NINS="$(grep -ac '^INSERT' "$OUT/$t.main.rows" || true)"
  NINS_C="$(grep -ac '^INSERT' "$OUT/$t.clone.rows" || true)"
  { [ "$NINS" -gt 0 ] && [ "$NINS_C" -gt 0 ]; } \
    || { echo "ROUNDTRIP RED: $t no INSERT data lines (main=$NINS clone=$NINS_C — 空表/抽行失败即红)" >&2; exit 1; }
  if ! diff -u "$OUT/$t.main.rows" "$OUT/$t.clone.rows" > "$OUT/$t.rowdiff.txt"; then
    echo "ROUNDTRIP RED: checksum green but row content differs in $t" >&2
    head -40 "$OUT/$t.rowdiff.txt" >&2
    exit 1
  fi
  echo "   $t: row-data identical ($NINS INSERT 数据行)"
done
echo "OK p4a-roundtrip: 3 表 CHECKSUM TABLE clone==main + row-data identical (binlog=$BIN, ${SECONDS}s elapsed)"
