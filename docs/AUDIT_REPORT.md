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
| **项目** | my2sql-rs（my2sql） |
| **审计日期** | 2026-09-24 |
| **审计员** | ai-dev-audit |
| **标准** | ai-dev-discipline v1 |
| **Rust Edition** | 2024 |
| **模式** | Multi-Crate Workspace（实际 1 个成员，`[workspace]` 仅用于排除 `fuzz/`） |
| **已运行工具** | rg, jq, `ci-gate.sh`（项目自身 fmt/clippy/test 门禁），cargo audit（未安装，跳过） |

---

## Executive Summary

**总体健康度：** 🟡 需要关注

| 类别 | 状态 | 问题数 |
|---|---|---|
| MIGRATE. 架构模式 | N/A（`[workspace]` 存在，§MIGRATE 不适用） | 0 |
| A. Workspace 结构 | 🟢 | 0 |
| B. 依赖方向 | 🟢 | 0 |
| S. 安全 | 🟢 | 1 |
| G. CI 门禁一致性 | 🟡 | 1 |
| C. 代码质量 | 🟠 | 4 |
| D. 模块组织 | N/A（无 role crate） | 0 |
| E. 前端（SvelteKit） | N/A（无前端） | 0 |

**问题计数**

| 严重度 | 数量 |
|---|---|
| 🔴 Critical | 0 |
| 🟠 High | 2 |
| 🟡 Medium | 1 |
| 🔵 Low | 3 |
| **合计** | **6** |

---

## CI Gate Parity

**门禁结果：** 全部门禁通过（G1–G4 通过，G5 有 3 条未验证命令）
**工具链：** 本地 `rustc 1.96.0 (ac68faa20 2026-05-25)` vs 钉定 `1.96.0`（`.github/workflows/ci.yml`）

| ID | 门禁 | 实际运行命令 | 来源 | 结果 | 发现数 |
|---|---|---|---|---|---|
| G1 | 格式化 | `cargo fmt --check` | `.github/workflows/ci.yml` | ✅ pass | 0 |
| G2 | Lint（`-D warnings`） | `cargo clippy --all-targets -- -D warnings` | `.github/workflows/ci.yml` | ✅ pass | 0 |
| G3 | 测试 | `cargo test --no-fail-fast` | `.github/workflows/ci.yml` | ✅ pass（364 个测试） | 0 |
| G4 | 工具链一致性 | `rustc -V` vs 钉定版本 | `.github/workflows/ci.yml` | ✅ pass | 0 |
| G5 | 门禁覆盖 | — | — | ⚠️ warn | 3 |

### CI 发现但本次未在本地运行的命令

- `cargo build --release --target x86_64-unknown-linux-musl` —— musl 静态发布构建门禁未本地验证（需要 musl 交叉工具链 + musl-tools）
- `cargo check --manifest-path fuzz/Cargo.toml` —— fuzz workspace 编译检查未本地验证（CI 中为 `continue-on-error`，非阻断）
- `cargo build --release --target ${{ matrix.target }}` —— release.yml 双目标矩阵构建未本地验证

这三条均属构建/打包类命令，与代码质量门禁（fmt/lint/test）无关；但 G5 要求如实声明：本次审计**未**完整复现 CI 的全部构建步骤。

### 门禁失败

无。G1–G4 全部通过。相较上一次审计（2026-09-24 早前条目记录的 G3 失败：`real_capture_flashback_*` 三项失败），本次 G3 已恢复绿色。

---

## Adversarial Audit Pass

**已审阅输入：** 原始扫描 JSONL（quality / security / gate / dep）、本报告草稿、`git diff`（未提交变更摘要）、适用检查清单章节（§A/§B/§S/§G/§C）。
**结果：** 未发现重大缺口

### 新增或重分类的发现

| ID | 动作 | 严重度 | 位置 | 证据 |
|---|---|---|---|---|
| S3 | 重分类 High→Low | Low | `tests/repl.rs` 等 9 处 + `src/pipeline/mod.rs:1353` | 全部命中位于测试代码（含 `#[cfg(test)]` 模块），插值对象为测试本地常量，无用户输入注入面；生产代码走 `mysql` crate 参数化 API |
| C9 | 重分类 Medium→Low | Low | `src/main.rs` 等 | CLI 工具，stdout/stderr 即产品输出通道（main.rs 注释明确「摘要行单独 println 到 stdout」「P1 逐字节不变」输出契约），非服务器语境下的调试残留 |
| C5 | 重分类 High→Low | Low | `src/main.rs:1` | 46 行中超 20 行为设计注释，实际代码约 20 行纯装配（tracing 初始化 + work_type 分派 + 错误处理），无业务逻辑/结构体/处理器内联 |
| C1 | 保持 High | High | `src/flashback/reverse.rs:96,105,118`、`src/pipeline/mod.rs:160` | 3 处 `lock().unwrap()` 位于 `std::thread::spawn` 工作线程内（gotcha 明确的最坏情况：panic 被吞、任务静默死亡）；`output_dir.unwrap()` 无前置校验 |
| C2 | 保持 High | High | `src/pipeline/mod.rs:579,582,586`、`src/repl/assembly.rs:613` | 运行时检查点队列不变量断言 + repl 装配不变量断言，属运行期 `expect()`，非启动期 fail-fast |

### Pass 覆盖

- S1：`src/` 中零 `unsafe` 块
- S2：未发现硬编码密钥字面量
- C4：无 domain role crate，DTO 泄漏检查跳过
- C10：`src/` 中零 TODO/FIXME/HACK 标记
- C7：无 `anyhow` 依赖（项目使用 `thiserror`）
- B1：无禁止依赖方向（角色映射为空，仅 1 个包）

### 假设

- 被审计仓库在审计开始时已处于脏状态（`src/pipeline/mod.rs` 及两份 docs 文件未提交）；G3 通过反映的是含该未提交修复的状态。
- 检测脚本的 `total_loc=402674` 被 `.qoder/worktrees/`（811 个 .rs 文件）与 `data/`（MySQL 数据目录）显著放大，真实源码规模约 19k 行（`src/`）+ 6k 行（`tests/`）。

---

## Issues

### 🔴 Critical

无。上一轮审计标记的 MIGRATE-1（单 crate 架构）本次不再成立：根 `Cargo.toml` 已含 `[workspace]`（`members = ["."]`、`exclude = ["fuzz"]`），字面上满足 MIGRATE-1 的通过条件。但见「Observations」中关于该 workspace 实为「单包 workspace 包装」的说明。

### 🟠 High

#### C1: 生产代码中的 `unwrap()`（含线程内 `lock().unwrap()`）

- **观察到的：** `quality-scan.sh` 共报 25 处测试外的 `unwrap()`。经上下文甄别，真正的高风险集中在：
  ```
  src/flashback/reverse.rs:96   q.lock().unwrap().pop_front()      // thread::spawn 内
  src/flashback/reverse.rs:105  *e.lock().unwrap() = Some(ioe)     // thread::spawn 内
  src/flashback/reverse.rs:118  err.lock().unwrap().take()         // 工作线程错误槽
  src/pipeline/mod.rs:160       cfg.output_dir.clone().unwrap()    // Option 无前置校验
  ```
  其余约 14 处为长度已校验的「切片转定长数组」不可失败转换（如 `src/binlog/event.rs:68` 前有 `buf.len() < EVENT_HEADER_SIZE` 守卫，注释明确「长度已验证，切片转数组必然成功」），5 处位于 `benches/decode.rs` 与 `fuzz/src/bin/seedgen.rs`（非生产代码），1 处 `src/metadata/store.rs:274` 为「先 push 后 find」逻辑上必然命中。
- **标准：** `C1` —— 生产代码不应有测试外的 `unwrap()`；gotcha「unwrap() Context Matters」特别指出：`thread::spawn` 内的 `unwrap()` 比普通 unwrap 更糟——panic 被吞、任务静默死亡。
- **评估：** High —— `flashback/reverse.rs` 的三处 `lock().unwrap()` 位于多工作线程中，锁污染将导致工作线程 panic；虽有 `h.join().is_err()` 兜底，但锁污染场景下会退化为「整批 flashback 失败」而非优雅降级。`pipeline/mod.rs:160` 的 `output_dir.unwrap()` 若为 `None` 直接 panic。
- **建议响应（咨询性质——由所有者决定）：**
  1. `lock().unwrap()` 改为 `lock().map_err(...)` 或确认锁从不跨 panic 路径持有并在注释中记录该不变量；
  2. `output_dir` 改为在配置校验阶段（`validate_*`）保证 `Some`，或在运行期用 `ok_or(...)` 传播错误。
- **工作量（粗略，咨询性质）：** S（涉及 2 个文件、约 5 处）

#### C2: 运行期 `expect()`（检查点队列 / repl 装配不变量）

- **观察到的：** `quality-scan.sh` 共报 12 处测试外的 `expect()`。真正属于运行期不变量断言的是：
  ```
  src/pipeline/mod.rs:579   self.ckpt_q.front().expect("non-empty checked")
  src/pipeline/mod.rs:582   self.ckpt_q.pop_front().expect("front checked")
  src/pipeline/mod.rs:586   .expect("ckpt_q 仅在 ckpt_out=Some 时记录")
  src/repl/assembly.rs:613  report.expect("上面 match 的非重连臂均已 break，此际必为 Some")
  ```
  其余约 6 处在 `config.rs`（`FixedOffset::east_opt(0).expect("zero offset is valid")`，零偏移恒合法）与 `output.rs`（「just inserted」/「always valid」）为逻辑上不可失败，2 处在 `benches`/`fuzz`（非生产）。
- **标准：** `C2` —— `expect()` 仅应在启动/配置期 fail-fast 可接受；运行期应返回 `Result` 或优雅处理。gotcha「expect() in Startup vs Runtime」。
- **评估：** High —— 检查点队列（`ckpt_q`）与 repl 装配是数据完整性的关键路径；不变量一旦被未来改动破坏，将以 panic 而非可诊断错误的形式暴露。
- **建议响应（咨询性质）：** 将这些不变量断言改为返回 `PipelineError`/`ReplError` 的显式分支，或至少在 panic 消息中保留可定位的上下文。
- **工作量（粗略，咨询性质）：** S–M（约 4 处）

---

### 🟡 Medium

#### G5: 3 条 CI 构建命令未本地验证

- **观察到的：** `.github/workflows/ci.yml` 与 `release.yml` 发现 3 条本次未运行的命令：musl release 构建、fuzz workspace `cargo check`、release 双目标矩阵构建。
- **标准：** `G5` —— 门禁覆盖诚实性：CI 运行而本次未运行的命令必须声明，不得以沉默冒充覆盖。
- **评估：** Medium —— 不影响代码质量门禁（fmt/lint/test 已绿），但意味着本次审计未验证 musl 静态构建与 fuzz workspace 的编译健康。
- **建议响应（咨询性质）：** 在有 musl-tools 的环境补跑 `cargo build --release --target x86_64-unknown-linux-musl` 与 `cargo check --manifest-path fuzz/Cargo.toml`。
- **工作量（粗略，咨询性质）：** XS（环境就绪前提下）

---

### 🔵 Low

#### C5: `main.rs` 46 行（超 30 行启发式）

- **观察到的：** `src/main.rs` 共 46 行，其中约 26 行为设计决策注释（mimalloc、tracing 输出通道、退出码 130 契约等），实际代码约 20 行为纯装配：`tracing_subscriber::fmt::init()` + `match cfg.work_type` 分派 + 错误处理。
- **标准：** `C5` —— `main.rs` ≤ 30 行、仅装配；gotcha「app/main.rs Line Count」明确纯装配的 main.rs（即便 45 行）比内联业务逻辑的 25 行更可接受。
- **评估：** Low —— 启发式超限，但底层规则（业务逻辑入 app）未被真正违反，仅注释密度拉高了行数。
- **建议响应（咨询性质）：** 可选；若在意启发式，可将设计注释收敛到文档。

#### C9: 测试外的 `println!`/`eprintln!`

- **观察到的：** 13 处，集中在 `src/main.rs`（摘要行 `println!`）、`src/pipeline/mod.rs`、`src/config.rs`、`benches`、`fuzz`。
- **标准：** `C9` —— 测试外零 `println!`/`dbg!`/`eprintln!`，改用 `tracing` 宏。
- **评估：** Low —— 本项目是 CLI 工具，stdout/stderr 即产品输出通道；`main.rs` 注释明确「摘要行单独 println 到 stdout」「P1 逐字节不变」输出契约。这不是服务器语境下的调试残留，而是对外契约。重分类 Medium→Low。
- **建议响应（咨询性质）：** 维持现状；仅 `src/pipeline/mod.rs:643` 的进度性 `eprintln!` 可考虑转 `tracing`。

#### S3: 测试代码中的 `format!()` SQL 拼接

- **观察到的：** 10 处，其中 9 处在 `tests/`（`tests/repl.rs`、`tests/e2e_drop_recovery.rs`），1 处在 `src/pipeline/mod.rs:1353`（位于 `#[cfg(test)]` 模块内）。插值对象均为测试本地常量（`test_db`、生成的 tag、`t10` 表名等）。
- **标准：** `S3` —— 所有 DB 查询应参数化；gotcha「SQL Injection」：插值为代码内常量而非用户输入时可接受，需甄别。
- **评估：** Low —— 无用户输入到达这些字符串，不存在注入面；生产代码走 `mysql` crate 参数化 API，`src/` 生产路径无 SQL 拼接。重分类 High→Low。
- **建议响应（咨询性质）：** 测试代码维持现状；如未来将类似模式带入生产，须参数化。

---

## Risk Priority Plan

按严重度优先、其次按置信度与影响半径排序。**仅咨询性质**——由所有者决定是否行动、以何顺序行动、以及是否行动。

| 优先级 | ID | 标题 | 建议响应 | 备注 |
|---|---|---|---|---|
| 1 | C1 | 工作线程内 `lock().unwrap()` + `output_dir.unwrap()` | 改 `map_err`/`ok_or` 或记录不变量 | 仅约 4 处生产路径，收益集中 |
| 2 | C2 | 检查点队列 / repl 装配运行期 `expect()` | 改为显式错误分支 | 数据完整性关键路径 |
| 3 | G5 | 3 条 CI 构建命令未验证 | 补跑 musl 构建与 fuzz check | 环境就绪后 XS |
| 4 | S3 | 测试内 `format!()` SQL | 维持现状，生产勿引入 | 无注入面 |
| 5 | C9 | CLI 输出契约 | 维持现状 | 产品输出通道 |
| 6 | C5 | `main.rs` 46 行 | 可选，收敛注释 | 纯装配，无业务逻辑 |

### 建议分诊

**立即所有者评审：**
- 无 Critical。

**计划性修复：**
- C1、C2（生产 panic 路径）。

**跟踪与复查：**
- G5（构建门禁覆盖缺口）。

**积压：**
- S3、C9、C5。

---

## Passed Checks

✅ S1（unsafe 块）：`rg "unsafe" src/` 零命中
✅ S2（硬编码密钥）：保守模式扫描零命中
✅ C10（TODO/FIXME/HACK 标记）：`src/` 零命中
✅ C7（anyhow 作用域）：无 `anyhow` 依赖（使用 `thiserror`）
✅ B1（依赖方向）：无禁止依赖方向（单成员 workspace，角色映射为空）
✅ A1/A2（workspace 清单与成员声明）：`[workspace]` + `members = ["."]` + `exclude = ["fuzz"]` 存在
✅ G1（格式化门禁）：`cargo fmt --check` 退出 0 —— 无文件需重排
✅ G2（lint 门禁）：`cargo clippy --all-targets -- -D warnings` 退出 0 —— 无被拒警告
✅ G3（测试门禁）：`cargo test --no-fail-fast` 退出 0 —— 364 个测试通过
✅ G4（工具链一致性）：本地 `rustc 1.96.0` 与 CI 钉定 `1.96.0` 一致

---

## Skipped / Not Applicable

- **S5（已知 CVE）：** cargo-audit 未安装 —— 运行 `cargo install cargo-audit` 后在部署前复检。
- **C4（DTO 泄漏入 domain）：** 无 domain role crate，检查不适用/未验证。
- **D1–D5（模块组织）：** 需 domain/server/api/infra/common role crate，单成员 workspace 下不适用。
- **E1–E6（前端）：** 无前端（`has_frontend=false`，0 个 Svelte 文件）。
- **MIGRATE-1/2/3：** `[workspace]` 存在，§MIGRATE 不适用。
- **S4（鉴权中间件）：** 无 HTTP 服务层，不适用。

---

## Observations

- **「单包 workspace 包装」模式：** 根 `Cargo.toml` 的 `[workspace] members = ["."]` + `exclude = ["fuzz"]` 使结构检测判定为 multi-crate，但实际仅 1 个成员 crate（`my2sql-rs`）。该 workspace 段的唯一目的是把 `fuzz/`（独立 cargo-fuzz workspace）排除出根构建。从 ai-dev-discipline 的「7 crate 分层」标准看，这仍是单包架构——但该标准针对 HTTP/Axum 服务设计（app/server/api/domain/infra/config/common），对一个 binlog 解析 CLI 工具并不自然。是否拆分（如拆出 `my2sql-core` 库 crate + 薄 CLI 壳）属所有者决策，本审计不作 Critical 判定。
- **模块组织良好：** 单 crate 内部已有清晰的领域分界——`binlog/`（协议解析）、`pipeline/`（调度）、`repl/`（实时复制）、`flashback/`（回滚生成）、`metadata/`（schema 存储）、`sqlopen/`、`stats/`。这比盲目套用 7 crate 更贴合本项目语义。
- **错误处理哲学：** binlog 解析路径大量使用「长度守卫 + 不可失败转换」的 `try_into().unwrap()`，配合 `thiserror` 类型化错误在边界兜底，构成一致的 fail-fast 语义（与既有 `AUDIT_LOG.md` 记录的「坏输入即坏回滚」设计立场一致）。真正的隐患是少量未走该语义的裸 `lock().unwrap()` / `output_dir.unwrap()`。
- **检测脚本 LoC 失真：** `detect-structure.sh` 报 `total_loc=402674`、`rust_files=787`，其中 811 个 .rs 文件来自 `.qoder/worktrees/`（历史 git worktree 副本）与 `data/`（MySQL 数据目录）。真实源码约 `src/` 37 文件 18820 行 + `tests/` 10 文件 5946 行。`ignore_paths` 未包含 `.qoder`/`data`，如需精确计数应补充。
- **仓库处于脏状态：** 审计开始时 `src/pipeline/mod.rs`、`docs/AUDIT_LOG.md`、`docs/AUDIT_REPORT.md` 已存在未提交变更。`src/pipeline/mod.rs` 的未提交修改正是修复上一轮 G3 失败（`real_capture_flashback_*` 三项）的关键：回退了「智能多文件扫描」`detect_next_binlog_exists` 为保守单文件行为，并使 `PartialNotSupported` 源级硬错误不被 `on-error-skip` 吞掉。G3 通过反映的是含该修复的状态。
- **测试规模变化：** 本次 G3 报 364 个测试，低于早前审计条目声称的 949 个。二者计数口径或测试裁剪原因未在本审计范围内确认，仅如实记录。

---

*Report generated by ai-dev-audit. Standards: ai-dev-discipline v1.*
*File: docs/AUDIT_REPORT.md*
