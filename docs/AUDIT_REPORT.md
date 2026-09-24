# Architecture & Quality Audit Report

<!-- AUDIT-META
language: zh-CN
sections: summary, gate-parity, adversarial, issues, risk-plan, passed, skipped, observations
gates: G1=fail G2=pass G3=pass G4=pass G5=warn
-->

> **本报告应作为「观察记录」而非「整改指令」阅读。** 每一条发现都记录了观察到的
> 事实、它所对照的规则，以及一个严重度判断。严重度是审计员对照 `ai-dev-discipline v1`
> 作出的评估——项目所有者可以不同意它，也可以把某条发现视为有意接受的权衡。
> 本报告不会改动仓库中的任何内容。

| 字段 | 值 |
|---|---|
| **项目** | my2sql-rs（目录 `my2sql`） |
| **审计日期** | 2026-09-24 |
| **审计员** | ai-dev-audit |
| **标准** | ai-dev-discipline v1 |
| **Rust Edition** | 2024 |
| **模式** | Multi-Crate Workspace（实际 1 个成员，`[workspace]` 仅用于排除 `fuzz/`） |
| **已运行工具** | rg, jq, `ci-gate.sh`（项目自身 fmt/clippy/test 门禁）、`quality-scan.sh`、`security-scan.sh`、`dep-check.sh`；cargo audit 未安装（S5 跳过） |
| **被审计版本** | HEAD `978854b`（2026-09-24 13:56:16 +0800），工作区干净（仅未跟踪 `RTEST_GUIDE.md`） |

---

## Executive Summary

**总体健康度：** 🟡 需要关注

| 类别 | 状态 | 问题数 |
|---|---|---|
| MIGRATE. 架构模式 | N/A（`[workspace]` 存在，§MIGRATE 不适用） | 0 |
| A. Workspace 结构 | 🟢 | 0 |
| B. 依赖方向 | 🟢 | 0 |
| S. 安全 | 🟢 | 0（S5 跳过，见 Skipped） |
| G. CI 门禁一致性 | 🟡 | 2（G1 失败 + G5 覆盖缺口） |
| C. 代码质量 | 🟡 | 4 |
| D. 模块组织 | N/A（无 role crate） | 0 |
| E. 前端（SvelteKit） | N/A（无前端） | 0 |

**问题计数**

| 严重度 | 数量 |
|---|---|
| 🔴 Critical | 0 |
| 🟠 High | 1 |
| 🟡 Medium | 3 |
| 🔵 Low | 3 |
| **合计** | **7** |

**一句话结论：** 与上一轮审计（同日早前，G1–G4 全绿）相比，本轮唯一新增的红灯是
**G1 格式化门禁失败**（`tests/e2e.rs` 两处），项目自身的 CI 会因此变红。G2 lint、
G3 测试（364 项）、G4 工具链一致性均通过，无 Critical 级问题。

---

## CI Gate Parity

**门禁结果：** 1 个门禁失败（G1），其余通过；G5 有 3 条未验证命令
**工具链：** 本地 `rustc 1.96.0 (ac68faa20 2026-05-25)` vs CI 钉定 `1.96.0`（`.github/workflows/ci.yml`，`dtolnay/rust-toolchain@1.96.0`）

| ID | 门禁 | 实际运行命令 | 来源 | 结果 | 发现数 |
|---|---|---|---|---|---|
| G1 | 格式化 | `cargo fmt --check` | `.github/workflows/ci.yml` | ❌ **fail** | 1 |
| G2 | Lint（`-D warnings`） | `cargo clippy --all-targets -- -D warnings` | `.github/workflows/ci.yml` | ✅ pass | 0 |
| G3 | 测试 | `cargo test --no-fail-fast` | `.github/workflows/ci.yml` | ✅ pass（364 个测试） | 0 |
| G4 | 工具链一致性 | `rustc -V` vs 钉定版本 | `.github/workflows/ci.yml` | ✅ pass（`1.96.0` vs `1.96.0`，edition=2024） | 0 |
| G5 | 门禁覆盖 | — | `.github/workflows/ci.yml` + `release.yml` | ⚠️ warn | 3 |

### CI 发现但本次未在本地运行的命令

- `cargo build --release --target x86_64-unknown-linux-musl` —— musl 静态构建门禁未本地验证（需 musl 交叉工具链 + musl-tools）
- `cargo check --manifest-path fuzz/Cargo.toml` —— fuzz workspace 编译检查未本地验证（CI 中标记 `continue-on-error: true`，非阻断）
- `cargo build --release --target ${{ matrix.target }}` —— `release.yml` 双目标（gnu + musl）矩阵构建未本地验证

这三条均为构建/打包类命令，不改变 fmt/lint/test 三门结论；但按 G5 的诚实性要求如实声明：本次审计**未**完整复现 CI 的全部构建步骤，因此“CI 一致性”只在 G1–G4 的范围内成立。

### 门禁失败

#### G1: `cargo fmt --check` 失败（`tests/e2e.rs` 两处）

- **观察到的：** `tests/e2e.rs` 有两行超出 rustfmt 默认 `array_width = 60` 的数组换行阈值，需要重排。`cargo fmt --check` 两次报 `Diff in` 同一文件（头号分别为 608 与 640，对应实际超限行 611 与 643）：
  ```
  Diff in /home/cxd/Projects/aiediter/my2sql/tests/e2e.rs:608:
  -        &["--stop-file", "binlog.000003", "--on-error", "stop", "--threads", "1"],
  +        &[
  +            "--stop-file",
  ...
  Diff in /home/cxd/Projects/aiediter/my2sql/tests/e2e.rs:640:
  -        &["--stop-file", "binlog.000003", "--on-error", "skip-bad-event", "--threads", "1"],
  +        &[
  ```
  实测行宽：611 行 82 字符、643 行 92 字符（同文件其余 `--stop-file` 数组恰为 60 字符，故不触发）；文件内不含 `rustfmt.toml` 覆盖，阈值即 rustfmt 默认。
- **标准：** `G1` —— 项目自身的格式化门禁（`cargo fmt --check`，来源 `.github/workflows/ci.yml:26-27`）。CI 在此步骤会中止，G2/G3 不会被云端走到（本地已全部运行）。
- **命令：** `cargo fmt --check`，退出码 1（`Diff in` 计数 = 2，均在 `tests/e2e.rs`）。
- **评估：** High —— 不是编译错误（构建未破坏），但它是项目自设且 CI 强制的门禁失败，阻塞合并/发布。
- **成因：** 未确定。可观察到的事实是：`git blame` 显示 611 与 643 两行均由 HEAD 提交 `978854b`（`fix(B020): Restore B001 default full scan fix`）引入，该提交同时改了 `tests/e2e.rs`（+5/-5）与 `src/pipeline/mod.rs`。**这是相关性，不是已证实的因果**——是否由该提交引入格式漂移、以及为何本地提交前未跑 `cargo fmt --check`，属所有者判断。
- **建议响应（咨询性质——由所有者决定）：** 在提交前链路补一次 `cargo fmt --check`（而非事后补 `cargo fmt`，后者属代码修改）；修复后需重跑 G1 与 G3，因为该文件同时被多个真件 e2e 测试消费。
- **工作量（粗略，咨询性质）：** XS（单文件两处换行）

---

## Adversarial Audit Pass

**已审阅输入：** 原始扫描 JSONL（quality / security / gate / dep）、本报告草稿、`git status` + `git log` + `git blame` 摘要、适用检查清单章节（§A/§B/§S/§G/§C）。
**结果：** 发现 1 处应剔除的误报、2 处应下调的严重度、1 处新增发现

### 新增或重分类的发现

| ID | 动作 | 严重度 | 位置 | 证据 |
|---|---|---|---|---|
| G1 | 新增发现 | High | `tests/e2e.rs:611,643` | `cargo fmt --check` 退出 1；上一轮审计（同日早前）G1 为 pass，本轮 HEAD 已推进到 `978854b` |
| C1 | 重分类 High→Medium | Medium | `src/flashback/reverse.rs:96,105,118` | 三处 `lock().unwrap()` 虽在 `std::thread::spawn` 内，但 `h.join().is_err()` 显式收集 panic（`reverse.rs:110-116`）并以 `Err` 返回——gotcha「panic 被吞、任务静默死亡」的前提不成立；锁临界区内仅 `pop_front()` / 赋值，无 panic 路径 |
| C1 | 误报剔除 | — | `src/pipeline/mod.rs:160` | 上一轮称该 `output_dir.unwrap()`「无前置校验」，实际同函数 `pipeline/mod.rs:138-142` 已先判 `output_dir.is_none()` 并返回 `PipelineError::Config`，unwrap 不可达 |
| C2 | 重分类 High→Medium | Medium | `src/pipeline/mod.rs:588,591,595`、`src/repl/assembly.rs:613`、`src/output.rs:298` | 均为同函数内自证不变量（`while !ckpt_q.is_empty()` 前置判空、`ensure_sink(&key)?` 后取 map、match 非重连臂已 break），panic 消息自带不变量说明；`output.rs:43` 与 `config.rs:299,302,316` 为数学上不可失败转换（`u32` 秒 / 零偏移） |
| S3 | 保持降级 High→Low | Low | `tests/repl.rs`（7 处）、`tests/e2e_drop_recovery.rs:274`、`src/pipeline/mod.rs:1362` | 9 处全部位于测试代码（`tests/` 或 `#[cfg(test)]` 模块内），插值对象为测试本地常量；`src/` 生产路径无 SQL 拼接 |
| C9 | 保持降级 Medium→Low | Low | `src/main.rs`、`src/config.rs:332`、`src/output.rs` 调用面 等 13 处 | CLI 工具，stdout/stderr 即产品输出契约（`main.rs` 注释明确「摘要行单独 println 到 stdout」「P1 逐字节不变」） |
| C5 | 保持降级 High→Low | Low | `src/main.rs:1` | 46 行中约 26 行为设计注释，实际代码约 20 行为纯装配，无业务逻辑/结构体/处理器内联 |

### Pass 覆盖

- G1：实际执行 `cargo fmt --check`，退出 1，`Diff in` 两处 → 非 pass
- G2：实际执行 `cargo clippy --all-targets -- -D warnings`，退出 0
- G3：实际执行 `cargo test --no-fail-fast`，退出 0，364 个测试
- G4：`rustc -V` = `1.96.0`，CI 钉定 `1.96.0`，edition 2024
- G5：逐条列出 CI/release 中发现而未本地执行的 3 条构建命令
- S1：`rg "\bunsafe\b" src/ tests/ benches/` 零命中
- S2：保守密钥模式扫描零命中
- S3：9 处 `format!()` SQL 逐处核对插值来源，全部为测试本地常量
- C1/C2：25 + 12 处逐处回读源码上下文，非仅模式匹配
- C4：无 domain role crate，检查不适用
- C5：回读 `src/main.rs` 全文确认无业务逻辑
- C7：无 `anyhow` 依赖
- C10：`src/` 中唯一 `XXXX` 命中位于 `#[cfg(test)] mod tests`（`src/binlog/file_reader.rs:619`），非生产
- B1：`dep-check.sh` 报无禁止依赖方向（角色映射为空，仅 1 个包）
- A1/A2：`[workspace] members = ["."] exclude = ["fuzz"]` 存在

### 假设

- 本次审计以工作区干净态（HEAD `978854b`，仅未跟踪 `RTEST_GUIDE.md`）为准。上一轮审计报告自述当时 `src/pipeline/mod.rs` 与两份 docs 处于未提交状态，两轮的对象不是同一棵树，故“G1 由 pass 变 fail”应读作版本推进后的事实，而非同一棵树上的回归。
- G1 的成因标注为「未确定」；`git blame` 指向 HEAD 提交只是相关性证据。
- `structure.total_loc = 402683` 不能作为本项目源码规模使用：`.qoder/worktrees/`（811 个 `.rs` 文件）与 `data/` 未被 `ignore_paths` 排除（二者均在 `.gitignore` 中）。实测 `src/ + tests/ + benches/` 合计约 24,916 行。

---

## Issues

### 🔴 Critical

无。无 `unsafe`、无硬编码密钥、无编译错误、G3 全绿；S5（CVE）因工具缺失跳过，属未验证而非通过（见 Skipped）。

### 🟠 High

#### G1: `cargo fmt --check` 失败（`tests/e2e.rs`）

- **观察到的：** 详见上节「门禁失败」。`cargo fmt --check` 退出 1，仅一个文件受影响：`tests/e2e.rs:611`（82 字符）与 `tests/e2e.rs:643`（92 字符）。
- **标准：** `G1` —— 项目自身的格式化门禁必须通过；CI 在 `.github/workflows/ci.yml` 中把它作为第一个门禁步骤。
- **评估：** High —— 合并/发布阻断级，但非正确性缺陷（代码语义不受影响）。
- **建议响应（咨询性质——由所有者决定）：** 走一次 `cargo fmt` 后重跑 G1 + G3；或把 `--stop-file` 数组改为显式多行以满足 `array_width`。
- **工作量（粗略，咨询性质）：** XS
- **Owner note：** 该文件被 3 个真件 e2e 测试消费（`real_capture_flashback_*`），改动后必须重跑测试门禁。

---

### 🟡 Medium

#### C1: 生产代码中的 `unwrap()`（工作线程锁路径）

- **观察到的：** `quality-scan.sh` 共报 25 处测试外 `unwrap()`。经逐处回读上下文，真正落在生产运行路径上的只有 `src/flashback/reverse.rs` 的三处：
  ```
  src/flashback/reverse.rs:96   q.lock().unwrap().pop_front()    // std::thread::spawn 工作线程内
  src/flashback/reverse.rs:105  *e.lock().unwrap() = Some(ioe)   // 工作线程错误槽
  src/flashback/reverse.rs:118  err.lock().unwrap().take()       // 主线程，join 之后
  ```
  其余 22 处的上下文核实结果：
  - `src/pipeline/mod.rs:160` —— **不可达**：同函数 `pipeline/mod.rs:138-142` 已先判 `output_dir.is_none()` 并返回 `PipelineError::Config`（上一轮曾记为「无前置校验」，本轮剔除为误报）。
  - `src/metadata/store.rs:274` —— 刚 push 后立即 find，逻辑上必然命中。
  - `src/binlog/`（`event.rs` 4 处、`json.rs` 5 处、`proto.rs` 2 处、`int.rs` 2 处、`time.rs` 1 处）、`src/repl/source.rs:265` —— 均为「切片转定长数组」的不可失败转换，前置有长度守卫（如 `event.rs:68` 前有 `buf.len() < EVENT_HEADER_SIZE` 判定）。
  - `benches/decode.rs`（4 处）、`fuzz/src/bin/seedgen.rs`（1 处）—— 非生产代码（基准与独立 fuzz workspace 的开发工具）。
- **标准：** `C1` —— 生产代码不应有测试外的 `unwrap()`；gotcha「unwrap() Context Matters」把 `thread::spawn` 内的 `unwrap()` 列为最坏情况，理由是 panic 被吞、任务静默死亡。
- **评估：** Medium（由上一轮的 High 下调，依据见下）—— 下调理由是证据变化，而非「时间久了就降级」：`reverse.rs:110-116` 显式 `h.join()` 并收集 `is_err()`，panic 会转化为 `Err(std::io::Error::other("reverse worker panicked"))` 上抛，不是静默死亡；且三处锁的临界区内只有 `pop_front()` 与赋值，无 panic 路径，锁污染在当前代码下不可达。残余风险是「不变量未文档化」：锁从不跨 panic 路径持有这一前提没有写在代码或注释里，未来改动会无声地把它变成真实风险。
- **建议响应（咨询性质——由所有者决定）：** 二选一：把 `lock().unwrap()` 改为 `lock().map_err(...)`/`unwrap_or_else(|p| p.into_inner())`（同仓库 `src/repl/assembly.rs:595` 已有此写法，属既有范式）；或在三处旁注释记录「临界区无 panic，锁污染不可达」的不变量。
- **工作量（粗略，咨询性质）：** S（1 个文件、3 处）
- **Owner note：** `AUDIT_LOG` 记录过 spec §3.2「CLI fail-fast」为有意设计取向。若该取向覆盖工作线程 panic，则本条可按所有者裁定接受为现状；本报告仍按清单 C1 记为一条待决发现，严重度不做升级。

#### C2: 运行期 `expect()`（内部不变量断言）

- **观察到的：** `quality-scan.sh` 共报 12 处测试外 `expect()`。逐处核实后，属于运行期不变量断言的是：
  ```
  src/pipeline/mod.rs:588   self.ckpt_q.front().expect("non-empty checked")
  src/pipeline/mod.rs:591   self.ckpt_q.pop_front().expect("front checked")
  src/pipeline/mod.rs:595   .expect("ckpt_q 仅在 ckpt_out=Some 时记录")
  src/repl/assembly.rs:613  report.expect("上面 match 的非重连臂均已 break，此际必为 Some")
  src/output.rs:298         self.sinks.get_mut(&key).expect("just inserted/left in map")
  ```
  其余 7 处不可失败或非生产：`src/output.rs:43`（`u32` 秒转 `FixedOffset` 恒合法）、`src/config.rs:299,302,316`（`FixedOffset::east_opt(0)` 恒合法）、`benches/decode.rs:86`、`fuzz/src/bin/seedgen.rs:191,199`（非生产）。
- **标准：** `C2` —— `expect()` 仅应在启动/配置期 fail-fast；运行期应返回错误。gotcha「expect() in Startup vs Runtime」。
- **评估：** Medium（由上一轮的 High 下调）—— 这 5 处都是同函数内可自证的不变量（`while !self.ckpt_q.is_empty()` 前置判空、`ensure_sink(&key)?` 成功后取 map、match 非重连臂已 `break`、`ckpt_out.is_some()` 前置返回），不是需要读环境/IO 才能确认的条件；panic 亦带定位消息。它们的风险形态是「未来改动破坏不变量时以 panic 而非可诊断错误暴露」，属复合型维护风险而非当前缺陷。
- **建议响应（咨询性质——由所有者决定）：** 改为返回 `PipelineError`/`ReplError` 的显式分支，或保留 `expect` 但在注释中固化各自不变量。
- **工作量（粗略，咨询性质）：** S–M（约 5 处）

#### G5: 3 条 CI 构建命令未本地验证

- **观察到的：** `.github/workflows/ci.yml:32-36` 与 `.github/workflows/release.yml:29-31` 中发现 3 条本次未执行的命令（musl release 构建、fuzz workspace `cargo check`、release 双目标矩阵构建）。
- **标准：** `G5` —— 门禁覆盖诚实性：CI 运行而本次未运行的命令必须声明，不得以沉默冒充覆盖。
- **评估：** Medium —— 不影响 fmt/lint/test 三门结论，但意味着本次审计未验证 musl 静态构建与 fuzz workspace 的编译健康。
- **建议响应（咨询性质）：** 在具备 `musl-tools` 的环境补跑 `cargo build --release --target x86_64-unknown-linux-musl` 与 `cargo check --manifest-path fuzz/Cargo.toml`。
- **工作量（粗略，咨询性质）：** XS

---

### 🔵 Low

#### C9: 测试外的 `println!`/`eprintln!`（CLI 输出契约）

- **观察到的：** 13 处 —— `benches/decode.rs:122,126,132`、`src/main.rs:24,26,28,33,42`、`src/config.rs:332`、`fuzz/src/bin/seedgen.rs:201,205`、`src/pipeline/mod.rs:154,652`。
- **标准：** `C9` —— 测试外零 `println!`/`dbg!`/`eprintln!`，改用 `tracing` 宏。
- **评估：** Low —— 本项目是 CLI 工具，stdout/stderr 即产品输出通道；`src/main.rs` 注释明确「摘要行单独 println 到 stdout」「P1 逐字节不变」的输出契约，`benches`/`fuzz` 更属工具输出。这不是服务器语境下的调试残留。重分类 Medium→Low（沿用上一轮结论）。
- **建议响应（咨询性质）：** 维持现状；仅 `src/pipeline/mod.rs:652` 的进度性 `eprintln!` 可考虑转 `tracing`。

#### S3: 测试代码中的 `format!()` SQL 拼接

- **观察到的：** 9 处 —— `tests/repl.rs:222,691,692,693,740,1215,2614`、`tests/e2e_drop_recovery.rs:274`、`src/pipeline/mod.rs:1362`（后者位于 `#[cfg(test)]` 模块内的断言中，用于拼装期望字符串）。
- **标准：** `S3` —— DB 查询应参数化；gotcha「SQL Injection: format!() vs Parameterized」要求先甄别插值对象是否为编译期常量。
- **评估：** Low —— 插值对象全为测试本地常量（`test_db`、生成的 tag、`t10` 表名、`{v}` 为期望行值），无用户输入到达这些字符串；`src/` 生产路径走 `mysql` crate 参数化/转移 API，无 SQL 拼接。重分类 High→Low。
- **建议响应（咨询性质）：** 测试代码维持现状；若同一模式进入生产路径需改为参数化。

#### C5: `main.rs` 46 行（超 30 行启发式）

- **观察到的：** `src/main.rs` 共 46 行，其中约 26 行为设计决策注释（mimalloc 全局分配器、tracing 输出通道、退出码 130 契约）；实际代码约 20 行纯装配：`Config::from_args()` + `tracing_subscriber::fmt::init()` + `match cfg.work_type` 分派 + 错误退出。
- **标准：** `C5` —— `main.rs` ≤ 30 行、仅装配；gotcha「app/main.rs Line Count」明确：45 行纯装配的 `main.rs` 比 25 行内联处理器的更可接受。
- **评估：** Low —— 启发式超限，但底层规则（业务逻辑不入 `main.rs`）未被违反，行数由注释密度拉高。重分类 High→Low。
- **建议响应（咨询性质）：** 可选；若在意启发式，将设计注释收敛到 `docs/`。

---

## Risk Priority Plan

按严重度优先、其次按置信度与影响半径排序。**仅咨询性质**——由所有者决定是否行动、以何顺序、以及是否行动。

| 优先级 | ID | 标题 | 建议响应 | 备注 |
|---|---|---|---|---|
| 1 | G1 | `cargo fmt --check` 失败（`tests/e2e.rs`） | 重排两行后重跑 G1 + G3 | 唯一阻断项，XS 工作量 |
| 2 | C1 | 工作线程内 `lock().unwrap()` | 改 `map_err`/`into_inner` 或注释固化不变量 | 3 处，生产路径 |
| 3 | C2 | 运行期 `expect()` 内部不变量 | 改为显式错误分支或注释固化 | 5 处，数据完整性路径 |
| 4 | G5 | 3 条 CI 构建命令未验证 | 补跑 musl 构建与 fuzz check | 环境就绪后 XS |
| 5 | S3 | 测试内 `format!()` SQL | 维持现状 | 无注入面 |
| 6 | C9 | CLI 输出契约 | 维持现状 | 产品输出通道 |
| 7 | C5 | `main.rs` 46 行 | 可选，收敛注释 | 纯装配，无业务逻辑 |

### 建议分诊

**立即所有者评审：**
- G1（唯一的门禁红灯；CI 会因此失败）

**计划性修复：**
- C1、C2（生产 panic 路径 / 内部不变量）

**跟踪与复查：**
- G5（构建门禁覆盖缺口）

**积压：**
- S3、C9、C5

---

## Passed Checks

✅ A1/A2（workspace 清单与成员声明）：`[workspace]` + `members = ["."]` + `exclude = ["fuzz"]` 存在，成员可由 `cargo metadata` 确定
✅ B1（依赖方向）：`dep-check.sh` 报无禁止依赖方向（单成员 workspace，角色映射为空）
✅ S1（`unsafe` 块）：`rg "\bunsafe\b" src/ tests/ benches/` 零命中
✅ S2（硬编码密钥）：保守模式扫描零命中
✅ C7（`anyhow` 作用域）：无 `anyhow` 依赖（项目使用 `thiserror`）
✅ C10（TODO/FIXME/HACK 标记）：`src/` 生产路径零命中（唯一 `XXXX` 命中在 `#[cfg(test)] mod tests` 内）
✅ G2（lint 门禁）：`cargo clippy --all-targets -- -D warnings` 退出 0 —— 无被拒警告，无编译错误
✅ G3（测试门禁）：`cargo test --no-fail-fast` 退出 0 —— 364 个测试通过
✅ G4（工具链一致性）：本地 `rustc 1.96.0 (ac68faa20 2026-05-25)` 与 CI 钉定 `1.96.0` 一致，edition 2024 受工具链支持

---

## Skipped / Not Applicable

- **S5（已知 CVE）：** `cargo-audit` 未安装，`security-scan.sh` 报 skipped。这是**未验证**，不是通过 —— 部署前应 `cargo install cargo-audit` 后复检依赖 CVE。
- **G1 的其余文件：** `cargo fmt --check` 只报 `tests/e2e.rs` 一个文件（`Diff in` 计数 2，同一文件两个 hunk），其余文件无需重排。
- **C4（DTO 泄漏入 domain）：** 无 domain role crate，检查不适用/未验证。
- **D1–D5（模块组织）：** 需 domain/server/api/infra/common 等 role crate，单成员 workspace 下不适用。
- **E1–E6（前端）：** 无前端（`has_frontend=false`，0 个 Svelte 文件）。
- **MIGRATE-1/2/3：** `[workspace]` 存在，§MIGRATE 不适用。
- **库层测试覆盖度细分（C8）：** 本清单的 C8 只判「是否存在测试」；本轮 G3 实测 364 个测试通过，但按模块的覆盖度权衡不在本清单范围。

---

## Observations

- **单包 workspace 包装：** `Cargo.toml` 的 `[workspace]` 仅含 `members = ["."]` 与 `exclude = ["fuzz"]`，实际仍是单 crate CLI。这满足了 §MIGRATE / §A1 的字面通过条件，但并不意味着已按 7-crate 分层组织。是否拆分属所有者决策；本报告不把它记为发现。
- **被审计树与上一轮不是同一棵：** 上一轮报告落盘于 12:39 之后（报告时间戳 12:39），自述当时 `src/pipeline/mod.rs` 等处于未提交状态；本轮 HEAD 为 13:56 的 `978854b`，工作区干净（仅未跟踪 `RTEST_GUIDE.md`）。比较两轮的 G1 结论时必须带上这个前提差异。
- **`total_loc` 被非源码目录放大：** `detect-structure.sh` 报 `total_loc = 402683`、`rust_files = 787`。其中 `.qoder/worktrees/` 有 811 个 `.rs` 文件（`.qoder/` 在 `.gitignore` 内），`data/` 为 MySQL 数据目录。建议在本项目审计时把 `.qoder`、`data`、`out`、`reference`、`.superpowers` 加入 `config.json` 的 `ignore_paths`，否则规模类判断会被显著误导。实测 `src/ + tests/ + benches/` 合计约 24,916 行。
- **`.gitignore` 已覆盖上述目录，但审计配置未跟随：** 仓库自身的忽略清单比审计脚本的 `ignore_paths` 更完整，这是配置漂移而非代码问题。
- **`fuzz/` 是独立 workspace 且被 root 排除：** 根 `cargo clippy --all-targets` 不会构建它；CI 中的 fuzz check 标为 `continue-on-error: true`。因此 `fuzz/src/bin/seedgen.rs` 中的 `unwrap()`/`expect()`/`println!` 既不进生产，也不进阻断门禁——本报告因此把它们按非生产代码处理。
- **同仓库内两种锁使用范式并存：** `src/repl/assembly.rs:595` 用 `lock().unwrap_or_else(|p| p.into_inner())` 容错，`src/flashback/reverse.rs:96,105` 用 `lock().unwrap()` 直接 panic。功能上都有兜底，但两处风格不一致，未来维护者难以从代码判断哪种是本项目范式（见 C1 建议）。
- **`RTEST_GUIDE.md`（35 KB）未入库：** 工作区中的未跟踪文件。它不影响门禁（fmt/clippy/test 均不引用），但若是应有产物，值得决定是否纳入版本控制。

---

*Report generated by ai-dev-audit. Standards: ai-dev-discipline v1.*
*File: docs/AUDIT_REPORT.md*
