//! binlog 解码层：事件头/checksum（event）、错误类型（error）、协议原语（proto）、
//! TABLE_MAP 解码（table_map）、定宽值解码 ColumnValue（int）、时间族字符串保真
//! 解码（time）、精确 DECIMAL（decimal）、JSON 二进制→紧凑文本（json）、
//! 值分发（value，Task 9）、rows 行解码占位（Task 10 填充）；
//! 后续 Task 追加 format_desc 等模块。

pub mod decimal;
pub mod error;
pub mod event;
/// MYSQL_TYPE_* 唯一常量表（T10 Step 0 合并三处私有 `mod tp`，仅层内使用）。
pub(crate) mod field_types;
pub mod file_reader;
pub mod int;
pub mod json;
pub mod proto;
pub mod rows;
pub mod table_map;
pub mod time;
pub mod value;
