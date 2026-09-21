// 事件源 / 过滤 / 事务状态机（Task 12）+ 保序 / worker（Task 14 装配）。
pub mod filter;
pub mod order;
pub mod source;
pub mod worker;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};

use crate::binlog::error::BinlogError;
use crate::binlog::file_reader::FileReader;
use crate::binlog::table_map::TableMapEvent;
use crate::config::Config;
use crate::metadata::schema::{Align, TableSchema, align_cols};
use crate::metadata::store::{MetaError, SchemaStore};
use crate::output::Writer;
use crate::pipeline::filter::Filters;
use crate::pipeline::order::Reorder;
use crate::pipeline::source::{EventSource, RawEvent, RawKind, TrxStateMachine};
use crate::pipeline::worker::{Job, SqlGroup, build_groups, worker_loop};
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

impl std::fmt::Display for RunSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "to-sql done: events={}, statements={}, files={}, errors={}",
            self.events, self.statements, self.files, self.errors
        )
    }
}

/// 端到端装配：FileReader→filter→trx 机→编号→workers→reorder→Writer。
/// `threads=1` 走单线程直通（与并行路径输出逐字节等价的契约由 e2e 测试钉死）。
pub fn run_to_sql(cfg: &Config) -> Result<RunSummary, PipelineError> {
    if !cfg.to_stdout && cfg.output_dir.is_none() {
        return Err(PipelineError::Config(
            "output target required: pass --output-dir or --to-stdout".into(),
        ));
    }
    let store = match (&cfg.schema_file, &cfg.uri) {
        (Some(p), _) => SchemaStore::offline(p)?,
        (None, Some(uri)) => SchemaStore::online(uri)?,
        (None, None) => unreachable!("Config::validate 已拦双缺"),
    };
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
        writer,
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

/// 一次运行的装配状态（dispatcher 侧独占；`SchemaStore` `&mut` 语义天然单线程）。
struct Runner<'a> {
    cfg: &'a Config,
    filters: Filters,
    store: SchemaStore,
    builder: DmlBuilder,
    writer: Writer,
    trx: TrxStateMachine,
    /// 已派发事件数 = 下一个 seq 编号。
    seq: u64,
    reorder: Reorder,
    summary: RunSummary,
    /// table_id → (建立时的 tm Arc, 表结构)。tm 更替（ptr 不等）即重取
    /// ——DDL 后结构漂移的保守重查路径；SchemaStore 自身还有 db.table 缓存。
    tmap: HashMap<u64, (Arc<TableMapEvent>, Arc<TableSchema>)>,
    threads: usize,
}

impl<'a> Runner<'a> {
    fn new(
        cfg: &'a Config,
        filters: Filters,
        store: SchemaStore,
        builder: DmlBuilder,
        writer: Writer,
    ) -> Self {
        Self {
            cfg,
            filters,
            store,
            builder,
            writer,
            trx: TrxStateMachine::new(),
            seq: 0,
            reorder: Reorder::new(),
            summary: RunSummary::default(),
            tmap: HashMap::new(),
            threads: cfg.threads.clamp(1, 64),
        }
    }

    fn dump_schema(&self, path: &std::path::Path) -> Result<(), PipelineError> {
        // SchemaStore 被借用为 &self 即可 dump（在线查得的结构已在 cache）
        Ok(self.store.dump(path)?)
    }

    /// 主循环：逐文件（上游镜像：仅当设了 stop 条件才续读下一文件，
    /// T12 裁定 7）→ 逐事件 → 线程形态分派。
    fn run(&mut self) -> Result<usize, PipelineError> {
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
        self.writer.finish().map_err(Into::into)
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
    fn prepare(&mut self, ev: RawEvent) -> Option<Job> {
        let (trx_id, _status) = self.trx.feed(&ev);
        if !matches!(ev.kind, RawKind::Rows(..)) {
            // 非行事件只喂事务机（上游 file 模式 DDL/Query 不出 SQL）
            return None;
        }
        if !self.filters.accept(&ev, None) {
            return None;
        }
        let Some(tm) = ev.tm.clone() else {
            // FileReader 已保证 rows 必带 tm（源不变式），防御分支
            self.bump_error(&ev, "rows event without table_map");
            return None;
        };
        match self.schema_for(&tm) {
            Ok(schema) => {
                let job = Job {
                    seq: self.seq,
                    ev,
                    trx_id,
                    schema,
                };
                self.seq += 1;
                self.summary.events += 1;
                Some(job)
            }
            Err(e) => {
                self.bump_error(
                    &ev,
                    &format!("schema lookup for `{}.{}`: {e:#}", tm.schema, tm.table),
                );
                None
            }
        }
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

    fn emit(&mut self, groups: Vec<SqlGroup>) -> Result<(), PipelineError> {
        for g in &groups {
            self.summary.statements += g.sqls.len() as u64;
        }
        for g in &groups {
            self.writer.write_group(g)?;
        }
        Ok(())
    }

    /// 单线程直通：无通道无线程，build 内联，reorder 恒零滞留。
    fn pump_direct<R: std::io::Read + std::io::Seek>(
        &mut self,
        mut reader: FileReader<R>,
    ) -> Result<(), PipelineError> {
        while let Some(ev) = reader.next()? {
            let Some(job) = self.prepare(ev) else {
                continue;
            };
            let seq = job.seq;
            let groups = match build_groups(&job, &self.builder) {
                Ok(g) => g,
                Err(e) => {
                    self.summary.errors += 1;
                    tracing::error!(
                        seq,
                        binlog = %job.ev.binlog,
                        pos = job.ev.start_pos,
                        "event skipped due to decode/SQL-build error: {e:#}"
                    );
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
        let (res_tx, res_rx): (Sender<(u64, Vec<SqlGroup>)>, _) = unbounded();
        let errors = Arc::new(AtomicU64::new(0));
        let mut handles = Vec::with_capacity(self.threads);
        for _ in 0..self.threads {
            let job_rx = job_rx.clone();
            let res_tx = res_tx.clone();
            let builder = self.builder.clone();
            let errors = errors.clone();
            handles.push(thread::spawn(move || {
                worker_loop(job_rx, res_tx, builder, errors)
            }));
        }
        drop(job_rx);
        drop(res_tx); //  dispatcher 侧只 recv；所有 worker 结束后通道才闭合

        while let Some(ev) = reader.next()? {
            if let Some(job) = self.prepare(ev) {
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
        Ok(())
    }

    /// 非阻塞收取全部已就绪结果并写出（反压前置）。
    fn reap(&mut self, res_rx: &Receiver<(u64, Vec<SqlGroup>)>) -> Result<(), PipelineError> {
        while let Ok((seq, g)) = res_rx.try_recv() {
            let ready = self.reorder.push(seq, g);
            self.emit(ready)?;
        }
        Ok(())
    }
}
