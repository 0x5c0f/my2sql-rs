# my2sql-rs P4b 性能面基线记录（T5 汇编）

**日期:** 2026-09-22 · **lane:** P4b-T5（合流收官）· **终态 tip:** mimalloc(T3 `281735d`) + assembly 搬运(T4 `8ff5fe0`) + 判定工装(T1 `b6844fe`+fix `2c670b9`) + profile 普查(T2 `5b6a363`)
**口径纪律:** 本文所有数字**逐字**抄自各 lane report 与其 `/tmp/p4b-t*-*` 源日志（禁手改），traceability 表见文末。回归六闸 T5 合流亲跑逐字日志 = `/tmp/p4b-merge-gate*.log`（详见 HANDOVER P4b T5 节点）。

---

## ① 机器与工装口径

环境事实沿用 P4b-T2《profile 普查报告》「环境」节（`docs/bench/p4b-profile.md`），本役合流机与其逐字一致：

- CPU：**13th Gen Intel Core i9-13900H**（8P+12E 可见 20 核）；`scaling_max_freq` cpu0-11 = 5.2/5.4 GHz（P 域）、cpu12-19 = 4.1 GHz（E 域）。
- **governor = 全 20 核 powersave，只记录不修改**（无交互 sudo 在册，环境事实）。全部计时 `taskset -c 0-11` 钉 P 域。
- `perf_event_paranoid` 实测 = `4`（CPU 事件采样被拒；函数级热点缺位为环境如实处置，见 p4b-profile.md「perf 面」降级声明）。
- 输入 = 主仓 528.9 MiB bench 缓存（worktree 读侧 symlink `data/bench` → 主仓，**零写入**），`.bench-ready` marker = `mysql-bin.000003`，**SZ = 554,555,214 bytes**。
- 判据分层（spec §6）：**criterion（`cargo bench --bench decode`）= 权威账本基线**；`tools/bench-ab.sh` = 轻量端到端 A/B 判定工具（median+MAD，`|Δmedian| > max(2×MAD_pool, 2%×medianA)` → 显著）；`tools/bench-profile.sh` = threads 曲线普查。三套口径输入/threads/构建档一致并在文档互引。
- 构建档：本役全部 `CARGO_TARGET_DIR=/tmp/p4b-merge`（合流独立 target，release 与 bench profile 同 `[profile.bench]` inherits release）。

## ② 正式 criterion 基线 + 回归闸判定（vs 103.85 MiB/s ±5%）

mimalloc（T3）与全部 src/ 改动落地后，在终态 tip 上跑权威 criterion 基线
（`CARGO_TARGET_DIR=/tmp/p4b-merge cargo bench --bench decode`，逐字
`/tmp/p4b-merge-bench.log`）：

```text
bench input: mysql-bin.000003 (528 MiB) binary=/tmp/p4b-merge/release/my2sql-rs
file_to_sql/threads=8/528 MiB
                        time:   [4.0550 s 4.1451 s 4.2409 s]
                        thrpt:  [124.71 MiB/s 127.59 MiB/s 130.42 MiB/s]
file_to_sql/threads=1/528 MiB
                        time:   [10.084 s 10.162 s 10.273 s]
                        thrpt:  [51.480 MiB/s 52.042 MiB/s 52.445 MiB/s]
```

- 判定口径：`Throughput::Bytes(binlog 输入字节)`，threads=8 median 时间 **4.1451 s** → 吞吐 median **127.59 MiB/s**。
- **回归闸判定（spec §5 / §7 DoD-6：vs P1 真值 103.85 MiB/s，劣化 >5% = 红）：**
  127.59 MiB/s vs 103.85 → **+22.86%（更快）**，五% 劣化红线**未触碰 → GREEN**。
  较 P1 账本（`docs/bench/p1.md:35` threads=8 median 5.093 s / 103.85 MiB/s）与
  P2 账本（`docs/bench/p2.md:44` 88.352 MiB/s）均回正并上抬，增益方向与 mimalloc
  glibc A/B（④）一致。DoD-3 绝对门槛（threads=8 ≥40 MB/s）以 2.4× 余量通过。
- threads=1 组（扩展性参考）median 10.162 s → 52.042 MiB/s；ledger 上
  t8/t1 吞吐比 = 127.59/52.042 = **2.45×**（时间口径 10.162/4.1451 = 2.45×）。
  **注记（见「裁定与免责」节）：** 该比值为 criterion 账本口径的**观测值**，
  threads 1→8 并行效率问题在 mimalloc 后仍**未结构性收口**（无热路径结构改动合入，
  机动项 O2 已尝试→不显著→回滚），且此口径不同于 T2 端到端曲线口径。

## ③ T1 ab7 —— P1 vs P2 代码增量回归闸复测（挂账 #7）

工装 = `tools/bench-ab.sh`，A = `v0.1.0-p1`(c167b0d) release，B = `v0.2.0-p2`(0905368)
release（独立 target dir 重构建，`cmp` 复验 `BINARIES-DISTINCT-GOOD`；同 target 会因
包名版本同为 `my2sql-rs 0.1.0` 被 cargo 指纹复用成逐字节相同的无效 A/B，见
`/tmp/p4b-t1-ab7.md`「被测二进制」节）。N=5 判定跑逐字（`/tmp/p4b-t1-ab7-r5.log`）：

```text
# bench-ab: rounds=5 threads=8 taskset=0-11 input=mysql-bin.000003(554555214 B)
# governor: powersave (record-only)
SAMPLE r=1 A=10.9439s B=11.9917s
SAMPLE r=2 A=10.6844s B=7.4720s
SAMPLE r=3 A=6.5020s B=6.9356s
SAMPLE r=4 A=6.7459s B=6.8640s
SAMPLE r=5 A=6.5953s B=6.6499s
median A=6.7459s B=6.9356s MAD A=0.2439 B=0.2857 delta=+2.812% (0.1897s) thr=0.5714s
throughput MiB/s A=78.4 B=76.3 (input=554555214 bytes)
VERDICT: not-significant (B slower-than A)
```

**结论（挂账 #7 钉死不显著）：** P1→P2 代码增量端到端 B(P2) 慢 **+2.812%**
（0.1897s < 阈值 0.5714s），与 P2 账本「−3.2%、95% CI 跨 0」弱信号同向且落在
工装噪声带内，**回归闸不亮红**。N=5 即判 not-significant → 按契约「显著才升级
N=9」**不升级**。r1 双侧为页缓存冷启动（10.9/12.0s），median+MAD 对冷启动离群不敏感。
A/A 恒等冒烟同日实测 not-significant（`/tmp/p4b-t1-aa.log` delta=+0.847% < thr=0.2902s），
工装守护有效。

## ④ mimalloc —— glibc A/B + musl 复测（挂账 #3）+ 裁定

### glibc A/B（mimalloc vs BASE_T3 `2c670b9`）

两侧二进制 `sha256` 逐字 DIFFER（base `9970ffb9…323f755` / tip `fda6465a…cbf5af03`，
`/tmp/p4b-t3-base-sha.txt` 在册）。`bench-ab --rounds 5` 逐字（`/tmp/p4b-t3-ab.log`）：

```text
# bench-ab: rounds=5 threads=8 taskset=0-11 input=mysql-bin.000003(554555214 B)
# governor: powersave (record-only)
SAMPLE r=1 A=2.2681s B=1.7119s
SAMPLE r=2 A=4.9935s B=4.5709s
SAMPLE r=3 A=5.9613s B=4.3448s
SAMPLE r=4 A=5.9889s B=4.4023s
SAMPLE r=5 A=5.9906s B=4.3779s
median A=5.9613s B=4.3779s MAD A=0.0293 B=0.0331 delta=-26.561% (1.5834s) thr=0.1192s
throughput MiB/s A=88.7 B=120.8 (input=554555214 bytes)
VERDICT: significant (B faster-than A)
```

**裁定：** mimalloc **显著快 26.561%**（远超 2% 门，MAD≈0.03 稳健）→ 保留接入。

### musl 复测（挂账 #3 悬崖消账）

环境事实：`/usr/bin/musl-gcc` 在册，`x86_64-unknown-linux-musl` target 已装，mimalloc
v0.1.52 的 C 核在 musl-gcc 下正常交叉编译。两棵独立 target 目录各自 musl 构建，
×3 交替单跑中位（taskset 0-11，threads=8，`/tmp/p4b-t3-musl-{base,tip}.txt`）：

```text
MUSL r=1 base=147.7655s mimalloc=8.1690s
MUSL r=2 base=158.0084s mimalloc=8.7113s
MUSL r=3 base=168.4141s mimalloc=8.2347s
base-musl     median=158.0084s  MiB/s=3.3
mimalloc-musl median=8.2347s    MiB/s=64.2   (SZ=554555214)
```

**裁定：** base-musl **3.3 MiB/s** ≈ 精确复现 P1 在册 3.4 MiB/s 悬崖
（`docs/bench/p1.md:88`）；mimalloc-musl **64.2 MiB/s ≥ 50** 门 → **悬崖消账（挂账 #3 = 已解决）**，
量级比 158.0084/8.2347 ≈ **19.2×**。std 目标 `--no-default-features` 不可用（见「裁定与免责」
mimalloc 硬依赖条），故对照 = 无 mimalloc 的 BASE_T3 worktree 同法 musl 构建。

## ⑤ 机动项 verdict（spec §3「低风险 + 已证实」top-1..2）

T2 排序表（`docs/bench/p4b-profile.md`）入表项 = O1 mimalloc（固定项，见 ④）+
O2 worker 行级分配节食（A3 直接派生）。**O2 已尝试 → not-significant → 回滚**，
逐字（`/tmp/p4b-t3-o2-ab.log`，base = mimalloc-tip，防收益双计）：

```text
# bench-ab: rounds=5 threads=8 taskset=0-11 input=mysql-bin.000003(554555214 B)
# governor: powersave (record-only)
SAMPLE r=1 A=1.5714s B=1.5951s
SAMPLE r=2 A=2.2489s B=4.5950s
SAMPLE r=3 A=4.6190s B=4.5511s
SAMPLE r=4 A=4.5383s B=4.5967s
SAMPLE r=5 A=4.5976s B=4.4969s
median A=4.5383s B=4.5511s MAD A=0.0807 B=0.0456 delta=+0.282% (0.0128s) thr=0.1614s
throughput MiB/s A=116.5 B=116.2 (input=554555214 bytes)
VERDICT: not-significant (B slower-than A)
```

**处置：** |Δ|=0.0128s ≪ thr=0.1614s 且方向 B 略慢 → 未过显著门。mimalloc 已收走
allocator 侧膨胀大头，残余 buffer-复用微增益落在噪声带内（与 T2「0-10%, high
uncertainty」预估一致）。按 spec §3「零胜合法、禁为动而动」→ **回滚 O2**（含行为钉
一并撤除），**未提交、不在历史**。**本役唯一合入的 src/ 性能改动 = mimalloc。**
结构性方向（channel/Reorder/IO/锁，A1/A2/A4/A5）T2 已判证伪或证据不足，无入表项。

## ⑥ T4 assembly 搬运 —— A/A' 抽测（挂账 #5 兜底）

T4 = 纯 move-only 搬运（`src/pipeline/mod.rs` 3473 → 1540 行 + 新建 `src/repl/assembly.rs`
1949 行，对外接口零变化），逐字节/350 计数/引用图三门硬证见 HANDOVER P4b T4 节点。
`tools/bench-ab.sh` 于 T4 派发时尚未在本分支落地，controller 裁定用内联法抽测（base=new
逐轮交替各 3 轮，暖缓存重采，`/tmp/p4b-t4-*`，taskset -c 0-11）：

```text
round1 base 11704ms   round1 new 10461ms
round2 base  6438ms   round2 new  5936ms
round3 base  6358ms   round3 new  6517ms
中位数：base 6438ms / new 6517ms → Δ = +1.2%（|Δ|≤3% 预期带内）
```

**结论：** 搬运不触热路径（to-sql 主链路不经 assembly），A/A' 中位差 +1.2% 落在预期带内、
无退化。装配块行为恒等由产物逐字节（`diff -r` rc=0，sha1 `49c81bcd…`/`7c4c0f73…` base≡new）
+ 350 计数硬证兜底，本抽测仅为吞吐侧兜底。

---

## 裁定与免责（必读，禁虚账口径）

1. **mimalloc 是无条件硬依赖：** 根 `Cargo.toml` `[dependencies]` `mimalloc = "0.1.52"` +
   `src/main.rs` 顶层 `#[global_allocator] static GLOBAL: mimalloc::MiMalloc`，**未做
   feature-gate** → std 目标 `--no-default-features` 无法产出「无 mimalloc」对照二进制。
   **musl release 发行面现含 mimalloc 的 C 核（`libmimalloc-sys`），在 musl-gcc 下正常
   交叉编译**（④ 实证）。若后续需可切换分配器，宜加 `default = ["mimalloc-alloc"]`
   feature（本役未做，超出 §3 固定项范围）。
2. **X3（t8 端到端回退归因）仍为 registered-only（挂账，本役未追）：** T2 登记「本役端到端
   t8 较 P1 回退 17%」（86.3 vs 103.9 MiB/s，端到端曲线口径）。本役 mimalloc 把 t8 端到端
   推至 120.8 MiB/s（④ A/B B 侧）越过 103.85 —— 该数字**顺带（incidental）**越过账本线，
   **无专属第二 A/B（P1 终审 tip vs 本 tip）**，**不得**当作「17% 回退被 mimalloc 收复」的
   归因结论，X3 继续在册。
3. **并行效率口径：** T2 端到端曲线**实测 2.01×** 作为已测事实**取代** P1 账本「2.5×」
   记录值（`docs/bench/p1.md:54`，其口径 12.783 s→5.093 s = 2.51×）；threads 1→8 缩放
   问题在 mimalloc 后**仍未收口**（无热路径结构改动合入；O2 尝试→不显著→回滚）。本役
   criterion 账本新基线观测比 t8/t1 = 2.45×（②），与端到端 2.01× 为不同口径，两者均
   如实登记、不互替（spec §6 判据分层）。
4. **回归闸口径统一：** ② 的 ±5% 红线是**账本 vs 账本**（criterion 103.85 → 127.59），
   ③ 的 2% 显著门是 **bench-ab 端到端 A/B**（P1 vs P2），④ 的 26.561% 是 **mimalloc A/B**
   ——三套数字口径不同、不可混用，各自判定独立成立。

## traceability（数字 → 源工件 file / 行）

| 文档数字 | 源工件 |
|---|---|
| 机器/SZ=554555214/paranoid=4/powersave | `/tmp/p4b-t2-env.txt`（p4b-profile.md「环境」） |
| t8 median 4.1451s / 127.59 MiB/s；t1 10.162s / 52.042 MiB/s | `/tmp/p4b-merge-bench.log:216-217,236-237` |
| 回归闸 +22.86% / 103.85 基线 | 本役算料（127.59 vs 103.85）+ `docs/bench/p1.md:35,46` |
| ab7 delta +2.812% / thr 0.5714s / A=78.4 B=76.3 | `/tmp/p4b-t1-ab7-r5.log` + 全文 `/tmp/p4b-t1-ab7.md` |
| A/A +0.847% / thr 0.2902s | `/tmp/p4b-t1-aa.log` |
| mimalloc glibc -26.561% / A=88.7 B=120.8 / sha DIFFER | `/tmp/p4b-t3-ab.log` + `/tmp/p4b-t3-base-sha.txt` |
| musl base 3.3 / tip 64.2 MiB/s | `/tmp/p4b-t3-musl-base.txt` + `/tmp/p4b-t3-musl-tip.txt` |
| O2 +0.282% / thr 0.1614s（回滚） | `/tmp/p4b-t3-o2-ab.log` |
| T4 抽测 Δ+1.2% / sha1 恒等 | `/tmp/p4b-t4-out` 抽测（task-4-report Step 3/4） |
| 挂账#4 p1.md 无 5.903 / :35 已 5.093 | `grep 5.903 docs/bench/p1.md`（空）+ `docs/bench/p1.md:35` |
| 挂账#6 gen-bench RSBIN 接线 | `/tmp/p4b-t1-genbench-trace.log:5` + `tools/gen-bench-binlog.sh:24,116` |
