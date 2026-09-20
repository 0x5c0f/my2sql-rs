#!/usr/bin/env bash
# Task 15 差分总入口：容器产 binlog → 构建 Go 裁判 + Rust → 双方 to-sql →
# 语义比较器 → 退出码。`make difftest` 即本脚本。Rust 用 debug 构建（省时；
# 语义与 release 无差，性能测试不归本 harness）。容器经 trap 清理。
# T17 扩展（均为环境开关，默认行为与 T15 一致）：
#   VER=5.6|5.7|8.0|8.4   目标服务器版本（默认 8.0）
#   CKSUM=none|crc32      显式 binlog_checksum（默认不传=服务器默认；产物目录加后缀）
#   V1ROWS=1              v1 rows 事件模式（docker-mysql.sh 透传；产物目录加后缀）
#   gen-data-$VER.sql 存在则优先（5.6 无 JSON 类型的矩阵级裁剪）
#   步骤 7 = 离线 schema 回放（在线 dump → --schema-file 重跑 → 与在线输出逐字节对差分）
#   步骤 3.5（仅 V1ROWS=1）= binlog 事件类型普查（tools/event-census.py，纯 stdlib
#   走读事件头，无需镜像内 mysqlbinlog）；产物 $OUT/EVENT_CENSUS.txt，
#   硬断言行事件全 V1（23/24/25 齐、30/31/32 零），违例即用例 FAIL（T17 修复轮 1）。
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"
VER="${VER:-8.0}"
NAME="my2sql-dt-${VER}"
OUT="$ROOT/out/difftest-$VER${CKSUM:+-$CKSUM}${V1ROWS:+-v1rows}"
rm -rf "$OUT" && mkdir -p "$OUT/go" "$OUT/rs" tools/bin
trap 'rc=$?; if [ $rc -ne 0 ] && [ -n "${KEEP:-}" ]; then echo "FAILED(rc=$rc) — container $NAME kept for debug"; else docker rm -f "$NAME" >/dev/null 2>&1 || true; fi' EXIT

echo "== [1/7] comparator selftest"
python3 tools/comparator/selftest.py

echo "== [2/7] build rust (debug) + go oracle"
cargo build --quiet
(export PATH="$PATH:/opt/go/bin"; cd reference/my2sql-go && go build -o ../../tools/bin/my2sql-go .)

# T17 修复轮 1：裸 `[ ] &&` 同型隐患 → 显式 if（见 docker-mysql.sh 注释）
if [ -n "${V1ROWS:-}" ]; then ROWSMODE=V1; else ROWSMODE=V2; fi
echo "== [3/7] mysql:$VER container + data matrix (checksum=${CKSUM:-server-default}, rows=$ROWSMODE)"
# 清空 datadir（999 属主文件宿主不可删——借 root 容器 wipe，保证每次全新状态）
if [ -d "data/$VER" ]; then
  docker run --rm -u 0 -v "$ROOT/data/$VER:/d" --entrypoint sh mysql:"$VER" \
    -c 'find /d -mindepth 1 -delete' >/dev/null
fi
PORT="$(bash tools/docker-mysql.sh "$VER")"
echo "   host port: $PORT"
docker exec "$NAME" mysql -uroot -N -e \
  "SELECT CONCAT('   server: ', VERSION(), ' binlog_checksum=', @@global.binlog_checksum, ' v1_row_events=', @@global.log_bin_use_v1_row_events)" 2>/dev/null \
  || docker exec "$NAME" mysql -uroot -N -e "SELECT CONCAT('   server: ', VERSION(), ' binlog_checksum=', @@global.binlog_checksum)"
GEN="tools/gen-data-${VER}.sql"
[ -f "$GEN" ] || GEN=tools/gen-data.sql
echo "   data script: $GEN"
docker exec -i "$NAME" mysql --default-character-set=utf8mb4 < "$GEN"
docker exec "$NAME" mysql -uroot -e "FLUSH BINARY LOGS"   # 封口，保证数据文件事件完整
# 数据全部落在 FLUSH 前的最后一个文件（SHOW BINARY LOGS 倒数第二个）
BIN="$(docker exec "$NAME" mysql -uroot -N -e "SHOW BINARY LOGS" | awk '{print $1}' | tail -2 | head -1)"
echo "   data binlog: $BIN"
echo "$BIN" > "$OUT/BINLOG"
# binlog 属 uid999(mode 640) 宿主不可读——root 助手容器放开读取（datadir 遍历 + binlog 可读）
docker run --rm -u 0 -v "$ROOT/data/$VER:/d" --entrypoint sh mysql:"$VER" \
  -c "find /d -maxdepth 1 -name 'mysql-bin*' -exec chmod a+r {} + && chmod a+rX /d" >/dev/null

if [ -n "${V1ROWS:-}" ]; then
  echo "== [3.5/7] event-type census (V1ROWS=1 hard gate → $OUT/EVENT_CENSUS.txt)"
  python3 tools/event-census.py "data/$VER/$BIN" --assert-v1-only | tee "$OUT/EVENT_CENSUS.txt"
fi

echo "== [4/7] go oracle (-mode file -work-type 2sql, TZ=UTC)"
(export TZ=UTC; cd "$OUT/go" && "$ROOT/tools/bin/my2sql-go" \
   -mode file -work-type 2sql -mysql-type mysql \
   -host 127.0.0.1 -port "$PORT" -user root -password "" \
   -local-binlog-file "$ROOT/data/$VER/$BIN" \
   -add-extraInfo -threads 4 -output-dir . ) > "$OUT/go.log" 2>&1 \
   || { tail -20 "$OUT/go.log"; echo "go oracle FAILED"; exit 1; }
# 裁判读不到 binlog 时仅 log error 且退出码 0（实踩）——产物存在性硬校验
ls "$OUT"/go/forward*.sql >/dev/null 2>&1 || { echo "go oracle produced no forward sql"; exit 1; }

echo "== [5/7] rust to-sql (online schema + dump)"
./target/debug/my2sql-rs to-sql \
  --binlog-dir "data/$VER" --start-file "$BIN" \
  --uri "mysql://root@127.0.0.1:$PORT" --time-zone +00:00 \
  --add-extra-info --threads 4 --output-dir "$OUT/rs" \
  --schema-dump "$OUT/schema.json" > "$OUT/rs.log" 2>&1 \
  || { tail -20 "$OUT/rs.log"; echo "rust FAILED"; exit 1; }

echo "== [6/7] semantic compare (A=go oracle, B=rust)"
python3 tools/comparator/compare.py "$OUT/go" "$OUT/rs"

echo "== [7/7] offline schema replay (--schema-file, 与在线输出逐字节对差)"
./target/debug/my2sql-rs to-sql \
  --binlog-dir "data/$VER" --start-file "$BIN" \
  --schema-file "$OUT/schema.json" --time-zone +00:00 \
  --add-extra-info --threads 4 --output-dir "$OUT/rs-offline" > "$OUT/rs-offline.log" 2>&1 \
  || { tail -20 "$OUT/rs-offline.log"; echo "rust offline replay FAILED"; exit 1; }
diff -r "$OUT/rs" "$OUT/rs-offline" || { echo "offline replay != online output"; exit 1; }
echo "OK difftest $VER${CKSUM:+ (checksum=$CKSUM)}: diff-green + replay-byte-identical"
