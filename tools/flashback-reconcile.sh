#!/usr/bin/env bash
# P2 T6（spec §5.5）：一次性「正灌 → 逆灌」活库语义对账——P2 语义正确性总闸。
# 流程：容器 mysql:8.0 建 rec_db.rec_t（含 JSON 列）灌 100 行提交 → 记基线
# CHECKSUM TABLE + 全表 dump → FLUSH BINARY LOGS 记起点 → 混合 DML（7 insert /
# 5 update〔含 1 条 JSON 列 update，故意〕/ 3 delete + 1 个多行事务）→ 记终点
# → docker cp 出 binlog → 宿主 my2sql-rs flashback（离线 --schema-file，默认
# on-error=stop，宁缺毋漏：产物若含坏事件即整跑红）→ 产物原样灌回同一活库 →
# CHECKSUM TABLE 必等于基线（另 dump 逐行 diff 出失败细节）。不等即非零退出。
#
# 语义裁定登记（对照简报 Step 3）：
#  - schema 源用 --schema-file（脚本自建表、schema 已知，手写 version-1 JSON）
#    而非 --uri 在线：mysqldump --no-data 产的是 DDL SQL，本工具离线 loader 只
#    认 version-1 JSON；离线通道免端口/认证管路，行为与在线同构（P1 T15 步骤7
#    已证在线/离线逐字节等价）。
#  - binlog 定位 = FLUSH 后新文件单文件覆盖全部 DML，只传 --start-file（不给
#    stop-pos：终点点出的事件按 stop「等号也停」会被排除，无需该边界）。
#
# 纪律：人工触发（不入 CI / Makefile 默认目标）；数据只进本脚本自建的 rec_db
# 与自建容器（无宿主 bind mount，datadir 全在容器可写层）；退出前 trap
# DROP DATABASE rec_db + docker rm -f 自清理。KEEP=1 且失败时保留容器排障。
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"
NAME=my2sql-t6-recon
VER=8.0
OUT="$ROOT/out/flashback-reconcile"
BINLOG_DIR="$OUT/binlog"
rm -rf "$OUT" && mkdir -p "$BINLOG_DIR" "$OUT/flashback"
trap 'rc=$?; docker exec "$NAME" mysql -uroot -e "DROP DATABASE IF EXISTS rec_db" >/dev/null 2>&1 || true; if [ $rc -ne 0 ] && [ -n "${KEEP:-}" ]; then echo "FAILED(rc=$rc) — container $NAME kept for debug"; else docker rm -f "$NAME" >/dev/null 2>&1 || true; fi' EXIT

echo "== [1/6] mysql:$VER container (ephemeral datadir, binlog ON, FULL image)"
docker rm -f "$NAME" >/dev/null 2>&1 || true
docker run -d --name "$NAME" \
  -e MYSQL_ALLOW_EMPTY_PASSWORD=1 \
  -e TZ=UTC \
  --memory=2g \
  mysql:"$VER" --log-bin=mysql-bin --binlog-format=row --binlog-row-image=full --server-id=1 >/dev/null
# 等就绪走 TCP ping（docker-mysql.sh 同款实踩：首启 initialize 期 skip-networking，
# socket ping 会误报就绪）。
for i in $(seq 1 180); do
  docker exec "$NAME" mysqladmin -uroot -h127.0.0.1 -P3306 ping >/dev/null 2>&1 && break
  [ "$i" = 180 ] && { echo "mysql not ready in 180s" >&2; docker logs "$NAME" >&2; exit 1; }
  sleep 1
done

echo "== [2/6] seed rec_db.rec_t (100 rows, JSON col) + baseline"
docker exec -i "$NAME" mysql -uroot <<'SQL'
CREATE DATABASE rec_db;
CREATE TABLE rec_db.rec_t (
  id INT NOT NULL PRIMARY KEY,
  val VARCHAR(32) NOT NULL,
  doc JSON
);
INSERT INTO rec_db.rec_t
  WITH RECURSIVE s(n) AS (
    SELECT 1 UNION ALL SELECT n+1 FROM s WHERE n < 100
  )
  SELECT n, CONCAT('v', n), JSON_OBJECT('n', n, 'tag', 'init') FROM s;
SQL
BASE_CK="$(docker exec "$NAME" mysql -uroot -N -e "CHECKSUM TABLE rec_db.rec_t" | awk '{print $2}')"
docker exec "$NAME" mysqldump -uroot --no-create-info --skip-extended-insert \
  --default-character-set=utf8mb4 rec_db rec_t > "$OUT/baseline.sql"
echo "   baseline CHECKSUM TABLE = $BASE_CK"

echo "== [3/6] FLUSH BINARY LOGS + mixed DML (7 ins / 5 upd incl 1 JSON / 3 del, 1 multi-row trx)"
docker exec "$NAME" mysql -uroot -e "FLUSH BINARY LOGS"
# 起点 = flush 后的当前文件（全部 DML 落入该文件，无中途 rotate）
BIN="$(docker exec "$NAME" mysql -uroot -N -e "SHOW BINARY LOGS" | awk '{print $1}' | tail -1)"
START_POS="$(docker exec "$NAME" mysql -uroot -N -e "SHOW MASTER STATUS" | awk '{print $2}')"
echo "   DML binlog: $BIN start_pos=$START_POS"
docker exec -i "$NAME" mysql -uroot rec_db <<'SQL'
-- 5 条 autocommit INSERT
INSERT INTO rec_t (id, val, doc) VALUES (101, 'a101', JSON_OBJECT('k', 1));
INSERT INTO rec_t (id, val, doc) VALUES (102, 'a102', NULL);
INSERT INTO rec_t (id, val, doc) VALUES (103, 'a103', JSON_OBJECT('k', 3, 'z', 'zz'));
INSERT INTO rec_t (id, val, doc) VALUES (104, 'a104', JSON_ARRAY(1, 2));
INSERT INTO rec_t (id, val, doc) VALUES (105, 'a105', NULL);
-- 4 条 autocommit UPDATE：含 1 条 JSON 列 update（已知风险区正靶）
UPDATE rec_t SET val='u2' WHERE id=2;
UPDATE rec_t SET doc=JSON_SET(doc, '$.tag', 'p2') WHERE id=3;   -- JSON 列 update
UPDATE rec_t SET val='u4', doc=NULL WHERE id=4;
UPDATE rec_t SET val='u6' WHERE id=6;
-- 多行事务：2 INSERT + 1 UPDATE + 1 DELETE 同事务提交
BEGIN;
INSERT INTO rec_t (id, val, doc) VALUES (106, 'm106', JSON_OBJECT('m', true));
INSERT INTO rec_t (id, val, doc) VALUES (107, 'm107', NULL);
UPDATE rec_t SET val='u5' WHERE id=5;
DELETE FROM rec_t WHERE id=7;
COMMIT;
-- 2 条 autocommit DELETE（id=6 此前被 UPDATE 过 → 逆序须先回灌改后镜像再逆 UPDATE）
DELETE FROM rec_t WHERE id=6;
DELETE FROM rec_t WHERE id=8;
SQL
# 终点仅记录展示（不给 CLI stop-pos，见头注裁定）
END_POS="$(docker exec "$NAME" mysql -uroot -N -e "SHOW MASTER STATUS" | awk '{print $2}')"
echo "   end_pos=$END_POS"
# DML 生效闸：100+7-3=104 行（UPDATE 打空 = 无 rows 事件 → 对账假绿，此处先拦）
NROW="$(docker exec "$NAME" mysql -uroot -N -e "SELECT COUNT(*) FROM rec_db.rec_t")"
[ "$NROW" = 104 ] || { echo "DML sanity FAILED: expected 104 rows after DML, got $NROW" >&2; exit 1; }

echo "== [4/6] extract binlog + offline schema.json"
docker cp "$NAME:/var/lib/mysql/$BIN" "$BINLOG_DIR/$BIN"
cat > "$OUT/schema.json" <<EOF
{
  "version": 1,
  "tables": [
    {"db":"rec_db","table":"rec_t","cols":[
       {"name":"id","type_name":"int","unsigned":false},
       {"name":"val","type_name":"varchar","unsigned":false},
       {"name":"doc","type_name":"json","unsigned":false}],
     "pk":["id"],"uks":[]}
  ]
}
EOF

echo "== [5/6] flashback (offline schema, default on-error=stop)"
cargo build --quiet
./target/debug/my2sql-rs flashback \
  --binlog-dir "$BINLOG_DIR" --start-file "$BIN" --start-pos "$START_POS" \
  --schema-file "$OUT/schema.json" --output-dir "$OUT/flashback" \
  --time-zone +00:00 --threads 4 2>&1 | tee "$OUT/flashback.log"
grep -q "^flashback done:" "$OUT/flashback.log" || { echo "flashback produced no summary line"; exit 1; }
ls "$OUT"/flashback/flashback.*.sql >/dev/null || { echo "no flashback sql emitted"; exit 1; }
# 产物结构自检：任何 Missing/partial 混入即 stop-Err（rc 已拦）；逆序脚手架在场
head -1 "$OUT"/flashback/flashback.*.sql | grep -q "SET NAMES"

echo "== [6/6] reverse-apply into live db + CHECKSUM TABLE gate"
for f in "$OUT"/flashback/flashback.*.sql; do
  echo "   applying $(basename "$f")"
  docker exec -i "$NAME" mysql -uroot rec_db < "$f"
done
AFTER_CK="$(docker exec "$NAME" mysql -uroot -N -e "CHECKSUM TABLE rec_db.rec_t" | awk '{print $2}')"
docker exec "$NAME" mysqldump -uroot --no-create-info --skip-extended-insert \
  --default-character-set=utf8mb4 rec_db rec_t > "$OUT/after.sql" || true
echo "   baseline checksum = $BASE_CK"
echo "   after-apply checksum = $AFTER_CK"
# checksum 等值有理论碰撞概率：逐行数据 diff（剔除 mysqldump 头部 SET/注释与
# 「Dump completed」时间戳伪差异）才是内容级终审。CHECKSUM TABLE 为简报硬闸。
dump_rows() { grep -E "^[(]|^INSERT|^--$" "$1" | grep -v '^-- Dump completed' | sed 's/[[:space:]]*$//'; }
dump_rows "$OUT/baseline.sql" > "$OUT/baseline.rows"
dump_rows "$OUT/after.sql"     > "$OUT/after.rows"
if [ "$BASE_CK" != "$AFTER_CK" ]; then
  echo "RECONCILE RED: checksum mismatch" >&2
  diff -u "$OUT/baseline.rows" "$OUT/after.rows" >&2 || true
  exit 1
fi
if ! diff -u "$OUT/baseline.rows" "$OUT/after.rows" > "$OUT/rowdiff.txt"; then
  echo "RECONCILE RED: checksum green but row content differs" >&2
  cat "$OUT/rowdiff.txt" >&2
  exit 1
fi
echo "OK flashback-reconcile: CHECKSUM TABLE $BASE_CK == $AFTER_CK + row-data identical (binlog=$BIN, ${SECONDS}s elapsed)"
