//! metadata 层（表结构/键信息获取与缓存）。
//!
//! - `schema`：`SchemaCol`/`TableSchema` 结构（T9）+ 列数对账 `align_cols`
//!   + 键名→binlog 序号映射 `key_indexes`（T11）；
//! - `store`：`SchemaStore`（online 连库懒查 + offline JSON 文件）与
//!   `MetaError`（T11）。

pub mod schema;
pub mod store;
