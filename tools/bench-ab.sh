#!/usr/bin/env bash
# P4b-T1（spec §1）端到端 A/B 吞吐判定工装。
# 契约: 同一 data/bench 输入（528.8MiB 单档）上，A/B 两个二进制各交替采
# N=5 轮 to-sql(threads=8) wall-clock；taskset 钉 P 核（实测 cpu0-11 高频域）；
# governor 只记录不修改（powersave 在册、无交互 sudo）。判定 = 中位数 +
# MAD：|Δmedian| > max(2×MAD_pool, 2%×medianA) → 显著（exit 1）。criterion
# 是权威账本，本脚本是判定工具——绝对值受页缓存偏乐观，两侧同窗交替使漂移对称。
# 统计核为便携形态：本机 mawk 1.3.4 无 asort → `sort -n | awk` 两遍扫
# 求 median/MAD（gawk 环境行为逐字相同，由 --selftest 钉死）。
# 用法:
#   bench-ab.sh --a <binA> --b <binB> [--rounds 5] [--threads 8] \
#               [--taskset 0-11] [--bench-dir <data/bench>]
#   bench-ab.sh --selftest     # 统计核 + 守护的无长测自证（rc: 0=过 / 1=挂）
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BENCH_DIR="${BENCH_DIR:-$ROOT/data/bench}"
usage() {
  echo "usage: bench-ab.sh --a <binA> --b <binB> [--rounds N] [--threads T] [--taskset CPULIST] [--bench-dir D]" >&2
  echo "       bench-ab.sh --selftest" >&2
  exit 2
}

# ---- 统计核（便携: sort -n 管道 awk 两遍扫；_median 无文件参数时读 stdin）----
_median() { # $@=样本文件（可省=stdin）→ stdout 单个数
  # 评审 finding 5: sort 加 LC_ALL=C，防逗号小数 locale 误排序（本机 C 下逐字等价）。
  LC_ALL=C sort -n "$@" | awk '{v[NR]=$1} END{
    if (NR == 0) { print "nan"; exit }
    if (NR % 2)  printf "%.6f", v[(NR+1)/2]
    else         printf "%.6f", (v[NR/2] + v[NR/2+1]) / 2 }'
}
_mad() { # $1=样本文件 → 第一遍中位数 → 绝对偏差列 → 第二遍中位数
  local m
  m="$(_median "$1")"
  awk -v m="$m" '{printf "%.6f\n", ($1 > m) ? $1 - m : m - $1}' "$1" | _median
}

verdict() { # $1=A样本文件 $2=B样本文件（SZ 存在则附吞吐行）
  # exit: 0=not-significant / 1=significant / 2=用法错（空/缺样本、计数非数字、<3/侧）
  local na nb ma mb wa wb
  # 评审 finding 2: 空/缺样本文件必须先挡（否则 na="" → [ "" -lt 3 ] 报错判假 → 放行打 nan 假绿）。
  [ -s "$1" ] && [ -s "$2" ] || { echo "usage: empty samples (A=$1 B=$2)"; return 2; }
  na="$(awk 'END{print NR}' "$1")"; nb="$(awk 'END{print NR}' "$2")"
  # 计数非数字（防御性，与 <3 同 rc=2 消息路径）：$na:$nb 只允许数字与冒号。
  case "$na:$nb" in (*[!0-9:]*|'') echo "usage: non-numeric sample count (A=$na B=$nb)"; return 2;; esac
  if [ "$na" -lt 3 ] || [ "$nb" -lt 3 ]; then
    echo "usage: need >=3 samples per side (A=$na B=$nb)"
    return 2
  fi
  ma="$(_median "$1")"; mb="$(_median "$2")"
  wa="$(_mad "$1")";   wb="$(_mad "$2")"
  awk -v ma="$ma" -v mb="$mb" -v wa="$wa" -v wb="$wb" -v sz="${SZ:-0}" '
    BEGIN {
      # 评审 finding 5: medianA<=0 时 delta%/吞吐会产出 inf/nan 假样 → 走 rc=2（最小实现，
      # 判定数学式与 printf 格式串逐字未动；≥500MB 输入下不可达，纯同源防御）。
      if (ma + 0 <= 0) { print "usage: nonpositive medianA=" ma; exit 2 }
      pool = (wa > wb) ? wa : wb
      delta = mb - ma; ad = (delta < 0) ? -delta : delta
      thr = (2 * pool > 0.02 * ma) ? 2 * pool : 0.02 * ma
      sig = (ad > thr) ? "significant" : "not-significant"
      printf "median A=%.4fs B=%.4fs MAD A=%.4f B=%.4f delta=%+.3f%% (%.4fs) thr=%.4fs\n", ma, mb, wa, wb, delta / ma * 100, ad, thr
      if (sz > 0) printf "throughput MiB/s A=%.1f B=%.1f (input=%d bytes)\n", sz / ma / 1048576, sz / mb / 1048576, sz
      printf "VERDICT: %s (B %s A)\n", sig, (delta > 0 ? "slower-than" : (delta < 0 ? "faster-than" : "equal-to"))
      exit (sig == "significant" ? 1 : 0)
    }'
}

if [ "${1:-}" = "--selftest" ]; then
  # 4 例: 已知漂移必判显著 / 同分布必判不显著 / 小样本守护必 rc=2 / 反号方向
  d="$(mktemp -d)"; trap 'rm -rf "$d"' EXIT
  printf '%s\n' 5.00 5.05 4.98 5.02 5.10 > "$d/a1"
  printf '%s\n' 5.60 5.55 5.62 5.58 5.51 > "$d/b1"   # +11%
  printf '%s\n' 5.00 5.02 4.99 5.01 5.03 > "$d/b2"   # ≈0%
  printf '%s\n' 5.60 5.55 5.62 5.58 5.51 > "$d/a3"   # 反号例的慢侧
  r=0; rc=0
  verdict "$d/a1" "$d/b1" > "$d/v1" || rc=$?
  { [ "$rc" -eq 1 ] && grep -q 'VERDICT: significant' "$d/v1"; } \
    || { echo "selftest FAIL: ±10% 未判显著"; r=1; }
  rc=0
  verdict "$d/a1" "$d/b2" > "$d/v2" || rc=$?
  { [ "$rc" -eq 0 ] && grep -q 'VERDICT: not-significant' "$d/v2"; } \
    || { echo "selftest FAIL: 同分布误判显著"; r=1; }
  rc=0
  printf '%s\n' 5.0 5.1 > "$d/short"
  verdict "$d/short" "$d/short" > /dev/null 2>&1 || rc=$?
  [ "$rc" -eq 2 ] || { echo "selftest FAIL: 小样本未 rc=2"; r=1; }
  rc=0
  verdict "$d/a3" "$d/a1" > "$d/v3" || rc=$?   # A 慢 B 快 → 显著且 B faster-than
  { [ "$rc" -eq 1 ] && grep -q 'VERDICT: significant' "$d/v3" \
      && grep -q '(B faster-than A)' "$d/v3"; } \
    || { echo "selftest FAIL: 反向漂移漏判"; r=1; }
  # ---- 例5-8: 端到端主路径守护回归针（自造临时 bench 目录 + 假二进制，零真实数据依赖）----
  mkdir -p "$d/bench"
  truncate -s 600M "$d/bench/mysql-bin.000003"          # 稀疏档，仅骗过 ≥500MB 闸
  printf 'mysql-bin.000003\n' > "$d/bench/.bench-ready"
  : > "$d/bench/schema.json"
  printf '#!/usr/bin/env bash\nexit 3\n' > "$d/fail3"; chmod +x "$d/fail3"
  printf '#!/usr/bin/env bash\nexit 0\n'  > "$d/okno";  chmod +x "$d/okno"
  { printf '#!/usr/bin/env bash\nprev=""; out=""\n'
    printf 'for a in "$@"; do [ "$prev" = "--output-dir" ] && out="$a"; prev="$a"; done\n'
    printf '[ -n "$out" ] && { mkdir -p "$out"; echo x > "$out/1.sql"; }\nexit 0\n'; } > "$d/okwrite"
  chmod +x "$d/okwrite"
  SB="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"  # 自身绝对路径，供子进程整跑
  # 例5（finding 1）: 崩溃二进制 exit 3 → 主路径 rc≠0 且**无 VERDICT 行**（评审注入实证）
  rc=0; bash "$SB" --a "$d/fail3" --b "$d/fail3" --rounds 3 --bench-dir "$d/bench" >"$d/o5" 2>&1 || rc=$?
  { [ "$rc" -ne 0 ] && ! grep -q 'VERDICT' "$d/o5"; } \
    || { echo "selftest FAIL: 例5 崩溃二进制未中止或仍出 VERDICT (rc=$rc)"; r=1; }
  # 例6（finding 4）: exit 0 但零产物 → rc≠0（产物闸）
  rc=0; bash "$SB" --a "$d/okno" --b "$d/okno" --rounds 3 --bench-dir "$d/bench" >"$d/o6" 2>&1 || rc=$?
  [ "$rc" -ne 0 ] || { echo "selftest FAIL: 例6 零产物仍判通过"; r=1; }
  # 例7（闸不误杀好路径）: exit 0 且写产物 → 正常判定 rc∈{0,1} 且有 VERDICT
  rc=0; bash "$SB" --a "$d/okwrite" --b "$d/okwrite" --rounds 3 --bench-dir "$d/bench" >"$d/o7" 2>&1 || rc=$?
  { { [ "$rc" -eq 0 ] || [ "$rc" -eq 1 ]; } && grep -q 'VERDICT' "$d/o7"; } \
    || { echo "selftest FAIL: 例7 好路径被闸误杀或无 VERDICT (rc=$rc)"; r=1; }
  # 例8（finding 2）: --rounds 0 / abc → rc=2（参数守护）
  rc=0; bash "$SB" --a "$d/okwrite" --b "$d/okwrite" --rounds 0   --bench-dir "$d/bench" >/dev/null 2>&1 || rc=$?
  [ "$rc" -eq 2 ] || { echo "selftest FAIL: 例8 --rounds 0 未 rc=2 (=$rc)"; r=1; }
  rc=0; bash "$SB" --a "$d/okwrite" --b "$d/okwrite" --rounds abc --bench-dir "$d/bench" >/dev/null 2>&1 || rc=$?
  [ "$rc" -eq 2 ] || { echo "selftest FAIL: 例8 --rounds abc 未 rc=2 (=$rc)"; r=1; }
  [ $r -eq 0 ] && echo "selftest OK (8 cases)"
  exit $r
fi

A=""; B=""; ROUNDS=5; THREADS=8; CPUS="0-11"
# 评审 finding 3: 每个带值选项先验剩余参数>=2，悬空值（如 `--a` 无值）走 usage(exit 2)，
# 而非 `shift 2` 越界 + set -u 崩(rc=1)。
while [ $# -gt 0 ]; do case "$1" in
  --a) [ $# -ge 2 ] || usage; A="$2"; shift 2;;
  --b) [ $# -ge 2 ] || usage; B="$2"; shift 2;;
  --rounds) [ $# -ge 2 ] || usage; ROUNDS="$2"; shift 2;;
  --threads) [ $# -ge 2 ] || usage; THREADS="$2"; shift 2;;
  --taskset) [ $# -ge 2 ] || usage; CPUS="$2"; shift 2;;
  --bench-dir) [ $# -ge 2 ] || usage; BENCH_DIR="$2"; shift 2;;
  *) usage;;
esac; done
[ -n "$A" ] && [ -n "$B" ] || usage
# 评审 finding 2: --rounds 必须是正整数。`seq 1 0`/`seq 1 abc` 会让采样循环静默跑 0 轮 →
# 空样本 → nan → 假绿 rc=0（契约要求 2）。守护仅拒 空/非数字/零（评审建议式 `*[!1-9]*|0*`
# 会误拒合法值 10/20，故用下面这条正确形：只允全数字且非 0）。THREADS 同型（正整数）。
case "$ROUNDS"  in (''|*[!0-9]*|0) usage;; esac
case "$THREADS" in (''|*[!0-9]*|0) usage;; esac
for b in "$A" "$B"; do [ -x "$b" ] || { echo "not executable: $b" >&2; exit 2; }; done
[ -f "$BENCH_DIR/.bench-ready" ] || { echo "missing $BENCH_DIR/.bench-ready" >&2; exit 2; }
BIN="$(head -1 "$BENCH_DIR/.bench-ready")"
SZ="$(stat -c %s "$BENCH_DIR/$BIN")"
[ "$SZ" -ge 500000000 ] || { echo "bench input $BIN only $SZ bytes" >&2; exit 2; }
[ -f "$BENCH_DIR/schema.json" ] || { echo "missing $BENCH_DIR/schema.json" >&2; exit 2; }

echo "# bench-ab: rounds=$ROUNDS threads=$THREADS taskset=$CPUS input=$BIN($SZ B)"
echo "# governor: $(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo NA) (record-only)"
d="$(mktemp -d)"; trap 'rm -rf "$d"' EXIT
run_side() { # $1=bin $2=输出目录 → stdout wall-clock 秒
  rm -rf "$2" && mkdir -p "$2"
  local t0 t1 rc=0
  t0="$(date +%s.%N)"
  # 评审 finding 1: taskset 在 `$(run_side ...)` 命令替换子壳里失败**不**触发 errexit
  # （bash 5.2.21 实测），旧注释「set-e 任一轮即中止」为假 → 崩溃二进制被记为有效样本。
  # 故显式捕获退出码并 return，让调用点 `|| exit` 生效。
  taskset -c "$CPUS" "$1" to-sql --binlog-dir "$BENCH_DIR" --start-file "$BIN" \
    --schema-file "$BENCH_DIR/schema.json" --time-zone +00:00 \
    --threads "$THREADS" --output-dir "$2" >/dev/null || rc=$?
  t1="$(date +%s.%N)"
  [ "$rc" -eq 0 ] || { echo "to-sql FAILED rc=$rc ($1)" >&2; return "$rc"; }
  # 评审 finding 4: exit-0 但零 SQL 产物的运行不得计为样本。
  find "$2" -type f | grep -q . || { echo "no output files ($1)" >&2; return 1; }
  awk -v a="$t0" -v b="$t1" 'BEGIN{printf "%.4f\n", b-a}'
}
for r in $(seq 1 "$ROUNDS"); do
  # run_side 现显式 return 失败码；此处 `|| exit 1` 自文档化（命令替换赋值失败确实中止，
  # 不再依赖被吞掉的 errexit），任一轮失败即整跑中止（不留半截账）。
  ta="$(run_side "$A" "$d/out-a")" || { echo "round $r (A) failed" >&2; exit 1; }
  tb="$(run_side "$B" "$d/out-b")" || { echo "round $r (B) failed" >&2; exit 1; }
  printf '%s\n' "$ta" >> "$d/a"
  printf '%s\n' "$tb" >> "$d/b"
  echo "SAMPLE r=$r A=${ta}s B=${tb}s"
done
rc=0
verdict "$d/a" "$d/b" || rc=$?
exit $rc
