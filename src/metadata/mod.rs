//! metadata 层（表结构/键信息获取与缓存）。
//!
//! - `schema`：本阶段（Task 9）仅定义 `SchemaCol` / `TableSchema` 结构体；
//!   SchemaStore 离线/在线存取、列数对账、索引解析在 Task 11 实现。

pub mod schema;
