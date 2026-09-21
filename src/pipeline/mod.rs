// 事件源 / 过滤 / 事务状态机（Task 12）+ 保序 / worker（Task 14 装配）。
pub mod filter;
pub mod order;
pub mod source;
pub mod worker;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};

use crate::binlog::error::BinlogError;
use crate::binlog::file_reader::FileReader;
use crate::binlog::table_map::TableMapEvent;
use crate::config::{Config, OnError};
use crate::flashback::final_for_tmp;
use crate::flashback::reverse::{self, Block};
use crate::metadata::schema::{Align, TableSchema, align_cols};
use crate::metadata::store::{MetaError, SchemaStore};
use crate::output::{Writer, datetime_str};
use crate::pipeline::filter::Filters;
use crate::pipeline::order::Reorder;
use crate::pipeline::source::{EventSource, RawEvent, RawKind, TrxStateMachine, TrxStatus};
use crate::pipeline::worker::{Job, Out, OutMode, build_out, worker_loop};
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
enum Emitter {
    Sql(Writer),
    Flash { tmp: Writer },
    Stats(crate::stats::Aggregator),
}

/// 表结构来源分派（run_to_sql / run_flashback 共用）。
fn open_store(cfg: &Config) -> Result<SchemaStore, PipelineError> {
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
    let mut st = Runner::new(
        cfg,
        Filters::from_config(cfg),
        store,
        DmlBuilder::flashback(SqlOpts::from_config(cfg)),
        Emitter::Flash { tmp: writer },
    );
    match st.run_flash() {
        Ok((mut summary, files)) => {
            summary.files = files;
            // --schema-dump：与 run_to_sql 同款装配层收口（P2 T7 补消费——
            // flashback 参数面与 to-sql 同构，旗标不得挂空）
            if let Some(p) = &cfg.schema_dump {
                st.dump_schema(p)?;
            }
            Ok(summary)
        }
        Err(e) => Err(e), // run_flash 内部已清 tmp/final（见 cleanup_flash_files）
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

/// 一次运行的装配状态（dispatcher 侧独占；`SchemaStore` `&mut` 语义天然单线程）。
struct Runner<'a> {
    cfg: &'a Config,
    filters: Filters,
    store: SchemaStore,
    builder: DmlBuilder,
    emitter: Emitter,
    trx: TrxStateMachine,
    /// 已派发事件数 = 下一个 seq 编号。
    seq: u64,
    reorder: Reorder<Out>,
    summary: RunSummary,
    /// table_id → (建立时的 tm Arc, 表结构)。tm 更替（ptr 不等）即重取
    /// ——DDL 后结构漂移的保守重查路径；SchemaStore 自身还有 db.table 缓存。
    tmap: HashMap<u64, (Arc<TableMapEvent>, Arc<TableSchema>)>,
    threads: usize,
    /// flashback 形态收集的被排除 DDL/非事务 QUERY（(timestamp, binlog,
    /// start_pos, sql)），run 收尾统一 warn 汇总（简报 Step 4）。
    ddl: Vec<(u32, String, u32, String)>,
}

impl<'a> Runner<'a> {
    fn new(
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
        // stop 条件 = stop-file/stop-pos（Filters.stop）或 stop-datetime（stop_ts）
        let cross_file = self.filters.stop.is_some() || self.filters.stop_ts.is_some();
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
                tracing::info!("{} not exists nor a file, stop", path.display());
                break;
            }
            let reader = FileReader::open(&self.cfg.binlog_dir, &name, self.filters.clone())?;
            self.pump_one_file(reader)?;
            if !cross_file {
                break; // 上游默认单文件真相（file.go:74-85）
            }
            match FileReader::<std::fs::File>::next_binlog_name(&name) {
                Some(next) => name = next,
                None => break,
            }
        }
        Ok(())
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

    /// 单文件消费（threads==1 直通 / 并行 worker 池两条路径）。
    fn pump_one_file<R: std::io::Read + std::io::Seek>(
        &mut self,
        reader: FileReader<R>,
    ) -> Result<(), PipelineError> {
        if self.threads == 1 {
            self.pump_direct(reader)
        } else {
            self.pump_parallel(reader)
        }
    }

    /// 事件 → （过滤/事务机/schema 配对）→ Option<Job>。errors 计数在此
    /// 累计（schema 获取失败 = 逐事件错误）；源不变式违背同样计数跳过。
    /// P2 修复轮：stop 形态（仅 flashback）下 prepare 侧错误升整跑 Err
    /// （spec §3.2 完整性——不完整且不标记的回滚脚本绝不落盘）；to-sql 侧
    /// stop_on_error() 恒 false，计数跳过行为逐字节不变。
    fn prepare(&mut self, ev: RawEvent) -> Result<Option<Job>, PipelineError> {
        let (trx_id, status) = self.trx.feed(&ev);
        if !matches!(ev.kind, RawKind::Rows(..)) {
            // 非行事件只喂事务机（上游 file 模式 DDL/Query 不出 SQL）。
            // flashback 形态：非事务性 QUERY（DDL 等）登记排除清单，run 收尾
            // 汇总告警（begin/commit/rollback/空文本 = 事务脚手架，不登记）。
            if self.is_flash()
                && let RawKind::Query(sql) = &ev.kind
            {
                let kw = sql.trim().trim_end_matches(';').trim().to_ascii_lowercase();
                if !kw.is_empty() && kw != "begin" && kw != "commit" && kw != "rollback" {
                    self.ddl
                        .push((ev.timestamp, ev.binlog.clone(), ev.start_pos, sql.clone()));
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
        Ok(())
    }

    /// 单线程直通：无通道无线程，build 内联，reorder 恒零滞留。
    fn pump_direct<R: std::io::Read + std::io::Seek>(
        &mut self,
        mut reader: FileReader<R>,
    ) -> Result<(), PipelineError> {
        while let Some(ev) = reader.next()? {
            let Some(job) = self.prepare(ev)? else {
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
    fn pump_parallel<R: std::io::Read + std::io::Seek>(
        &mut self,
        mut reader: FileReader<R>,
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

        while let Some(ev) = reader.next()? {
            if let Some(job) = self.prepare(ev)? {
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
                    return Err(PipelineError::Config("worker pool died mid-run".into()));
                }
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
        Ok(())
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
