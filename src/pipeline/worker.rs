//! worker 层：每线程收 `Job`（已编号的 rows 事件 + dispatcher 备好的
//! table_map/schema）→ `decode_rows` → `DmlBuilder` → `SqlGroup` 批次。
//!
//! ## 分工口径（简报绑定 + T12/T13 接缝结论）
//!
//! - **schema 获取/缓存全部在 dispatcher**（`SchemaStore::get` 是 `&mut`，
//!   天然单线程）：新 `table_id`（或 table_map 更替）时取一次 `TableSchema`、
//!   包 `Arc` 随 Job 克隆下发；worker 零共享、零连库。
//! - **dropped 列对账（Align）**：dispatcher 在 (tm, schema) 配对建立时计算
//!   一次，用于 strict 形态的早期甄别/告警去重（每对一次而非每事件一次）。
//!   每事件 SQL 内仍由 `DmlBuilder::plan` 消费对齐结果——T13 审定 API 不变
//!   （对齐是 O(列数) 纯计算，重复成本可忽略，报告有注）。
//! - **逐事件错误策略（robust-continue，P1 文档化决策）**：worker 对
//!   decode/build 错误 `tracing::error!` + 跳过该事件 + `errors` 原子计数，
//!   最终汇入摘要行。上游 my2sql-go 大多数同类错误直接 `log.Fatalf` 全进程
//!   终止（events.go:87 等）——本工具对**文件级**损坏（checksum/截断，源层
//!   Err）同样终止，仅对**单事件解码/生成**错误继续；差分夹具均为良构 binlog，
//!   两种策略在 T15 期望输出上无分歧。
//! - **panic 亦填洞（T14 审阅 Finding 1）**：worker 侧 build 路径 panic 经
//!   `catch_unwind` 捕获，走与 Err 完全相同的计数+空批填洞契约——否则 seq
//!   空洞 + 存活 worker 持有 sender 会让 dispatcher 在 `recv()` 上永久挂死。

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crossbeam_channel::{Receiver, Sender};

use crate::binlog::error::BinlogError;
use crate::binlog::rows::{RowsKind, decode_rows};
use crate::metadata::schema::TableSchema;
use crate::pipeline::source::{RawEvent, RawKind, TrxStatus};
use crate::sqlopen::SqlError;
use crate::sqlopen::dml::DmlBuilder;
use crate::stats::{FactKind, StatFact};

/// 一条 rows 事件生成的 SQL 批次（简报接口绑定：写出侧最小单元；
/// trx_id 为 P2 keep-trx/回滚顺序预留，P1 仅透传）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlGroup {
    pub binlog: String,
    /// rows 事件 = 所属 table_map 起始（上游 tbMapPos 口径，T12 裁定）。
    pub start_pos: u32,
    /// 事件尾 log_pos（extra-info `stoppos` 分量）。
    pub end_pos: u32,
    /// 事件头 unix 秒（extra-info `datetime` 分量，输出边沿换算）。
    pub timestamp: u32,
    pub db: String,
    pub table: String,
    pub trx_id: u64,
    pub sqls: Vec<String>,
}

/// dispatcher → worker 的工作单元：seq 保序编号 + 原始事件 + 备好的表结构。
/// P2 T4：`status` = 该事件的事务状态（`self.trx.feed` 第二返回元，
/// stats 通道 biglong 判定的物化事件流所需）。
pub struct Job {
    pub seq: u64,
    pub ev: RawEvent,
    pub trx_id: u64,
    pub schema: Arc<TableSchema>,
    pub status: TrxStatus,
}

/// worker→dispatcher 回流的统一载荷（P2 T4 简报钉死）：SQL 组（to-sql/
/// flashback）、stats 轻量事实、事务标记。`Status` 仅由 dispatcher 在
/// stats 形态直推 reorder（不经 worker——它是 dispatcher 已有信息）；
/// `Sql` 包装不改变任何写出字节（to-sql/flashback 行为守卫在既有 e2e）。
#[derive(Debug, Clone)]
pub enum Out {
    Sql(SqlGroup),
    Fact(StatFact),
    Status {
        binlog: String,
        pos: u32,
        ts: u32,
        status: TrxStatus,
    },
}

/// worker_loop 的作业形态（P2 T4）：Sql = 现路径（build_groups）；
/// Stats = 只解码计数（build_out_stats），不触碰 SQL 构建器。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutMode {
    Sql,
    Stats,
}

/// 纯函数形态的单事件处理（threads=1 直通路径与 worker 线程共用）：
/// rows 事件 → `Vec<SqlGroup>`（0 或 1 个：整事件一批，空 SQL 不出组）。
/// 非 rows 事件按源不变式不可达（dispatcher 只给 rows），防御性报错。
pub fn build_groups(job: &Job, builder: &DmlBuilder) -> Result<Vec<SqlGroup>, SqlError> {
    let RawKind::Rows(kind, v2) = &job.ev.kind else {
        return Err(BinlogError::InvalidData(format!(
            "worker received non-rows event ({:?})",
            job.ev.kind
        ))
        .into());
    };
    let tm = job.ev.tm.as_deref().ok_or::<SqlError>(
        BinlogError::InvalidData("rows event without table_map".into()).into(),
    )?;
    let rows = decode_rows(&job.ev.body, tm, &job.schema, *kind, *v2)?;
    // P2 T3：统一分派入口（ToSql 下与旧三臂逐字节等价；Flashback 下
    // Write↔Delete 互换 + 逆向 UPDATE，T1 硬规则错误原样上抛）。
    let sqls = builder.dml_for(*kind, tm, &job.schema, &rows)?;
    if sqls.is_empty() {
        // 全部语句被跳过（如无变化行对，T13 策略）：不出组、不出注释头
        return Ok(Vec::new());
    }
    Ok(vec![SqlGroup {
        binlog: job.ev.binlog.clone(),
        start_pos: job.ev.start_pos,
        end_pos: job.ev.end_pos,
        timestamp: job.ev.timestamp,
        db: tm.schema.clone(),
        table: tm.table.clone(),
        trx_id: job.trx_id,
        sqls,
    }])
}

/// stats 形态的单事件处理（P2 T4 简报钉死实现）：rows 事件 → 恰一条
/// `Out::Fact`（update 行数 = 行对数，`rows.len()/2` 镜像上游
/// stats_process.go:111）；非 rows 事件投空批填洞（Query/Xid 的 Status
/// 标记由 dispatcher 侧直接入 reorder 流，seq 与 rows 事件同源编号）。
/// 解码错误原样上抛——填洞/计数契约与 SQL 形态共用 `process_job`。
pub fn build_out_stats(job: &Job) -> Result<Vec<Out>, SqlError> {
    match &job.ev.kind {
        RawKind::Rows(kind, v2) => {
            let tm = job.ev.tm.as_deref().ok_or::<SqlError>(
                BinlogError::InvalidData("rows event without table_map".into()).into(),
            )?;
            let rows = decode_rows(&job.ev.body, tm, &job.schema, *kind, *v2)?;
            Ok(vec![Out::Fact(StatFact {
                binlog: job.ev.binlog.clone(),
                start_pos: job.ev.start_pos,
                end_pos: job.ev.end_pos,
                timestamp: job.ev.timestamp,
                db: tm.schema.clone(),
                table: tm.table.clone(),
                kind: match kind {
                    RowsKind::Write => FactKind::Insert,
                    RowsKind::Update => FactKind::Update,
                    RowsKind::Delete => FactKind::Delete,
                },
                rows: match kind {
                    RowsKind::Update => (rows.len() / 2) as u64,
                    _ => rows.len() as u64,
                },
                trx_id: job.trx_id,
            })])
        }
        // Query/Xid：Out::Fact 空——marker 由 dispatcher 侧直接入 reorder 流
        _ => Ok(Vec::new()),
    }
}

/// 按作业形态分派单事件构建（worker_loop / pump_direct 共用）。
pub fn build_out(job: &Job, builder: &DmlBuilder, mode: OutMode) -> Result<Vec<Out>, SqlError> {
    match mode {
        OutMode::Sql => build_groups(job, builder).map(|gs| gs.into_iter().map(Out::Sql).collect()),
        OutMode::Stats => build_out_stats(job),
    }
}

/// worker 线程主循环：recv → build → 回投 `(seq, groups)`。
/// 错误即计数 + `tracing::error!` + 投空批（空洞必须填，否则 reorder 永挂）。
/// `job_rx` 断开（dispatcher 投递完成）→ 自然退出。
/// 测试专用 panic 注入缝：`binlog` 等于该标记的作业在处理时 panic，
/// 用于钉死 worker_loop 的 catch_unwind 填洞契约（Finding 1）。
#[cfg(test)]
pub(crate) const PANIC_MARKER: &str = "__panic__";

/// 单作业处理（catch_unwind 保护区）：Ok=批；Err/panic → 计数 + error 日志 +
/// 空批（**同一填洞契约**：res 流绝不允许出现 seq 空洞，否则 reorder/dispatcher
/// 永挂——存活 worker 持有 sender 时 `recv()` 不会断开）。
/// P2 T3：Err 分支额外置 abort 哨兵——**当且仅当** `stop_on_error`（flashback
/// `--on-error stop` 形态；to-sql 侧恒 false，行为零变）。
/// P2 T4：`mode` 决定构建分派（Sql/Stats），产物统一 `Vec<Out>`。
fn process_job(
    job: &Job,
    builder: &DmlBuilder,
    errors: &AtomicU64,
    abort: &AtomicBool,
    stop_on_error: bool,
    mode: OutMode,
) -> Vec<Out> {
    #[cfg(test)]
    if job.ev.binlog == PANIC_MARKER {
        panic!("injected worker panic (seq={})", job.seq);
    }
    match build_out(job, builder, mode) {
        Ok(g) => g,
        Err(e) => {
            errors.fetch_add(1, Ordering::Relaxed);
            if stop_on_error {
                abort.store(true, Ordering::Relaxed);
            }
            tracing::error!(
                seq = job.seq,
                binlog = %job.ev.binlog,
                pos = job.ev.start_pos,
                "event skipped due to decode/SQL-build error: {e:#}"
            );
            Vec::new()
        }
    }
}

/// panic 载荷 → 日志可读文本（&str/String 之外的任意载荷兜底）。
fn panic_payload(p: &(dyn Any + Send)) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

/// worker 线程主循环：recv → build → 回投 `(seq, groups)`。
/// 错误即计数 + `tracing::error!` + 投空批（空洞必须填，否则 reorder 永挂）。
/// build 路径 **panic 同样捕获**并走同款填洞（Finding 1：单作业 panic 曾致
/// 流水线永久挂死——肇事线程死后存活 worker 仍持有 sender，`recv()` 永断不了）。
/// P2 T3 新末两参：`stop_on_error=true`（flashback `--on-error stop`）时
/// Err/panic 两分支在计数后置 `abort` 哨兵，由 Runner 收取循环后统一转 Err；
/// to-sql 侧恒 `stop_on_error=false` + 独立哨兵（零改动行为）。
/// P2 T4 新末参 `mode`：Sql = 既有路径（载荷 `Out::Sql` 包装，写出字节不变）；
/// Stats = `build_out_stats` 轻量事实流。
/// `job_rx` 断开（dispatcher 投递完成）→ 自然退出。
pub fn worker_loop(
    job_rx: Receiver<Job>,
    res_tx: Sender<(u64, Vec<Out>)>,
    builder: DmlBuilder,
    errors: Arc<AtomicU64>,
    abort: Arc<AtomicBool>,
    stop_on_error: bool,
    mode: OutMode,
) {
    while let Ok(job) = job_rx.recv() {
        let seq = job.seq;
        let groups = match catch_unwind(AssertUnwindSafe(|| {
            process_job(&job, &builder, &errors, &abort, stop_on_error, mode)
        })) {
            Ok(g) => g,
            Err(p) => {
                errors.fetch_add(1, Ordering::Relaxed);
                if stop_on_error {
                    abort.store(true, Ordering::Relaxed);
                }
                tracing::error!(
                    seq,
                    "event skipped due to worker panic: {}",
                    panic_payload(&*p)
                );
                Vec::new()
            }
        };
        // 通道断开（dispatcher 已放弃收集）时只能整跑作废，静默退出
        if res_tx.send((seq, groups)).is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binlog::rows::RowsKind;
    use crate::binlog::table_map::TableMapEvent;
    use crate::metadata::schema::SchemaCol;

    fn tm2() -> TableMapEvent {
        TableMapEvent {
            table_id: 7,
            schema: "d1".into(),
            table: "t1".into(),
            n_cols: 1,
            column_type: vec![3],
            column_meta: vec![0],
            null_bits: vec![0],
            charset: vec![],
        }
    }

    fn schema1() -> TableSchema {
        TableSchema {
            db: "d1".into(),
            table: "t1".into(),
            cols: vec![SchemaCol {
                name: "id".into(),
                type_name: "int".into(),
                unsigned: false,
            }],
            pk: vec!["id".into()],
            uks: vec![],
        }
    }

    /// WRITE_ROWS_V2 最小体：tid6+flags2+extra(2)+ncols+bm+nb+一个 int。
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

    fn rows_job(seq: u64, body: Vec<u8>) -> Job {
        Job {
            seq,
            ev: RawEvent {
                binlog: "mysql-bin.000001".into(),
                start_pos: 100,
                end_pos: 150,
                timestamp: 555,
                kind: RawKind::Rows(RowsKind::Write, true),
                body,
                tm: Some(Arc::new(tm2())),
            },
            trx_id: 3,
            schema: Arc::new(schema1()),
            status: TrxStatus::Process,
        }
    }

    #[test]
    fn build_groups_produces_single_group_with_positions_and_trx() {
        let job = rows_job(9, write_body(7, 42));
        let g = build_groups(&job, &DmlBuilder::default()).unwrap();
        assert_eq!(g.len(), 1);
        assert_eq!(
            g[0].sqls,
            vec!["INSERT INTO `d1`.`t1` (`id`) VALUES (42);".to_string()]
        );
        assert_eq!(g[0].trx_id, 3);
        assert_eq!(
            (g[0].start_pos, g[0].end_pos, g[0].timestamp),
            (100, 150, 555)
        );
        assert_eq!((g[0].db.as_str(), g[0].table.as_str()), ("d1", "t1"));
    }

    #[test]
    fn build_groups_errors_propagate_for_dispatcher_counting() {
        // table_id 不符 → decode_rows InvalidData（逐事件错误，T14 策略=跳过+计数）
        let mut job = rows_job(1, write_body(8, 1));
        job.ev.kind = RawKind::Rows(RowsKind::Write, true);
        assert!(build_groups(&job, &DmlBuilder::default()).is_err());
        // 非 rows 事件混入 = 调用方 bug → 防御性错误
        let mut j2 = rows_job(2, vec![]);
        j2.ev.kind = RawKind::Xid;
        assert!(build_groups(&j2, &DmlBuilder::default()).is_err());
    }

    /// Finding 1（RED→GREEN）：单 worker 处理作业 panic 不得留下 seq 空洞——
    /// 与 Err 分支同款填洞契约（投 (seq, 空批) + errors 计数）。复现真实挂死
    /// 形态：双 worker，肇事线程死亡后存活 worker 仍持有 res_tx sender，
    /// dispatcher 侧 `res_rx.recv()` 将永无结果。recv_timeout 保证失败模式
    /// 是 FAIL 而非测试挂起。
    #[test]
    fn worker_loop_panics_are_caught_and_hole_filled() {
        let (job_tx, job_rx) = crossbeam_channel::unbounded::<Job>();
        let (res_tx, res_rx) = crossbeam_channel::unbounded::<(u64, Vec<Out>)>();
        let errors = Arc::new(AtomicU64::new(0));
        let abort = Arc::new(AtomicBool::new(false));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let (rx, tx, e, b, a) = (
                job_rx.clone(),
                res_tx.clone(),
                errors.clone(),
                DmlBuilder::default(),
                abort.clone(),
            );
            handles.push(std::thread::spawn(move || {
                worker_loop(rx, tx, b, e, a, false, OutMode::Sql)
            }));
        }
        drop(job_rx);
        drop(res_tx);
        let mut bad = rows_job(0, write_body(7, 1));
        bad.ev.binlog = PANIC_MARKER.into();
        job_tx.send(bad).unwrap();
        job_tx.send(rows_job(1, write_body(7, 2))).unwrap();
        let mut got = Vec::new();
        for _ in 0..2 {
            let (seq, g) = res_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("dispatcher would hang forever: seq hole left by worker panic");
            got.push((seq, g.len()));
        }
        got.sort();
        assert_eq!(
            got,
            vec![(0, 0), (1, 1)],
            "panicked seq must be hole-filled"
        );
        assert_eq!(errors.load(Ordering::Relaxed), 1, "panic counted as error");
        assert!(
            !abort.load(Ordering::Relaxed),
            "stop_on_error=false（to-sql 形态）：panic/Err 一律不置哨兵"
        );
        drop(job_tx);
        for h in handles {
            h.join().unwrap();
        }
    }

    /// P2 T3 哨兵契约：`stop_on_error=true` 时 Err 与 panic 两分支都必须
    /// 置 abort（Runner 并行收取循环后据此转 `--on-error stop` 的 Err）；
    /// 正常批不置。
    #[test]
    fn worker_loop_stop_on_error_sets_abort_sentinel_on_err_and_panic() {
        let (job_tx, job_rx) = crossbeam_channel::unbounded::<Job>();
        let (res_tx, res_rx) = crossbeam_channel::unbounded::<(u64, Vec<Out>)>();
        let errors = Arc::new(AtomicU64::new(0));
        let abort = Arc::new(AtomicBool::new(false));
        let builder = DmlBuilder::default();
        let handle = std::thread::spawn({
            let (e, a) = (errors.clone(), abort.clone());
            move || worker_loop(job_rx, res_tx, builder, e, a, true, OutMode::Sql)
        });
        job_tx.send(rows_job(0, write_body(7, 1))).unwrap(); // 正常批
        job_tx.send(rows_job(1, write_body(8, 2))).unwrap(); // table_id 不符 → Err
        for _ in 0..2 {
            res_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("hole-filled stream");
        }
        assert!(
            abort.load(Ordering::Relaxed),
            "Err 分支 + stop_on_error → 哨兵必须置位"
        );
        let mut bad = rows_job(2, write_body(7, 3));
        bad.ev.binlog = PANIC_MARKER.into();
        job_tx.send(bad).unwrap();
        res_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("panic hole-filled");
        drop(job_tx);
        handle.join().unwrap();
        assert_eq!(errors.load(Ordering::Relaxed), 2);
    }
}
