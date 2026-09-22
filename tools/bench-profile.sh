#!/usr/bin/env bash
# P4b-T2 profile 普查（spec §2，只测不动）：threads∈{1,2,4,8} 吞吐曲线
# + perf 可用性探测 + /proc 线程采样（perf_event_paranoid 高时的兜底通道）。
# 产物全落 /tmp/p4b-t2-*，文档只摘录逐字行。taskset 钉 cpu0-11（实测 P 域）。
#
# 相对任务书骨架的修正（均记入报告「方法注记」）：
#  1) 采样器 busy 等效改为**相邻 tick 差值/间隔**（原 `busy=max(dtot)` 取的是
#     进程寿命内累计 CPU 秒，量纲随运行时长增长，不是"核等效"；用 `yes`
#     进程自证时原式给 ≈N 秒而非 ≈1 核）。
#  2) 采样器按 **comm 匹配全部存活 my2sql-rs 进程**再遍历其 task/（原式锁
#     单个 pid；threads=8 单轮仅 ~6s，撑不满 ≥30s 稳态窗口，改为连跑 RUNS
#     轮，采样器跨进程跟踪）。
#  3) 字段解析仍用 rindex(")") 锚定（comm 可含空格/括号），utime/stime 为
#     post-comm remainder 0-based idx 11/12（= 全行 field 14/15），与任务书
#     一致，自证通过。
set -uo pipefail
cd "$(dirname "$0")/.."
BIN="${RSBIN:-target/release/my2sql-rs}"
BENCH_DIR="${BENCH_DIR:-data/bench}"
CPUS="${CPUS:-0-11}"
RUNS="${RUNS:-6}"      # threads=8 采样连跑轮数（单轮 ~6s，6 轮 ≥30s 稳态窗口）
PROC="${PROC:-my2sql-rs}" # 采样器按 comm 匹配的进程名（自证时传 yes）
SECS="${SECS:-32}"     # 采样窗口秒数
BIN_F="$(cat "$BENCH_DIR/.bench-ready")"
SZ="$(stat -c %s "$BENCH_DIR/$BIN_F")"

one() { # $1=threads $2=outdir → 打印秒数
  rm -rf "$2" && mkdir -p "$2"
  local t0 t1; t0="$(date +%s.%N)"
  taskset -c "$CPUS" "$BIN" to-sql --binlog-dir "$BENCH_DIR" --start-file "$BIN_F" \
    --schema-file "$BENCH_DIR/schema.json" --time-zone +00:00 --threads "$1" \
    --output-dir "$2" >/dev/null
  t1="$(date +%s.%N)"; awk -v a="$t0" -v b="$t1" 'BEGIN{printf "%.3f\n", b-a}'
}

echo "== [0/3] 环境事实登记（paranoid/governor/频率域/输入）"
{
  echo "uname: $(uname -srvmo)"
  echo "model: $(grep -m1 'model name' /proc/cpuinfo)"
  echo "perf_event_paranoid: $(cat /proc/sys/kernel/perf_event_paranoid)"
  echo "governors: $(cat /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor | sort -u | tr '\n' ' ')"
  echo "maxfreq 0-11: $(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_max_freq) $(cat /sys/devices/system/cpu/cpu11/cpufreq/scaling_max_freq)"
  echo "maxfreq 12-19: $(cat /sys/devices/system/cpu/cpu12/cpufreq/scaling_max_freq) $(cat /sys/devices/system/cpu/cpu19/cpufreq/scaling_max_freq)"
  echo "taskset CPUS=$CPUS  BIN=$BIN  input=$BENCH_DIR/$BIN_F  SZ=$SZ bytes"
  echo "loadavg(运行前): $(cat /proc/loadavg)"
} | tee /tmp/p4b-t2-env.txt

echo "== [1/3] threads 曲线（每档 3 轮，中位数）"
for t in 1 2 4 8; do
  : > "/tmp/p4b-t2-thr$t"; for r in 1 2 3; do one "$t" "/tmp/p4b-t2-out" >> "/tmp/p4b-t2-thr$t"; done
  med="$(sort -n "/tmp/p4b-t2-thr$t" | awk '{a[NR]=$1} END{print a[int((NR+1)/2)]}')"
  awk -v s="$SZ" -v m="$med" -v t="$t" 'BEGIN{printf "threads=%d median=%.3fs MiB/s=%.1f\n", t, m, s/m/1048576}'
done

echo "== [2/3] perf 可用性探测（paranoid 实测值 + 原文拒绝行）"
cat /proc/sys/kernel/perf_event_paranoid
{
  echo "\$ perf record -e cpu-clock -o /tmp/p4b-t2-perf.data -- sleep 0.2"
  perf record -e cpu-clock -o /tmp/p4b-t2-perf.data -- sleep 0.2 2>&1
  echo "rc_record=$?"
  echo "\$ perf stat true"
  perf stat true 2>&1
  echo "rc_stat=$?"
} | tee /tmp/p4b-t2-perfprobe.txt
# 通过则追加 `perf record -- <run>` 热点采集；被拒 → [3/3] 兜底并在报告注记原文
if [ "${PERF_OK:-0}" = 1 ]; then
  taskset -c "$CPUS" perf record -o /tmp/p4b-t2-perf-run.data -- \
    taskset -c "$CPUS" "$BIN" to-sql --binlog-dir "$BENCH_DIR" --start-file "$BIN_F" \
    --schema-file "$BENCH_DIR/schema.json" --time-zone +00:00 --threads 8 \
    --output-dir /tmp/p4b-t2-out >/dev/null 2>&1
  perf report -i /tmp/p4b-t2-perf-run.data --no-children --stdio 2>/dev/null | head -30 | tee /tmp/p4b-t2-perf-top.txt
fi

echo "== [3/3] /proc 线程采样（threads=8 稳态连跑 ${RUNS} 轮 ≥30s，R/S/D 占比 + busy 等效）"
( for i in $(seq "$RUNS"); do one 8 /tmp/p4b-t2-out >> /tmp/p4b-t2-thr8-sample.log; done ) &
runner=$!
python3 - "$PROC" "$SECS" <<'PY' > /tmp/p4b-t2-proc.txt
import sys, time, os, glob
proc, secs = sys.argv[1], float(sys.argv[2])
clk = os.sysconf("SC_CLK_TCK")
t0 = time.monotonic()
states, prev, ptime, busy, ticks, maxtid = {}, 0.0, None, 0.0, 0, 0
perints = []                                 # 每有效 tick 的核等效（差值法）
while time.monotonic() - t0 < secs:
    if not glob.glob("/proc/[0-9]*/comm"): break
    ssum, ntid = 0.0, 0
    for cpath in glob.glob("/proc/[0-9]*/comm"):
        try:
            with open(cpath) as f:
                if f.read().strip() != proc: continue
            pid = cpath.split("/")[2]
            try:
                tids = os.listdir(f"/proc/{pid}/task")
            except OSError:
                continue
            for tid in tids:
                try:
                    with open(f"/proc/{pid}/task/{tid}/stat") as f: s = f.read()
                    i = s.rindex(")")              # comm 可含括号 → rindex 锚定
                    st = s[i + 2]                  # 状态字符（全行 field 3）
                    f2 = s[i + 2:].split()         # f2[0]=state
                    ssum += (int(f2[11]) + int(f2[12])) / clk  # utime/stime=field14/15
                except (FileNotFoundError, ProcessLookupError, ValueError, IndexError):
                    continue
                states[st] = states.get(st, 0) + 1
                ntid += 1
        except OSError:
            continue
    now = time.monotonic()
    if ptime is not None:
        dt = now - ptime
        if 0 < dt < 0.25:                          # 跨进程更替/长扫描 tick 跳过分差
            b = (ssum - prev) / dt
            busy = max(busy, b)                    # 差值法核等效
            if b >= 0: perints.append(b)
    prev, ptime, ticks = ssum, now, ticks + 1
    maxtid = max(maxtid, ntid)
    time.sleep(0.05)
tot = sum(states.values()) or 1
for st, n in sorted(states.items()):
    print(f"state {st}: {n/tot:.1%} ({n})")
print(f"ticks={ticks} wall={time.monotonic()-t0:.1f}s max-threads-seen={maxtid}")
if perints:
    perints.sort()
    print(f"busy-core-equivalent(median) = {perints[len(perints)//2]:.2f} (n={len(perints)})")
print(f"busy-core-equivalent(max) = {busy:.2f}")
PY
kill "$runner" 2>/dev/null; wait "$runner" 2>/dev/null
echo "done: /tmp/p4b-t2-proc.txt"
