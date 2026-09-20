//! 表结构列元数据（本任务仅定义结构，存取逻辑见 Task 11）。
//!
//! 字段口径对照 my2sql-go `base/mysqlFuncs.go:298`（SHOW COLUMNS 的 Type 列经
//! `GetFiledType`（base/funcs.go:86-92）按 `(` 分割取首段得 `type_name`（小写、
//! 无括号/无 unsigned 后缀），`IsUnsigned`（funcs.go:94-96）按含 "unsigned"
//! 判 `unsigned`）。

// TableSchema/SchemaCol 的生产消费者在 Task 10/11（decode_value 仅借用
// SchemaCol），骨架阶段允许死代码，T11 接入后移除。
#![allow(dead_code)]

/// 单列 schema 元数据（来自 SHOW COLUMNS / information_schema）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaCol {
    pub name: String,
    /// 小写、无括号、无 unsigned 后缀的类型名（如 `int`、`tinytext`、`enum`）。
    pub type_name: String,
    pub unsigned: bool,
}

/// 表结构（库名 + 表名 + 列 + 键），Task 11 实现 SchemaStore 存取。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableSchema {
    pub db: String,
    pub table: String,
    pub cols: Vec<SchemaCol>,
    pub pk: Vec<String>,
    pub uks: Vec<Vec<String>>,
}
