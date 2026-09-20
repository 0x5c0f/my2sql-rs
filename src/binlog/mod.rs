//! binlog 解码层：事件头/checksum（event）、错误类型（error）、协议原语（proto）、
//! TABLE_MAP 解码（table_map）、定宽值解码 ColumnValue（int）、时间族字符串保真
//! 解码（time）、精确 DECIMAL（decimal）、JSON 二进制→紧凑文本（json）；
//! 后续 Task 追加 format_desc / rows 等模块。

pub mod decimal;
pub mod error;
pub mod event;
pub mod int;
pub mod json;
pub mod proto;
pub mod table_map;
pub mod time;
