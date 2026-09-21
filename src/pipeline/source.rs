//! 原始事件模型 + EventSource trait + 事务状态机（Task 12）。
//!
//! ## 上游语义对照（my2sql-go base/file.go，权威引用）
//!
//! - `start_pos`：**rows 事件的 start_pos = 所属 TABLE_MAP 事件的起始位置**
//!   （file.go:197-198 `if h.EventType == TABLE_MAP_EVENT { tbMapPos = h.LogPos -
//!   h.EventSize }`，file.go:214-215 `oneMyEvent{... StartPos: tbMapPos}`——
//!   rows 事件打印/进 extra-info 用的都是「最近一次 table_map 的起始」，
//!   非 rows 事件本体起始；简报注释 "tablemap 起始" 与上游一致，证实）。
//!   本层非 rows 事件（Query/Xid/Rotate/…）start_pos = 自身起始
//!   （log_pos - event_size，上游 stats 通道对 query 亦此口径，file.go:276）。
//! - GTID 事件（33/34）：上游 base/com.go `CheckBinEvent` 的 `default` 分支直接
//!   `C_reContinue` **完全忽略**（不解析 body、不参与事务定界；事务编号只由
//!   QUERY("BEGIN") 驱动，XID_EVENT 收尾——base/file.go:229-236 +
//!   base/stats_process.go:136-141 XID→"commit"）。据此裁定 1：`RawKind::Gtid`
//!   仅作标记变体（33 与 34 不区分、不携带 uuid/ trx id——P1 无消费者，D5 不猜测）。
//! - 事务口径（上游 base/file.go）：`fileTrxIndex` 仅在 sqlLower=="begin" 时 +1；
//!   Xid 事件 → commit 状态；Query "ROLLBACK" → rollback；rows 事件携带当前
//!   (index, in-progress)。DDL Query 在 file 模式**不产出 SQL**（IfRowsEvent=false
//!   不入 EventChan，file.go:245-268），上游对其无事务语义——简报「DDL→独立事务」
//!   是本层为 T13 附加注释/回滚顺序引入的自有设计（记录为有意的上游超集）。

use std::sync::Arc;

use crate::binlog::error::BinlogError;
use crate::binlog::rows::RowsKind;
use crate::binlog::table_map::TableMapEvent;

/// 事件源产出的原始事件（body 已剥 CRC；行解码推迟到消费方，接缝 T13）。
#[derive(Debug)]
pub struct RawEvent {
    /// 当前 binlog 文件名（rotate 事件后更新为 next 名，上游 com.go:41-46 口径）。
    pub binlog: String,
    /// rows 事件 = 所属 table_map 起始；其余 = 自身起始（见模块注释）。
    pub start_pos: u32,
    /// 事件头 log_pos（事件结束位置）。
    pub end_pos: u32,
    pub timestamp: u32,
    pub kind: RawKind,
    /// 剥 19B 头与 CRC 后的事件体。
    pub body: Vec<u8>,
    /// rows 事件携带其 table_map（源内跟踪最近一个 TABLE_MAP，Arc 共享）。
    pub tm: Option<Arc<TableMapEvent>>,
}

/// 事件语义类别（裁定 1：Gtid 为最小标记，上游不解析）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawKind {
    /// QUERY 事件的 SQL 文本（含 BEGIN/COMMIT/ROLLBACK/DDL）。
    Query(String),
    Xid,
    /// GTID_LOG(33) 与 ANONYMOUS_GTID_LOG(34) 均归此标记变体。
    Gtid,
    /// (行事件种类, 是否 V2[30/31/32]；V1[23/24/25]→false)。
    Rows(RowsKind, bool /*v2*/),
    /// ROTATE 事件的下一个文件名。
    Rotate(String),
    /// 其余事件（P1 源内大多直接吞掉，保留变体供扩展）。
    Other,
}

/// 事件源抽象：`Ok(None)` = 干净停止（EOF 或 stop 条件到达）；IO/坏数据 = Err。
/// （T6b r3：并行泵把源读取挪进作用域线程边等事件边收割 worker 结果，
/// 泵入口以 `dyn EventSource + Send` 收口——生产 `ReplSource`/`FileReader`
/// 与测试假源本就 Send，trait 面不加超轨、`src/binlog/` 免触碰。）
pub trait EventSource {
    fn next(&mut self) -> Result<Option<RawEvent>, BinlogError>;
}

/// 事务状态机状态（上游 C_trxBegin/C_trxCommit/C_trxRollback/C_trxProcess，
/// base/context.go:30-33）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrxStatus {
    Begin,
    Process,
    Commit,
    Rollback,
}

/// BEGIN/Xid 定界的事务状态机（上游口径：MySQL GTID 事件不参与定界）。
#[derive(Debug, Default)]
pub struct TrxStateMachine {
    trx_id: u64,
}

impl TrxStateMachine {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入一个原始事件，返回 (当前事务号, 该事件的事务状态)。
    /// 编号只在 BEGIN 递增（上游 file.go:229-231 `fileTrxIndex++`）；
    /// rows/Gtid/Rotate/Other 为状态透明（Process）；XID 收尾当前事务
    /// （上游 stats_process.go:136-141 XID→commit）；DDL QUERY →
    /// 独立 autocommit 事务（简报绑定，上游 file 模式不产出 DDL SQL——
    /// 本层为 T13 注释/回滚顺序引入的自有超集，见模块注释）。
    pub fn feed(&mut self, ev: &RawEvent) -> (u64, TrxStatus) {
        match &ev.kind {
            RawKind::Query(sql) => {
                // 上游 file.go:227-239 直接 strings.ToLower(sql) 全等比较；
                // 真机 BEGIN/COMMIT/ROLLBACK 文本无尾分号（fixture 证实），
                // 这里额外容忍空白与 ';' 仅增强鲁棒，不改变判定结果。
                let kw = sql.trim().trim_end_matches(';').trim().to_ascii_lowercase();
                match kw.as_str() {
                    "begin" => {
                        self.trx_id += 1;
                        (self.trx_id, TrxStatus::Begin)
                    }
                    "commit" => (self.trx_id, TrxStatus::Commit),
                    "rollback" => (self.trx_id, TrxStatus::Rollback),
                    // 空 QUERY（部分版本 GTID 载体形态）：无文本语义 → no-op
                    "" => (self.trx_id, TrxStatus::Process),
                    // DDL / 其他语句：autocommit 独立事务
                    _ => {
                        self.trx_id += 1;
                        (self.trx_id, TrxStatus::Commit)
                    }
                }
            }
            RawKind::Xid => (self.trx_id, TrxStatus::Commit),
            _ => (self.trx_id, TrxStatus::Process),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binlog::event::EventType;

    fn ev(kind: RawKind) -> RawEvent {
        RawEvent {
            binlog: "mysql-bin.000001".into(),
            start_pos: 4,
            end_pos: 100,
            timestamp: 0,
            kind,
            body: vec![],
            tm: None,
        }
    }

    /// 简报 Step 1：begin→rows→xid 序列产出 (1,Begin),(1,Process),(1,Commit)。
    #[test]
    fn trx_begin_rows_xid_sequence() {
        let mut m = TrxStateMachine::new();
        assert_eq!(
            m.feed(&ev(RawKind::Query("BEGIN".into()))),
            (1, TrxStatus::Begin)
        );
        assert_eq!(
            m.feed(&ev(RawKind::Rows(RowsKind::Write, true))),
            (1, TrxStatus::Process)
        );
        assert_eq!(m.feed(&ev(RawKind::Xid)), (1, TrxStatus::Commit));
    }

    /// 多事务：编号只在 BEGIN 递增（上游 file.go:229-231 fileTrxIndex++）。
    #[test]
    fn trx_id_increments_only_on_begin() {
        let mut m = TrxStateMachine::new();
        m.feed(&ev(RawKind::Query("BEGIN".into())));
        m.feed(&ev(RawKind::Xid));
        assert_eq!(
            m.feed(&ev(RawKind::Query("BEGIN".into()))),
            (2, TrxStatus::Begin)
        );
        assert_eq!(m.feed(&ev(RawKind::Xid)), (2, TrxStatus::Commit));
        // 显式 COMMIT query 不新开事务
        assert_eq!(
            m.feed(&ev(RawKind::Query("COMMIT".into()))),
            (2, TrxStatus::Commit)
        );
    }

    #[test]
    fn trx_rollback_keeps_id_and_marks_rollback() {
        let mut m = TrxStateMachine::new();
        m.feed(&ev(RawKind::Query("BEGIN".into())));
        m.feed(&ev(RawKind::Rows(RowsKind::Delete, true)));
        assert_eq!(
            m.feed(&ev(RawKind::Query("ROLLBACK".into()))),
            (1, TrxStatus::Rollback)
        );
        // rollback 后下一个 begin 仍递增（上游 begin 无条件 ++）
        assert_eq!(
            m.feed(&ev(RawKind::Query("BEGIN".into()))),
            (2, TrxStatus::Begin)
        );
    }

    /// 简报绑定「DDL Query→独立事务」：不来自 begin/commit/rollback 关键字的
    /// QUERY 视为 autocommit 单语句事务（新号 + 立即 Commit）。
    #[test]
    fn ddl_query_is_standalone_committed_trx() {
        let mut m = TrxStateMachine::new();
        assert_eq!(
            m.feed(&ev(RawKind::Query("CREATE TABLE t (a INT)".into()))),
            (1, TrxStatus::Commit)
        );
        m.feed(&ev(RawKind::Query("BEGIN".into())));
        m.feed(&ev(RawKind::Xid));
        assert_eq!(
            m.feed(&ev(RawKind::Query("DROP TABLE t".into()))),
            (3, TrxStatus::Commit)
        );
    }

    /// GTID 标记（含匿名组）与 Rotate/Other：不改变事务号与定界（上游忽略 33/34）。
    #[test]
    fn gtid_and_misc_are_state_transparent() {
        let mut m = TrxStateMachine::new();
        assert_eq!(m.feed(&ev(RawKind::Gtid)), (0, TrxStatus::Process));
        m.feed(&ev(RawKind::Query("BEGIN".into())));
        assert_eq!(m.feed(&ev(RawKind::Gtid)), (1, TrxStatus::Process));
        assert_eq!(
            m.feed(&ev(RawKind::Rotate("x".into()))),
            (1, TrxStatus::Process)
        );
        assert_eq!(m.feed(&ev(RawKind::Other)), (1, TrxStatus::Process));
        assert_eq!(m.feed(&ev(RawKind::Xid)), (1, TrxStatus::Commit));
    }

    /// 边界：大小写/尾分号/空白鲁棒；空 QUERY（GTID 载体形态）为 no-op。
    #[test]
    fn begin_recognition_is_lenient_and_empty_is_noop() {
        let mut m = TrxStateMachine::new();
        assert_eq!(
            m.feed(&ev(RawKind::Query("  beGiN; ".into()))),
            (1, TrxStatus::Begin)
        );
        assert_eq!(
            m.feed(&ev(RawKind::Query(String::new()))),
            (1, TrxStatus::Process)
        );
    }

    #[test]
    fn rawkind_eq_and_event_basics_compile() {
        // 烟雾：EventType 常量与 RawKind 无耦合，仅钉住 PartialEq 派生可用。
        assert_eq!(RawKind::Xid, RawKind::Xid);
        assert_ne!(RawKind::Gtid, RawKind::Xid);
        assert_eq!(EventType::GTID_LOG, 33);
    }
}
