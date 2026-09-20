//! sqlopen：SQL 值编码 + DML 语句构建（Task 13）。
//!
//! 权威对照：
//!
//! - my2sql-go `base/sqlgen.go` 三族函数（简报所称
//!   `GenerateInsertSql/GenerateUpdateSql/GenerateDeleteSql` 在参考实现中不存
//!   在——实际为 `GenInsertSqlsForOneRowsEvent`(:137)/
//!   `GenDeleteSqlsForOneRowsEvent`(:237)/`GenUpdateSqlsForOneRowsEvent`(:288)，
//!   简报命名速记失真第 7 例，按权威语义实现，详见报告）；
//! - `sqltypes/sqltypes.go` 字面量编码（`SqlEncodeMap`/`encodeRef` :604-621、
//!   `String.encodeSql` :548-566、null 字面量 :27）；
//! - `sqlbuilder/expression.go` `Eq`(:441-447)：NULL 右值渲染为 `col IS null`
//!   而非 `col = null`——本层镜像为反引号列名 + ` IS NULL`。

pub mod dml;
pub mod encode;

use crate::binlog::error::BinlogError;
use crate::metadata::store::MetaError;

/// 单事件级错误：strict 模式列数对账 fatal（MetaError 透传）或值位置不可编码
/// （Missing 等，BinlogError 透传）。T14 按事件粒度捕获、计错、继续。
#[derive(Debug, thiserror::Error)]
pub enum SqlError {
    #[error(transparent)]
    Meta(#[from] MetaError),
    #[error(transparent)]
    Value(#[from] BinlogError),
}
