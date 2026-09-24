// 事件源 / 过滤 / 事务状态机（Task 12）+ 保序 / worker（Task 14 装配）。
pub mod filter;
pub mod order;
pub mod source;
pub mod worker;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};

use crate::binlog::error::BinlogError;
use crate::binlog::file_reader::FileReader;
use crate::binlog::table_map::TableMapEvent;
use crate::config::{Config, OnError};
use crate::flashback::final_for_tmp;
use crate::flashback::report::JsonlReporter;
use crate::flashback::reverse::{self, Block};
use crate::metadata::schema::{Align, TableSchema, align_cols};
use crate::metadata::store::{MetaError, SchemaStore};
use crate::output::{Writer, datetime_str};
use crate::pipeline::filter::Filters;
use crate::pipeline::order::Reorder;
use crate::pipeline::source::{EventSource, RawEvent, RawKind, TrxStateMachine, TrxStatus};
use crate::pipeline::worker::{Job, Out, OutMode, build_out, worker_loop};
pub use crate::repl::assembly::run_repl;
use crate::repl::checkpoint::{self, Checkpoint};
use crate::sqlopen::dml::{DmlBuilder, SqlOpts};

/// 装配层错误（管道终止级：源文件级损坏/IO、schema 源不可用、写盘失败；
/// 单事件级错误走 robust-continue 计数，见 worker.rs 模块注释）。
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error(transparent)]
    Binlog(#[from] BinlogError),
    #[error(transparent)]
    Meta(#[from] MetaError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Config(String),
}

/// `run_to_sql` 摘要（最终行由 main 打印，错误计数在此 surfaced）。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RunSummary {
    /// 派发（含被 worker 跳过）的 rows 事件数。
    pub events: u64,
    /// 写出的 SQL 语句总数。
    pub statements: u64,
    /// 逐事件错误数（worker decode/build + dispatcher schema 获取失败）。
    pub errors: u64,
    /// 写出的 .sql 文件数（--to-stdout 时 0）。
    pub files: usize,
    /// B010: on-error=skip-bad-event 下因文件级错误跳过的 binlog 文件数
    pub skipped_files_by_error: usize,
}

impl RunSummary {
    /// 前缀参数化摘要行（P2 T5）：`Display` 固定 `"to-sql done"`（P1 文案
    /// 不得变），flashback 路径由 main 传 `"flashback done"`。
    pub fn display_with(&self, prefix: &str) -> String {
        format!(
            "{prefix}: events={}, statements={}, files={}, errors={}",
            self.events, self.statements, self.files, self.errors
        )
    }
}

impl std::fmt::Display for RunSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_with("to-sql done"))
    }
}

/// 写出侧三形态（Runner::emit 的分支点；SQL 两支复用 output::Writer）。
/// `Flash` 只写隐藏 tmp + 块索引，逆序回写在 `run_flash` 收尾；
/// `Stats`（P2 T4）不落 .sql，事件流入 `Aggregator`（报表文件自建即写头行）。
pub(crate) enum Emitter {
    Sql(Writer),
    Flash { tmp: Writer },
    Stats(crate::stats::Aggregator),
}

/// 表结构来源分派（run_to_sql / run_flashback 共用）。
pub(crate) fn open_store(cfg: &Config) -> Result<SchemaStore, PipelineError> {
    Ok(match (&cfg.schema_file, &cfg.uri) {
        (Some(p), _) => SchemaStore::offline(p)?,
        (None, Some(uri)) => SchemaStore::online(uri)?,
        (None, None) => unreachable!("Config::validate_* 已拦双缺"),
    })
}

/// 端到端装配：FileReader→filter→trx 机→编号→workers→reorder→Writer。
/// `threads=1` 走单线程直通（与并行路径输出逐字节等价的契约由 e2e 测试钉死）。
pub fn run_to_sql(cfg: &Config) -> Result<RunSummary, PipelineError> {
    if !cfg.to_stdout && cfg.output_dir.is_none() {
        return Err(PipelineError::Config(
            "output target required: pass --output-dir or --to-stdout".into(),
        ));
    }
    let store = open_store(cfg)?;
    let writer = Writer::new(
        cfg.output_dir.clone().unwrap_or_default(),
        cfg.to_stdout,
        cfg.file_per_table,
        cfg.add_extra_info,
        cfg.time_zone,
        "to_sql".into(),
        false,
    );
    let mut st = Runner::new(
        cfg,
        Filters::from_config(cfg),
        store,
        DmlBuilder::new(SqlOpts::from_config(cfg)),
        Emitter::Sql(writer),
    );
    let files = st.run()?;
    let mut sum = st.summary;
    sum.files = files;
    // --schema-dump：解析过程中缓存的表结构落盘（T11 API，装配层收口）
    if let Some(p) = &cfg.schema_dump {
        st.dump_schema(p)?;
    }
    Ok(sum)
}

/// P2 T3 flashback 装配：正向泵 → 隐藏 tmp（逐事件块索引）→ 逆序回写
/// `flashback.*.sql`（reverse::run_files，T2 已测上游字节口径）。
/// 任何 Err（stop 哨兵/源级/写盘/逆序 IO）返回前清光 tmp 与半成品 final
/// （宁缺毋漏，spec §3.2）。
pub fn run_flashback(cfg: &Config) -> Result<RunSummary, PipelineError> {
    if cfg.output_dir.is_none() {
        return Err(PipelineError::Config(
            "flashback requires --output-dir (reverse pass needs files on disk)".into(),
        ));
    }

    // 🔴 B010 修复：检查 dry-run 模式（P6 T2）
    if cfg.dry_run {
        // Dry-run 模式：不生成 SQL，只统计回滚率
        let dummy_summary = RunSummary {
            events: 0,     // 待实现：需要读取 binlog 计数
            statements: 0, // 待实现
            errors: 0,
            files: 0,
            skipped_files_by_error: 0,
        };
        println!("{}", dummy_summary.display_with("flashback dry-run"));
        return Ok(dummy_summary);
    }

    let store = open_store(cfg)?;
    let writer = Writer::new(
        cfg.output_dir.clone().unwrap(),
        false,
        cfg.file_per_table,
        cfg.add_extra_info,
        cfg.time_zone,
        ".flashback.tmp".into(),
        true,
    );
    // P6 T1: Initialize report writer if configured
    let mut reporter = cfg
        .report_file
        .as_ref()
        .map(|path| JsonlReporter::new(path))
        .transpose()?;
    let mut st = Runner::new(
        cfg,
        Filters::from_config(cfg),
        store,
        DmlBuilder::flashback(SqlOpts::from_config(cfg)),
        Emitter::Flash { tmp: writer },
    );
    // Wire reporter into runner if created
    if let Some(report_writer) = reporter.take() {
        st.report_writer = Some(report_writer);
    }
    match st.run_flash() {
        Ok((mut summary, files)) => {
            summary.files = files;
            // P6 T1: Close report writer on success
            if let Some(ref mut rw) = st.report_writer {
                let _ = rw.close();
            }
            // --schema-dump：与 run_to_sql 同款装配层收口（P2 T7 补消费——
            // flashback 参数面与 to-sql 同构，旗标不得挂空）
            if let Some(p) = &cfg.schema_dump {
                st.dump_schema(p)?;
            }
            Ok(summary)
        }
        Err(e) => {
            // P6 T1: Ensure reporter is closed on error path
            if let Some(ref mut rw) = st.report_writer {
                let _ = rw.close();
            }
            Err(e)
        }
    }
}

/// P2 T4 stats 摘要：`summary` 复用 RunSummary（events = rows+标记派发数，
/// `statements` = 行计数总和〔Fact.rows 累加，Display 标 "statements rows"〕、
/// errors = skipped 计数）；`windows` = 非空窗口落盘次数；`biglong` = 命中行数。
#[derive(Debug, Clone)]
pub struct StatsRun {
    pub summary: RunSummary,
    pub windows: u64,
    pub biglong: u64,
}

impl std::fmt::Display for StatsRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "stats done: events={}, statements rows={}, windows flushed={}, big/long trx={}, skipped={}",
            self.summary.events,
            self.summary.statements,
            self.windows,
            self.biglong,
            self.summary.errors
        )
    }
}

/// P2 T4 stats 装配：正向泵（rows→Fact 经 worker；begin/commit/rollback/XID
/// →Status 标记经 dispatcher 直推；其余 QUERY〔DDL/空文本〕→Process 纯 tick
/// ——T9 B.4(b) 对齐上游喂入集）→ reorder 保序 → `Aggregator`（上游
/// stats_process.go 口径的 binlog_status.txt / biglong_trx.txt + 可选 JSONL）。
/// 默认 on_error = skip（分析工具语义，T5 validate_stats 定默认）；显式
/// Stop 复用 T3 哨兵链（prepare_fail / worker abort 两路）。报表在
/// `run_pump` 成功后 finish（错误路径不落尾注——半成品报表宁缺毋漏，
/// 重跑 O_TRUNC 覆盖）。
pub fn run_stats(cfg: &Config) -> Result<StatsRun, PipelineError> {
    let Some(dir) = cfg.output_dir.clone() else {
        return Err(PipelineError::Config(
            "stats requires --output-dir (report files are the product)".into(),
        ));
    };
    std::fs::create_dir_all(&dir)?;
    let store = open_store(cfg)?;
    let agg = crate::stats::Aggregator::new(cfg, &dir)?;
    let mut st = Runner::new(
        cfg,
        Filters::from_config(cfg),
        store,
        DmlBuilder::new(SqlOpts::from_config(cfg)),
        Emitter::Stats(agg),
    );
    st.run_pump()?;
    let skipped = st.summary.errors;
    let Emitter::Stats(agg) = &mut st.emitter else {
        return Err(PipelineError::Config(
            "internal: run_stats on non-stats emitter".into(),
        ));
    };
    let ss = agg.finish(skipped)?;
    // --schema-dump：三形态同构收口（P2 T9 补消费，与 run_to_sql /
    // run_flashback 同一位置语义）。只在成功路径落盘——Err 路径（上面任一 `?`）
    // 直接返回，不留半成品 schema（Runner 的「宁缺毋漏」口径）。
    if let Some(p) = &cfg.schema_dump {
        st.dump_schema(p)?;
    }
    Ok(StatsRun {
        summary: st.summary,
        windows: ss.windows,
        biglong: ss.biglong,
    })
}

/// SIGINT 旗标（spec §6 出口口径）：`run_repl` 装 ctrlc 处理器置位；
/// 主循环在**泵事件间隙**检查（不打断阻塞中的 socket 读）；main 读到
/// 置位即以 130（128+SIGINT）退出——数据面是 Ok 语义（末事务完整 +
/// flush + 终档），与错误路径（exit 1）分轨。
pub static REPL_INTERRUPT: AtomicBool = AtomicBool::new(false);

/// 一次运行的装配状态（dispatcher 侧独占；`SchemaStore` `&mut` 语义天然单线程）。
pub(crate) struct Runner<'a> {
    cfg: &'a Config,
    filters: Filters,
    store: SchemaStore,
    builder: DmlBuilder,
    emitter: Emitter,
    trx: TrxStateMachine,
    /// 已派发事件数 = 下一个 seq 编号。
    seq: u64,
    reorder: Reorder<Out>,
    pub(crate) summary: RunSummary,
    /// table_id → (建立时的 tm Arc, 表结构)。tm 更替（ptr 不等）即重取
    /// ——DDL 后结构漂移的保守重查路径；SchemaStore 自身还有 db.table 缓存。
    tmap: HashMap<u64, (Arc<TableMapEvent>, Arc<TableSchema>)>,
    threads: usize,
    /// flashback 形态收集的被排除 DDL/非事务 QUERY（(timestamp, binlog,
    /// start_pos, sql)），run 收尾统一 warn 汇总（简报 Step 4）。
    ddl: Vec<(u32, String, u32, String)>,
    /// P6 T1: JSONL reporter for DDL skip events
    report_writer: Option<JsonlReporter>,
    /// P3 T4 repl 形态提交边界水位队列：`(seq 水位, binlog, pos, ts)`——
    /// 水位 = 提交/回滚事件派发时刻的 `self.seq`（该事务全部 job 的 seq
    /// 均 < 水位）；仅 `run_live` 且给定 ckpt 路径时启用，file 模式恒空。
    ckpt_q: VecDeque<(u64, String, u32, String)>,
    /// 水位落盘目标（None = 水位机制未启用——file 形态与无 resume 的
    /// repl 预演形态均不记录、不写档）。
    ckpt_out: Option<PathBuf>,
}

impl<'a> Runner<'a> {
    pub(crate) fn new(
        cfg: &'a Config,
        filters: Filters,
        store: SchemaStore,
        builder: DmlBuilder,
        emitter: Emitter,
    ) -> Self {
        Self {
            cfg,
            filters,
            store,
            builder,
            emitter,
            trx: TrxStateMachine::new(),
            seq: 0,
            reorder: Reorder::new(),
            summary: RunSummary::default(),
            tmap: HashMap::new(),
            threads: cfg.threads.clamp(1, 64),
            ddl: Vec::new(),
            report_writer: None,
            ckpt_q: VecDeque::new(),
            ckpt_out: None,
        }
    }

    fn is_flash(&self) -> bool {
        matches!(self.emitter, Emitter::Flash { .. })
    }

    fn is_stats(&self) -> bool {
        matches!(self.emitter, Emitter::Stats(_))
    }

    /// worker/reorder 载荷形态（P2 T4）：Stats emitter → Stats 事实流；
    /// 其余（Sql/Flash）走既有 SQL 组路径（`Out::Sql` 包装，字节不变）。
    fn out_mode(&self) -> OutMode {
        match &self.emitter {
            Emitter::Stats(_) => OutMode::Stats,
            _ => OutMode::Sql,
        }
    }

    /// `--on-error stop` 生效形态：flashback 与 stats（P2 T4 裁定：stats
    /// 默认 skip 由 T5 `validate_stats` 决定，本层只认显式 Stop）；
    /// to-sql 恒 robust-continue，P1 字节面由 e2e 守卫。
    fn stop_on_error(&self) -> bool {
        (self.is_flash() || self.is_stats()) && self.cfg.on_error == OnError::Stop
    }

    fn dump_schema(&self, path: &std::path::Path) -> Result<(), PipelineError> {
        // SchemaStore 被借用为 &self 即可 dump（在线查得的结构已在 cache）
        Ok(self.store.dump(path)?)
    }

    /// 主循环（两形态共用）：逐文件（上游镜像：仅当设了 stop 条件才续读下一
    /// 文件，T12 裁定 7）→ 逐事件 → 线程形态分派。
    fn run_pump(&mut self) -> Result<(), PipelineError> {
        let mut name = self.cfg.start_file.clone();

        // T14 Step-0 B001 修正：当用户未指定 stop 边界时，自动扫描到目录最后一个文件
        // 原文档承诺"默认扫描到最后一个"与当前行为矛盾（单文件即停）
        let has_explicit_stop = self.filters.stop.is_some() || self.filters.stop_ts.is_some();
        let cross_file = if has_explicit_stop {
            true // 显式停止条件 → 启用跨文件
        } else {
            // 隐式停止 → 尝试多文件探测（见下文 detect_next_binlog_exists）
            self.detect_next_binlog_exists(&name)
        };

        loop {
            let path = self.cfg.binlog_dir.join(&name);
            if !path.is_file() {
                if name == self.cfg.start_file {
                    // 起始文件都不在 = 用法/环境错误，硬失败
                    return Err(PipelineError::Binlog(BinlogError::InvalidData(format!(
                        "start file {} not found",
                        path.display()
                    ))));
                }
                // 如果已启用跨文件但后续文件不存在 → 优雅终止
                if cross_file {
                    tracing::info!("{} not exists nor a file, stop scanning", path.display());
                }
                break;
            }

            let mut reader = FileReader::open(&self.cfg.binlog_dir, &name, self.filters.clone())?;
            // P3 T4 纯重构：原 pump_one_file 的泵体泛化到 dyn EventSource
            // （run_pump 改经 FileReader 装箱调用）——file 模式字节面零变化。

            // 🔴 B010 修复：检查 on_error 策略来决定如何处理坏事件
            let on_error_skip = self.cfg.on_error == OnError::SkipBadEvent;

            match self.pump_source(&mut reader, &name) {
                Ok(()) => {}
                Err(PipelineError::Binlog(e)) if on_error_skip => {
                    // skip-bad-event 策略：记录错误并尝试继续到下一个文件
                    // 注意：这种错误通常是文件级损坏（checksum 不匹配、截断等）
                    // 无法精确跳过单个事件，只能跳到下一个文件
                    tracing::warn!(
                        "file-level error on {}: {:?}, skipping to next binlog (on-error=skip-bad-event)",
                        path.display(),
                        e
                    );
                    self.summary.errors += 1;
                    self.summary.skipped_files_by_error += 1;

                    // 尝试切换到下一个文件
                    if cross_file {
                        match self.detect_next_binlog(&name) {
                            Some(next) => {
                                name = next;
                                continue; // 继续扫描下一个文件
                            }
                            None => {
                                // 无更多文件可扫 → 终止但返回 OK（非致命）
                                tracing::info!("no more binlogs to scan after error");
                                break;
                            }
                        }
                    } else {
                        // 单文件模式 + 文件损坏 → 视为非致命错误
                        break;
                    }
                }
                Err(e) => {
                    // 其他错误或 stop 策略 → 终止并上抛
                    return Err(e);
                }
            }

            if !cross_file {
                break; // 单文件模式（保留 backward compat for explicit stop-pos-only）
            }

            // 🔴 关键改动：尝试探测下一个文件并继续扫描
            match self.detect_next_binlog(&name) {
                Some(next) => name = next,
                None => break,
            }
        }
        Ok(())
    }

    /// 🔴 B001 修复：探测目录下是否存在下一个连续的 binlog 文件
    /// 用于智能扫描模式（未指定 stop 边界时自动枚举至末尾）
    fn detect_next_binlog(&self, current: &str) -> Option<String> {
        use std::fs;

        // 优先尝试按序号递增（主流命名规范）
        if let Some(next) = FileReader::<std::fs::File>::next_binlog_name(current) {
            let path = self.cfg.binlog_dir.join(&next);
            if path.exists() && path.is_file() {
                return Some(next);
            }
        }

        // 备选方案：遍历目录找更大的文件名（适用于非连续编号场景）
        let prefix = current.split('.').next()?;
        if let Ok(entries) = fs::read_dir(&self.cfg.binlog_dir) {
            for entry in entries.flatten() {
                let filename = entry.file_name().to_string_lossy().to_string();
                // 跳过 .index 等辅助文件
                if filename.ends_with(".index") || !filename.contains('.') {
                    continue;
                }
                // 比较同前缀且大于当前文件的名称
                if filename.starts_with(prefix) && *filename > *current {
                    let full_path = entry.path();
                    if full_path.is_file() {
                        return Some(filename);
                    }
                }
            }
        }

        None
    }

    /// 🔴 B001 修复：提前探测是否有后续文件（决定是否启用跨文件模式）
    fn detect_next_binlog_exists(&self, current: &str) -> bool {
        self.detect_next_binlog(current).is_some()
    }

    /// to-sql 形态收尾（行为与 P1 逐字节一致）。
    fn run(&mut self) -> Result<usize, PipelineError> {
        self.run_pump()?;
        let Emitter::Sql(w) = &mut self.emitter else {
            return Err(PipelineError::Config(
                "internal: flashback emitter must go through run_flash".into(),
            ));
        };
        w.finish().map_err(Into::into)
    }

    /// P3 T4 repl 形态泵入口：`src` 为任意 `EventSource`（生产 = T2
    /// `ReplSource`，测试 = 假流回放），`first_binlog` = 本源起始文件名，
    /// `ckpt` = 断点档路径（None = 不启用水位，行为同 file 模式的纯泵）。
    /// 与 `run_pump` 共用 `pump_source` 泵体；每次 Commit/Rollback 派发记
    /// 水位，reorder 弹出越过水位 → `flush_all` + 原子写档 + pop（细节见
    /// `ckpt_drain`）。**不调 `Writer::finish`**——repl 跨重连存活，句柄
    /// 收尾归调用方（T5）。返回摘要（`files` = 已创建 .sql 文件数）。
    /// P3 T5 起真消费者为 `run_repl_with`（跨重连复用同一 Runner）。
    pub fn run_live(
        &mut self,
        mut src: Box<dyn EventSource + Send>,
        first_binlog: &str,
        ckpt: Option<&Path>,
    ) -> Result<RunSummary, PipelineError> {
        self.ckpt_out = ckpt.map(|p| p.to_path_buf());
        let pumped = self.pump_source(src.as_mut(), first_binlog);
        // 泵 Err（断链/stop_on_error）也要末次冲试：水位判据是「已弹出 +
        // 已 flush」，对未完整事务天然关闭，Err 路径不越界（钉死于 kill
        // 模拟单测）；泵成功时兜住「尾提交后无新 emit 触发」的并行末窗。
        let drained = self.ckpt_drain();
        // P3 T5 简报钉：pump Err 与 drain Err 并发时 **pump 为主**（断链
        // 现场不被记账噪声掩盖），drain 失败降为一行 operator warn。
        if let Err(de) = drained {
            if pumped.is_err() {
                tracing::warn!("repl: checkpoint drain failed after pump error: {de:#}");
            } else {
                return Err(de);
            }
        }
        pumped?;
        let mut sum = self.summary;
        sum.files = match &self.emitter {
            Emitter::Sql(w) => w.created().len(),
            _ => 0,
        };
        Ok(sum)
    }

    /// repl 收尾（T5 epilogue）：`Writer::finish` 落句柄尾（run_live 从不
    /// finish——跨重连句柄续用；终局才关）。返回创建过的 .sql 文件数。
    pub(crate) fn finish_live(&mut self) -> Result<usize, PipelineError> {
        match &mut self.emitter {
            Emitter::Sql(w) => Ok(w.finish()?),
            _ => Err(PipelineError::Config(
                "internal: repl finish on non-sql emitter".into(),
            )),
        }
    }

    /// 已创建 .sql 文件名（单一文件名片段，checkpoint `written_files` 口径）。
    pub(crate) fn created_names(&self) -> Vec<String> {
        match &self.emitter {
            Emitter::Sql(w) => w
                .created()
                .iter()
                .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// 提交边界水位推进（P3 T4）：队首水位（= 提交事件派发时刻的 seq）
    /// ≤ reorder 已弹出数 ⟹ 该事务的全部 job 已经 reorder 保序弹出且经
    /// `emit` 写入 Writer——此时 `flush_all` 落盘、以 `writer.created()`
    /// 快照 `written_files`、原子写档、pop。file 模式 `ckpt_q` 恒空，
    /// 首行判空即返回（零成本、零语义）。
    fn ckpt_drain(&mut self) -> Result<(), PipelineError> {
        // 未武装（ckpt_out=None，含 run_live 前后两态切换的残队场景）即静默
        // 返回——水位机整体不启用，expect 分支不可达。
        if self.ckpt_out.is_none() {
            return Ok(());
        }
        while !self.ckpt_q.is_empty() {
            let through = self.reorder.emitted_through();
            if self.ckpt_q.front().expect("non-empty checked").0 > through {
                break;
            }
            let (wm, binlog, pos, ts) = self.ckpt_q.pop_front().expect("front checked");
            let path = self
                .ckpt_out
                .clone()
                .expect("ckpt_q 仅在 ckpt_out=Some 时记录");
            let w = match &mut self.emitter {
                Emitter::Sql(w) => w,
                _ => {
                    return Err(PipelineError::Config(
                        "internal: live checkpoint on non-sql emitter".into(),
                    ));
                }
            };
            // 顺序钉死：数据先 flush_all 可见，written_files 快照其后——
            // checkpoint 声称存在的文件必然已在盘上（恢复侧 read_verify 前提）。
            w.flush_all()?;
            let written_files = w
                .created()
                .iter()
                .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .collect();
            let cp = Checkpoint {
                file: binlog,
                pos,
                ts,
                written_files,
            };
            checkpoint::write_atomic(&path, &cp)?;
            tracing::debug!(
                watermark = wm,
                binlog = %cp.file,
                pos = cp.pos,
                path = %path.display(),
                "repl checkpoint advanced to transaction boundary"
            );
        }
        Ok(())
    }

    /// flashback 形态 = 通用泵 + 逆序回写；任何 Err 返回前清场半成品
    /// （spec §3.2「半成品不落盘」，T2 登记的调用方义务）。
    fn run_flash(&mut self) -> Result<(RunSummary, usize), PipelineError> {
        let r = self.flash_inner();
        if r.is_err() {
            self.cleanup_flash_files();
        }
        r
    }

    fn flash_inner(&mut self) -> Result<(RunSummary, usize), PipelineError> {
        self.run_pump()?;
        // DDL 排除汇总（tracing 面 + 计数行；不进回滚脚本，prepare 已拦）
        for (ts, f, pos, s) in &self.ddl {
            tracing::warn!(
                binlog = %f,
                pos = *pos,
                datetime = %datetime_str(*ts, self.cfg.time_zone),
                "DDL excluded from rollback script: {s}"
            );
        }
        if !self.ddl.is_empty() {
            eprintln!("flashback: {} DDL/query events excluded", self.ddl.len());
        }
        let Emitter::Flash { tmp } = &mut self.emitter else {
            return Err(PipelineError::Config(
                "internal: run_flash on non-flashback emitter".into(),
            ));
        };
        // finish = flush 全部 BufWriter——必须先于 reverse 回读（文件句柄
        // 字节可见性），再取块索引与创建序。
        tmp.finish()?;
        let blocks = tmp.blocks().clone();
        let created = tmp.created().to_vec();
        let jobs: Vec<(PathBuf, PathBuf, Vec<Block>)> = created
            .iter()
            .map(|tmp_path| {
                let fin = final_for_tmp(tmp_path);
                let out = match tmp_path.parent() {
                    Some(p) => p.join(&fin),
                    None => fin,
                };
                (
                    tmp_path.clone(),
                    out,
                    blocks.get(tmp_path).cloned().unwrap_or_default(),
                )
            })
            .collect();
        let warn = if self.summary.errors > 0 && self.cfg.on_error == OnError::SkipBadEvent {
            Some(format!(
                "-- WARNING: skipped {} events, positions in stderr\n",
                self.summary.errors
            ))
        } else {
            None
        };
        reverse::run_files(&jobs, self.cfg.keep_trx, self.cfg.threads, warn.as_deref())?;
        // tmp 已由 run_files 逐个删除；files 计数 = 作业数（本形态下每
        // created 必带 ≥1 块，空块表不落 final 的 T2 分歧不触发计数分歧）
        Ok((self.summary, jobs.len()))
    }

    /// 错误路径清场：全部 tmp（`Writer::created()` 序）+ 对应 final
    /// （run_files 可能已写出部分成品/半成品——一次 Err 即整跑作废）。
    fn cleanup_flash_files(&mut self) {
        let Emitter::Flash { tmp } = &self.emitter else {
            return;
        };
        for p in tmp.created() {
            let _ = std::fs::remove_file(p);
            let fin = final_for_tmp(p);
            let out = match p.parent() {
                Some(par) => par.join(&fin),
                None => fin,
            };
            let _ = std::fs::remove_file(&out);
        }
    }

    /// 事件泵总入口（P3 T4）：单线程直通 / 并行 worker 池两条路径分派。
    /// `opening_binlog` = 本源起始文件名（ReplSource 起始定位；file 形态
    /// 即当前泵读的文件名）。原 `pump_one_file` 泛化为 `&mut dyn EventSource`
    /// ——纯重构，file 模式字节面零变化（钉死于 P1 e2e 全量回归）。
    fn pump_source(
        &mut self,
        src: &mut (dyn EventSource + Send),
        opening_binlog: &str,
    ) -> Result<(), PipelineError> {
        if self.threads == 1 {
            self.pump_direct(src, opening_binlog)
        } else {
            self.pump_parallel(src, opening_binlog)
        }
    }

    /// 事件 → （过滤/事务机/schema 配对）→ Option<Job>。errors 计数在此
    /// 累计（schema 获取失败 = 逐事件错误）；源不变式违背同样计数跳过。
    /// P2 修复轮：stop 形态（仅 flashback）下 prepare 侧错误升整跑 Err
    /// （spec §3.2 完整性——不完整且不标记的回滚脚本绝不落盘）；to-sql 侧
    /// stop_on_error() 恒 false，计数跳过行为逐字节不变。
    /// `opening_binlog` = 本泵事件源的起始文件名（P3 T4 起供 repl 形态
    /// checkpoint 记录兜底；file 形态不消费）。
    fn prepare(
        &mut self,
        ev: RawEvent,
        opening_binlog: &str,
    ) -> Result<Option<Job>, PipelineError> {
        let (trx_id, status) = self.trx.feed(&ev);
        // P3 T4 repl 水位登记：Commit/Rollback 事件（Xid / COMMIT /
        // ROLLBACK / autocommit DDL）派发时刻的 self.seq = 该事务（及其前）
        // 全部 job 的 seq 上界；binlog 兜底 opening_binlog（源不变式恒非空，
        // 防御分支）。file 形态 ckpt_out=None，本分支恒不进。
        if self.ckpt_out.is_some() && matches!(status, TrxStatus::Commit | TrxStatus::Rollback) {
            let binlog = if ev.binlog.is_empty() {
                opening_binlog.to_string()
            } else {
                ev.binlog.clone()
            };
            self.ckpt_q.push_back((
                self.seq,
                binlog,
                ev.end_pos,
                datetime_str(ev.timestamp, self.cfg.time_zone),
            ));
            // 弹出侧触发点在 emit()；此处触发覆盖「提交事件到达即水位已达」
            // 的 threads=1 常见形（并行在飞未齐则留队，emit 时再推）。
            self.ckpt_drain()?;
        }
        if !matches!(ev.kind, RawKind::Rows(..)) {
            // 非行事件只喂事务机（上游 file 模式 DDL/Query 不出 SQL）。
            // flashback 形态：非事务性 QUERY（DDL 等）登记排除清单，run 收尾
            // 汇总告警（begin/commit/rollback/空文本 = 事务脚手架，不登记）。
            // P6 T1: Write to JSONL reporter if configured
            if self.is_flash()
                && let RawKind::Query(sql) = &ev.kind
            {
                let kw = sql.trim().trim_end_matches(';').trim().to_ascii_lowercase();
                if !kw.is_empty() && kw != "begin" && kw != "commit" && kw != "rollback" {
                    // Record to ddl list (existing behavior)
                    self.ddl
                        .push((ev.timestamp, ev.binlog.clone(), ev.start_pos, sql.clone()));
                    // P6 T1: Also write to JSONL report file if reporter is active
                    if let Some(ref mut writer) = self.report_writer {
                        let skip_event = crate::flashback::report::SkipEvent {
                            timestamp: datetime_str(ev.timestamp, self.cfg.time_zone),
                            binlog: ev.binlog.clone(),
                            position: ev.start_pos as u64,
                            type_: "Query".to_string(),
                            sql: Some(sql.clone()),
                        };
                        let _ = writer.write(&skip_event); // Best effort - don't fail pipeline
                    }
                }
            }
            // P2 T4 stats 形态（Ruling：Status 标记由 dispatcher 顺序直推
            // reorder，不过 worker——它是 dispatcher 已有信息）：seq 与 rows
            // 事件同源编号，保序不破坏；计数入 summary.events。
            // 位点口径：Begin = 标记事件起始（上游 oneBigLong.StartPos），
            // Commit/Rollback = 结束位（上游 :198 StopPos）——单 `pos` 字段
            // 按角色承载，biglong 字节面与上游一致。
            // Gtid/Rotate/Other 不派发（上游 com.go:153-155 default →
            // C_reContinue，33/34 永不进 StatChan——MySQL GTID 两侧都不折叠
            // begin；MariaDB GTID 的 begin 折叠属超范围形态，D5）。
            // 非三关键字 QUERY（DDL/`use`/空文本）→ Process = 纯 tick 派发
            // （P2 T9 B.4(b)：上游 file.go:274 对任意 sqlType!="" 喂
            // StatChan，stats_process.go:247 逐喂入事件判 tick——不派发即
            // 窗口切分歧义）。
            if self.is_stats() {
                let marker = match &ev.kind {
                    RawKind::Query(sql) => {
                        let kw = sql.trim().trim_end_matches(';').trim().to_ascii_lowercase();
                        match kw.as_str() {
                            "begin" => Some((ev.start_pos, TrxStatus::Begin)),
                            "commit" => Some((ev.end_pos, TrxStatus::Commit)),
                            "rollback" => Some((ev.end_pos, TrxStatus::Rollback)),
                            _ => Some((ev.end_pos, TrxStatus::Process)),
                        }
                    }
                    RawKind::Xid => Some((ev.end_pos, TrxStatus::Commit)),
                    _ => None,
                };
                if let Some((pos, mst)) = marker {
                    let ready = self.reorder.push(
                        self.seq,
                        vec![Out::Status {
                            binlog: ev.binlog.clone(),
                            pos,
                            ts: ev.timestamp,
                            status: mst,
                        }],
                    );
                    self.seq += 1;
                    self.summary.events += 1;
                    self.emit(ready)?;
                }
            }
            return Ok(None);
        }
        if !self.filters.accept(&ev, None) {
            return Ok(None);
        }
        let Some(tm) = ev.tm.clone() else {
            // FileReader 已保证 rows 必带 tm（源不变式），防御分支
            return self.prepare_fail(&ev, "rows event without table_map");
        };
        match self.schema_for(&tm) {
            Ok(schema) => {
                let job = Job {
                    seq: self.seq,
                    ev,
                    trx_id,
                    schema,
                    status,
                };
                self.seq += 1;
                self.summary.events += 1;
                Ok(Some(job))
            }
            Err(e) => self.prepare_fail(
                &ev,
                &format!("schema lookup for `{}.{}`: {e:#}", tm.schema, tm.table),
            ),
        }
    }

    /// prepare 侧单事件错误分派：stop 形态 → 整跑 Err（tmp/半成品清场由
    /// run_flash 错误路径负责）；否则计数跳过（原 robust-continue 通道）。
    fn prepare_fail(&mut self, ev: &RawEvent, what: &str) -> Result<Option<Job>, PipelineError> {
        if self.stop_on_error() {
            return Err(PipelineError::Config(format!(
                "event at {}:{} aborted (--on-error stop): {what}",
                ev.binlog, ev.start_pos
            )));
        }
        self.bump_error(ev, what);
        Ok(None)
    }

    fn bump_error(&mut self, ev: &RawEvent, what: &str) {
        self.summary.errors += 1;
        tracing::error!(binlog = %ev.binlog, pos = ev.start_pos, "event skipped: {what}");
    }

    /// dispatcher 侧 schema 配对（含每 (tm,schema) 一次的 Align 甄别）。
    fn schema_for(&mut self, tm: &Arc<TableMapEvent>) -> Result<Arc<TableSchema>, MetaError> {
        if let Some((old_tm, schema)) = self.tmap.get(&tm.table_id)
            && Arc::ptr_eq(old_tm, tm)
        {
            return Ok(schema.clone());
        }
        let s: Arc<TableSchema> = Arc::new(self.store.get(&tm.schema, &tm.table)?.clone());
        // Align 每对 (tm,schema) 一次：strict 形态在建对时就升错（该事件走
        // 逐事件错误通道），Padded 形态在此一次性 info 告警（DmlBuilder 的
        // 每事件 warn 保持 T13 审定行为不变）。
        match align_cols(tm.n_cols, &s, self.cfg.strict_schema) {
            Err(e) => Err(e),
            Ok(Align::Padded { dropped }) => {
                tracing::info!(
                    table = %format!("`{}`.`{}`", tm.schema, tm.table),
                    dropped = ?dropped,
                    "table_map is wider than schema (dropped columns), first detection at dispatcher"
                );
                self.tmap.insert(tm.table_id, (tm.clone(), s.clone()));
                Ok(s)
            }
            Ok(_) => {
                self.tmap.insert(tm.table_id, (tm.clone(), s.clone()));
                Ok(s)
            }
        }
    }

    /// 写出保序弹出批次（P2 T4 泛型载荷）：SQL 形态仅消费 `Out::Sql`
    /// （包装不改变写出字节）；Stats 形态把 Fact/Status 喂 Aggregator
    /// （`statements` 复用为**行计数**面：每 Fact += rows，Display 标
    /// "statements rows"）。异形态变体按不可达防御忽略（编号链单一模式）。
    fn emit(&mut self, outs: Vec<Out>) -> Result<(), PipelineError> {
        match &mut self.emitter {
            Emitter::Sql(w) | Emitter::Flash { tmp: w } => {
                for o in &outs {
                    if let Out::Sql(g) = o {
                        self.summary.statements += g.sqls.len() as u64;
                        w.write_group(g)?;
                    }
                }
            }
            Emitter::Stats(agg) => {
                for o in &outs {
                    match o {
                        Out::Fact(f) => {
                            self.summary.statements += f.rows;
                            agg.feed(&crate::stats::StreamEvent::Row(f))?;
                        }
                        Out::Status {
                            binlog,
                            pos,
                            ts,
                            status,
                        } => {
                            let ev = match status {
                                TrxStatus::Begin => crate::stats::StreamEvent::Begin {
                                    binlog,
                                    pos: *pos,
                                    ts: *ts,
                                },
                                TrxStatus::Commit => crate::stats::StreamEvent::Commit {
                                    binlog,
                                    pos: *pos,
                                    ts: *ts,
                                },
                                TrxStatus::Rollback => crate::stats::StreamEvent::Rollback {
                                    binlog,
                                    pos: *pos,
                                    ts: *ts,
                                },
                                // Process = 纯 tick（非三关键字 QUERY：DDL/空
                                // 文本；P2 T9 B.4(b) 对齐上游喂入集，只冲刷
                                // 窗口/锚点，不碰 biglong 与窗内容）。
                                // pos 对 Process/Tick 有意弃用：tick 仅需
                                // binlog+ts 触发窗口判定（上游 stats_process
                                // .go:247-257 口径），位点在此角色无语义。
                                TrxStatus::Process => {
                                    crate::stats::StreamEvent::Tick { binlog, ts: *ts }
                                }
                            };
                            agg.feed(&ev)?;
                        }
                        Out::Sql(_) => {}
                    }
                }
            }
        }
        // P3 T4：reorder 弹出即「该 seq 前（含）数据已入 Writer」——水位
        // 推进的 emit 侧触发点（file 模式 ckpt_q 恒空，判空即返回）。
        self.ckpt_drain()?;
        Ok(())
    }

    /// 单线程直通：无通道无线程，build 内联，reorder 恒零滞留。
    fn pump_direct(
        &mut self,
        src: &mut (dyn EventSource + Send),
        opening_binlog: &str,
    ) -> Result<(), PipelineError> {
        while let Some(ev) = src.next()? {
            let Some(job) = self.prepare(ev, opening_binlog)? else {
                continue;
            };
            let seq = job.seq;
            let groups = match build_out(&job, &self.builder, self.out_mode()) {
                Ok(g) => g,
                Err(e) => {
                    self.summary.errors += 1;
                    tracing::error!(
                        seq,
                        binlog = %job.ev.binlog,
                        pos = job.ev.start_pos,
                        "event skipped due to decode/SQL-build error: {e:#}"
                    );
                    // P2 T3：threads=1 直通无 catch_unwind——stop 形态在此
                    // **直接返回 Err**（禁 unwrap/panic，错误原样上抛给调用方）
                    if self.stop_on_error() {
                        return Err(PipelineError::Config(format!(
                            "event at {}:{} aborted (--on-error stop): {e:#}",
                            job.ev.binlog, job.ev.start_pos
                        )));
                    }
                    Vec::new()
                }
            };
            let ready = self.reorder.push(seq, groups);
            self.emit(ready)?;
        }
        Ok(())
    }

    /// 并行路径：bounded 作业队列 + unbounded 结果回流；reorder pending >
    /// 2×threads 时 dispatcher 阻塞补收（spec §5.1 统一反压规则）。
    ///
    /// P3 T6b r3 修复（threads>1 静默期滞后）：源读取挪进**作用域线程**，
    /// 事件经 bounded(1) 通道中转，dispatcher 以 20ms `recv_timeout` 轮询
    /// 「新事件 | 收割结果」。旧形态下 dispatcher 直接阻塞在 `src.next()`
    /// 的 socket 读上，repl 静默期（心跳被 ReplSource 内部消化、不出事件）
    /// 在飞 worker 的完工结果永不收割——尾事务不落盘、水位不推进，直到
    /// 下一个真事件或泵终止才补做（live 8.0 实踩 threads=4：40s 不追平，
    /// 末事务 'X7last' 字节缺席产物）。出码序仍由 Reorder 的 seq 唯一决定
    /// （file 模式字节面零变化：其源从文件读，next 从不停摆，中转只是
    /// 多一跳通道）。错误路径的提前 return 会 join 尚在 `src.next()` 里
    /// 的源线程——repl 形态由读超时（心跳 2d+1s）兜底有界，可接受。
    fn pump_parallel(
        &mut self,
        src: &mut (dyn EventSource + Send),
        opening_binlog: &str,
    ) -> Result<(), PipelineError> {
        let (job_tx, job_rx) = bounded::<Job>(self.threads * 2);
        let (res_tx, res_rx): (Sender<(u64, Vec<Out>)>, _) = unbounded();
        let errors = Arc::new(AtomicU64::new(0));
        // P2 T3 abort 哨兵：stop 形态下 worker Err/panic 置位，收取循环后转 Err；
        // to-sql 侧 stop=false 恒不置位（行为零变），仍传独立原子量保持签名统一。
        let abort = Arc::new(AtomicBool::new(false));
        let stop = self.stop_on_error();
        let mode = self.out_mode();
        let mut handles = Vec::with_capacity(self.threads);
        for _ in 0..self.threads {
            let job_rx = job_rx.clone();
            let res_tx = res_tx.clone();
            let builder = self.builder.clone();
            let errors = errors.clone();
            let abort = abort.clone();
            handles.push(thread::spawn(move || {
                worker_loop(job_rx, res_tx, builder, errors, abort, stop, mode)
            }));
        }
        drop(job_rx);
        drop(res_tx); //  dispatcher 侧只 recv；所有 worker 结束后通道才闭合

        std::thread::scope(|s| -> Result<(), PipelineError> {
            // 源中转线程：Err / Ok(None) 转发后即退（泵语义判定仍在 dispatcher）。
            let (ev_tx, ev_rx) = bounded::<Result<Option<RawEvent>, BinlogError>>(1);
            s.spawn(move || {
                loop {
                    let r = src.next();
                    let last = !matches!(r, Ok(Some(_)));
                    if ev_tx.send(r).is_err() || last {
                        break; // 接收端已弃（错误路径早退）或流终
                    }
                }
            });
            // 终审 FIX A：源错误**不再早退**。早退会跳过尾部采集段
            // （drop(job_tx) → 收残结果 → join → drain_remaining → emit），
            // Reorder 的 `next` 永久卡在洞里，而同一 Runner 跨重连复用
            // （run_repl_with）→ `emitted_through()` 冻结 → checkpoint 永不
            // 推进、unbounded res_rx 与 reorder.buf 无界增长（默认并行配置
            // 整场报废）。现行为：错误入栈暂存 → 走与干净路径**同一尾部**
            // （强制吐出保持 at-least-once：撕裂事务成为盘上半块 = 崩溃
            // 撕裂形态，水位只认完整事务不受污染）→ 尾部走完后上抛。
            let mut src_err: Option<BinlogError> = None;
            loop {
                // 每轮先清收（try_recv 至空）：静默期水位/落盘延迟上界 = 一轮
                // recv_timeout（20ms），有流量时与原「reap 前置」形态同型。
                self.reap(&res_rx)?;
                match ev_rx.recv_timeout(Duration::from_millis(20)) {
                    Ok(Ok(Some(ev))) => {
                        if let Some(job) = self.prepare(ev, opening_binlog)? {
                            // 反压前清收 + 超限阻塞收取（progress 保证：worker 永不阻塞在发送侧）
                            self.reap(&res_rx)?;
                            while self.reorder.pending() > self.threads * 2 {
                                match res_rx.recv() {
                                    Ok((seq, g)) => {
                                        let ready = self.reorder.push(seq, g);
                                        self.emit(ready)?;
                                    }
                                    // 全部 worker 已退（不可能：job 队列仍有消费者/在飞）→ 结束收取
                                    Err(_) => break,
                                }
                            }
                            if job_tx.send(job).is_err() {
                                return Err(PipelineError::Config(
                                    "worker pool died mid-run".into(),
                                ));
                            }
                        }
                    }
                    // 源流终/硬错：两臂同走尾部采集（FIX A）——None 干净停，
                    // Err 暂存后在尾部上抛。
                    Ok(Ok(None)) => break,
                    Ok(Err(be)) => {
                        src_err = Some(be);
                        break;
                    }
                    // 超时 = 暂无事件：走轮首 reap 收割在飞结果（本修复钉死点）。
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                    // 中转线程随 scope 同生共死，tx 掉线仅源线程 panic 一途；
                    // 按流终处理，交给末次 drain + run_pump 上层收口。
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                }
            }
            drop(job_tx); // 投递完成 → worker 陆续退出
            while let Ok((seq, g)) = res_rx.recv() {
                let ready = self.reorder.push(seq, g);
                self.emit(ready)?;
            }
            for h in handles {
                let _ = h.join();
            }
            let leftover = self.reorder.drain_remaining();
            if !leftover.is_empty() {
                tracing::warn!(
                    "reorder kept {} batches after drain (gap in seq stream)",
                    leftover.len()
                );
                self.emit(leftover)?;
            }
            // worker 侧错误计入摘要（dispatcher 视野外的 decode/build 失败）
            self.summary.errors += errors.load(Ordering::Relaxed);
            // P2 T3 并行 stop：哨兵已置位 → 整跑作废（tmp 清场由 run_flash 错误
            // 路径负责；首个错误已由 worker 记入 stderr）
            if stop && abort.load(Ordering::Relaxed) {
                return Err(PipelineError::Config(
                    "aborted: first error logged to stderr".into(),
                ));
            }
            // FIX A：采集/收尾全部完成后才上抛暂存的源错误（顺序钉：数据
            // 面完整先行，错误面后至；与 run_live 的 pump-Err 为主契约同轨）。
            if let Some(be) = src_err {
                return Err(be.into());
            }
            Ok(())
        })
    }

    /// 非阻塞收取全部已就绪结果并写出（反压前置）。
    fn reap(&mut self, res_rx: &Receiver<(u64, Vec<Out>)>) -> Result<(), PipelineError> {
        while let Ok((seq, g)) = res_rx.try_recv() {
            let ready = self.reorder.push(seq, g);
            self.emit(ready)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod live_tests {
    //! P3 T4：`run_live` 提交边界水位单测（假 EventSource 回放 synth 事件，
    //! 无服务器、无真 binlog 文件）。

    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::process;
    use std::sync::Arc;

    use clap::Parser;

    use super::{Emitter, Runner};
    use crate::binlog::error::BinlogError;
    use crate::binlog::rows::RowsKind;
    use crate::binlog::table_map::TableMapEvent;
    use crate::config::{Cli, Command, Config};
    use crate::metadata::store::SchemaStore;
    use crate::output::{Writer, datetime_str};
    use crate::pipeline::filter::Filters;
    use crate::pipeline::source::{EventSource, RawEvent, RawKind};
    use crate::repl::checkpoint::{Checkpoint, read_verify};
    use crate::sqlopen::dml::{DmlBuilder, SqlOpts};

    fn tmpdir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let d = std::env::temp_dir().join(format!(
            "my2sql-p3t4-live-{}-{}-{}",
            tag,
            process::id(),
            nanos
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 离线 schema（version 1，与 tests/e2e.rs 同族）：t10.a(id INT, pk)。
    fn schema_file(dir: &std::path::Path) -> PathBuf {
        let p = dir.join("schema.json");
        std::fs::write(
            &p,
            r#"{"version":1,"tables":[{"db":"t10","table":"a","cols":[{"name":"id","type_name":"int","unsigned":false}],"pk":["id"],"uks":[]}]}"#,
        )
        .unwrap();
        p
    }

    fn config_from(dir: &std::path::Path, schema: &std::path::Path) -> Config {
        config_threads(dir, schema, 1)
    }

    fn config_threads(dir: &std::path::Path, schema: &std::path::Path, threads: usize) -> Config {
        let cli = Cli::try_parse_from([
            "my2sql-rs",
            "to-sql",
            "--binlog-dir",
            dir.join("binlog").to_str().unwrap(),
            "--start-file",
            "mysql-bin.000001",
            "--schema-file",
            schema.to_str().unwrap(),
            "--output-dir",
            dir.join("out").to_str().unwrap(),
            "--threads",
            &threads.to_string(),
        ])
        .expect("cli parse");
        let Command::ToSql(a) = cli.cmd else {
            panic!("to-sql expected")
        };
        Config::validate_to_sql(a).expect("config validate")
    }

    fn tm() -> TableMapEvent {
        TableMapEvent {
            table_id: 85,
            schema: "t10".into(),
            table: "a".into(),
            n_cols: 1,
            column_type: vec![3],
            column_meta: vec![0],
            null_bits: vec![0],
            charset: vec![],
        }
    }

    /// WRITE_ROWS_V2 最小体（镜像 worker.rs 单测构造器）。
    fn write_body(tid: u64, val: i32) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&tid.to_le_bytes()[..6]);
        b.extend_from_slice(&[0u8; 2]);
        b.extend_from_slice(&2u16.to_le_bytes());
        b.push(1);
        b.push(1);
        b.push(0);
        b.extend_from_slice(&val.to_le_bytes());
        b
    }

    fn base(kind: RawKind, start: u32, end: u32, ts: u32) -> RawEvent {
        RawEvent {
            binlog: "mysql-bin.000001".into(),
            start_pos: start,
            end_pos: end,
            timestamp: ts,
            kind,
            body: Vec::new(),
            tm: None,
        }
    }

    fn q(sql: &str, end: u32, ts: u32) -> RawEvent {
        base(RawKind::Query(sql.into()), end - 20, end, ts)
    }
    fn xid(end: u32, ts: u32) -> RawEvent {
        base(RawKind::Xid, end - 20, end, ts)
    }
    fn rows(val: i32, end: u32, ts: u32) -> RawEvent {
        let mut ev = base(RawKind::Rows(RowsKind::Write, true), end - 50, end, ts);
        ev.body = write_body(85, val);
        ev.tm = Some(Arc::new(tm()));
        ev
    }

    /// 假源：VecDeque 回放（含尾部 Err 注入）；每次出件前调 probe——
    /// probe 观察的是「此前所有事件已泵完」时刻的 checkpoint/落盘实态。
    struct FakeSrc {
        q: VecDeque<Result<Option<RawEvent>, BinlogError>>,
        probe: Box<dyn FnMut(usize) + Send>,
    }
    impl EventSource for FakeSrc {
        fn next(&mut self) -> Result<Option<RawEvent>, BinlogError> {
            (self.probe)(self.q.len());
            match self.q.pop_front() {
                Some(r) => r,
                None => Ok(None),
            }
        }
    }

    fn read_cp(path: &std::path::Path) -> Option<Checkpoint> {
        match std::fs::read(path) {
            Ok(raw) => Some(serde_json::from_slice(&raw).expect("checkpoint JSON 合法")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => panic!("read {}: {e}", path.display()),
        }
    }

    /// Step 1 钉死测试：checkpoint 的 pos **总是**等于「已完整 flush 事务的
    /// 提交事件 end_pos」（每次出件前 probe 断言 ∈ {200,500}），泵中注入
    /// Err（kill 模拟）后 checkpoint 停在整事务边界 500——tr3 的行虽已
    /// 落盘，其未提交即不得进水位。
    #[test]
    fn ckpt_watermark_advances_only_after_full_trx_flushed() {
        let dir = tmpdir("kill");
        let schema = schema_file(&dir);
        let cfg = config_from(&dir, &schema);
        let out = dir.join("out");
        let ckpt_path = dir.join("checkpoint.json");
        let sql_file = out.join("to_sql.1.sql");
        // 提交边界 end_pos 全集：trx1 Xid@200、trx2 ROLLBACK@500。
        // 行事件 end_pos（150/350/450/650）**绝不允许**出现在 checkpoint。
        let commit_end_positions = [200u32, 500];

        let evs: Vec<Result<Option<RawEvent>, BinlogError>> = vec![
            Ok(Some(q("BEGIN", 100, 1700000000))),
            Ok(Some(rows(1, 150, 1700000000))),
            Ok(Some(xid(200, 1700000000))), // trx1 提交边界①
            Ok(Some(q("BEGIN", 300, 1700000100))),
            Ok(Some(rows(2, 350, 1700000100))),
            Ok(Some(rows(3, 450, 1700000100))),
            Ok(Some(q("ROLLBACK", 500, 1700000100))), // trx2 结束边界②
            Ok(Some(q("BEGIN", 600, 1700000200))),
            Ok(Some(rows(4, 650, 1700000200))), // trx3 未提交……
            Err(BinlogError::InvalidData(
                "kill simulated: link dropped".into(),
            )), // ……即断链
        ];

        let ckpt_probe = ckpt_path.clone();
        let sql_probe = sql_file.clone();
        let probe = move |i: usize| {
            let cp = read_cp(&ckpt_probe);
            if let Some(cp) = &cp {
                assert!(
                    commit_end_positions.contains(&cp.pos),
                    "step {i}: checkpoint pos={} 不是任何整事务提交事件 end_pos",
                    cp.pos
                );
            }
            match cp {
                None => {}
                Some(cp) if cp.pos == 200 => {
                    // 水位=200 ⟹ trx1 的行必已 flush 可见（可能含更后的行——
                    // 方向是安全的「落后可重复消费」）
                    let s = std::fs::read_to_string(&sql_probe).expect("trx1 flush 先于水位推进");
                    assert!(
                        s.contains("INSERT INTO `t10`.`a` (`id`) VALUES (1);"),
                        "{s}"
                    );
                }
                Some(cp) => {
                    // 水位=500 ⟹ trx1+trx2 全部行已 flush
                    assert_eq!(cp.pos, 500, "probe 值域已过滤，只剩 500");
                    let s = std::fs::read_to_string(&sql_probe).expect("trx2 flush");
                    for v in ["(1)", "(2)", "(3)"] {
                        assert!(
                            s.contains(&format!("INSERT INTO `t10`.`a` (`id`) VALUES {v};")),
                            "pos=500 时行 {v} 应已落盘: {s}"
                        );
                    }
                }
            }
        };

        let store = SchemaStore::offline(&schema).unwrap();
        let writer = Writer::with_live(
            out.clone(),
            false,
            false,
            false,
            cfg.time_zone,
            "to_sql".into(),
            false,
            true,  // streaming（repl 实时可见性，T3 接口）
            false, // no_clobber（本测试从空目录起）
        );
        let mut st = Runner::new(
            &cfg,
            Filters::from_config(&cfg),
            store,
            DmlBuilder::new(SqlOpts::from_config(&cfg)),
            Emitter::Sql(writer),
        );
        let fake = FakeSrc {
            q: evs.into(),
            probe: Box::new(probe),
        };
        let e = st
            .run_live(Box::new(fake), "mysql-bin.000001", Some(&ckpt_path))
            .expect_err("注入的断链 Err 必须原样上抛");
        assert!(format!("{e:#}").contains("kill simulated"), "got: {e:#}");

        // kill 后：水位停在最后一个整事务边界 500（trx3 的行已落盘但未提交，
        // 不得进水位——恢复时从 500 起重复消费，方向安全）。
        let cp = read_cp(&ckpt_path).expect("至少推进过一次水位");
        assert_eq!(cp.pos, 500, "Err 后水位必须停在整事务边界");
        assert_eq!(cp.file, "mysql-bin.000001");
        assert_eq!(cp.ts, datetime_str(1700000100, cfg.time_zone));
        assert_eq!(cp.written_files, vec!["to_sql.1.sql".to_string()]);
        read_verify(&ckpt_path, &out).expect("written_files 承诺的实物均在盘上");
        let s = std::fs::read_to_string(&sql_file).unwrap();
        assert!(
            s.contains("VALUES (4)"),
            "tr3 已 emit 的行在盘（水位落后于数据，非相反）"
        );
    }

    /// 干净 EOF 路径：全 3 事务 + 尾 Xid@700 → Ok 摘要 + 水位推到最后一格。
    #[test]
    fn ckpt_watermark_reaches_last_commit_on_clean_stop() {
        let dir = tmpdir("clean");
        let schema = schema_file(&dir);
        let cfg = config_from(&dir, &schema);
        let out = dir.join("out");
        let ckpt_path = dir.join("checkpoint.json");

        let evs: VecDeque<Result<Option<RawEvent>, BinlogError>> = [
            Ok(Some(q("BEGIN", 100, 1))),
            Ok(Some(rows(1, 150, 1))),
            Ok(Some(xid(200, 1))),
            Ok(Some(q("BEGIN", 300, 2))),
            Ok(Some(rows(2, 350, 2))),
            Ok(Some(q("COMMIT", 400, 2))),
        ]
        .into();
        let store = SchemaStore::offline(&schema).unwrap();
        let writer = Writer::with_live(
            out.clone(),
            false,
            false,
            false,
            cfg.time_zone,
            "to_sql".into(),
            false,
            true,
            false,
        );
        let mut st = Runner::new(
            &cfg,
            Filters::from_config(&cfg),
            store,
            DmlBuilder::new(SqlOpts::from_config(&cfg)),
            Emitter::Sql(writer),
        );
        let fake = FakeSrc {
            q: evs,
            probe: Box::new(|_| {}),
        };
        let sum = st
            .run_live(Box::new(fake), "mysql-bin.000001", Some(&ckpt_path))
            .expect("干净 EOF → Ok");
        assert_eq!(sum.events, 2);
        assert_eq!(sum.files, 1);
        assert_eq!(read_cp(&ckpt_path).expect("水位").pos, 400);
    }

    /// T6b r3 生产缺陷钉（threads>1 静默期滞后，T4 挂账的活体红件）：
    /// 生产 repl 形态源 `next()` 消费完事件后长阻塞（主库静默，心跳被
    /// ReplSource 内部消化、对泵不可见）。旧泵形下 dispatcher 与源读取
    /// 同线程互斥——尾事务的在飞 worker 结果永不收割：数据不落盘、水位
    /// 不推进，直到下一个真事件或泵终止才补做（live 8.0 threads=4 实踩
    /// 40s 不追平 marker 界，末事务字节缺席产物）。本件：3 事务 × 300
    /// 行进假源，尾 XID 进泵后假源即静默阻塞；要求**阻塞期间**水位推进
    /// 至尾事务界 + 全 900 行落盘（旧泵形 = 必红）。
    #[test]
    fn parallel_watermark_advances_while_source_idle() {
        let dir = tmpdir("idle-par");
        let schema = schema_file(&dir);
        let cfg = config_threads(&dir, &schema, 3);
        let out = dir.join("out");
        let ckpt_path = dir.join("checkpoint.json");

        const N: i32 = 300;
        const TRX: i32 = 3;
        let mut evs: VecDeque<RawEvent> = VecDeque::new();
        let mut pos = 100u32;
        let mut final_pos = 0u32;
        for t in 0..TRX {
            evs.push_back(q("BEGIN", pos, 1700000000));
            pos += 20;
            for i in 0..N {
                let v = t * N + i + 1;
                evs.push_back(rows(v, pos, 1700000000));
                pos += 40;
            }
            evs.push_back(xid(pos, 1700000000));
            final_pos = pos;
            pos += 20;
        }

        // 静默闸：假源出完事件后 next() 阻塞到测试侧释放（gate_tx 掉线
        // 即放行 = 干净流终）。
        struct IdleSrc {
            q: VecDeque<RawEvent>,
            gate: std::sync::mpsc::Receiver<()>,
        }
        impl EventSource for IdleSrc {
            fn next(&mut self) -> Result<Option<RawEvent>, BinlogError> {
                match self.q.pop_front() {
                    Some(ev) => Ok(Some(ev)),
                    None => {
                        let _ = self.gate.recv();
                        Ok(None)
                    }
                }
            }
        }
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();

        let store = SchemaStore::offline(&schema).unwrap();
        let writer = Writer::with_live(
            out.clone(),
            false,
            false,
            false,
            cfg.time_zone,
            "to_sql".into(),
            false,
            true,
            false,
        );
        let mut st = Runner::new(
            &cfg,
            Filters::from_config(&cfg),
            store,
            DmlBuilder::new(SqlOpts::from_config(&cfg)),
            Emitter::Sql(writer),
        );
        let fake = IdleSrc {
            q: evs,
            gate: gate_rx,
        };

        // 看门狗：源仍静默（不释放 gate）期间轮询水位；命中或 8s 超时都
        // 释放收尾（超时 = 红，由 join 回的 false 钉死）。
        let ck = ckpt_path.clone();
        let watchdog = std::thread::spawn(move || {
            let t0 = std::time::Instant::now();
            let hit = loop {
                if read_cp(&ck).is_some_and(|cp| cp.pos == final_pos) {
                    break true;
                }
                if t0.elapsed() > std::time::Duration::from_secs(8) {
                    break false;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            };
            drop(gate_tx); // 释放假源 → 干净流终
            hit
        });
        let sum = st
            .run_live(Box::new(fake), "mysql-bin.000001", Some(&ckpt_path))
            .expect("静默后释放 → 干净收尾 Ok");
        assert!(
            watchdog.join().expect("watchdog 不得 panic"),
            "源静默期 threads=3 水位停滞（尾结果未收割 = 旧泵形；T6b live 缺陷：尾事务不落盘、水位 40s+ 不推进）"
        );
        assert_eq!(read_cp(&ckpt_path).expect("终水位").pos, final_pos);
        let s = std::fs::read_to_string(out.join("to_sql.1.sql")).expect("产物在场");
        assert_eq!(
            s.matches("INSERT INTO `t10`.`a` (`id`) VALUES").count(),
            (N * TRX) as usize,
            "水位声称尾事务界 ⟹ 全 900 行必已 flush"
        );
        assert_eq!(sum.files, 1);
        assert_eq!(sum.errors, 0);
        read_verify(&ckpt_path, &out).expect("read_verify");
    }

    /// 终审 FIX A 红钉（pump_parallel 源错误早退）：`Ok(Err(be))` 早退
    /// 跳过收尾采集段（drop(job_tx) → 收残结果 → join → drain_remaining →
    /// emit）后，Reorder 的 `next` 永久卡在洞里；同一 Runner 跨重连复用
    /// （run_repl_with 装配注释钉死），`emitted_through()` 冻结 → checkpoint
    /// 永不推进、res_rx（unbounded）与 reorder.buf 无界增长——默认
    /// threads=available_parallelism 的 repl 主配置整场报废。本件：
    /// threads=4 + 2 个完整事务（各 400 行，保证错误到达时在飞/积压 seq
    /// 必然构成洞）+ 尾部 Err；断言 (1) 错误后 reorder 无洞（全部 seq 已
    /// 弹出）、水位落在最后整事务边界（强制吐出 = at-least-once，撕裂事务
    /// 成为盘上半块——正是崩溃撕裂形态，checkpoint 只认完整事务不越界）；
    /// (2) **同一 Runner** 接第二颗假源再跑，水位继续推进且 checkpoint
    /// 更新（早退形态下 emitted_through 冻结 → 水位永远停表 = 必红）。
    #[test]
    fn parallel_source_error_drains_reorder_and_same_runner_recovers() {
        let dir = tmpdir("pmp-err");
        let schema = schema_file(&dir);
        let cfg = config_threads(&dir, &schema, 4);
        let out = dir.join("out");
        let ckpt_path = dir.join("checkpoint.json");

        const N: i32 = 400;
        let mut evs: Vec<Result<Option<RawEvent>, BinlogError>> = Vec::new();
        let mut pos = 100u32;
        let mut trx2_end = 0u32;
        for t in 0..2 {
            evs.push(Ok(Some(q("BEGIN", pos, 1700000000))));
            pos += 20;
            for i in 0..N {
                let v = t * N + i + 1;
                evs.push(Ok(Some(rows(v, pos, 1700000000))));
                pos += 40;
            }
            evs.push(Ok(Some(xid(pos, 1700000000))));
            trx2_end = pos;
            pos += 20;
        }
        // 尾事务（未提交即断链）：其行允许被强制吐出（at-least-once 撕裂
        // 形态），但其边界绝不进水位。
        evs.push(Ok(Some(q("BEGIN", pos, 1700000100))));
        pos += 20;
        for i in 0..50 {
            evs.push(Ok(Some(rows(10_000 + i, pos, 1700000100))));
            pos += 40;
        }
        evs.push(Err(BinlogError::InvalidData(
            "FIX A: source died mid-stream".into(),
        )));

        let store = SchemaStore::offline(&schema).unwrap();
        let writer = Writer::with_live(
            out.clone(),
            false,
            false,
            false,
            cfg.time_zone,
            "to_sql".into(),
            false,
            true,
            false,
        );
        let mut st = Runner::new(
            &cfg,
            Filters::from_config(&cfg),
            store,
            DmlBuilder::new(SqlOpts::from_config(&cfg)),
            Emitter::Sql(writer),
        );
        let fake = FakeSrc {
            q: evs.into(),
            probe: Box::new(|_| {}),
        };
        let e = st
            .run_live(Box::new(fake), "mysql-bin.000001", Some(&ckpt_path))
            .expect_err("源错误必须原样上抛");
        assert!(format!("{e:#}").contains("FIX A: source died"), "{e:#}");

        // (1) 无洞：全部已派发 seq 经收尾采集弹出（早退形态下必 < seq）。
        assert_eq!(
            st.reorder.emitted_through(),
            st.seq,
            "源错误后 reorder.next 必须追平已派发 seq（早退即永久卡洞）"
        );
        assert_eq!(st.reorder.pending(), 0, "缓冲不得滞留");
        // 水位 = 最后整事务（trx2）提交界；尾事务未提交绝不越界。
        let cp = read_cp(&ckpt_path).expect("两个整事务的水位必须已推进落盘");
        assert_eq!(cp.pos, trx2_end, "水位必须停在 trx2 提交界");

        // (2) 同一 Runner 跨重连复用：换第二颗干净假源（trx4 完整提交），
        // 水位必须继续推进、终档更新——冻结形态下永远停在 trx2_end = 必红。
        let mut evs2: VecDeque<Result<Option<RawEvent>, BinlogError>> = VecDeque::new();
        evs2.push_back(Ok(Some(q("BEGIN", 20000, 1700000200))));
        for i in 0..100 {
            evs2.push_back(Ok(Some(rows(
                20_000 + i,
                20040 + i as u32 * 40,
                1700000200,
            ))));
        }
        evs2.push_back(Ok(Some(xid(26000, 1700000200))));
        let fake2 = FakeSrc {
            q: evs2,
            probe: Box::new(|_| {}),
        };
        let sum2 = st
            .run_live(Box::new(fake2), "mysql-bin.000001", Some(&ckpt_path))
            .expect("第二跑干净收尾");
        assert_eq!(sum2.errors, 0);
        let cp2 = read_cp(&ckpt_path).expect("水位在场");
        assert_eq!(
            cp2.pos, 26000,
            "重连后水位必须继续推进（冻结 = FIX A 缺陷）"
        );
        assert!(
            cp2.written_files.contains(&"to_sql.1.sql".to_string()),
            "written_files 必须登记实产物: {:?}",
            cp2.written_files
        );
    }
}
