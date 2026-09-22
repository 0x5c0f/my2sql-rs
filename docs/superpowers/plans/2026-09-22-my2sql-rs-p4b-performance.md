# P4b「性能面」实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把吞吐从「偶发测量」升格为「可判定、可归因、可优化」的面：交付 A/B 判定工装与 profile 普查报告，消费全部在册性能挂账（mimalloc/musl 悬崖、2.5× 扩展上限归因、P2 −3.2% 未决、assembly 搬运、工装杂项），且不触碰输出语义。

**Architecture:** 三并行一串行一收官——T1（tools 工装面）∥ T2（诊断报告面）∥ T4（src/ 结构搬运面）文件面两两不相交，可同时派发；T3（优化）依赖 T1 工装 + T2 报告且与 T4 共 src/ 面 → 在两者之后串行；T5 独占 Makefile/README/HANDOVER/docs/bench 合流收口。

**Tech Stack:** Rust 2024（现有依赖 + mimalloc 0.1.52）、bash 工装脚本、python3 只读 /proc 采样、criterion（不动）、docker mysql（容器产 binlog 不变）。

**Spec:** `docs/superpowers/specs/2026-09-22-my2sql-rs-p4b-performance-design.md`

## Global Constraints

- **基线:** `cargo test` **350 passed / 0 failed / 14 ignored**（main@aba8293 亲跑，`/tmp/p4a-e2e-test.log`）；live repl **13 件**；compat **18/18**；difftest plain **21/21 组**、P4A **14/14**；账本吞吐真值 = **103.85 MiB/s @ threads=8**（P2 态，docs/bench/p1.md:35 + p2.md 注记，>5% 劣化 = 回归红）。
- **输出语义不变量压倒性能:** 任何 src/ 改动必须过「同输入产物逐字节等 + 全量测试绿 + compat 18 + difftest 双模」；P4a 解码器开闸条款扩展至 pipeline 热路径（红钉先行）。
- **判据分层:** criterion = 权威账本（`cargo bench --bench decode`）；`tools/bench-ab.sh` = 判定工具；两者同输入（`data/bench` 528.8 MiB 单档）同 threads=8 同 bench/release 编译档，文档互引。
- **环境事实（本计划编写时实测）:** `perf_event_paranoid=4`（perf CPU 事件被拒，降级路径已在 T2 钉死）；**P 核 = cpu0-11**（scaling_max_freq 5.2/5.4 GHz；cpu12-19 = 4.1 GHz E 域），taskset 一律 `-c 0-11`；governor 全 20 核 powersave 且**无交互 sudo 不可改**（脚本只记录不修改）；`data/bench` 缓存在主仓（`.bench-ready` marker + ≥500MB 校验，worktree 读侧 symlink 合法——gen 脚本仅重生成时写）；`reference/` 必须 `cp -a` 真拷贝（禁 symlink，P4a 实踩）。
- **文件互斥:** T1 = `tools/bench-ab.sh`（新）+ `tools/gen-bench-binlog.sh` + 报告工件；T2 = `tools/bench-profile.sh`（新）+ `docs/bench/p4b-profile.md`（新）；T4 = `src/pipeline/mod.rs` + `src/repl/assembly.rs`（新）+ `src/lib.rs`（一行 mod 声明）；T3 = `Cargo.toml`/`Cargo.lock` + `src/main.rs` + T2 裁定的热路径文件；**四者均不触 Makefile/README/HANDOVER**（T5 独占）。
- 常设纪律: TDD 先红、禁虚账（吞吐/轮次/毫秒逐字对 artifact）、`reference/` 只读、live 容器化测试覆盖 5.6+、破坏性操作限 out//data/ 与自建容器、RSBIN 接线 `${CARGO_TARGET_DIR:-$ROOT/target}` 口径（edb2148 先例）。

## 执行编排

1. **并行波 1:** Task 1、Task 2、Task 4 三路子代理同刻派发（文件面互斥自证）。
2. **串行:** Task 3（需 Task 1 工装可用 + Task 2 报告在手 + Task 4 已合入）。
3. **收官:** Task 5（合流 lane）。
4. 全分支终审 + finishing（ff main + tag `v0.4.1-p4b` + push）。

---

### Task 1: bench-ab 判定工装 + 挂账杂项（#4/#6/#7）

**Files:**
- Create: `tools/bench-ab.sh`
- Modify: `tools/gen-bench-binlog.sh:114`（`./target/debug/my2sql-rs` → RSBIN 接线）
- Create（仓外工件，报告引用）: `/tmp/p4b-t1-*.log`、`/tmp/p4b-t1-ab7.md`

**Interfaces:**
- Produces: `tools/bench-ab.sh --a <bin> --b <bin> [--rounds N] [--threads T] [--taskset CPULIST] [--bench-dir D]` → stdout 逐轮 SAMPLE 行 + `VERDICT: significant|not-significant …`；exit 0=不显著 / 1=显著 / 2=用法或输入错。`--selftest` 子命令 = 统计核 + 守护的无长测自证（exit 0/1）。Task 2/3/5 直接调用本契约。
- Consumes: `data/bench`（`.bench-ready` marker + ≥500MB 档校验，同 `benches/decode.rs::load_input` 口径）。

- [ ] **Step 1: 红 —— selftest 在脚本不存在时失败**

Run: `bash tools/bench-ab.sh --selftest; echo rc=$?`
Expected: `rc=127`（文件不存在）。

- [ ] **Step 2: 写 `tools/bench-ab.sh`（全文）**

```bash
#!/usr/bin/env bash
# P4b-T1（spec §1）端到端 A/B 吞吐判定工装。
# 契约: 同一 data/bench 输入（528.8MiB 单档）上，A/B 两个二进制各交替采
# N=5 轮 to-sql(threads=8) wall-clock；taskset 钉 P 核（实测 cpu0-11 高频域）；
# governor 只记录不修改（powersave 在册、无交互 sudo）。判定 = 中位数 +
# MAD：|Δmedian| > max(2×MAD_pool, 2%) → 显著（exit 1）。criterion 是权威
# 账本，本脚本是判定工具——绝对值受页缓存偏乐观，两侧同窗交替使漂移对称。
# 用法:
#   bench-ab.sh --a <binA> --b <binB> [--rounds 5] [--threads 8] \
#               [--taskset 0-11] [--bench-dir <data/bench>]
#   bench-ab.sh --selftest
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BENCH_DIR="${BENCH_DIR:-$ROOT/data/bench}"
usage() { sed -n '2,12p' "$0"; exit 2; }

# ---- 统计核（纯 awk，--selftest 与真跑共用同一路径） ----
# 输入: 两侧样本（换行分隔秒数） → 输出: median/MAD 行 + verdict 行
verdict() { # $1=A样本文件 $2=B样本文件（SZ 存在则附吞吐行）
  awk -v sz="${SZ:-0}" '
    function median(arr, n,  i, j, t, v) {
      n = asort(arr, v)                       # 升序副本
      return (n % 2) ? v[(n+1)/2] : (v[n/2] + v[n/2+1]) / 2
    }
    function mad(arr, n,  i, d, v, m) {
      m = median(arr, n)
      for (i = 1; i <= n; i++) d[i] = (arr[i] > m) ? arr[i] - m : m - arr[i]
      return median(d, i - 1)
    }
    BEGIN { na = nb = 0 }
    FNR == NR && FILENAME == ARGV[1] { A[++na] = $1; next }
    { B[++nb] = $1 }
    END {
      if (na < 3 || nb < 3) { print "usage: need >=3 samples per side"; exit 2 }
      ma = median(A, na); mb = median(B, nb)
      wa = mad(A, na); wb = mad(B, nb)
      pool = (wa > wb) ? wa : wb
      delta = mb - ma; ad = (delta < 0) ? -delta : delta
      rel = ad / ma * 100
      thr = (2 * pool > 0.02 * ma) ? 2 * pool : 0.02 * ma
      sig = (ad > thr) ? "significant" : "not-significant"
      printf "median A=%.4fs B=%.4fs MAD A=%.4f B=%.4f delta=%+.3f%% (%.4fs) thr=%.4fs\n", ma, mb, wa, wb, delta / ma * 100, ad, thr
      if (sz > 0) printf "throughput MiB/s A=%.1f B=%.1f (input=%d bytes)\n", sz / ma / 1048576, sz / mb / 1048576, sz
      printf "VERDICT: %s (B %s A)\n", sig, (delta > 0 ? "slower-than" : (delta < 0 ? "faster-than" : "equal-to"))
      exit (sig == "significant" ? 1 : 0)
    }' "$1" "$2"
}

if [ "${1:-}" = "--selftest" ]; then
  # 已知漂移必判显著 / 同分布必判不显著 / 小样本守护必 rc=2
  d="$(mktemp -d)"; trap 'rm -rf "$d"' EXIT
  printf '%s\n' 5.00 5.05 4.98 5.02 5.10 > "$d/a1"
  printf '%s\n' 5.60 5.55 5.62 5.58 5.51 > "$d/b1"   # +11%
  printf '%s\n' 5.00 5.02 4.99 5.01 5.03 > "$d/b2"   # ≈0%
  r=0
  verdict "$d/a1" "$d/b1" > "$d/v1" || true; grep -q 'VERDICT: significant' "$d/v1" || { echo "selftest FAIL: ±10% 未判显著"; r=1; }
  verdict "$d/a1" "$d/b2" > "$d/v2"; [ $? -eq 0 ] && grep -q 'VERDICT: not-significant' "$d/v2" || { echo "selftest FAIL: 同分布误判显著"; r=1; }
  printf '%s\n' 5.0 5.1 > "$d/short"
  verdict "$d/short" "$d/short" 2>/dev/null; [ $? -eq 2 ] || { echo "selftest FAIL: 小样本未 rc=2"; r=1; }
  # 反号方向: A 慢 B 快也必须 significant 且 B faster-than
  printf '%s\n' 5.60 5.55 5.62 5.58 5.51 > "$d/a3"
  verdict "$d/a3" "$d/b1" > "$d/v3" || true; grep -q 'VERDICT: significant' "$d/v3" || { echo "selftest FAIL: 反向漂移漏判"; r=1; }
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
BIN="$(cat "$BENCH_DIR/.bench-ready")" || { echo "missing $BENCH_DIR/.bench-ready" >&2; exit 2; }
SZ="$(stat -c %s "$BENCH_DIR/$BIN")"
[ "$SZ" -ge 500000000 ] || { echo "bench input $BIN only $SZ bytes" >&2; exit 2; }

echo "# bench-ab: rounds=$ROUNDS threads=$THREADS taskset=$CPUS input=$BIN($SZ B)"
echo "# governor: $(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo NA) (record-only)"
d="$(mktemp -d)"; trap 'rm -rf "$d"' EXIT
run_side() { # $1=bin $2=outdir
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
  printf '%s\n' "$(run_side "$A" "$d/out-a")" >> "$d/a"
  printf '%s\n' "$(run_side "$B" "$d/out-b")" >> "$d/b"
  echo "SAMPLE r=$r A=$(tail -1 "$d/a")s B=$(tail -1 "$d/b")s"
done
verdict "$d/a" "$d/b"
exit $?
```

注：若 gawk `asort` 不在册（mawk 环境）→ 统计核换纯 bash `sort -n | awk` 中位数两遍扫（selftest 行为必须不变）。

- [ ] **Step 3: 绿 —— selftest**

Run: `bash -n tools/bench-ab.sh && bash tools/bench-ab.sh --selftest; echo rc=$?`
Expected: `selftest OK (4 cases)` + rc=0。

- [ ] **Step 4: 真跑恒等 A/A 冒烟（同一二进制两侧，判不显著）**

前置: `cargo build --release --quiet`（或复用既有 release 产物路径）+ 输入缓存
就位：`[ -e data/bench/.bench-ready ] || ln -s ../../../data/bench data/bench`
（worktree 读侧复用主仓缓存；spec §6 注记——gen 脚本仅重生成时写，读不污染）。
Run: `tools/bench-ab.sh --a target/release/my2sql-rs --b target/release/my2sql-rs --rounds 5 | tee /tmp/p4b-t1-aa.log`
Expected: 5 组 SAMPLE 行 + throughput 行 + `VERDICT: not-significant`，rc=0。逐字入报告。

- [ ] **Step 5: gen-bench-binlog.sh RSBIN 接线（挂账 #6）**

Modify `tools/gen-bench-binlog.sh`: 脚本头部（ROOT 定义之后）加
`RSBIN="${CARGO_TARGET_DIR:-$ROOT/target}/debug/my2sql-rs"`，:114 的
`./target/debug/my2sql-rs to-sql \` 改为 `"$RSBIN" to-sql \`。
Run: `bash -n tools/gen-bench-binlog.sh && CARGO_TARGET_DIR=/tmp/p4b-t1-tgt bash -x tools/gen-bench-binlog.sh 2>&1 | grep -m1 'RSBIN\|/tmp/p4b-t1-tgt/debug/my2sql-rs'`
Expected: trace 行显示走 `/tmp/p4b-t1-tgt/debug`（`-x` 若触发全脚本重跑过重，可用 `bash -n` + `grep -n 'RSBIN' tools/gen-bench-binlog.sh` 双证代替，注记说明）。

- [ ] **Step 6: 挂账 #7 复测 —— P1 vs P2 代码增量钉死**

```bash
# 两个历史 tag 各建 release 二进制（独立 target dir，不碰本分支）
git worktree add /tmp/p4b-ab7-p1 v0.1.0-p1
git worktree add /tmp/p4b-ab7-p2 v0.2.0-p2
(CARGO_TARGET_DIR=/tmp/p4b-ab7-t1 cargo build --manifest-path /tmp/p4b-ab7-p1/Cargo.toml --release -q)
(CARGO_TARGET_DIR=/tmp/p4b-ab7-t2 cargo build --manifest-path /tmp/p4b-ab7-p2/Cargo.toml --release -q)
tools/bench-ab.sh --a /tmp/p4b-ab7-t1/release/my2sql-rs \
                  --b /tmp/p4b-ab7-t2/release/my2sql-rs --rounds 5 \
                  | tee /tmp/p4b-t1-ab7.log
# 显著 → 升级 N=9 复采；仍跨 0/不显著 → 钉死「不显著」
```
结论（含两轮全部 SAMPLE/VERDICT 行逐字）写 `/tmp/p4b-t1-ab7.md`（T5 汇编入
`docs/bench/p4b.md`）。跑完 `git worktree remove` 两临时树。
**挂账 #4**：本计划编写时实测 `grep -n '5\.903' docs/bench/p1.md` **无命中**
（:35 已是 5.093 正确值）→ Step 7 复验并如实入账「挂账虚假/已被后手修正」，
不改文件。

- [ ] **Step 7: 三门 + 报告**

Run: `CARGO_TARGET_DIR=/tmp/p4b-t1-tgt cargo test --no-fail-fast 2>&1 | tail -3`（本 lane 零 src/ 改动，应恒 350/0）+ `bash -n` 两脚本。
报告写 lane report 文件：selftest 输出、A/A 冒烟逐字、ab7 全样本表 + 结论、gen-bench 接线证据、#4 复验结果。Commit: `git add tools/bench-ab.sh tools/gen-bench-binlog.sh && git commit -m "feat(p4b-T1): bench-ab A/B rig (taskset P-cores, median+MAD, selftest) + gen-bench RSBIN wiring + p1-vs-p2 delta re-judgement"`

---

### Task 2: profile 普查（只测不动，产出 T3 裁定输入）

**Files:**
- Create: `tools/bench-profile.sh`
- Create: `docs/bench/p4b-profile.md`

**Interfaces:**
- Consumes: `data/bench` 输入、`target/release/my2sql-rs`（本任务自建）。
- Produces: `docs/bench/p4b-profile.md` —— T3 机动项的**排序表**（每项含 收益预估/风险/验证方式 三列）+ 挂账 #2 假设逐条判定；T3 报告与 HANDOVER 引用该文件名。

- [ ] **Step 1: 红 —— 报告不存在则 T2 未完成（结构闸自证）**

写 `tools/bench-profile.sh` 前先建报告骨架并跑 `grep -c '## 排序表\|## 假设判定\|## 曲线' docs/bench/p4b-profile.md`，骨架下 = 0 → 红；步骤完成后 = 3 → 绿（报告以文本断言代 TDD，判定点 = 三节齐 + 每节有逐字数据）。

- [ ] **Step 2: 写 `tools/bench-profile.sh`（全文）**

```bash
#!/usr/bin/env bash
# P4b-T2 profile 普查（spec §2，只测不动）：threads∈{1,2,4,8} 吞吐曲线
# + perf 可用性探测 + /proc 线程采样（perf_event_paranoid 高时的兜底通道）。
# 产物全落 /tmp/p4b-t2-*，文档只摘录逐字行。taskset 钉 cpu0-11（实测 P 域）。
set -uo pipefail
cd "$(dirname "$0")/.."
BIN="${RSBIN:-target/release/my2sql-rs}"
BENCH_DIR="${BENCH_DIR:-data/bench}"
CPUS="${CPUS:-0-11}"
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

echo "== [1/3] threads 曲线（每档 3 轮，中位数）"
for t in 1 2 4 8; do
  : > "/tmp/p4b-t2-thr$t"; for r in 1 2 3; do one "$t" "/tmp/p4b-t2-out" >> "/tmp/p4b-t2-thr$t"; done
  med="$(sort -n "/tmp/p4b-t2-thr$t" | awk '{a[NR]=$1} END{print a[int((NR+1)/2)]}')"
  awk -v s="$SZ" -v m="$med" -v t="$t" 'BEGIN{printf "threads=%d median=%.3fs MiB/s=%.1f\n", t, m, s/m/1048576}'
done

echo "== [2/3] perf 可用性探测（paranoid 实测值 + 原文拒绝行）"
cat /proc/sys/kernel/perf_event_paranoid
perf record -e cpu-clock -o /tmp/p4b-t2-perf.data -- sleep 0.2 2>&1 | tail -2 || true
# 通过则追加 `perf record -- <run>` 热点采集；被拒 → [3/3] 兜底并在报告注记原文

echo "== [3/3] /proc 线程采样（threads=8 稳态，R/S/D 占比 + busy 等效）"
taskset -c "$CPUS" "$BIN" to-sql --binlog-dir "$BENCH_DIR" --start-file "$BIN_F" \
  --schema-file "$BENCH_DIR/schema.json" --time-zone +00:00 --threads 8 \
  --output-dir /tmp/p4b-t2-out >/dev/null &
pid=$!
python3 - "$pid" <<'PY' > /tmp/p4b-t2-proc.txt
import sys, time, os
pid = sys.argv[1]
tdir = f"/proc/{pid}/task"
clk = os.sysconf("SC_CLK_TCK")
states, busy = {}, 0.0
try:
    for _ in range(600):  # ~30s @ 50Hz（进程先退则早停）
        if not os.path.isdir(tdir): break
        dtot = 0
        for tid in os.listdir(tdir):
            try:
                with open(f"{tdir}/{tid}/stat") as f: s = f.read()
                st = s[s.rindex(")") + 2]
                f2 = s[s.rindex(")") + 2:].split()
                cpu = (int(f2[11]) + int(f2[12])) / clk   # utime+stime
            except (FileNotFoundError, ValueError, IndexError): continue
            states[st] = states.get(st, 0) + 1
            dtot += cpu
        busy = max(busy, dtot)
        time.sleep(0.05)
except KeyboardInterrupt: pass
tot = sum(states.values()) or 1
for st, n in sorted(states.items()):
    print(f"state {st}: {n/tot:.1%}")
print(f"busy-core-equivalent(max) = {busy:.2f}")
PY
wait $pid 2>/dev/null
echo "done: /tmp/p4b-t2-proc.txt"
```

Run: `bash -n tools/bench-profile.sh`（语法绿）。

- [ ] **Step 3: 真跑采集**

前置: `cargo build --release -q`（记 `/usr/bin/time -v` 无关；构建时长不入报告不入账）。
Run: `RSBIN=target/release/my2sql-rs bash tools/bench-profile.sh 2>&1 | tee /tmp/p4b-t2-run.log`
Expected: 四档曲线行（threads=8 应落在 ~5.0-6.5 s 量级——偏离 >15% 时先查环境再写报告）、perf 拒绝/通过原文、采样占比表。全部逐字留存。

- [ ] **Step 4: 写 `docs/bench/p4b-profile.md`**

结构（每节必须有逐字数据，禁空判词）：
1. `## 环境` —— 机器/lscpu 频率分组/paranoid 值/governor/输入（SZ 字节、BIN 名）/构建档。
2. `## 曲线` —— run.log 的四档 MiB/s 行 + **并行效率折算**（threads=8 吞吐 ÷ threads=1，对照 P1 挂账「2.5×」：本役实测 X×，归因结论一句话）。
3. `## perf 面` —— 探测原文；被拒 → 兜底路径声明（/proc 采样 + 下述证据法）；若意外可用，补 folded top-10。
4. `## 假设判定` —— 逐条给「证实/证伪/证据不足」+ 证据行：
   - A1 channel 交接：busy-equivalent ≈ threads 数?（是 → 证伪「等待主导」，指 CPU 饱和/分配）
   - A2 Reorder 等待窗：状态分布中 S 高占比 + 线程数 > CPU 数的形态?
   - A3 行级 Vec/String 分配：CPU 饱和但吞吐不随线程线性 → 分配器/序列化上界（结合 T3 mimalloc 实验互证，此处仅登记推断）
   - A4 文件读 syscall：threads 曲线在 1→2 的跳幅（IO 主导则小跳）
   - A5 锁争用：R% 高但 busy 低?（无 perf 时以曲线形态 + 采样占比给「证据不足」也合法）
5. `## 排序表` —— 优化候选（**至少含**：O1 mimalloc（挂账 #3 关联，预期=消除分配竞争/musl 悬崖，风险低，验证=T1 工装 A/B + musl 复测）；O2..On 由 4 节判定产生，各带 收益/风险/验证 三列。判定为「证据不足」的方向必须标注所需追加实验而非直接排序。

- [ ] **Step 5: 结构闸转绿 + 三门 + commit**

Run: `grep -c '## 排序表\|## 假设判定\|## 曲线' docs/bench/p4b-profile.md` → 3。
`cargo test` 恒 350/0（零 src/）。
Commit: `git add tools/bench-profile.sh docs/bench/p4b-profile.md && git commit -m "bench(p4b-T2): profile survey — threads curve, perf probe (paranoid=4 fallback), /proc thread-state sampling, hypothesis verdicts + ranked candidates"`

---

### Task 3: 优化开闸（mimalloc 固定项 + T2 排序表机动项；在 T1/T2/T4 合入后）

**Files:**
- Modify: `Cargo.toml`（+`mimalloc = "0.1.52"`）、`Cargo.lock`、`src/main.rs`
- Modify: T2 排序表 top-1..2 对应的热路径 src/ 文件（机动，裁定驱动）
- Test: `tests/` 现有全量 + 机动项新红钉（按裁定内容定形）

**Interfaces:**
- Consumes: `tools/bench-ab.sh`（T1 契约）、`docs/bench/p4b-profile.md` 排序表（T2）、Task 4 已合入的稳定结构。
- Produces: 性能结论行（VERDICT 逐字）入报告 → T5 汇编 `docs/bench/p4b.md`。

- [ ] **Step 1: 红 —— mimalloc 未接入时断言失败**

`grep -n 'global_allocator' src/main.rs` → 无命中（红）；同时 `cargo tree | grep mimalloc` → 空。

- [ ] **Step 2: 接入 mimalloc（最小改动）**

`Cargo.toml` `[dependencies]` 加 `mimalloc = "0.1.52"`；`src/main.rs` 顶部：

```rust
/// P4b-T3（spec §3 固定项）：mimalloc 全局分配器——P1 musl 悬崖（3.4 MiB/s，
/// musl malloc arena 竞争）候选解 + glibc 侧 A/B 实测定方向。收益裁定入档。
#[global_allocator]
static GLOBAL: mimalloc::Mimalloc = mimalloc::Mimalloc;
```

Run: `CARGO_TARGET_DIR=/tmp/p4b-t3-tgt cargo build --release -q && CARGO_TARGET_DIR=/tmp/p4b-t3-tgt cargo test --no-fail-fast 2>&1 | tail -2`
Expected: 编译绿 + **350/0**（语义零变化首证）。

- [ ] **Step 3: glibc A/B（对照 base = 本任务开工前的合入 tip）**

开工第一动作记录 `BASE_T3=$(git rev-parse HEAD)`（= T1/T2/T4 全部合入后、
T3 任何 commit 前的 tip，派发 brief 亦会携带该值）。

```bash
git worktree add --detach /tmp/p4b-t3-base "$BASE_T3"
CARGO_TARGET_DIR=/tmp/p4b-t3-tbase cargo build --manifest-path /tmp/p4b-t3-base/Cargo.toml --release -q
tools/bench-ab.sh --a /tmp/p4b-t3-tbase/release/my2sql-rs \
                  --b target/release/my2sql-rs --rounds 5 | tee /tmp/p4b-t3-ab.log
```
Expected: 两侧产物**逐字节语义等**另证（Step 5）；VERDICT 方向如实入账（显著快/不显著/显著慢均合法，慢 >2% 显著 → 撤该接入并登记裁定）。

- [ ] **Step 4: musl 复测（挂账 #3）**

```bash
CARGO_TARGET_DIR=/tmp/p4b-t3-musl cargo build --release --target x86_64-unknown-linux-musl -q || echo "MUSL-BUILD-FAIL rc=$?"
# 过 → 同一 to-sql 调用 taskset 单跑 ×3 取中位（bench-ab 的 --a/--b 均可喂
# 同 target 无妨；或直接用 bench-profile.sh 的 one() 同型内联跑）：
taskset -c 0-11 /tmp/p4b-t3-musl/x86_64-unknown-linux-musl/release/my2sql-rs to-sql \
  --binlog-dir data/bench --start-file "$(cat data/bench/.bench-ready)" \
  --schema-file data/bench/schema.json --time-zone +00:00 --threads 8 \
  --output-dir /tmp/p4b-t3-musl-out
```
基线对照 = 同 worktree 链上 `--no-default-features` 不可用（mimalloc 是硬依赖），
故 base musl = base worktree（无 mimalloc）同法 musl 构建 ×3。判定：mimalloc 后
musl ≥50 MiB/s → 悬崖消账；否则处置裁定 = 发布面口径书面化（glibc 发行 +
musl 仅功能面，挂账转「已裁定不修」）。构建失败（musl-gcc 缺失等）→ 环境事实
入报告 + 挂账续挂书面化，**不静默跳过**。

- [ ] **Step 5: 语义恒等硬证（mimalloc 也不许动语义）**

用 `data/8.0` 现存 binlog（或重跑 `bash tools/run-difftest.sh` 的产物）：
base 与 new 各跑 `to-sql` + `flashback`（同参，`--time-zone +00:00 --threads 4 --add-extra-info`）→ `diff -r` 全等。

- [ ] **Step 6: 机动项（T2 排序表裁定驱动）**

对排序表中「低风险 ∧ 已证实」top-1..2 逐项独立 commit：先红钉（行为恒等测试
+ 该项专属吞吐假设的 bench-ab 预期）→ 实现 → `tools/bench-ab.sh` 判「显著
改善」→ 全门禁。**无达标项 → 本步 = 零改动 + 裁定入报告**（spec §3 允许零胜，
禁为动而动；每项失败候选如实登记「尝试过 + 回滚 + 原因」）。

- [ ] **Step 7: 全门禁 + commit**

`cargo test`（≥350，机动项可增不可减）+ `cargo clippy --all-targets -- -D warnings` + `cargo fmt --check` + `make compat`（18/18）+ `bash tools/run-difftest.sh`（plain 绿）+ `P4A=1 bash tools/run-difftest.sh`（14/14）。
Commit（分笔）: `perf(p4b-T3): mimalloc global allocator — glibc A/B <dir> + musl <消账/裁定>`；机动项各一 commit `perf(p4b-T3): <项名>（判据=bench-ab significant）`。

---

### Task 4: pipeline repl 装配块搬运 → src/repl/assembly.rs（纯 move）

**Files:**
- Create: `src/repl/assembly.rs`
- Modify: `src/pipeline/mod.rs`（删移动项 + re-export）、`src/repl/mod.rs` 或 `src/lib.rs`（`pub mod assembly;` 一行）

**Interfaces:**
- Consumes: 现有 `my2sql_rs::pipeline::run_repl` 调用面（src/main.rs import）。
- Produces: 同名同路径符号（对 crate 外零感知）；`src/repl/assembly.rs` 内 `pub(crate)` 项由 pipeline 原位置 `pub use` 转出。

- [ ] **Step 1: 划界（先证后搬，边界入报告）**

对 `src/pipeline/mod.rs` :265-:1766 区间的顶层项（`BACKOFF_CAP_SECS`/`FAST_FAIL_LIMIT`/`Locate`/`decide_locate`/`FailKind`/`FailReport`/`classify_repl_failure`/`FailureTracker`/`reconnect_backoff_secs`/`jitter_unit`/`reconnect_warn`/`bisect_index`/`PROBE_FIRST_TS_CAP`/`HEARTBEAT_KINDS`/`ProbeTap`/`probe_consume`/`probe_first_ts`/`Pumper`/`ReplEnv`/`cp_start_for_retry`/`locate_fresh`/`run_repl_with`/`run_repl`/`Runner`）逐项 `grep -n '\bNAME\b' src/ -r`：**仅被 repl 路径引用 → 移动项；与 to-sql/flashback/stats 共享（如 Runner 若 file 模式亦用）→ 留守**。移动集合大小预期 ≈570 逻辑行；实际以引用图为准，报告列「移动/留守/理由」三列清单（这是对本任务边界的裁定，评审据此核 move-only）。

- [ ] **Step 2: 搬运**

整段剪切（**禁顺手改写**——函数体零触碰，评审以 `git diff` 核「删除块 ≡ 新增块」）入 `src/repl/assembly.rs`，头部补 `use`；pipeline/mod.rs 原位置 `pub(crate) use crate::repl::assembly::{…}`（run_repl 需 `pub use`，main.rs import 路径不变）。单元测试 `mod repl_tests`/`mod live_tests`（pipeline/mod.rs:1767/2310 起）随被移动代码**整块迁 assembly.rs** 的 `#[cfg(test)] mod`（测试体零触碰）。
Run: `CARGO_TARGET_DIR=/tmp/p4b-t4-tgt cargo test --no-fail-fast 2>&1 | tail -2`
Expected: **350/0/14 计数不变**（测试搬家但数量恒等 = 搬家无损的硬证）。

- [ ] **Step 3: 行为恒等 —— 产物逐字节**

`cargo build --release -q`（new）+ base worktree 同法 → 同输入（`data/8.0` 现存 binlog 或 difftest 产物）各跑 to-sql + flashback（同参），`diff -r` 全等（repl live 等价由 Step 2 测试 + T5 全量 repl 13 件兜底，本任务不占共享容器）。

- [ ] **Step 4: bench-ab 抽测不退化**

Run: `tools/bench-ab.sh --a <base release bin> --b <new release bin> --rounds 3`（3 轮 = 抽测档；T1 已合入才有此工具）
Expected: `VERDICT: not-significant`（搬运不改热路径语义与 codegen 预期；若显著慢 → 查 `inline`/单态化受模块边界影响，调整 `pub(crate)` 面而非接受退化）。

- [ ] **Step 5: 三门 + commit**

clippy/fmt 绿。Commit: `refactor(p4b-T4): move repl assembly (~<N> logical lines, per ref-graph) from pipeline/mod.rs to src/repl/assembly.rs — move-only, byte-identity + test-count-350 proven`

---

### Task 5: 合流收官（T5 独占文档面 + 全量回归 + 新基线）

**Files:**
- Modify: `Makefile`（`bench-ab`/`bench-profile` 两目标行 + `.PHONY`）
- Modify: `README.md`（性能行：新基线 + 工装存在性）
- Create: `docs/bench/p4b.md`（新基线记录 + 挂账 #7 结论汇编 + mimalloc A/B + 回归闸判定）
- Modify: `docs/HANDOVER.md`（P4b 节点日志 + §0 七条挂账处置对账 + 状态行）

**Interfaces:**
- Consumes: T1-T4 全部 lane report + 逐字日志。
- Produces: P4b DoD 对账（spec §7 七条）+ 合流亲跑证据。

- [ ] **Step 1: Makefile 两行**

```make
## P4b 性能面入口（env 透传：A/B/ROUNDS/RSBIN/TASKSET 归脚本自身）
bench-ab:
	bash tools/bench-ab.sh $(ARGS)
bench-profile:
	bash tools/bench-profile.sh
```
`.PHONY` 追加两者；`make -n bench-ab bench-profile` 验。

- [ ] **Step 2: `docs/bench/p4b.md`**

汇编（逐字，禁手改数字）：① 机器与工装口径（引 p4b-profile 环境节）；② **正式
criterion 基线**：`cargo bench --bench decode` 终态跑（mimalloc/机动项落地后），
threads=8 组 median → MiB/s 与 **vs 103.85 回归闸判定**（>5% 劣化 = 红 STOP 上报）；
③ T1 ab7（P1 vs P2）结论段；④ mimalloc glibc A/B + musl 复测数字与裁定；
⑤ 机动项各笔 verdict 行（或零机动裁定）；⑥ T4 A/A' 抽测行。

- [ ] **Step 3: README 性能行 + HANDOVER P4b 全节点**

README 矩阵/性能行更新（如有吞吐引用 → 指 p4b.md）；HANDOVER 新增
「P4b 任务节点日志（T1–T5）」各 lane 一节（交付/证据逐字/Ruling）+
「P4b DoD 对账（spec §7 七条）」+ §0 挂账处置表逐行销账（含 #4「实测无
5.903，挂账虚假」注记）。

- [ ] **Step 4: 全量回归六闸逐字入档**

`CARGO_TARGET_DIR=/tmp/p4b-merge` 下：① `cargo test`（≥350）+ clippy + fmt；
② `FUZZ_TIME=20 make fuzz-min`；③ `make shadow-test`（8.0）；④
`make difftest` + `P4A=1 make difftest`；⑤ `make compat`（18）；⑥
**`make repl-test` 13 件全跑**（src/ 动过 = 硬条件）。逐字日志 + 时间戳入
HANDOVER 节点（禁虚账）。

- [ ] **Step 5: commit + 状态行收口**

`git add Makefile README.md docs/bench/p4b.md docs/HANDOVER.md && git commit -m "docs(p4b): T5 merge lane — make entries, p4b baseline, HANDOVER nodes + 挂账 disposition, full regression transcript"`

---

## 自检记录（writing-plans §Self-Review）

1. **Spec 覆盖:** §1→T1（含 #4/#6/#7 杂项）、§2→T2、§3→T3（mimalloc 固定 +
   机动规则化）、§4→T4、§5→T5、§6→Global Constraints、§7→各 Task 步骤 + T5
   对账。七条挂账 ↔ 任务映射见 spec §0 表，每条有消费步骤。无缺。
2. **占位扫描:** T3 Step 6 机动项 = 真·裁定依赖（T2 报告未产，形不可预写），
   已给死规则（低风险∧证实、红钉→实现→显著判定→门禁、零胜合法）非占位；
   T1 Step 5 `-x` 兜底注明替代证据形。其余步骤全代码/全命令。
3. **类型一致:** bench-ab 契约（exit 0/1/2、VERDICT 行形）三处引用一致；
   `data/bench/.bench-ready` + ≥500MB 与 `benches/decode.rs::load_input` 同
   口径；taskset `0-11`（实测 P 域）全文一致；RSBIN `${CARGO_TARGET_DIR:-$ROOT/target}` 同 edb2148 先例。
