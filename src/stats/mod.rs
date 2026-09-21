//! stats 子系统（P2 T4）：轻量事实流 + 上游口径聚合器。
//!
//! 数据源是流水线的 `worker::Out::Fact` / `Out::Status` 保序流（dispatcher
//! 单线程消费，聚合非瓶颈），聚合算法逐行照抄上游
//! `reference/my2sql-go/base/stats_process.go:150-268`（只读权威）：
//!
//! - `binlog_status.txt`：窗口×表 行计数报表。头行在建文件时写入
//!   （context.go:530 口径，`GetStatsPrintHeaderLine` = 同宽 `%s` 版）；
//!   内容行 `%-17s %-19s %-19s %-10d %-10d %-8d %-8d %-8d %-15s %-20s\n`，
//!   datetime 为下划线形（= P1 `datetime_str`，constvar DATETIME_FORMAT_NOSPACE）。
//! - `biglong_trx.txt`：大/长事务命中行（`RowCnt >= big || Duration >= long`，
//!   `>=` 边界照抄 stats_process.go:201），内容行
//!   `%-17s %-19s %-19s %-10d %-10d %-8d %-10d %s\n`；明细
//!   `[db.tb(inserts=N, updates=N, deletes=N) …]` 单空格连接。
//! - 窗口落盘时机：`last_print_time==0 → ts+interval`；`ts >= last_print_time`
//!   全量落盘并重置 `ts+interval`；binlog 切换落盘清空；`finish` 落残余
//!   （上游 StatChan 关闭后的收尾循环，stats_process.go:262-265）。
//!
//! 对上游的有意超越（计划「超越项 3」登记）：Go map 随机序 → 本实现
//! 窗口行 = **首次出现序**、`[...]` 明细 = **db.tb 升序**（确定性输出）。

/// 行事件种类（stats 口径 = 上游 QueryType 的 insert/update/delete 三支；
/// update 行数按**行对**计，stats_process.go:111 `rowCnt = len/2`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactKind {
    Insert,
    Update,
    Delete,
}

/// worker 上送的轻量行事实（P2 T4 简报钉死形态；`rows` = 已按对折算的行数）。
#[derive(Debug, Clone)]
pub struct StatFact {
    pub binlog: String,
    pub start_pos: u32,
    pub end_pos: u32,
    pub timestamp: u32,
    pub db: String,
    pub table: String,
    pub kind: FactKind,
    pub rows: u64,
    pub trx_id: u64,
}
