#!/usr/bin/env bash
# Task 17：MySQL 5.6/5.7/8.0/8.4 全版本兼容矩阵编排（`make compat`）。
# 复用 Task 15 差分入口（VER 参数化的 tools/run-difftest.sh），逐版本跑：
#   to-sql 差分（Go 裁判 vs Rust，含离线 schema 回放 = run-difftest 步骤 7）
#   + binlog_checksum 双态（实测本机镜像 5.6.51/5.7.x 默认均 CRC32；
#     5.6 与 5.7 各加跑 CKSUM=none——两条 checksum 代码路径在两个大版本上正反都过）
#   + 5.6 V1 rows 事件用例（log_bin_use_v1_row_events=1，真机 23/24/25 解码路径；
#     run-difftest 步骤 3.5 事件普查硬门，产物 EVENT_CENSUS.txt）
#   + 8.4 专项：原版 caching_sha2_password 在线元数据探针（Rust mysql crate
#     对 8.4 默认认证插件的握手验证；Go 裁判侧走 native policy 已由差分覆盖）
# P2-T8 追加：flashback×4（WORK_TYPE=rollback，各版本 plain 默认捕获同 datadir 流程）
#   + stats 冒烟×2（5.6/8.0；5.7/8.4 有意不跑 = spec §3.6 省时裁决）。
# P3-T7 追加：repl 族×4（work=repl——无 Go 裁判（spec §8：上游 repl 无优雅停止/
#   Fatalf 即崩），对照物 = **file 模式同段逐字节**（等价性总闸的矩阵化，
#   比较器 = diff -r，唯一排除项 resume.json = repl 独有 checkpoint）。
#   容器生命周期/DML 灌流复用 tools/repl-e2e-lib.sh（Task 6 契约，勿再造第二套）。
# 结果行落 out/compat-results.tsv；任一 FAIL 则退出码非 0。
# 环境开关：VERSIONS="5.6 8.0" 只跑部分版本（调试用；出矩阵须全跑）。
set -uo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"
VERSIONS="${VERSIONS:-5.6 5.7 8.0 8.4}"
RESULTS="$ROOT/out/compat-results.tsv"
mkdir -p out
# repl 族复用件（只定义函数，source 零副作用；契约见该文件头注释）
source tools/repl-e2e-lib.sh
FAILS=0
PROBE_NAME=my2sql-t17-sha2
trap 'docker rm -f "$PROBE_NAME" >/dev/null 2>&1 || true
      p3e2e_container_stop "${P3E2E_CTR:-}" >/dev/null 2>&1 || true' EXIT

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

run_case() { # label ver [cksum] [v1rows] [work]
  local label=$1 ver=$2 cksum=${3:-} v1=${4:-} work=${5:-2sql} log rc=0 desc
  # P3-T7：work=repl 不走 difftest 通道（无 Go 裁判），转 repl 专属件。
  if [ "$work" = repl ]; then repl_run "$label" "$ver"; return; fi
  log="out/compat-$label.log"
  echo "==== [$label] VER=$ver WORK=$work CKSUM=${cksum:-server-default} V1ROWS=${v1:-0} → $log ===="
  ( export VER="$ver" CKSUM="$cksum" V1ROWS="$v1" WORK_TYPE="$work"; bash tools/run-difftest.sh ) > "$log" 2>&1 || rc=$?
  if [ "$rc" -eq 0 ] && grep -q "^OK difftest" "$log"; then
    # stats 冒烟无比较器 groups 行 → 配平行作 PASS 描述；2sql/rollback 仍记裁判比较行
    if [ "$work" = stats ]; then
      desc="$(grep -o 'stats smoke: .*' "$log" | tail -1)"
    else
      desc="$(grep -o 'groups A=[0-9]* .*' "$log" | tail -1)"
    fi
    record "$ver" "$label" "PASS $desc"
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
    # T17 修复轮 1：插件实证落日志（root + probe 实际 plugin 值逐行打印，
    # caching_sha2 声明自此有 artifact 背书），随后 grep 仍作硬断言。
    docker exec "$PROBE_NAME" mysql -uroot -N -e \
      "SELECT CONCAT('plugin proof: ', user, '@', host, ' -> ', plugin) FROM mysql.user WHERE user IN ('root','probe')"
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

# ── P3-T7 repl 族 ───────────────────────────────────────────────────────────
# 单版本用例主体（无 Go 裁判，spec §8）：容器起 → seed → 钉 start 位点 f0:p0 →
# 算 stop 时刻 D = 服务器钟 +12s（容器 TZ=UTC，--time-zone +00:00 同口径；
# stop-datetime 谓词 repl/file 两侧共用同一 Runner 停止逻辑）→ **后台灌流器**
# （p3e2e_feed_mixed 每调用一轮、tag=A<序号> 窗口指纹，200 轮上限 ≫ 12s 窗口）
# → 灌流中 FLUSH LOGS（矩阵跨档件：钉 T6 lib 契约「跨档矩阵归 T7」，repl 须
# 经 ROTATE 事件跟档且与 file 模式跨档产物逐字节同）→ repl 实时抓取到 D 优雅
# 收尾 → kill 灌流器 → B 段补灌一轮（全 ts>D，两侧必须不可见）→ docker cp
# 取段 f0..f1 → file 模式同窗口/同旗标 to-sql → diff -r -x resume.json 严格
# 比较 + DML 指纹非空闸（禁「同为空」假绿）。
# server-id 每用例递增（7200+序号）：1236 "A slave with the same
# server_uuid/server_id" 踢下线只发生在同 id 并发在场时；本族逐用例串跑、
# repl 进程退出即断连、容器用后即弃——递增 id 仍是对同宿主残留连接的兜底。
REPL_FEEDER=""
REPL_CTR=""

repl_body() { # <ver> <sid>；全部 stdout/stderr 由 repl_run 汇入用例日志
  local ver=$1 sid=$2
  local db="p3t7_${ver//./_}"
  local work="$ROOT/out/compat-repl-work-$ver"
  local ro="$work/repl" fo="$work/file" bd="$work/bins"
  rm -rf "$work"; mkdir -p "$ro" "$fo" "$bd"
  # 先调后取名：P3E2E_CTR 在 docker run 前即置（lib:59），wait_healthy 超时等
  # 中途失败时容器已存在——失败路径同样要把它交给 trap 收尸（T7 评审 I1）。
  p3e2e_container_start "$ver" "t7$$" >/dev/null
  local start_rc=$?
  REPL_CTR=$P3E2E_CTR
  [ "$start_rc" -eq 0 ] || return 1
  local ctr=$P3E2E_CTR uri=$P3E2E_URI
  p3e2e_seed_schema "$ctr" "$db" || return 1
  local f0 p0
  read -r f0 p0 <<<"$(p3e2e_master_pos "$ctr")" || return 1
  local dtext
  dtext=$(p3e2e_sql "$ctr" -N -e "SELECT FROM_UNIXTIME(UNIX_TIMESTAMP() + 12)") || return 1
  echo ">> window: start=$f0:$p0 stop-datetime='$dtext' (server clock +12s) sid=$sid"
  # 后台灌流器：值 =f(tag,round) 纯函数；中途 kill 时未提交事务随连接断开
  # 回滚消失，已提交部分照常进 binlog——两侧对照同一份 binlog，无歧义。
  ( for i in $(seq 1 200); do p3e2e_feed_mixed "$ctr" "$db" 1 "A$i"; done
    echo FEEDER_DONE ) > "$work/feeder.log" 2>&1 &
  REPL_FEEDER=$!
  sleep 3
  p3e2e_sql "$ctr" -e "FLUSH LOGS" || return 1   # 跨档：灌流中段轮转
  timeout 300 ./target/debug/my2sql-rs repl \
    --binlog-dir /nonused --start-file "$f0" --start-pos "$p0" \
    --stop-datetime "$dtext" --time-zone +00:00 \
    --uri "$uri" --db "$db" --add-extra-info \
    --output-dir "$ro" --server-id "$sid" --heartbeat-secs 5 \
    > "$work/repl.out" 2>&1
  local rrc=$?
  echo ">> repl rc=$rrc summary: $(grep -o 'repl done: .*' "$work/repl.out" | tail -1)"
  if [ $rrc -ne 0 ] || ! grep -q 'repl done: .*errors=0' "$work/repl.out"; then
    echo "REPL-RED: repl 运行失败 rc=$rrc，repl.out 尾部："; tail -30 "$work/repl.out"; return 1
  fi
  # 心跳探活实测记录（spec §2 勘误只钉过 8.0；5.6/5.7 的 SET 接受与否以本轮
  # 日志为准——transport.rs 对 SET 失败仅 warn 降级，不致命）。
  echo ">> heartbeat: SET 降级告计数=$(grep -c 'heartbeat SET' "$work/repl.out" || true)"
  kill "$REPL_FEEDER" >/dev/null 2>&1 || true
  wait "$REPL_FEEDER" 2>/dev/null || true
  REPL_FEEDER=""
  # B 段（stop 时刻之后）补灌一轮：产物必须不可见（窗口右界双侧一致的实证）
  p3e2e_feed_mixed "$ctr" "$db" 1 B || return 1
  local f1
  read -r f1 _ <<<"$(p3e2e_master_pos "$ctr")" || return 1
  p3e2e_capture_binlogs "$ctr" "$f0" "$f1" "$bd" || return 1
  timeout 300 ./target/debug/my2sql-rs to-sql \
    --binlog-dir "$bd" --start-file "$f0" --start-pos "$p0" \
    --stop-datetime "$dtext" --time-zone +00:00 \
    --uri "$uri" --db "$db" --add-extra-info \
    --output-dir "$fo" > "$work/file.out" 2>&1
  local frc=$?
  echo ">> file-mode summary: $(grep -o 'to-sql done: .*' "$work/file.out" | tail -1)"
  if [ $frc -ne 0 ] || ! grep -q 'to-sql done: .*errors=0' "$work/file.out"; then
    echo "REPL-RED: file 对照运行失败 rc=$frc，尾部："; tail -30 "$work/file.out"; return 1
  fi
  # 严格比较器：唯一排除项 resume.json（repl 独有 checkpoint，非 SQL 产物）
  if ! diff -r -x resume.json "$ro" "$fo"; then
    echo "REPL-RED: diff -r 非干净（真版本差，禁放宽）"; return 1
  fi
  local fcnt total
  read -r fcnt total <<<"$(find "$fo" -name '*.sql' -printf '%s\n' | awk '{s+=$1; n++} END{print n+0, s+0}')"
  [ "$fcnt" -ge 1 ] && [ "$total" -gt 1000 ] || {
    echo "REPL-RED: 产物过小 files=$fcnt bytes=$total（疑未灌到流量/同为空假绿）"; return 1; }
  cat "$fo"/*.sql > "$work/allsql.txt"
  grep -aq "INSERT INTO \`$db\`." "$work/allsql.txt" && grep -aq 'A1doc1' "$work/allsql.txt" &&
    grep -aq 'A1trx5' "$work/allsql.txt" || {
    echo "REPL-RED: A 段混合 DML 指纹缺失（窗口未含流量？）"; return 1; }
  if grep -aq 'Bdoc1' "$work/allsql.txt" || grep -aq 'JUNKRB' "$work/allsql.txt"; then
    echo "REPL-RED: B 段/回滚垃圾泄漏进产物（stop 右界失效）"; return 1
  fi
  [ "$f0" != "$f1" ] || echo "REPL-WARN: 本轮未跨档（FLUSH LOGS 失效？窗口未覆盖跨档形态）" >&2
  echo "EQUIV repl-$ver: files=$fcnt bytes=$total window=$f0:$p0..stop@'$dtext' span=$f0..$f1 sid=$sid heartbeat_set_degrades=$(grep -c 'heartbeat SET' "$work/repl.out" || true)"
}

repl_run() { # label ver
  local label=$1 ver=$2 log="out/compat-$label.log" rc=0
  echo "==== [$label] repl live vs file-mode 同窗等价（无 Go 裁判，spec §8）→ $log ===="
  (
    REPL_FEEDER=""; REPL_CTR=""
    # 信号/中断下的唯一保险形态（lib 头契约）：EXIT trap 兜底清 feeder 与容器
    trap '[ -n "$REPL_FEEDER" ] && kill "$REPL_FEEDER" 2>/dev/null || true
          p3e2e_container_stop "${REPL_CTR:-}" >/dev/null 2>&1 || true' EXIT
    repl_body "$ver" "$((7200 + ${REPL_SEQ:-0}))"; rc=$?
    exit $rc
  ) > "$log" 2>&1 || rc=$?
  REPL_SEQ=$(( ${REPL_SEQ:-0} + 1 ))
  if [ "$rc" -eq 0 ] && grep -q '^EQUIV ' "$log"; then
    record "$ver" "$label" "PASS $label equivalent=$(grep '^EQUIV ' "$log" | tail -1 | sed 's/.*bytes=\([0-9]*\).*/\1/') bytes"
  else
    FAILS=$((FAILS+1))
    record "$ver" "$label" "FAIL rc=$rc (log $log)"
    tail -25 "$log"
  fi
}

# T17 修复轮 1：PROBE_ONLY=1 只重跑 8.4 caching_sha2 探针（复用既有
# out/difftest-8.4/BINLOG + data/8.4 产物，不碰版本用例与全量 tsv——
# 结果行落 out/compat-probe-only.tsv），使插件实证日志可低成本再生。
if [ -n "${PROBE_ONLY:-}" ]; then
  RESULTS="$ROOT/out/compat-probe-only.tsv"; : > "$RESULTS"
  ensure_image 8.4
  probe_84_caching_sha2
  echo "==== probe-only results ($RESULTS) ===="
  cat "$RESULTS"
  if [ "$FAILS" -eq 0 ]; then echo "PROBE ONLY: GREEN"; exit 0; else echo "PROBE ONLY: FAIL"; exit 1; fi
fi
# P3-T7 调试入口：REPL_ONLY=1 [VERSIONS="8.0"] 只跑 repl 族（结果落独立 tsv，
# 不碰全量矩阵——最终记录在案的 PASS 必须出自一次完整干净的 18 用例全跑）。
if [ -n "${REPL_ONLY:-}" ]; then
  RESULTS="$ROOT/out/compat-repl-only.tsv"; : > "$RESULTS"
  for v in $VERSIONS; do run_case "repl-$v" "$v" "" "" repl; done
  echo "==== repl-only results ($RESULTS) ===="
  cat "$RESULTS"
  if [ "$FAILS" -eq 0 ]; then echo "REPL ONLY: GREEN"; exit 0; else echo "REPL ONLY: FAIL"; exit 1; fi
fi
: > "$RESULTS"   # 全量矩阵才截断（PROBE_ONLY 分支已在上方 exit）

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

# P2-T8 追加族（在 to-sql 族与 8.4 探针之后跑，不扰动 P1 用例次序/datadir 时序）：
#   flashback-{5.6,5.7,8.0,8.4} = WORK_TYPE=rollback——复用各版本 plain 默认捕获的
#     同套 datadir 流程（run-difftest 自含产数+裁判+我方+比较器三件套；CKSUM/V1ROWS
#     特殊用例按 brief 12 行 scope 不扩展）；
#   stats-{5.6,8.0} = WORK_TYPE=stats 冒烟（5.7/8.4 有意不跑 = spec §3.6 省时裁决）。
for v in $VERSIONS; do run_case "flashback-$v" "$v" "" "" rollback; done
run_case stats-5.6 5.6 "" "" stats
run_case stats-8.0 8.0 "" "" stats

# P3-T7：repl 族×4（既有 14 + repl 4 = 18）。放在所有 difftest 族之后跑，
# 不扰动 P1/P2 用例次序与 data/<ver> datadir 时序（repl 用例自带一次性容器）。
for v in $VERSIONS; do run_case "repl-$v" "$v" "" "" repl; done

echo "==== compat matrix results ($RESULTS) ===="
cat "$RESULTS"
if [ "$FAILS" -ne 0 ]; then echo "COMPAT MATRIX: $FAILS FAIL"; exit 1; fi
echo "COMPAT MATRIX: ALL GREEN"
