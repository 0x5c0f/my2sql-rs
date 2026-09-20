//! binlog 解码层：事件头/checksum（event）、错误类型（error）；后续 Task 追加
//! format_desc / table_map / rows 等模块。

pub mod error;
pub mod event;
