#!/usr/bin/env bash
# Task 15 差分总入口：容器产 binlog → 构建 Go 裁判 + Rust → 双方 to-sql →
# 语义比较器 → 退出码。`make difftest` 即本脚本。Rust 用 debug 构建（省时；
# 语义与 release 无差，性能测试不归本 harness）。容器经 trap 清理。
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"
VER="${VER:-8.0}"
NAME="my2sql-dt-${VER}"
OUT="$ROOT/out/difftest-$VER"
rm -rf "$OUT" && mkdir -p "$OUT/go" "$OUT/rs" tools/bin
trap 'rc=$?; if [ $rc -ne 0 ] && [ -n "${KEEP:-}" ]; then echo "FAILED(rc=$rc) — container $NAME kept for debug"; else docker rm -f "$NAME" >/dev/null 2>&1 || true; fi' EXIT

echo "== [1/6] comparator selftest"
python3 tools/comparator/selftest.py

echo "== [2/6] build rust (debug) + go oracle"
cargo build --quiet
(export PATH="$PATH:/opt/go/bin"; cd reference/my2sql-go && go build -o ../../tools/bin/my2sql-go .)

echo "== [3/6] mysql:$VER container + data matrix"
# 清空 datadir（999 属主文件宿主不可删——借 root 容器 wipe，保证每次全新状态）
if [ -d "data/$VER" ]; then
  docker run --rm -u 0 -v "$ROOT/data/$VER:/d" --entrypoint sh mysql:"$VER" \
    -c 'find /d -mindepth 1 -delete' >/dev/null
fi
PORT="$(bash tools/docker-mysql.sh "$VER")"
echo "   host port: $PORT"
docker exec -i "$NAME" mysql --default-character-set=utf8mb4 < tools/gen-data.sql
docker exec "$NAME" mysql -uroot -e "FLUSH BINARY LOGS"   # 封口，保证数据文件事件完整
# 数据全部落在 FLUSH 前的最后一个文件（SHOW BINARY LOGS 倒数第二个）
BIN="$(docker exec "$NAME" mysql -uroot -N -e "SHOW BINARY LOGS" | awk '{print $1}' | tail -2 | head -1)"
echo "   data binlog: $BIN"
# binlog 属 uid999(mode 640) 宿主不可读——root 助手容器放开读取（datadir 遍历 + binlog 可读）
docker run --rm -u 0 -v "$ROOT/data/$VER:/d" --entrypoint sh mysql:"$VER" \
  -c "find /d -maxdepth 1 -name 'mysql-bin*' -exec chmod a+r {} + && chmod a+rX /d" >/dev/null

echo "== [4/6] go oracle (-mode file -work-type 2sql, TZ=UTC)"
(export TZ=UTC; cd "$OUT/go" && "$ROOT/tools/bin/my2sql-go" \
   -mode file -work-type 2sql -mysql-type mysql \
   -host 127.0.0.1 -port "$PORT" -user root -password "" \
   -local-binlog-file "$ROOT/data/$VER/$BIN" \
   -add-extraInfo -threads 4 -output-dir . ) > "$OUT/go.log" 2>&1 \
   || { tail -20 "$OUT/go.log"; echo "go oracle FAILED"; exit 1; }
# 裁判读不到 binlog 时仅 log error 且退出码 0（实踩）——产物存在性硬校验
ls "$OUT"/go/forward*.sql >/dev/null 2>&1 || { echo "go oracle produced no forward sql"; exit 1; }

echo "== [5/6] rust to-sql"
./target/debug/my2sql-rs to-sql \
  --binlog-dir "data/$VER" --start-file "$BIN" \
  --uri "mysql://root@127.0.0.1:$PORT" --time-zone +00:00 \
  --add-extra-info --threads 4 --output-dir "$OUT/rs" > "$OUT/rs.log" 2>&1 \
  || { tail -20 "$OUT/rs.log"; echo "rust FAILED"; exit 1; }

echo "== [6/6] semantic compare (A=go oracle, B=rust)"
python3 tools/comparator/compare.py "$OUT/go" "$OUT/rs"
