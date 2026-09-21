//! my2sql-rs：MySQL binlog 解析 / 还原 SQL 工具（库层根，供 `main` 与集成测试共用）。
//!
//! 层序（spec §2）：`binlog`（协议解码）→ `metadata`（表结构）→
//! `pipeline`（源/过滤/事务机/装配）→ `sqlopen`（SQL 文本）→ `output`（写盘）。

pub mod binlog;
pub mod config;
pub mod flashback;
pub mod metadata;
pub mod output;
pub mod pipeline;
pub mod sqlopen;
