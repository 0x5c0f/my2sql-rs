#!/usr/bin/env bash
# Task 17：MySQL 5.6/5.7/8.0/8.4 全版本兼容矩阵编排（`make compat`）。
# 复用 Task 15 差分入口（VER 参数化的 tools/run-difftest.sh），逐版本跑：
#   to-sql 差分（Go 裁判 vs Rust，含离线 schema 回放 = run-difftest 步骤 7）
#   + binlog_checksum 双态（实测本机镜像 5.6.51/5.7.x 默认均 CRC32；
#     5.6 与 5.7 各加跑 CKSUM=none——两条 checksum 代码路径在两个大版本上正反都过）
#   + 5.6 V1 rows 事件用例（log_bin_use_v1_row_events=1，真机 23/24/25 解码路径）
#   + 8.4 专项：原版 caching_sha2_password 在线元数据探针（Rust mysql crate
#     对 8.4 默认认证插件的握手验证；Go 裁判侧走 native policy 已由差分覆盖）
# 结果行落 out/compat-results.tsv；任一 FAIL 则退出码非 0。
# 环境开关：VERSIONS="5.6 8.0" 只跑部分版本（调试用；出矩阵须全跑）。
set -uo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"
VERSIONS="${VERSIONS:-5.6 5.7 8.0 8.4}"
RESULTS="$ROOT/out/compat-results.tsv"
mkdir -p out && : > "$RESULTS"
FAILS=0
PROBE_NAME=my2sql-t17-sha2
trap 'docker rm -f "$PROBE_NAME" >/dev/null 2>&1 || true' EXIT

record() { # ver case verdict
  printf '%s\t%s\t%s\n' "$1" "$2" "$3" | tee -a "$RESULTS"
}

# 镜像缺失则先拉（拉不下来 = 环境级阻塞，按 brief 立即停止而非伪造矩阵行）
ensure_image() {
  local v=$1
  docker image inspect "mysql:$v" >/dev/null 2>&1 && return 0
  echo ">> pulling mysql:$v (timeout 900s)"
  timeout 900 docker pull "mysql:$v" || { echo "BLOCKED: cannot pull mysql:$v" >&2; exit 2; }
}

run_case() { # label ver [cksum] [v1rows]
  local label=$1 ver=$2 cksum=${3:-} v1=${4:-} log rc=0
  log="out/compat-$label.log"
  echo "==== [$label] VER=$ver CKSUM=${cksum:-server-default} V1ROWS=${v1:-0} → $log ===="
  ( export VER="$ver" CKSUM="$cksum" V1ROWS="$v1"; bash tools/run-difftest.sh ) > "$log" 2>&1 || rc=$?
  if [ "$rc" -eq 0 ] && grep -q "^OK difftest" "$log"; then
    record "$ver" "$label" "PASS $(grep -o 'groups A=[0-9]* .*' "$log" | tail -1)"
  else
    FAILS=$((FAILS+1))
    record "$ver" "$label" "FAIL rc=$rc (log $log)"
    tail -25 "$log"
  fi
}

# 8.4 caching_sha2 在线元数据探针：差分跑完后 datadir(data/8.4) 已有矩阵数据
# 与 binlog；起第二实例（不传 --authentication-policy，即 8.4 默认态），
# 显式创建 caching_sha2_password 用户，Rust to-sql --uri 走该用户读在线
# schema，产物须与差分跑（native 用户）逐字节一致。
probe_84_caching_sha2() {
  local BIN PORT rc=0 outd="$ROOT/out/difftest-8.4"
  echo "==== [8.4-caching_sha2] 在线元数据探针 → out/compat-8.4-sha2.log ===="
  {
    BIN="$(cat "$outd/BINLOG")"
    docker rm -f "$PROBE_NAME" >/dev/null 2>&1 || true
    PORT="$(AUTH=stock DT_NAME=$PROBE_NAME bash tools/docker-mysql.sh 8.4)"
    docker exec -i "$PROBE_NAME" mysql -uroot <<'SQL'
CREATE USER IF NOT EXISTS 'probe'@'%' IDENTIFIED WITH caching_sha2_password BY 'ProbePass123';
GRANT ALL PRIVILEGES ON *.* TO 'probe'@'%';
FLUSH PRIVILEGES;
SQL
    docker exec "$PROBE_NAME" mysql -uroot -N -e \
      "SELECT CONCAT('probe plugin=', plugin) FROM mysql.user WHERE user='probe'" | grep -q "plugin=caching_sha2_password"
    ./target/debug/my2sql-rs to-sql \
      --binlog-dir data/8.4 --start-file "$BIN" \
      --uri "mysql://probe:ProbePass123@127.0.0.1:$PORT" --time-zone +00:00 \
      --add-extra-info --threads 4 --output-dir "$outd/rs-sha2"
    diff -r "$outd/rs" "$outd/rs-sha2"
  } > "out/compat-8.4-sha2.log" 2>&1 || rc=$?
  docker rm -f "$PROBE_NAME" >/dev/null 2>&1 || true
  if [ "$rc" -eq 0 ]; then
    record 8.4 "8.4-caching_sha2-online" "PASS (default auth, output == native-auth run)"
  else
    FAILS=$((FAILS+1))
    record 8.4 "8.4-caching_sha2-online" "FAIL rc=$rc (log out/compat-8.4-sha2.log)"
    tail -25 out/compat-8.4-sha2.log
  fi
}

for v in $VERSIONS; do ensure_image "$v"; done

for v in $VERSIONS; do
  case "$v" in
    # 5.6.51 实测默认 CRC32 + V2 事件（brief 的 5.6 历史假设由实测校正，见 matrix.md）；
    # 另用 log_bin_use_v1-row-events 真机覆盖 V1 rows 解码路径
    5.6) run_case "$v-default" "$v"; run_case "$v-none" "$v" none; run_case "$v-v1rows" "$v" "" 1 ;;
    5.7) run_case "$v-default" "$v"; run_case "$v-none"  "$v" none ;;
    8.0) run_case "$v-default" "$v" ;;
    8.4) run_case "$v-default" "$v" && probe_84_caching_sha2 ;;
    *)   echo "unsupported version $v (want 5.6|5.7|8.0|8.4)" >&2; exit 1 ;;
  esac
done

echo "==== compat matrix results ($RESULTS) ===="
cat "$RESULTS"
if [ "$FAILS" -ne 0 ]; then echo "COMPAT MATRIX: $FAILS FAIL"; exit 1; fi
echo "COMPAT MATRIX: ALL GREEN"
