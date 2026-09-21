# my2sql-rs P3 repl 模式 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 新增 `repl` 子命令：以从库协议实时拉取主库 binlog 流，复用既有解码/SQL 生成/输出链路按事务边界流式产出 to-sql，带安全位点 checkpoint（`--resume-file` 接续）、心跳探活与指数退避自动重连。

**Architecture:** 方案 A——`mysql` crate `binlog` feature 只做「连接+认证+帧协议管道」，`src/repl/` 把帧字节转成既有 `RawEvent` 并实现 `EventSource` trait；`Runner` 泵层从「FileReader 专用」泛化为「`&mut dyn EventSource` 通用」；`Writer` 加流式刷盘 + 防重写覆盖开关；checkpoint 在事务提交边界（flush 之后）原子落盘。解码层 `src/binlog/*` **零改动**（冻结不变量升格门禁）。

**Tech Stack:** Rust std::thread、`mysql = { version = "28.0.2", features = ["binlog"] }`（Cargo.toml 改 feature）、`ctrlc = "3"`（SIGINT 优雅收尾）、serde_json（checkpoint）、docker（live e2e 与矩阵）。

**Spec:** `docs/superpowers/specs/2026-09-21-my2sql-rs-p3-repl-design.md`（本计划是其论证；spec §2 spike 项与 §7-8 冻结口径以本计划 Task 0/2 为准执行）。

## Global Constraints

- **`reference/my2sql-go/` 只读**（裁判源；`go build -o` 允许）。**repl 不做裁判差分**（spec §8：上游 repl 无优雅停止/Fatalf 即崩，不可作 oracle）；正确性总闸 = Task 6 的「repl vs file 模式同段逐字节等价」。
- **解码冻结门禁**：`git diff main..HEAD -- src/binlog/` 必须为空。repl 侧与 FileReader 相似的逐事件编排逻辑（header 解析→CRC 剥离→kind 映射→tm 跟踪→start_pos 语义）在 `src/repl/source.rs` **有意重复**（~80 行，冻结优先于 DRY，两处各有钉死测试）；复用 `src/binlog` 的公开件：`event::{EVENT_HEADER_SIZE, EventHeader, EventType, crc32_ok, fde_checksum_ok, parse_header, strip_checksum}`、`fde` 解析、`rows::RowsKind`、`table_map::TableMapEvent`。
- **P1/P2 回归红线**：to-sql/flashback/stats 既有 296 测试 + P1 字节面 e2e + `make compat` 14 件 + difftest 全绿；repl 是纯增量。
- **解码器不得 panic**；repl 直通形态（threads=1）无 catch_unwind，同守。
- **TDD**：每段生产代码先红后绿；**不许虚报**：矩阵/e2e/spike 数字逐字入档；**每任务收尾**三门（`cargo test`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`）+ `docs/HANDOVER.md` 节点。
- **无 async**；新依赖仅 `ctrlc`（+mysql feature），理由如 spec。
- 依赖图与并行策略见文末「执行编排」：T2∥T3、T8-debt∥主链可并行派发（并行实现者各设独立 `CARGO_TARGET_DIR=/tmp/p3-<task>` 防构建锁互抢，收尾三门在主 target 复跑）。

---

### Task 0: 协议 spike（闸部门，throwaway）

**Files:**
- Create: `examples/repl_spike.rs`（examples 目录随仓库走，标注 throwaway 后由 Task 8 决定去留——默认保留为诊断工具）
- Modify: `Cargo.toml`（`mysql = { version = "28.0.2", features = ["binlog"] }`）
- 产物: spec §2 勘误回填（Edit spec 文件本体，同提交）

**Interfaces:**
- Produces: 下表 6 项事实的**实测结论**，钉进 spec §2 勘误与 `src/repl/` 模块注释；Task 1/2 的字段形状以此为据。

- [ ] **Step 1: 起真容器** `docker run -d --name p3spike -e MYSQL_ROOT_PASSWORD=*** -p 13307:3306 mysql:8.0`（等健康），灌循环 DML（后台 shell 每 0.2s 一批 insert）。
- [ ] **Step 2: 写 `examples/repl_spike.rs`**：`mysql::Conn` 从 URL 连接（root；caching_sha2 默认插件）→ `SHOW MASTER STATUS` 打印 → 构造 `BinlogRequest`（server_id=9999, file/pos=当前）→ `get_binlog_dump` → 迭代打印每事件的：可及字节形态（`raw_event`/`SlicedEvent`/header+data 分离？逐 API 名记录）、event type、log_pos、CRC 尾在不在、fake rotate/heartbeat 是否出现且 crate 层是否透明、`FORMAT_DESCRIPTION_EVENT` 到达顺序。**不 panic 迭代**：`for ev in stream { ... }` 内 Err 打印后 continue，跑 60s。
- [ ] **Step 3: 六问清单**（结论逐条写进 spec §2 勘误，附本例输出摘要行）：
  1. 事件**完整原始字节**（19B 头+体+CRC）可及？路径名？（若否 → §9 降级 B2'：register/dump 自研走 crate 内部 packet 缝，评估后上报 BLOCKED 由控制方裁决）
  2. CRC32 剥离是 crate 做了还是没做？（决定 repl/source.rs 是否调 `strip_checksum`）
  3. FDE 是否总在流首？mid-file 起点时 fake rotate 事件形态？（决定 `--start-pos` 语义）
  4. heartbeat 请求面：BinlogRequest/dump flags 有无 heartbeat period 入口？无则退路 = 客户端读超时近似（记 `--heartbeat-secs` 实现策略）
  5. 认证：caching_sha2 + native 双过？`ssl-mode` query 参透传 Opts？
  6. 断链表现：`docker restart p3spike` 后迭代器 Err 形态（错误类型/文本），供 §6 终止/重试分类。
- [ ] **Step 4: 结论落 spec**（`docs/superpowers/specs/…p3-repl-design.md` §2 追加「Spike 实测（2026-09-21, mysql 28.0.2 @8.0.46）」小节，勘误随本次提交）。
- [ ] **Step 5: 清理 + 提交** `docker rm -f p3spike`；commit `spike(p3): binlog-dump surface of mysql crate 28 — six facts pinned to spec`。（spike 无测试=豁免 TDD 的 throwaway，examples 代码不得被生产模块引用：`grep -rn "repl_spike" src/` 必须空。）

### Task 1: CLI / Config 面（`repl` 第四子命令）

**Files:**
- Modify: `src/config.rs`（`Command` + `ReplArgs` + `ReplSpec` + `validate_repl` + `Config` 新字段）、`src/main.rs`（dispatch `Command::Repl` → `pipeline::run_repl`）。不用 `todo!()`：`run_repl` 空壳在本任务以真实返回落 `src/pipeline/mod.rs`——参数合法则 `Err(PipelineError::Config("repl: pipeline not built (P3 T5)"))`，main 打印该 Err 并退 1；T5 仅替换函数体，dispatch 面本任务定稿。
- Test: `tests/cli.rs` 追加、`src/config.rs` 单测追加

**Interfaces:**
- Produces: `Command::Repl(ReplArgs)`；`ReplArgs { #[command(flatten)] common: CommonArgs, #[command(flatten)] text: SqlTextArgs, #[arg(long)] server_id: u32, #[arg(long)] resume_file: Option<PathBuf>, #[arg(long, default_value_t = 30)] heartbeat_secs: u32 }`（`--uri` 走 CommonArgs，**repl 下必填**：validate 硬校）；`Config` 新字段 `server_id: Option<u32>, resume_file: Option<PathBuf>, heartbeat_secs: u32`（其余位点/窗口/输出字段全复用）；`Config::validate_repl(ReplArgs) -> Result<Config, String>`。
- Consumes: 既有 `build_common`/`SqlTextArgs`。

- [ ] **Step 1 红**（`src/config.rs` mod tests）：
```rust
#[test] fn repl_requires_uri_and_server_id() { /* 缺 uri → Err 含 "uri"；缺 --server-id → clap 缺参 exit2 */ }
#[test] fn repl_rejects_resume_with_explicit_start() { /* --resume-file + --start-file → Err 含 "歧义"；--resume-file + --to-stdout → Err 含 "output-dir" */ }
#[test] fn repl_defaults() { /* 缺省位点=now 哨兵：start_file 空且 start_pos==0 且无 resume → Config{..} Ok */ }
```
`tests/cli.rs`：`repl_subcommand_visible_and_rejects_bad_args`（`repl --help` exit 0 且含 `--server-id`；`repl`（裸）exit 2）。
- [ ] **Step 2 跑红** `cargo test repl_ -- --nocapture` → FAIL（Repl 变体不存在）。
- [ ] **Step 3 实现**：`validate_repl` 依 validate_to_sql 骨架 + 上列拒绝 + `heartbeat_secs` 仅校验 ≤3600（0 合法=禁用）；位点三态互斥复用现有 start 校验并新增「resume 在场时禁 start-*」；`run_repl` 空壳 + main match 分支。
- [ ] **Step 4 跑绿** + 三门。- [ ] **Step 5 提交** `feat(cli): repl subcommand surface — validate_repl with uri/server-id mandatory, resume exclusivity, heartbeat knob`。

### Task 2: `src/repl/` transport + `ReplSource`（EventSource 实现）

**Files:**
- Create: `src/repl/mod.rs`、`src/repl/transport.rs`、`src/repl/source.rs`
- Modify: `src/lib.rs`（`pub mod repl;`）、`Cargo.toml`（如无 T0 已加）
- Test: `src/repl/source.rs` 内 `#[cfg(test)]`（字节序列构造，无服务器）；`tests/common/synth.rs` 追加「事件字节流导出」helper（`pub fn frame_bytes(...)` 产 19B 头+体+CRC 全帧）

**Interfaces:**
- Consumes: T0 勘误事实（字节可及路径名、CRC 口径）；`crate::binlog::event::*` 公开件；`RawEvent/RawKind/EventSource`（`pipeline::source`）。
- Produces: `transport::open(uri: &str, server_id: u32, file: &str, pos: u32, heartbeat: Option<std::time::Duration>) -> Result<Box<dyn FrameStream>, ReplError>`；`trait FrameStream { fn next_frame(&mut self) -> Result<Option<Frame>, ReplError> }`、`struct Frame { pub bytes: Vec<u8> /* 全帧原始字节，CRC 未剥 */, pub binlog_hint: Option<String> /* fake rotate 指名 */ }`；`ReplError`（`thiserror`：`Auth/MissingPriv/Purged/Protocol/Server{code,msg}/Io`——T5 的重连分类学依据）；`ReplSource::new(transport: Box<dyn FrameStream>, first_binlog: String, filters: crate::pipeline::filter::Filters) -> ReplSource`（与 `FileReader::new(name, rdr, filters)` 同形，`src/binlog/file_reader.rs:70`），`impl EventSource for ReplSource`。
- 关键语义：`binlog_hint` 或帧内 ROTATE → 更新当前文件名；**FDE 到达 → `fde_checksum_ok` 定 CRC 有无**（与 FileReader 同套公开件，编排逻辑有意重复，头注释互相引用钉死）；rows 事件 `start_pos` = 最近 TABLE_MAP 起始（`source.rs` 模块注释原样引用 file_reader 的口径出处 file.go:197-215）；heartbeat/FakeRotate → 内部消化（`next()` 返回 `Ok(None)` 仅当**干净停止**：调用方 `stop` 命中由 dispatcher 层判，源层 EOF=断链 Err）。

- [ ] **Step 1 红**（单测，帧字节喂入不需服务器）：`fn repl_source_maps_synthetic_stream_byte_equal_to_file_reader()`——同一 synth 事件集：(a) 走 FileReader（既有通道）、(b) 包成 Vec<Frame> 走 ReplSource（`transport::open` 不碰——测试注入 `FrameStream` 的 `VecDeque` 假实现 `src/repl/mod.rs` 下 `#[cfg(test)]` + `pub(crate) fn for_test(frames)` 构造器），断言两侧 `RawEvent` 的 binlog/start_pos/end_pos/ts/kind 逐字段一致 + `body` 字节一致。`fn repl_source_tracks_rotate_and_checksum()`（fake rotate 更名、5.7 恒带 CRC 的 FDE 特例复用 `fde_checksum_ok` 红绿两态）。
- [ ] **Step 2 跑红**（模块不存在）。
- [ ] **Step 3 实现** `source.rs` 编排（~80 行，与 file_reader 同形异构，模块头写明「解码冻结下的有意重复，两测互钉」）+ `transport.rs`（mysql crate 适配进 `Frame`；heartbeat 由 T0 事实定：有入口就请求，没有就只暴露 `Option` 由 T5 用读超时兜底）。
- [ ] **Step 4 绿** + 三门（file 模式回归必全绿——没碰它）。
- [ ] **Step 5 提交** `feat(repl): FrameStream transport adapter + ReplSource EventSource with file-reader-parity framing (duplication pinned by dual test)`。

### Task 3: Writer 流式刷盘 + 防覆盖；checkpoint 模块

**Files:**
- Create: `src/repl/checkpoint.rs`
- Modify: `src/output.rs`（`Writer` 新开关两枚 + `flush_all`）
- Test: `src/repl/checkpoint.rs` 单测、`src/output.rs` 既有单测追加

**Interfaces:**
- Consumes: 无（与 T2 并行，文件不相交）。
- Produces: `Writer::with_live(dir: PathBuf, stdout: bool, file_per_table: bool, extra_info: bool, tz: FixedOffset, prefix: String, index: bool, streaming: bool, no_clobber: bool) -> Writer`（前三参起与既有 `Writer::new` 同序，尾追加两开关；`new` 改为委托 `with_live(.., false, false)`——T4 调用点同步）：`streaming=true` 时 `Sink::flush_short` 对 File 也 flush；`no_clobber=true` 时 `ensure_sink` 见**既存同名文件**即 `Err(Io(other("refusing to overwrite ...")))`（file 模式默认 (false,false) 行为字节不变）；`Writer::flush_all(&mut) -> io::Result<()>`。checkpoint：`struct Checkpoint { file: String, pos: u32, ts: String, written_files: Vec<String> }`（serde 双端）、`write_atomic(path, &Checkpoint) -> io::Result<()>`（tmp+rename，tmp 名 `.{file}.tmp`）、`read_verify(path, dir: &Path) -> Result<Checkpoint, CpError>`（JSON 合法 + `written_files` 与目录实物**一一对应**（缺/多均 `Stale`/`Missing` 硬错））、`CpError`。

- [ ] **Step 1 红**：`checkpoint_roundtrip_and_rename_atomicity`（写→读等值；残留 tmp 不顶正式档；`read_verify` 对 written_files 不符 → Err 含文件名）；`writer_streaming_flushes_file_sink_early`（临时目录：streaming 下未 finish 即可 `fs::read` 到全部内容；非 streaming 下 finish 前读为空=钉旧行为）；`writer_no_clobber_refuses_existing`（预建同名文件 → Err 含 "refusing to overwrite"；(false,false) 照常覆盖=回归钉）。
- [ ] **Step 2 跑红**。- [ ] **Step 3 实现**（字段两枚 + flush_short 分支 + checkpoint.rs ~120 行）。
- [ ] **Step 4 绿**（P1 字节面 e2e 必全绿：默认参未动）。- [ ] **Step 5 提交** `feat(repl): writer streaming-flush + no-clobber gates, atomic checkpoint file with written_files reconciliation`。

### Task 4: Runner 泛化泵 + 事务边界 checkpoint 队列

**Files:**
- Modify: `src/pipeline/mod.rs`（`pump_one_file` 的事件泵体抽 `pump_source(&mut self, src: &mut dyn EventSource, opening_binlog: &str) -> Result<(), PipelineError>`；run_pump 改经 `FileReader` 装箱调用之——**字节面零变化的纯重构**）；Runner 新字段 `ckpt_q: VecDeque<(u64 /*seq 水位*/, String /*binlog*/, u32 /*pos*/, String /*ts*/) >`（仅 repl 形态启用，file 模式恒空）
- Test: `tests/e2e.rs` P1 件（回归即门禁）；`src/pipeline/mod.rs` 追加 `ckpt_watermark_advances_only_after_full_trx_flushed` 单测

**Interfaces:**
- Produces: `Runner::run_live(&mut self, src: Box<dyn EventSource>, first_binlog: &str, ckpt: Option<&Path>) -> Result<RunSummary, PipelineError>`：泵事件；dispatcher 见 `TrxStatus::Commit/Rollback` 记 `(self.seq, ev.binlog.clone(), ev.end_pos, datetime_str(ev.timestamp))` 入 `ckpt_q`；emit 侧 Reorder 出组后：若已 emit 的 seq 越过队首水位 → `writer.flush_all()` → `checkpoint::write_atomic`（`written_files = writer.created()` 快照）→ pop。`stop_on_error` 链、DDL 收集、stats 通道**零改语义**。
- Consumes: T2 `ReplSource`（仅类型）、T3 `Checkpoint/write_atomic/flush_all`。

- [ ] **Step 1 红**：构造假 `EventSource`（VecDeque 回放 synth 帧，含两事务+提交位交错）跑 `run_live` 到临时 dir，断言 checkpoint 的 pos **总是**等于「已完整 flush 事务的提交事件 end_pos」且 kill 模拟（泵中返回 Err）后 checkpoint 停在整事务边界。
- [ ] **Step 2 跑红**（`run_live` 不存在）。- [ ] **Step 3 实现**（重构 + 队列 ~60 行）。
- [ ] **Step 4 绿 + 回归**：P1 e2e 逐字节、`cargo test` 全量。- [ ] **Step 5 提交** `refactor(pipeline): dyn EventSource pump + commit-boundary checkpoint watermark (file-mode bytes unchanged)`。

### Task 5: `run_repl` 装配——定位、重连、心跳、SIGINT

**Files:**
- Modify: `src/pipeline/mod.rs`（替换 T1 空壳为真 `run_repl`）、`src/metadata/store.rs`（在线 store 加 `pub fn list_binlogs(&mut self) -> Result<Vec<(String, u64, Option<u32>)>, MetaError>`（`SHOW BINARY LOGS`，5.6 无 `Purge` 列差异实测后钉））、`Cargo.toml`+`src/main.rs`（`ctrlc`）
- Test: `tests/repl.rs`（live docker 组：`#[ignore]` + 环境变量 `MY2SQL_TEST_URI` 门，本地 make 跑——与既有 live 件同型）

**Interfaces:**
- Consumes: T1 Config、T2 `transport::open/ReplSource`、T4 `run_live`。
- Produces: `run_repl(cfg) -> Result<RunSummary, PipelineError>`：resume（`read_verify` → start；冲突/失效硬错文案见下）→ 三态定位（now=`SHOW MASTER STATUS`；file+pos 直给；datetime=二分 `list_binlogs` 候选文件后 dump-from-4 + Filters.start_ts 客户端过滤）→ 内层泵 `run_live` → Err(`ReplError::Purged|Auth|MissingPriv` 或 3 连同因秒断) → 终止文案；其余 → 退避重连（1s 翻倍封顶 30s ±25% 抖动，起点=checkpoint）→ 循环；`stop` 命中/Ctrl-C → 收尾完整事务 + flush + checkpoint → Ok（Ctrl-C 时 main 退 130）。错误文案钉（逐字入测）：purge→`"replication position ... does not exist on master (binlog purged): choose a newer start"`；权限→`"user lacks REPLICATION SLAVE/CLIENT privilege"`；resume 失效文件被 purge→同 purge 文案前缀 `"resume point is gone: "`。
- 重连日志（一行/次，限流计数器单测钉）：`tracing::warn!("repl: reconnect #{K} in {backoff:.1}s (cause: {})")`。

- [ ] **Step 1 红**：`tests/repl.rs::repl_locates_now_and_streams_new_writes`（容器 uri → 起 `run_repl`（stop-file+pos=预灌段终点）→ 产出与 file 模式**同段逐字节一致**预演版：本任务只断言非空 + `SET NAMES` 头 + errors=0；完整等价性在 T6）；`repl_refuses_purged_start`（start-pos=999999999 于不存在文件 → Err 文案 contains）；单测：`reconnect_backoff_caps_and_jitters`、`same_cause_fast_fail_three`（注入假 transport）。
- [ ] **Step 2 跑红**。- [ ] **Step 3 实现**（装配 ~150 行 + ctrlc 桥：AtomicBool → 泵事件间隙检查）。
- [ ] **Step 4 绿**（live 组本任务先过 now+purge 两件；其余留 T6）。- [ ] **Step 5 提交** `feat(repl): run_repl assembly — tri-state locating, checkpoint resume, capped-backoff reconnect, SIGINT drain`。

### Task 6: live e2e 套件（等价性总闸 + resume + 重连）

**Files:**
- Modify: `tests/repl.rs` 补全；`Makefile`（`repl-test` 目标：起容器/等健康/跑 `cargo test --test repl -- --ignored`/清容器）
- Create: `tools/repl-e2e-lib.sh`（容器生命周期 + DML 灌流器，T7 复用）

**Interfaces:** Consumes: T5 全部真行为。Produces: 矩阵可复用的「灌流 → repl 抓取 → file 模式对照」脚本函数。

- [ ] **Step 1** `repl_stream_equals_file_mode_byte_for_byte`：8.0 容器灌混合 DML（含 JSON/blob/多行事务）→ repl 从起点拉到 stop-datetime → 同段事后 file 模式跑 → `diff -r` 逐字节相等（含 extra-info 头行——datetime 字段两边同为事件 ts 口径）。红例预检：若 Writer 头 `SET NAMES` 与文件名 N 一致而内容漂移 = 真缺陷上报。
- [ ] **Step 2** `repl_kill9_resume_zero_loss`：灌流中 `kill -9` repl 进程 → 校验 checkpoint=pos 停整事务界、`written_files` 与磁盘一致 → `--resume-file` + 新 output-dir 接续 → 两段合并后与不中断基准比对：**已提交事务零缺失**、跨界重复仅整事务且可列出。
- [ ] **Step 3** `repl_survives_server_restart`：灌流中 `docker restart` 容器 → repl 不退出、日志现 `reconnect #` ≥1、恢复后拉到 stop 条件 exit 0，产物与基准等价（允许重连段整事务重复，按 Step 2 同法比对）。
- [ ] **Step 4** 位点/停止矩阵：`repl_start_from_file_pos_matches_slice`（mid-file pos 起点 = file 模式同 pos 切片逐字节）；`repl_start_from_datetime_bisects_then_filters`；`repl_stop_pos_finalizes`（收尾 flush+checkpoint 在场、exit 0）；`repl_idle_60s_no_false_drop`（心跳 20s 下静默 60s 不断链）。
- [ ] **Step 5** 全量 `cargo test`（live 件之外全绿；live 件本地 uri 在场全绿）+ 三门；提交 `test(p3): live repl e2e — file-mode byte equivalence, kill-9 resume, restart-reconnect, locating matrix`。

### Task 7: compat 矩阵 repl 族（5.6–8.4 真件）

**Files:**
- Modify: `tools/compat-matrix.sh`（`run_case` 第 5 参 work 扩 `repl`：容器内灌流 + `repl` 抓取 + **对照物=file 模式同段**（无 Go 裁判，§8）→ tsv 行 `PASS repl-<ver> equivalent=<n> bytes`）、`docs/compat/matrix.md`（repl 4 行 + 无裁判理由一句）

- [ ] **Step 1** 扩脚本并**单轮真实跑** `make compat`（既有 14 + repl 4 = 18 全量）；tsv 逐字进 matrix.md（禁虚账：repl 行含等价性字节数）。任一版本红 → 停下修到绿或上报 BLOCKED（版本差（如 5.6 无 heartbeat 入口）按 T0 事实口径处理并在矩阵注记）。
- [ ] **Step 2** HANDOVER T7 节点 + 提交 `test(compat): repl family on 5.6-8.4 vs file-mode equivalence (no Go repl oracle, registered in spec 8)`。

### Task 8: 文档收口 + P2 挂账消费 + DoD

**Files:**
- Modify: `README.md`（矩阵行 repl ✅、快速上手实跑例句、差异登记 23+：repl 超集四件+无裁判差分理由）、`docs/HANDOVER.md`（T0–T7 节点、`P3 DoD` 节逐条证据、挂账消费/新挂账）、`src/stats/`（**P2 挂账**：stats Err 路径 JSONL 头收口——先红：现行为留半成品头；TDD 修）、`tools/run-difftest.sh` + 比较器（**P2 挂账**：`--dml`×stats 冒烟维度：stats smoke 断言在 `--dml insert` 下 report 总和==to-sql `--dml insert` 行数）
- Delete/keep 决定: `examples/repl_spike.rs`（默认保留，README 注「诊断样例」）

- [ ] **Step 1** stats JSONL 头收口（红→绿）+ `--dml`×stats 维度（跑一次真 difftest 记录）——两项独立于 repl 主链，**可与 T6/T7 并行**。
- [ ] **Step 2** bench 免跑声明核验（P3 无解码热路径改动：`git diff main..HEAD -- src/binlog/ benches/` 空即引用 P2 数字）；DoD §6/spec §10 逐条（命令+输出摘要）入 HANDOVER。
- [ ] **Step 3** 三门 + 全量回归 + 提交 `docs(p3): repl readme/matrix/handover + DoD, retire P2 stats carryovers`。

---

## 执行编排（并行图）

```
T0(spike,闸) ──┬─▶ T2 ──┐
               │        ├─▶ T4 ─▶ T5 ─▶ T6 ─▶ T7 ─┐
T1(cli) ───────┴─▶ T3 ──┘                          ├─▶ T8(文档/DoD 收尾)
T8-debt 子项（stats 头收口、--dml 维度）: 与 T2..T7 并行（独立文件）
```

- T0/T1 可同刻派发（T1 不依赖 spike 事实）；T2∥T3 文件面不相交（`src/repl/{transport,source}.rs` vs `src/repl/checkpoint.rs`+`src/output.rs`）。
- 并行实现者各用独立 `CARGO_TARGET_DIR`，任务收尾三门在主 target 复跑；共享 target 的合并冲突由控制方在 review 合流点处理（同文件相邻改动禁止并行）。
- SDD 每任务评审照旧（fresh implementer + task reviewer + ≤5 修复轮）；T6/T7 为 docker/长跑件，评审以 transcript+tsv 独立复现。

## 验收清单（whole-branch 终审前自查）

- [ ] `git diff main..HEAD -- src/binlog/` 空（冻结门禁）
- [ ] repl vs file 同段逐字节等价（8.0 主件 + 矩阵 4/4，tsv 逐字入档）
- [ ] kill-9 resume 零丢 + 整事务重复可界定；容器重启自动重连续拉
- [ ] `cargo test`（含 live ignored 组实跑）/ clippy -D / fmt 三门绿；P1/P2 全量回归绿
- [ ] `make compat` 18/18；README 差异登记 23+ 与 HANDOVER 节点/DoD 节齐；P2 挂账两项消费完毕
- [ ] reference/ 零改动；`grep -rn repl_spike src/` 空
