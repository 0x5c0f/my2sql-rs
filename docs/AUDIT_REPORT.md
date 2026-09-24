# Architecture & Quality Audit Report

<!-- AUDIT-META
language: zh-CN
sections: summary, gate-parity, adversarial, issues, risk-plan, passed, skipped, observations
gates: G1=pass G2=pass G3=pass G4=pass G5=warn
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
| **被审计版本** | HEAD `eba3a68`（2026-09-24 14:08:13 +0800），工作区干净（仅未跟踪 `RTEST_GUIDE.md`） |

---

## Executive Summary

**总体健康度：** 🟢 健康（无 Critical / High；两项未验证见下）

| 类别 | 状态 | 问题数 |
|---|---|---|
| MIGRATE. 架构模式 | N/A（`[workspace]` 存在，§MIGRATE 不适用） | 0 |
| A. Workspace 结构 | 🟢 | 0 |
| B. 依赖方向 | 🟢 | 0 |
| S. 安全 | 🟢 | 0（S5 跳过，见 Skipped） |
| G. CI 门禁一致性 | 🟢 | 1（G1–G4 全绿；G5 覆盖缺口） |
| C. 代码质量 | 🟡 | 4 |
| D. 模块组织 | N/A（无 role crate） | 0 |
| E. 前端（SvelteKit） | N/A（无前端） | 0 |

**问题计数**

| 严重度 | 数量 |
|---|---|
| 🔴 Critical | 0 |
| 🟠 High | 0 |
| 🟡 Medium | 3 |
| 🔵 Low | 3 |
| **合计** | **6** |

**一句话结论：** 上一轮唯一的 High（G1 格式化门禁失败）已修复并在本轮以同一命令复验通过：
G1/G2/G3/G4 全绿，G3 仍为 364 个测试通过。本轮无 Critical、无 High；剩余 6 条均为
Medium/Low 的既有项（C1/C2 生产 panic 路径、G5 构建覆盖缺口、S3/C9/C5）。

**两项未验证（不构成 🟢 的例外，但必须声明）：**
1. **G5** —— CI/release 中的 3 条构建命令未在本地执行（musl release、fuzz check、release 双目标矩阵）；
2. **S5** —— `cargo-audit` 未安装，依赖 CVE 未扫描。

---

## CI Gate Parity

**门禁结果：** G1–G4 全部通过；G5 有 3 条未验证命令
**工具链：** 本地 `rustc 1.96.0 (ac68faa20 2026-05-25)` vs CI 钉定 `1.96.0`（`.github/workflows/ci.yml`，`dtolnay/rust-toolchain@1.96.0`）

| ID | 门禁 | 实际运行命令 | 来源 | 结果 | 发现数 |
|---|---|---|---|---|---|
| G1 | 格式化 | `cargo fmt --check` | `.github/workflows/ci.yml` | ✅ pass | 0 |
| G2 | Lint（`-D warnings`） | `cargo clippy --all-targets -- -D warnings` | `.github/workflows/ci.yml` | ✅ pass | 0 |
| G3 | 测试 | `cargo test --no-fail-fast` | `.github/workflows/ci.yml` | ✅ pass（364 个测试） | 0 |
| G4 | 工具链一致性 | `rustc -V` vs 钉定版本 | `.github/workflows/ci.yml` | ✅ pass（`1.96.0` vs `1.96.0`，edition=2024） | 0 |
| G5 | 门禁覆盖 | — | `.github/workflows/ci.yml` + `release.yml` | ⚠️ warn | 3 |

`ci-gate.sh` 本轮结论（stderr）：`ci-gate: all discovered gates passed locally`。

### CI 发现但本次未在本地运行的命令

- `cargo build --release --target x86_64-unknown-linux-musl` —— musl 静态构建门禁未本地验证（需 musl 交叉工具链 + musl-tools）
- `cargo check --manifest-path fuzz/Cargo.toml` —— fuzz workspace 编译检查未本地验证（CI 中标记 `continue-on-error: true`，非阻断）
- `cargo build --release --target ${{ matrix.target }}` —— `release.yml` 双目标（gnu + musl）矩阵构建未本地验证

这三条均为构建/打包类命令，不影响 fmt/lint/test 三门结论；按 G5 的诚实性要求声明：本次审计**未**完整复现 CI 的全部构建步骤，因此「CI 一致性」只在 G1–G4 的范围内成立。

### 门禁失败

无。G1 在本轮首次通过；上一轮的失败详情与修复证据见「Issues → 上一轮 High 的处置」。

---

## Adversarial Audit Pass

**已审阅输入：** 原始扫描 JSONL（quality / security / gate / dep）、本报告草稿、`git status` + `git log` + `git show` 摘要、适用检查清单章节（§A/§B/§S/§G/§C）。
**结果：** 未发现重大缺口；1 条上一轮 High 经复验关闭，其余发现维持原判

### 新增或重分类的发现

| ID | 动作 | 严重度 | 位置 | 证据 |
|---|---|---|---|---|
| G1 | 关闭（High → 无发现） | — | `tests/e2e.rs` | `cargo fmt --check` 退出 0（本轮独立复跑，非引用提交信息）；修复提交 `eba3a68` 对 `tests/e2e.rs` 的改动仅为数组换行展开，无语义变化 |
| G3 | 复验（不降级） | — | `tests/e2e.rs` | G1 的修复触及真件 e2e 输入（`--stop-file` 数组），上一轮报告已指出须重跑 G3；本轮 `cargo test --no-fail-fast` 退出 0，364 个测试通过，与修复前同数 |
| C1 | 维持 Medium | Medium | `src/flashback/reverse.rs:96,105,118` | `src/` 自上一轮以来未改动（`git show --stat eba3a68` 仅含 `docs/` 与 `tests/e2e.rs`），上一轮的下调依据（`h.join()` 收集 `is_err()`、临界区无 panic 路径）不变 |
| C2 | 维持 Medium | Medium | `src/pipeline/mod.rs:588,591,595`、`src/repl/assembly.rs:613`、`src/output.rs:298` | 同上，`src/` 未改动；逐处不变量回读结论不变 |
| S3 | 维持 Low | Low | `tests/repl.rs`（7）、`tests/e2e_drop_recovery.rs:274`、`src/pipeline/mod.rs:1362` | 9 处全在测试代码；插值为测试本地常量 |
| C9 | 维持 Low | Low | `src/main.rs` 等 13 处 | CLI stdout/stderr 即产品输出契约 |
| C5 | 维持 Low | Low | `src/main.rs:1` | 46 行中约 26 行为设计注释，实际约 20 行纯装配 |
| — | 误报检查 | — | 全部 Medium/Low | 逐条比对 gotchas「False Positive Patterns」表：无新增误报；无新增 Critical/High 需要复核 |

### Pass 覆盖

- G1：实际执行 `cargo fmt --check`，退出 0
- G2：实际执行 `cargo clippy --all-targets -- -D warnings`，退出 0
- G3：实际执行 `cargo test --no-fail-fast`，退出 0，364 个测试
- G4：`rustc -V` = `1.96.0`，CI 钉定 `1.96.0`，edition 2024
- G5：逐条列出 CI/release 中发现而未本地执行的 3 条构建命令
- S1：`rg "\bunsafe\b" src/ tests/ benches/` 零命中
- S2：保守密钥模式扫描零命中
- S3：9 处 `format!()` SQL 逐处核对插值来源
- C1/C2：25 + 12 处逐处回读源码上下文（本轮 `src/` 未变，沿用上一轮回读结果并核对行号一致）
- C4：无 domain role crate，检查不适用
- C5：回读 `src/main.rs` 全文
- C7：无 `anyhow` 依赖
- C10：`src/` 生产路径零命中（唯一 `XXXX` 在 `#[cfg(test)] mod tests` 内）
- B1：`dep-check.sh` 报无禁止依赖方向
- A1/A2：`[workspace] members = ["."] exclude = ["fuzz"]` 存在

### 假设

- 本轮以 HEAD `eba3a68` 的干净工作区为对象；上一轮（HEAD `978854b`）与本轮的差异仅为 `tests/e2e.rs` 的格式化修复与审计文档。
- G1 的关闭依据是本轮独立执行的 `cargo fmt --check`，不采信提交信息中「all gates now pass」的自述（其中也不包含 G5 的 3 条未验证构建命令）。
- `structure.total_loc = 402697` 仍不可作为源码规模使用：`.qoder/worktrees/`（811 个 `.rs` 文件）与 `data/` 未被 `ignore_paths` 排除；实测 `src/ + tests/ + benches/` 合计约 24,972 行。

---

## Issues

本轮无 🔴 Critical 与 🟠 High 级发现。上一轮的唯一 High（G1）已关闭，证据如下。

### 上一轮 High 的处置（G1）

- **上一轮观察：** `cargo fmt --check` 退出 1，`tests/e2e.rs` 两处（82 / 92 字符）超 rustfmt 默认 `array_width = 60`。
- **本轮复验：** 同一命令 `cargo fmt --check` 退出 0；`tests/e2e.rs` 的改动为把该两处数组展开为多行（`git show eba3a68 -- tests/e2e.rs`），无语义变化。
- **附带复验：** G3 仍为 364 个测试通过，与修复前同数——该文件被多个 `real_capture_flashback_*` 真件 e2e 消费，故这条复验是必要的，不是形式检查。
- **评估：** 关闭。本轮不再作为发现计入。

---

### 🟡 Medium

#### C1: 生产代码中的 `unwrap()`（工作线程锁路径）

- **观察到的：** `quality-scan.sh` 共报 25 处测试外 `unwrap()`。落在生产运行路径上的仍是 `src/flashback/reverse.rs` 三处：
  ```
  src/flashback/reverse.rs:96   q.lock().unwrap().pop_front()    // std::thread::spawn 工作线程内
  src/flashback/reverse.rs:105  *e.lock().unwrap() = Some(ioe)   // 工作线程错误槽
  src/flashback/reverse.rs:118  err.lock().unwrap().take()       // 主线程，join 之后
  ```
  其余 22 处：`src/pipeline/mod.rs:160`（同函数 `:138-142` 有守卫，不可达）、`src/metadata/store.rs:274`（push 后 find）、`src/binlog/` 13 处与 `src/repl/source.rs:265`（长度守卫下的定长转换）、`benches/decode.rs` 4 处与 `fuzz/src/bin/seedgen.rs` 1 处（非生产）。
- **标准：** `C1` —— 生产代码不应有测试外的 `unwrap()`；gotcha「unwrap() Context Matters」把 `thread::spawn` 内的 `unwrap()` 列为最坏情况（panic 被吞、任务静默死亡）。
- **评估：** Medium（RECURRING，同严重度复报，未升级）—— 下调依据在上一轮已记录且本轮仍成立：`reverse.rs:110-116` 显式 `h.join()` 收集 `is_err()`，工作线程 panic 会转为 `Err` 上抛而非静默死亡；三处锁临界区内仅 `pop_front()`/赋值，无 panic 路径。残余风险是「锁从不跨 panic 路径持有」这一不变量未文档化。
- **建议响应（咨询性质——由所有者决定）：** 改用 `lock().map_err(...)` 或 `unwrap_or_else(|p| p.into_inner())`（同仓库 `src/repl/assembly.rs:595` 已有此写法），或在三处注释记录该不变量。
- **工作量（粗略，咨询性质）：** S（1 个文件、3 处）
- **Owner note：** `AUDIT_LOG` 记录 spec §3.2「CLI fail-fast」为有意设计取向。若该取向覆盖工作线程 panic，可按所有者裁定接受为现状。

#### C2: 运行期 `expect()`（内部不变量断言）

- **观察到的：** 12 处测试外 `expect()`，其中属运行期不变量断言的是：
  ```
  src/pipeline/mod.rs:588   self.ckpt_q.front().expect("non-empty checked")
  src/pipeline/mod.rs:591   self.ckpt_q.pop_front().expect("front checked")
  src/pipeline/mod.rs:595   .expect("ckpt_q 仅在 ckpt_out=Some 时记录")
  src/repl/assembly.rs:613  report.expect("上面 match 的非重连臂均已 break，此际必为 Some")
  src/output.rs:298         self.sinks.get_mut(&key).expect("just inserted/left in map")
  ```
  其余 7 处不可失败或非生产：`src/output.rs:43`、`src/config.rs:299,302,316`、`benches/decode.rs:86`、`fuzz/src/bin/seedgen.rs:191,199`。
- **标准：** `C2` —— `expect()` 仅应在启动/配置期 fail-fast；运行期应返回错误。gotcha「expect() in Startup vs Runtime」。
- **评估：** Medium（RECURRING，同严重度复报）—— 这 5 处均为同函数内可自证的不变量（前置判空 / `ensure_sink(&key)?` 成功 / match 非重连臂已 break / `ckpt_out.is_some()` 前置返回），非环境依赖条件；panic 消息自带不变量说明。风险形态是「未来改动破坏不变量时以 panic 暴露」，属复合型维护风险。
- **建议响应（咨询性质——由所有者决定）：** 改为返回 `PipelineError`/`ReplError` 的显式分支，或保留 `expect` 但在注释中固化不变量。
- **工作量（粗略，咨询性质）：** S–M（约 5 处）

#### G5: 3 条 CI 构建命令未本地验证

- **观察到的：** `.github/workflows/ci.yml:32-36` 与 `.github/workflows/release.yml:29-31` 中的 3 条命令本轮未执行（musl release 构建、fuzz workspace `cargo check`、release 双目标矩阵构建）。
- **标准：** `G5` —— 门禁覆盖诚实性：CI 运行而本次未运行的命令必须声明，不得以沉默冒充覆盖。
- **评估：** Medium（RECURRING，同严重度复报）—— 不影响 fmt/lint/test 三门结论，但意味着 musl 静态构建与 fuzz workspace 的编译健康未经验证。
- **建议响应（咨询性质）：** 在具备 `musl-tools` 的环境补跑 `cargo build --release --target x86_64-unknown-linux-musl` 与 `cargo check --manifest-path fuzz/Cargo.toml`。
- **工作量（粗略，咨询性质）：** XS

---

### 🔵 Low

#### C9: 测试外的 `println!`/`eprintln!`（CLI 输出契约）

- **观察到的：** 13 处 —— `benches/decode.rs:122,126,132`、`src/main.rs:24,26,28,33,42`、`src/config.rs:332`、`fuzz/src/bin/seedgen.rs:201,205`、`src/pipeline/mod.rs:154,652`。
- **标准：** `C9` —— 测试外零 `println!`/`dbg!`/`eprintln!`，改用 `tracing` 宏。
- **评估：** Low（RECURRING，维持降级 Medium→Low）—— CLI 工具的 stdout/stderr 即产品输出通道；`src/main.rs` 注释明确「摘要行单独 println 到 stdout」「P1 逐字节不变」的输出契约。
- **建议响应（咨询性质）：** 维持现状；仅 `src/pipeline/mod.rs:652` 的进度性 `eprintln!` 可考虑转 `tracing`。

#### S3: 测试代码中的 `format!()` SQL 拼接

- **观察到的：** 9 处 —— `tests/repl.rs:222,691,692,693,740,1215,2614`、`tests/e2e_drop_recovery.rs:274`、`src/pipeline/mod.rs:1362`（后者位于 `#[cfg(test)]` 模块内的断言中）。
- **标准：** `S3` —— DB 查询应参数化；gotcha「SQL Injection: format!() vs Parameterized」要求先甄别插值对象。
- **评估：** Low（RECURRING，维持降级 High→Low）—— 插值对象全为测试本地常量，无用户输入到达；`src/` 生产路径走 `mysql` crate 参数化 API。
- **建议响应（咨询性质）：** 测试代码维持现状；若同一模式进入生产路径需改为参数化。

#### C5: `main.rs` 46 行（超 30 行启发式）

- **观察到的：** `src/main.rs` 共 46 行，其中约 26 行为设计注释；实际代码约 20 行纯装配（`Config::from_args()` + `tracing_subscriber::fmt::init()` + `match cfg.work_type` 分派 + 错误退出）。
- **标准：** `C5` —— `main.rs` ≤ 30 行、仅装配；gotcha「app/main.rs Line Count」明确纯装配的 `main.rs` 可接受。
- **评估：** Low（RECURRING，维持降级 High→Low）—— 启发式超限，但「业务逻辑不入 `main.rs`」未被违反。
- **建议响应（咨询性质）：** 可选；若在意启发式，将设计注释收敛到 `docs/`。

---

## Risk Priority Plan

按严重度优先、其次按置信度与影响半径排序。**仅咨询性质**——由所有者决定是否行动、以何顺序、以及是否行动。

| 优先级 | ID | 标题 | 建议响应 | 备注 |
|---|---|---|---|---|
| 1 | C1 | 工作线程内 `lock().unwrap()` | 改 `map_err`/`into_inner` 或注释固化不变量 | 3 处，生产路径 |
| 2 | C2 | 运行期 `expect()` 内部不变量 | 改为显式错误分支或注释固化 | 5 处，数据完整性路径 |
| 3 | G5 | 3 条 CI 构建命令未验证 | 补跑 musl 构建与 fuzz check | 环境就绪后 XS |
| 4 | S3 | 测试内 `format!()` SQL | 维持现状 | 无注入面 |
| 5 | C9 | CLI 输出契约 | 维持现状 | 产品输出通道 |
| 6 | C5 | `main.rs` 46 行 | 可选，收敛注释 | 纯装配，无业务逻辑 |

### 建议分诊

**立即所有者评审：**
- 无（无 Critical / High）

**计划性修复：**
- C1、C2（生产 panic 路径 / 内部不变量）

**跟踪与复查：**
- G5（构建门禁覆盖缺口）、S5（CVE 扫描工具缺失）

**积压：**
- S3、C9、C5

---

## Passed Checks

✅ A1/A2（workspace 清单与成员声明）：`[workspace]` + `members = ["."]` + `exclude = ["fuzz"]` 存在，成员可由 `cargo metadata` 确定
✅ B1（依赖方向）：`dep-check.sh` 报无禁止依赖方向（单成员 workspace，角色映射为空）
✅ S1（`unsafe` 块）：`rg "\bunsafe\b" src/ tests/ benches/` 零命中
✅ S2（硬编码密钥）：保守模式扫描零命中
✅ C7（`anyhow` 作用域）：无 `anyhow` 依赖（项目使用 `thiserror`）
✅ C10（TODO/FIXME/HACK 标记）：`src/` 生产路径零命中
✅ G1（格式化门禁）：`cargo fmt --check` 退出 0 —— 无文件需重排
✅ G2（lint 门禁）：`cargo clippy --all-targets -- -D warnings` 退出 0 —— 无被拒警告，无编译错误
✅ G3（测试门禁）：`cargo test --no-fail-fast` 退出 0 —— 364 个测试通过
✅ G4（工具链一致性）：本地 `rustc 1.96.0 (ac68faa20 2026-05-25)` 与 CI 钉定 `1.96.0` 一致，edition 2024 受工具链支持

---

## Skipped / Not Applicable

- **S5（已知 CVE）：** `cargo-audit` 未安装，`security-scan.sh` 报 skipped。这是**未验证**，不是通过 —— 部署前应 `cargo install cargo-audit` 后复检依赖 CVE。
- **G5 的 3 条构建命令：** 未本地执行（见 CI Gate Parity 与 Issues → G5）。
- **C4（DTO 泄漏入 domain）：** 无 domain role crate，检查不适用/未验证。
- **D1–D5（模块组织）：** 需 domain/server/api/infra/common 等 role crate，单成员 workspace 下不适用。
- **E1–E6（前端）：** 无前端（`has_frontend=false`，0 个 Svelte 文件）。
- **MIGRATE-1/2/3：** `[workspace]` 存在，§MIGRATE 不适用。
- **C8（测试存在性）：** 通过（G3 有 364 个测试）；但按模块的覆盖度权衡不在本清单范围。

---

## Observations

- **修复提交的改动面与预期一致：** `eba3a68` 只改 `tests/e2e.rs`（数组换行，无语义变化）与两份审计文档，`src/` 一行未动。因此本轮 C1/C2 的行号与不变量结论可以直接沿用上一轮的回读结果，无需重新判定。
- **上一轮报告中的「须重跑 G3」被落实：** 被改文件同时是 3 个 `real_capture_flashback_*` 真件 e2e 的输入载体，G3 复跑仍为 364 通过。
- **审计交付物已随代码入库：** `docs/AUDIT_REPORT.md` 与 `docs/AUDIT_LOG.md` 现在与源码同仓同提交（`eba3a68`），报告中的 `gates: G1=fail` 元数据块保留的是当时观测，与最新一处提交的「all gates now pass」自述并存——这是历史快照而非当前矛盾。
- **单包 workspace 包装：** `Cargo.toml` 的 `[workspace]` 仅含 `members = ["."]` 与 `exclude = ["fuzz"]`，实际仍是单 crate CLI。它满足 §MIGRATE / §A1 的字面通过条件，但不代表已按 7-crate 分层组织；是否拆分属所有者决策，本报告不记为发现。
- **`total_loc` 被非源码目录放大：** `detect-structure.sh` 报 `total_loc = 402697`、`rust_files = 787`，其中 `.qoder/worktrees/` 有 811 个 `.rs` 文件、`data/` 为 MySQL 数据目录（二者均在 `.gitignore` 内）。建议本项目审计在 `config.json` 的 `ignore_paths` 补 `.qoder`、`data`、`out`、`reference`、`.superpowers`；实测 `src/ + tests/ + benches/` 合计约 24,972 行。
- **`fuzz/` 是独立 workspace 且被 root 排除：** 根 `cargo clippy --all-targets` 不构建它；CI 中的 fuzz check 标为 `continue-on-error: true`。因此 `fuzz/src/bin/seedgen.rs` 中的 `unwrap()`/`expect()`/`println!` 既不进生产，也不进阻断门禁，本报告按非生产代码处理。
- **同仓库内两种锁使用范式并存：** `src/repl/assembly.rs:595` 用 `lock().unwrap_or_else(|p| p.into_inner())` 容错，`src/flashback/reverse.rs:96,105` 用 `lock().unwrap()` 直接 panic。功能上都有兜底，但风格不一致（见 C1 建议）。
- **`RTEST_GUIDE.md`（35 KB）仍未入库：** 工作区中的未跟踪文件。它不影响门禁（fmt/clippy/test 均不引用），但若是应有产物，值得决定是否纳入版本控制。

---

*Report generated by ai-dev-audit. Standards: ai-dev-discipline v1.*
*File: docs/AUDIT_REPORT.md*
