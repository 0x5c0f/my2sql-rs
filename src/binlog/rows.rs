//! ROWS 事件行解码（占位模块，Task 10 实现）。
//!
//! 计划接口（见 P1 计划 Task 10）：
//! `pub struct Row { pub cols: Vec<ColumnValue> }` 与
//! `pub fn decode_rows(body:&[u8], tm:&TableMapEvent, schema:&TableSchema,
//! kind:RowsKind, v2:bool, with_crc:bool) -> Result<Vec<Row>, BinlogError>`，
//! 逐列经 [`crate::binlog::value::decode_value`] 分发。行为对照 go-mysql
//! `replication/row_event.go` 的 `read_rows`。
