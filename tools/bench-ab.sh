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
  sort -n "$@" | awk '{v[NR]=$1} END{
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
  # exit: 0=not-significant / 1=significant / 2=样本不足(<3/侧)
  local na nb ma mb wa wb
  na="$(awk 'END{print NR}' "$1")"; nb="$(awk 'END{print NR}' "$2")"
  if [ "$na" -lt 3 ] || [ "$nb" -lt 3 ]; then
    echo "usage: need >=3 samples per side (A=$na B=$nb)"
    return 2
  fi
  ma="$(_median "$1")"; mb="$(_median "$2")"
  wa="$(_mad "$1")";   wb="$(_mad "$2")"
  awk -v ma="$ma" -v mb="$mb" -v wa="$wa" -v wb="$wb" -v sz="${SZ:-0}" '
    BEGIN {
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
  [ $r -eq 0 ] && echo "selftest OK (4 cases)"
  exit $r
fi

A=""; B=""; ROUNDS=5; THREADS=8; CPUS="0-11"
while [ $# -gt 0 ]; do case "$1" in
  --a) A="$2"; shift 2;; --b) B="$2"; shift 2;;
  --rounds) ROUNDS="$2"; shift 2;; --threads) THREADS="$2"; shift 2;;
  --taskset) CPUS="$2"; shift 2;; --bench-dir) BENCH_DIR="$2"; shift 2;;
  *) usage;;
esac; done
[ -n "$A" ] && [ -n "$B" ] || usage
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
  local t0 t1
  t0="$(date +%s.%N)"
  taskset -c "$CPUS" "$1" to-sql --binlog-dir "$BENCH_DIR" --start-file "$BIN" \
    --schema-file "$BENCH_DIR/schema.json" --time-zone +00:00 \
    --threads "$THREADS" --output-dir "$2" >/dev/null
  t1="$(date +%s.%N)"
  awk -v a="$t0" -v b="$t1" 'BEGIN{printf "%.4f\n", b-a}'
}
for r in $(seq 1 "$ROUNDS"); do
  ta="$(run_side "$A" "$d/out-a")"   # set -e: 任一轮失败即中止（不留半截账）
  tb="$(run_side "$B" "$d/out-b")"
  printf '%s\n' "$ta" >> "$d/a"
  printf '%s\n' "$tb" >> "$d/b"
  echo "SAMPLE r=$r A=${ta}s B=${tb}s"
done
rc=0
verdict "$d/a" "$d/b" || rc=$?
exit $rc
