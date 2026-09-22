#!/usr/bin/env bash
# P4a T1：fuzz 最小闸 —— 每靶 300s×1 轮，crash 归零判据（spec §1）。
# 注（实测修正，报告登记）：cargo-fuzz 0.13 从仓库根解析 <root>/fuzz/Cargo.toml，
# 子 shell `cd fuzz` 会二次拼接成 fuzz/fuzz 而失败（简报已预留「以实测为准修正」）；
# 故 run 在仓库根发起，corpus 路径为 fuzz/corpus/<t>，产物与日志逐靶落 out/fuzz/<t>/。
set -euo pipefail
cd "$(dirname "$0")/.."
TIME="${FUZZ_TIME:-300}"
ROOT=$PWD
for t in decode_event event_stream; do
  mkdir -p "out/fuzz/$t"
  timeout $((TIME + 240)) cargo +nightly fuzz run "$t" \
     "fuzz/corpus/$t" -- "-max_total_time=$TIME" "-artifact_prefix=$ROOT/out/fuzz/$t/" \
     > "out/fuzz/$t/run.log" 2>&1 &
done
wait
n=0
for t in decode_event event_stream; do
  c=$(find "out/fuzz/$t" -maxdepth 1 -type f -name 'crash-*' | wc -l)
  echo "[fuzz-min] $t: crashes=$c"; n=$((n + c))
done
[ "$n" = 0 ] && { echo "[fuzz-min] OK 0 new crashes"; exit 0; }
echo "[fuzz-min] FAIL $n crash artifact(s)"; exit 1
