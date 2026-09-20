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

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crossbeam_channel::{Receiver, Sender};

use crate::binlog::error::BinlogError;
use crate::binlog::rows::{RowsKind, decode_rows};
use crate::metadata::schema::TableSchema;
use crate::pipeline::source::{RawEvent, RawKind};
use crate::sqlopen::SqlError;
use crate::sqlopen::dml::DmlBuilder;

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
pub struct Job {
    pub seq: u64,
    pub ev: RawEvent,
    pub trx_id: u64,
    pub schema: Arc<TableSchema>,
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
    let sqls = match kind {
        RowsKind::Write => builder.inserts(tm, &job.schema, &rows)?,
        RowsKind::Update => builder.updates(tm, &job.schema, &rows)?,
        RowsKind::Delete => builder.deletes(tm, &job.schema, &rows)?,
    };
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

/// worker 线程主循环：recv → build → 回投 `(seq, groups)`。
/// 错误即计数 + `tracing::error!` + 投空批（空洞必须填，否则 reorder 永挂）。
/// `job_rx` 断开（dispatcher 投递完成）→ 自然退出。
pub fn worker_loop(
    job_rx: Receiver<Job>,
    res_tx: Sender<(u64, Vec<SqlGroup>)>,
    builder: DmlBuilder,
    errors: Arc<AtomicU64>,
) {
    while let Ok(job) = job_rx.recv() {
        let seq = job.seq;
        let groups = match build_groups(&job, &builder) {
            Ok(g) => g,
            Err(e) => {
                errors.fetch_add(1, Ordering::Relaxed);
                tracing::error!(
                    seq,
                    binlog = %job.ev.binlog,
                    pos = job.ev.start_pos,
                    "event skipped due to decode/SQL-build error: {e:#}"
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
}
