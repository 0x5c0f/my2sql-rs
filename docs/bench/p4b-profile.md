# my2sql-rs P4b-T2 profile 普查报告（只测不动 · T3 裁定输入）

**日期:** 2026-09-22 · **lane:** P4b-T2（spec §2 Lane P）· **基线:** 分支 tip `c59def0`
**采集工装:** `tools/bench-profile.sh`（本役新建，零改既有文件）· **红线遵守:** 本 lane 对 `src/` 零改动（构建/测试面见「方法注记 M1」）。

三节结构闸（曲线/假设判定/排序表）:骨架期实测输出 `0`（红）→ 本文件完成后输出 `3`（绿）。两次 grep 的命令行全文（模式含三个二级标题子串，正文若逐字引用会自匹配把计数抬到 4）存 `/tmp/p4b-t2-gate-red.log` 与 `/tmp/p4b-t2-gate-green.log`，其输出行逐字为:

```text
0        ← /tmp/p4b-t2-gate-red.log（骨架期）
3        ← /tmp/p4b-t2-gate-green.log（完成后）
```

## 环境

```text
uname: Linux 6.17.0-35-generic #35~24.04.1-Ubuntu SMP PREEMPT_DYNAMIC Tue May 26 19:30:42 UTC 2 x86_64 GNU/Linux
model: model name	: 13th Gen Intel(R) Core(TM) i9-13900H
perf_event_paranoid: 4
governors: powersave
maxfreq 0-11: 5200000 5200000
maxfreq 12-19: 4100000 4100000
taskset CPUS=0-11  BIN=/tmp/p4b-t2-tgt/release/my2sql-rs  input=data/bench/mysql-bin.000003  SZ=554555214 bytes
loadavg(运行前): 0.95 1.21 3.64 2/2877 4044257
```

（以上逐字 = `/tmp/p4b-t2-env.txt`。）补充在册环境事实:

- CPU 频率域按 `scaling_max_freq` 分组:**cpu0-11 = 5.2/5.4 GHz（P 域）、cpu12-19 = 4.1 GHz（E 域）**;governor 全 20 核 `powersave`,**只记录不修改**（无交互 sudo 在册）。全部计时 `taskset -c 0-11` 钉 P 域。
- `/proc/sys/kernel/perf_event_paranoid` 实测值 = `4`（见上行与 perf 面拒绝原文）。
- 输入 = 主仓 528.9 MiB bench 缓存（worktree 读侧 symlink `data/bench` → 主仓，`.bench-ready` marker = `mysql-bin.000003`,554555214 B，未重生成）。
- 构建档:`git archive c59def0` 干净导出至 `/tmp/p4b-t2-head-src`,`CARGO_TARGET_DIR=/tmp/p4b-t2-tgt cargo build --release -q`;产物 `/tmp/p4b-t2-tgt/release/my2sql-rs`（md5 `2767692ba8d5ab7e4f6ed80e40d39632`，2026-09-22 13:29 构建）。**不用脏树构建**——原因见 M1。
- 计时窗杂患处置:采集前检测到并行 lane 的 rustc/my2sql-rs 高负载（loadavg 峰 30.1），等待其退场且 loadavg 稳定 <2（运行前 0.95）后才开表;`pi` 进程恒定 ~30-35% 单核，为该机背景基线，两侧同环境。

## 曲线

threads ∈ {1,2,4,8}，每档 3 轮 wall-clock，中位数;MiB/s = SZ/median/2^20。四档行逐字（`/tmp/p4b-t2-run.log` [1/3] 段;各档 3 轮原始秒数 = `/tmp/p4b-t2-thr{1,2,4,8}`）:

```text
threads=1 median=12.288s MiB/s=43.0
threads=2 median=10.044s MiB/s=52.7
threads=4 median=7.069s  MiB/s=74.8
threads=8 median=6.125s  MiB/s=86.3
```

```text
thr1: 11.378 12.288 13.345
thr2: 10.362 10.044 9.988
thr4: 7.047 7.069 7.122
thr8: 6.641 6.125 6.018
```

threads=8 median 6.125 s 落在预期 ~5-7 s 带内（未偏离 >15%，无需环境异常调查）。

**并行效率折算:** threads=8 MiB/s ÷ threads=1 MiB/s = 86.3/43.0 = **2.01×**（时间口径 12.288/6.125 = 2.006×，一致）。对照 P1 挂账「2.5×」（`docs/bench/p1.md:54`「threads 1→8 仅 **2.5×**（非 8×）」，其口径 12.783 s→5.093 s = 2.51×）:**本役实测 2.01×，低于 P1 在册值约 0.5 个倍数点**。归因拆解:t1 侧反而略快（43.0 vs 41.4 MiB/s，+3.9%），t8 侧慢 17%（86.3 vs 103.9 MiB/s，median 6.125 vs 5.093 s）→ 效率收缩全部来自 **t8 档绝对值回退**（P1→本役 tip 之间 threads>1 路径的形态变化，本普查只登记不裁定，处置见「需追加实验」X3）。曲线形态本身:1→2 仅 1.23×，2→4 1.42×，4→8 1.15×——次线性且早早趋平，与假设判定节结论（CPU 工作量超线性膨胀，非等待主导）一致。

## perf 面

`perf_event_paranoid` 实测 `4`。探测命令与拒绝原文逐字（`/tmp/p4b-t2-perfprobe.txt`，同 run.log [2/3] 段）:

```text
$ perf record -e cpu-clock -o /tmp/p4b-t2-perf.data -- sleep 0.2
Missing support for build id in kernel mmap events.
Disable this warning with --no-buildid-mmap
Error:
Access to performance monitoring and observability operations is limited.
Consider adjusting /proc/sys/kernel/perf_event_paranoid setting to open
access to performance monitoring and observability operations for processes
without CAP_PERFMON, CAP_SYS_PTRACE or CAP_SYS_ADMIN Linux capability.
More information can be found at 'Perf events and tool security' document:
https://www.kernel.org/doc/html/latest/admin-guide/perf-security.html
perf_event_paranoid setting is 4:
  -1: Allow use of (almost) all events by all users
      Ignore mlock limit after perf_event_mlock_kb without CAP_IPC_LOCK
>= 0: Disallow raw and ftrace function tracepoint access
>= 1: Disallow CPU event access
>= 2: Disallow kernel profiling
To make the adjusted perf_event_paranoid setting permanent preserve it
in /etc/sysctl.conf (e.g. kernel.perf_event_paranoid = <setting>)
rc_record=255
$ perf stat true
Error:
Access to performance monitoring and observability operations is limited.
Consider adjusting /proc/sys/kernel/perf_event_paranoid setting to open
access to performance monitoring and observability operations for processes
without CAP_PERFMON, CAP_SYS_PTRACE or CAP_SYS_ADMIN Linux capability.
More information can be found at 'Perf events and tool security' document:
https://www.kernel.org/doc/html/latest/admin-guide/perf-security.html
perf_event_paranoid setting is 4:
  -1: Allow use of (almost) all events by all users
      Ignore mlock limit after perf_event_mlock_kb without CAP_IPC_LOCK
>= 0: Disallow raw and ftrace function tracepoint access
>= 1: Disallow CPU event access
>= 2: Disallow kernel profiling
To make the adjusted perf_event_paranoid setting permanent preserve it
in /etc/sysctl.conf (e.g. kernel.perf_event_paranoid = <setting>)
rc_stat=255
```

**降级路径声明:** CPU 事件采样被拒（rc=255），无 folded top-10 可用。本普查热点证据法 = **/proc 线程状态采样（差值法 busy 核等效）+ 曲线形态**双通道，函数级热点在本档中**缺位**（这是 paranoid=4 环境的如实处置，非遗漏;T3 若需函数级归因，唯一在册通道是 criterion `--profile` 或本文件附录的推断 + A/B 实验判定，禁止把推断写成实测）。

threads=8 稳态采样（连跑通道，`/tmp/p4b-t2-proc.txt`）。threads=8 单轮仅 ~6.1 s，单进程撑不满 ≥30 s 窗口，故以同参数连跑（`RUNS=6`，32.1 s 窗口内完成 4 轮:`6.151 6.293 6.848 6.716`，`/tmp/p4b-t2-thr8-sample.log`，窗口到时收尾）:

```text
state D: 0.0% (1)
state R: 89.0% (2923)
state S: 11.0% (362)
ticks=341 wall=32.1s max-threads-seen=10
busy-core-equivalent(median) = 8.84 (n=335)
busy-core-equivalent(max) = 9.31
```

threads=1 对照采样（12 s 窗口，`/tmp/p4b-t2-proc-thr1.txt`）:

```text
state R: 100.0% (188)
ticks=200 wall=12.0s max-threads-seen=1
busy-core-equivalent(median) = 0.99 (n=198)
busy-core-equivalent(max) = 1.28
```

**核心派生量（由上两档逐字数据相乘，非新测量）:** 全负载 CPU 工作量 threads=1 ≈ 0.99×12.288 = **12.2 core·s**;threads=8 ≈ 8.84×6.125 = **54.2 core·s** → 8 线程档 **CPU 工作量膨胀 4.45×** 而换到的吞吐只有 2.01×。墙钟 halving 的背后是核时烧在了超线性成本上（分配/释放快路径、跨线程缓存一致性流量、glibc arena 自旋等 on-CPU 项——无 perf 不能点名，点名即虚账）。

## 假设判定

（挂账 #2 串行点假设清单，spec §2 ③。每条 = 裁定词 + 证据行。）

### A1 channel 交接（等待主导）—— **证伪**

判据原文:busy-equivalent ≈ threads 数?（是 → 证伪「等待主导」，指 CPU 饱和/分配）。
证据:`busy-core-equivalent(median) = 8.84`，`max-threads-seen=10`（dispatcher + 8 worker + 源中转线程），`state R: 89.0%` / `state S: 11.0%`——worker 群几乎全程在核上运行，不是堵在通道 recv 的睡眠态。**8.84 ≈ 可忙线程数（扣除恒半睡的源线程）→ 等待不主导 → 证伪;矛头按判据原文转向 CPU 饱和/分配（接 A3）。**

### A2 Reorder 等待窗 —— **证伪**

判据:状态分布中 S 高占比 + 线程数 > CPU 数的形态?
证据:S 全进程合计仅 `11.0% (362)`（10 线程 ×341 tick 满额 R 的话应为 3410 样本），`state D: 0.0% (1)`;`max-threads-seen=10` < 钉住的 12 个 P 核，无线程超订形态;反压路径（`reorder.pending() > 2×threads` 阻塞补收）若常开，dispatcher 会呈可观 S/D 占比，未见。**两形态俱无 → 证伪（作为主导瓶颈）。**

### A3 行级 Vec/String 分配（分配器/序列化上界）—— **证实（推断级，待 T3 mimalloc A/B 互证）**

判据:CPU 饱和但吞吐不随线程线性。
证据 1:CPU 饱和 = A1 两条（busy 8.84 / R 89%）;证据 2:不线性 = 本役曲线 `2.01×`（4→8 段仅 1.15×）;证据 3（超线性成本直接量化）:上节派生 **12.2 → 54.2 core·s，膨胀 4.45×**——纯解码/构建的有用 CPU 不随线程数增长，膨胀项只能挂在**每行 Vec/String 分配簇的 on-CPU 代价**（allocator 快路径 + arena 自旋 + 缓存失效跨核流量）;证据 4（旁证，同负载异 allocator）:P1 在册 musl 悬崖「threads=8 … **165 s ≈ 3.4 MiB/s**（比 1 线程还慢 4.3×）……典型 musl malloc 跨线程 arena 锁竞争」（`docs/bench/p1.md`）——分配器能独自把并行收益打成负数，机制在册。
按 spec 红线登记为**推断级**:无 perf 不能给出分配函数占比;互证手段 = T3 固定项 mimalloc A/B（判据:若 t8 档收益 >> t1 档收益且 core·s 膨胀收窄，则 A3 升级为实测级）。

### A4 文件读 syscall 主导 —— **证伪**

判据:threads 曲线 1→2 跳幅（IO 主导则小跳）。
证据:1→2 跳幅确实小（43.0→52.7 = 1.23×），单看该指标会指向 IO;但等待侧证据把它排除——`state D: 0.0% (1)`、S 仅 11%、且 threads=1 档 `busy = 0.99 / R = 100%`（**纯 CPU 运行，无任何阻塞态**：528 MB 输入全在 page cache，读 syscall 是 on-CPU 拷贝而非等待）。小跳的真实载体是 t1 档本来就低的串行余量，非 IO 墙。**IO-等待形态证伪**;读 syscall 的 on-CPU 常数归并 A3 上界，不单独成项。

### A5 锁争用 —— **证据不足**

判据:R% 高但 busy 低?
证据:观测形态是 **R 高且 busy 也高**（89% / 8.84），该签名不出现 → **futex 睡眠型争用排除**;但 glibc malloc arena 的 **自旋段是 on-CPU 的**，与真实分配工作、缓存失效停顿在 /proc 通道下**三者不可分**（需 off-CPU/火焰图，perf 被拒）。按任务书「无 perf 时给『证据不足』合法」处置 → **证据不足**;判别实验见 X2（本身即 O1 的 A/B）。

## 排序表

（T3 机动项裁定输入。O1 = spec §3 固定项;O2 由 A3「证实」产生。方向性判定为「证据不足」的 A4/A5 不入表，见「需追加实验」。本表之外本轮零机动项，符合 spec §3「T2 无达标项→零机动」条款的精神:达标项只有 1 固定 + 1 证实。）

| # | 候选 | 收益预估 | 风险 | 验证方式 |
|---|------|---------|------|---------|
| O1 | **mimalloc 全局分配器**（挂账 #3 关联;spec §3 固定项，不依赖本表） | 上界 = 收回 A3 记 8 线程档膨胀 core·s 的 allocator 分量;t8 现状 6.125 s/86.3 MiB/s，乐观带 4.5-6 s（回到 P1 2.5× 效率线附近 ≈103 MiB/s），保守非负（musl 侧预期独大:165 s 悬崖消/减账，≥50 MiB/s 视销账） | **低**（GlobalAlloc 一行，输出语义零触点;P4a 先例=分配器不换字节） | T1 工装 glibc 侧 A/B N=5 判显著 + musl 目标复测（挂账 #3 销账判定）+ 全门禁（350/cargo bench 账本 + 差分产物逐字节） |
| O2 | **worker 行级分配节食**（`worker_loop`/`build_out` 路径 buffer 复用、容量预留，减每行 Vec/String 生灭次数;A3 直接下游） | 中，不确定度高:目标吃掉 O1 未收走的膨胀余量（4.45× core·s 膨胀若 allocator 占半，另一半在分配**次数**/缓存形态上）;t8 收益预估 0-10% | **中-高**（触热路径 src/，开闸条款=行为恒等硬证;与 O1 收益同源有重叠风险，**必须在 O1 之后复测再立项**，否则记虚账） | 先 O1 落地复采（本文件曲线协议），随后 bench-ab 判显著 + 同输入产物逐字节 + cargo test 全绿（P4a 热路径开闸条款同款） |

入表门槛自证:O1 = 固定项 + A3 方向一致;O2 = 判定「证实」的 A3 的直接派生。A1/A2/A4/A5 的证伪与证据不足结果**没有**被强行翻成表内条目（禁止为动而动）。

### 需追加实验（不入排序表的方向）

- **X1（A4 残余）**:`strace -c` 数读 syscall 频次/字节（免 perf，ptrace 权限实测可用则做）;或 FileReader 加 BufReader 的最小 A/B 探针。当前:证伪只覆盖了「等待形态」，syscall 常数占比未数出来。
- **X2（A5 判别）**:`MALLOC_ARENA_MAX=1/2` 环境变量 A/B（bench-ab 同窗交替，零代码）+ O1 mimalloc A/B 本身——若 arena 上限调节能动 median 即锁争用实锤;若 mimalloc 收走大头则一并归档归因。
- **X3（曲线旁生发现，挂账 #2 侧面）**:**本役 t8 较 P1 基线回退 17%**（6.125 s vs 5.093 s;效率 2.01× vs 2.51×），t1 同档微升 → 回退特异于 threads>1 路径。P1→本 tip 间候选改动面 = P2 泛型载荷/P3 T6b r3 源中转 bounded(1) 跳 + 20ms 轮询/P4a 解码器热路径。需 T1 工装对 P1 终审 tip 与本 tip 二进制做 N=5 A/B 钉显著性（单次 3 轮 + 17% 尚不构成钉死证据，禁直接立项）。
- **X4（t1 档轮间方差）**:`thr1: 11.378 12.288 13.345`（±8%）异常宽（thr2/4/8 均 ±5% 内）——powersave+HWP 单线程频移或上轮 page-cache 回写串扰;曲线协议若常设需在两档间插冷却轮验证。

## 方法注记

- **M1 脏共享 worktree 处置:** 本 lane 开跑时 `feat-p4b` 工作树已含并行 lane 未提交的 `src/` 改动（T4 搬运在途:`src/repl/assembly.rs` 新增、`src/pipeline/mod.rs` −1942 行、`src/repl/mod.rs` 修改）。为守「T2 = 对已提交基线做普查 + 零 src/ 改动」红线，构建与 `cargo test` 全部改在 `git archive c59def0` 干净导出（`/tmp/p4b-t2-head-src`）执行，产物曲线**不代表脏树**;本 lane 提交面仅 `tools/bench-profile.sh` + 本报告两文件。回归闸逐字（`/tmp/p4b-t2-cargo-test.log`）:9 个 test result 行全 `ok`，合计 **350 passed / 0 failed / 14 ignored**（314+8+9+7+2+2+8），与 P4a 收官在册值恒等 = 本 lane 零 src/ 改动自证。
- **M2 采样器修正（相对任务书内嵌脚本）:** ① busy 等效由 `max(累计 CPU 秒)` 改为**相邻 tick 差值/间隔**（原式量纲随运行时长增长，不是核等效）并加 median/max 双出口;② 按 comm 匹配全部存活进程、跨轮跟踪（支撑 ≥30 s 稳态窗口）;③ 字段解析维持 `rindex(")")` 锚定 + post-comm remainder 0-based idx 11/12（= 全行 field 14/15 utime/stime），自证通过。
- **M3 采样器自证（先于真跑，任务书要求）:** 单实例 `yes > /dev/null`（应 ≈100% R、busy ≈1 核）:

  ```text
  state R: 100.0% (63)
  ticks=63 wall=4.1s max-threads-seen=1
  busy-core-equivalent(median) = 0.96 (n=62)
  busy-core-equivalent(max) = 1.14
  ```

  （`/tmp/p4b-t2-sampler-selftest.txt`，`max-threads-seen=1`、median 0.96≈1 ✓。首次自证曾受残留第二个 yes 进程干扰——`max-threads-seen=2`、median 2.02，证伪排查有效，清理后复采如上。）
- **M4 产物台账（全部 git-ignored /tmp）:** `p4b-t2-run.log`（总输出）、`-env.txt`、`-thr{1,2,4,8}`、`-thr8-sample.log`、`-perfprobe.txt`、`-proc.txt`、`-proc-thr1.txt`、`-sampler-selftest.txt`、`-gate-red.log`、`-gate-green.log`、`-cargo-test.log`、`-head-src/`、`-tgt/`。本报告所有数字可在上述文件中逐字找到;派生量（效率 2.01×、core·s 12.2/54.2、膨胀 4.45×）的算料行均已引用。
- **M5 criterion 口径分工不变:** 账本基线仍 = `cargo bench --bench decode`（criterion）;本普查与 T3 判定用端到端 wall（bench-ab 工装/本脚本），两套口径互引不互替（spec §6）。
