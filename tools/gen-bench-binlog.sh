#!/usr/bin/env bash
# Task 16: 生成 criterion bench 用合成 binlog（≥500MB，DoD-3 吞吐基线的输入）。
#
# 为什么不是「gen-data.sql ×N」：实测一次全量回放只产 ~37KB binlog
# （差分产物 data/8.0/mysql-bin.000002 的 ~3MB 实为镜像初始化期系统 schema DDL，
# 回放数据在 000003=36,966B）——到 500MB 需上万次回放。真实姿势 =
# run-difftest 同款容器 + gen-data.sql 一次（全类型矩阵、多表事件形态打底），
# 再用 bench.t_grow 表做服务端放大：每轮
#   ① INSERT(col_list) SELECT ... LIMIT 30000 分批灌入（多轮类型模板轮换，
#      行宽 ~100-500B，含 JSON/DECIMAL/时间族/BLOB/非UTF8 字节/emoji）；
#   ② 全镜像 UPDATE 3000 行 + DELETE 2000 行（U/D 事件形态与体量占比对齐矩阵）。
# 每轮 ≈12MB binlog，45+ 轮过 550MB；单轮字节 << max_binlog_size(1GB 上限)，
# 全程落同一个活动文件（8.0 把 max_binlog_size 钳到 1GB，见 MY-000081）。
# 收尾 FLUSH BINARY LOGS 封口 → chmod a+r（uid999 教训同 run-difftest）→
# debug 构建在线 --uri 冒烟全量解码 + --schema-dump 产 data/bench/schema.json
# （bench 本体走 --schema-file，离线、无需容器）。
# 缓存：data/bench/.bench-ready 记主 binlog 名；重入校验存在且 ≥500MB。
# FORCE=1 强制重生成。容器 trap 清理，不留 my2sql-* 残留。
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"
# 挂账#6（P4b T1）: 冒烟二进制走 CARGO_TARGET_DIR 口径（同 edb2148 RSBIN 约定），
# 不再硬编码 ./target/debug（外部 TARGET_DIR 下会指向旧/不存在产物）。
RSBIN="${CARGO_TARGET_DIR:-$ROOT/target}/debug/my2sql-rs"
VER=8.0
NAME=my2sql-bench-80
DATADIR="$ROOT/data/bench"
MARKER="$DATADIR/.bench-ready"
TARGET="${TARGET:-550000000}"   # 字节；≥500MB 判定线之上留裕量
MAXROUNDS="${MAXROUNDS:-200}"

cleanup() { docker rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT

if [ -z "${FORCE:-}" ] && [ -f "$MARKER" ]; then
  BIN="$(head -1 "$MARKER")"
  SZ="$(stat -c %s "$DATADIR/$BIN" 2>/dev/null || echo 0)"
  if [ "$SZ" -ge 500000000 ]; then
    echo "cached: $DATADIR/$BIN ($SZ bytes ≥ 500MB) — 跳过生成（FORCE=1 重生成）"
    exit 0
  fi
  echo "stale marker ($BIN size=$SZ < 500MB) — 重新生成"
fi

mkdir -p "$DATADIR"
rm -f "$MARKER"
# datadir 属主修正 + wipe（本机 uid 999 被 dnsmasq 占用 → 借 root 助手容器，同 run-difftest）
if [ "$(stat -c %u "$DATADIR")" != "999" ]; then
  docker run --rm -u 0 -v "$DATADIR:/d" --entrypoint chown mysql:"$VER" -R 999:999 /d
fi
docker run --rm -u 0 -v "$DATADIR:/d" --entrypoint sh mysql:"$VER" \
  -c 'find /d -mindepth 1 -delete' >/dev/null

docker rm -f "$NAME" >/dev/null 2>&1 || true
docker run -d --name "$NAME" \
  -e MYSQL_ALLOW_EMPTY_PASSWORD=1 \
  -e TZ=UTC \
  -v "$DATADIR:/var/lib/mysql" \
  -p 127.0.0.1::3306 \
  --memory=2g \
  mysql:"$VER" \
  --log-bin=mysql-bin --binlog-format=row --binlog-row-image=full --server-id=1 \
  --default-authentication-plugin=mysql_native_password >/dev/null

# 就绪探测必须 TCP（socket ping 会误中 skip-networking 的初始化实例，实踩）
for i in $(seq 1 180); do
  if docker exec "$NAME" mysqladmin -uroot -h127.0.0.1 -P3306 ping >/dev/null 2>&1; then break; fi
  if [ "$i" = 180 ]; then echo "mysql $VER not ready in 180s" >&2; docker logs "$NAME" >&2; exit 1; fi
  sleep 1
done
PORT="$(docker port "$NAME" 3306/tcp | head -1 | sed 's/.*://')"
echo "container $NAME up, host port $PORT"

echo "== [1/3] 全类型矩阵打底：gen-data.sql 一次（dt 库，W/U/D/多事务/全类型）"
docker exec -i "$NAME" mysql --default-character-set=utf8mb4 < tools/gen-data.sql

echo "== [2/3] bench.t_grow 放大到活动 binlog ≥ $TARGET"
docker exec -i "$NAME" mysql -uroot --default-character-set=utf8mb4 < tools/bench-growth.sql

# 活动 binlog 字节数 = SHOW BINARY LOGS 最后一行 File_size（5.6/8.x 列形兼容取 $2）
active_size() {
  docker exec "$NAME" mysql -uroot -N -e "SHOW BINARY LOGS" | tail -1 | awk '{print $2}'
}

SZ="$(active_size)"; ROUND=0
while [ "$SZ" -lt "$TARGET" ]; do
  ROUND=$((ROUND + 1))
  # 一轮 = grow_round()（3×INSERT 分批 + 全镜像 UPDATE + DELETE，见 bench-growth.sql）
  docker exec "$NAME" mysql -uroot -N -e "USE bench; CALL grow_round($ROUND)" >/dev/null
  SZ="$(active_size)"
  if [ $((ROUND % 5)) = 0 ]; then echo "  round=$ROUND active-binlog=$SZ"; fi
  if [ "$ROUND" -ge "$MAXROUNDS" ]; then
    echo "MAXROUNDS=$MAXROUNDS reached with only $SZ bytes (<$TARGET)" >&2
    exit 1
  fi
done
echo "amplified: $ROUND rounds, active binlog=$SZ bytes"

# 封口 + 数据文件 = 倒数第二个（run-difftest 同款：000001=180 为镜像 bootstrap FDE，
# 用户事件在活动文件；FLUSH 后活动文件让位，tail -2 取回）
docker exec "$NAME" mysql -uroot -e "FLUSH BINARY LOGS"
BIN="$(docker exec "$NAME" mysql -uroot -N -e "SHOW BINARY LOGS" | awk '{print $1}' | tail -2 | head -1)"
BINZ="$(docker exec "$NAME" mysql -uroot -N -e "SHOW BINARY LOGS" | awk -v b="$BIN" '$1==b{print $2}')"
if [ "${BINZ:-0}" -lt 500000000 ]; then
  echo "data binlog $BIN only $BINZ bytes — 事件跨文件了（检查 max_binlog_size），中止" >&2
  exit 1
fi
echo "data binlog: $BIN ($BINZ bytes)"

# binlog 640/uid999 → root 助手容器放开读取；datadir 顶格 777（后续宿主侧
# 要写 schema.json/.bench-ready，目录属 999 时宿主无权落盘，实踩）
docker run --rm -u 0 -v "$DATADIR:/d" --entrypoint sh mysql:"$VER" \
  -c "find /d -maxdepth 1 -name 'mysql-bin*' -exec chmod a+r {} + && chmod a+rX /d && chmod 777 /d" >/dev/null

echo "== [3/3] debug 构建在线 --uri 全量冒烟 + schema dump → data/bench/schema.json"
"$RSBIN" to-sql \
  --binlog-dir "$DATADIR" --start-file "$BIN" \
  --uri "mysql://root@127.0.0.1:$PORT" --time-zone +00:00 \
  --to-stdout --schema-dump "$DATADIR/schema.json" \
  --threads 8 >/dev/null 2> "$ROOT/out/bench-smoke.log" \
  || { tail -20 "$ROOT/out/bench-smoke.log"; echo "smoke to-sql FAILED"; exit 1; }
[ -s "$DATADIR/schema.json" ] || { echo "empty schema.json"; exit 1; }

echo "$BIN" > "$MARKER"
echo "ready: $DATADIR/$BIN ($(stat -c %s "$DATADIR/$BIN") bytes), marker=$MARKER"
