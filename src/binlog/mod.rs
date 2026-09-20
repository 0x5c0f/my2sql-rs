//! binlog 解码层：事件头/checksum（event）、错误类型（error）、协议原语（proto）、
//! TABLE_MAP 解码（table_map）；后续 Task 追加 format_desc / rows 等模块。

pub mod error;
pub mod event;
pub mod proto;
