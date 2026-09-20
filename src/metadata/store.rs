//! SchemaStore：表结构存取（online 连库 SHOW 查询 / offline JSON 文件）。
//!
//! 权威对照（my2sql-go，行号引用见 docs/HANDOVER.md Task 11 对照表）：
//! - 懒查缓存：`mysqlFuncs.go:309-320`（GetTableInfoJson：命中 map 直接返回，
//!   miss 才 GetTbDefFromDb）；
//! - 列查询：`mysqlFuncs.go:243-306` 用 `SHOW COLUMNS`（简报写 SHOW FULL
//!   COLUMNS，二者 Field/Type 前两列同序同值，本层按简报用 FULL、按列名取值，
//!   有效行为一致）；无任何生成列/不可见列过滤——上游原样吃服务器输出；
//! - 键查询：`mysqlFuncs.go:119-240`（SHOW INDEX、Non_unique==0、
//!   ContainsString 去重、`:194` 键名小写含 "primary" 即视为主键——已核实
//!   属实，见 parse_keys 测试）；
//! - 空库表名报错：`mysqlFuncs.go:126-130/248-252`；
//! - JSON 文件格式为 controller 裁定 1（上游 ReadTblDefJsonFile/
//!   DumpTblDefToFile 在参考拷贝中仅有字段、无实现，不存在可对照行为）。

// SchemaStore 生产接线在 T14（cli --uri/--schema-file/--schema-dump），
// 骨架期允许死代码，接入后移除。
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::Path;

use mysql::prelude::Queryable;
use mysql::{Conn, Row, Value};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;

use super::schema::{SchemaCol, TableSchema, norm_type};

/// metadata 层错误类型（thiserror，白名单依赖；不扩 binlog::BinlogError，
/// 保持层间边界——align_cols 亦返回本类型，见 schema.rs 签名说明）。
#[derive(Debug, Error)]
pub enum MetaError {
    /// mysql crate 连接/查询错误（P1 不做重试，上游 GetTbDefFromDb 亦直接
    /// Fatalf，context.go CreateDB:561-568）。
    #[error("mysql error: {0}")]
    Db(#[from] mysql::Error),
    /// schema 文件读写 IO 错误。
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// schema 文件 JSON 解析/序列化错误。
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    /// offline 模式下请求了文件中不存在的库表。
    #[error("table schema not found for `{0}` in schema file")]
    NotFound(String),
    /// schema 文件结构非法（如 version 不识别）。
    #[error("bad schema file: {0}")]
    BadFile(String),
    /// 库名或表名为空（mysqlFuncs.go:126-130 "schema/table is empty"）。
    #[error("schema/table is empty")]
    EmptyIdent,
    /// 列数对账 fatal（strict-schema；对照 events.go:87 的无条件 fatal）。
    #[error(
        "column count mismatch for {table}: binlog has {binlog_cols}, \
         current schema has {schema_cols} (strict-schema; DDL in the middle?)"
    )]
    ColCountFatal {
        table: String,
        binlog_cols: usize,
        schema_cols: usize,
    },
}

/// `--schema-dump` 文件格式（controller 裁定 1）：
/// `{ "version": 1, "tables": [TableSchema...] }`，tables 按 `db.table` 键
/// 字典序（BTreeMap 迭代序，保证 dump 输出逐字节稳定）。
#[derive(Debug, Serialize, Deserialize)]
struct DumpFile {
    version: u32,
    tables: Vec<TableSchema>,
}

/// 当前 P1 唯一支持的 schema 文件版本。
const SCHEMA_FILE_VERSION: u32 = 1;

enum Source {
    /// offline：全部表在构造时载入 cache，get 仅查缓存。
    Offline,
    /// online：构造时建连一次，get miss 时懒查并缓存（无重试，裁定 2）。
    Online(Box<Conn>),
}

/// 表结构仓库：`db.table` → `TableSchema` 缓存 + 惰性来源。
pub struct SchemaStore {
    source: Source,
    cache: BTreeMap<String, TableSchema>,
}

impl std::fmt::Debug for SchemaStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SchemaStore")
            .field("cache", &self.cache)
            .field(
                "source",
                &match self.source {
                    Source::Offline => "offline",
                    Source::Online(_) => "online",
                },
            )
            .finish()
    }
}

/// 缓存键：`db.table`（funcs.go:123-125 GetAbsTableName，`.` 分隔）。
fn abs_key(db: &str, tb: &str) -> String {
    format!("{db}.{tb}")
}

impl SchemaStore {
    /// 读 `--schema-file`：一次性载入全部表；重复 `db.table` 后者生效并
    /// `tracing::warn`（裁定 1）。
    pub fn offline(json: &Path) -> Result<Self, MetaError> {
        let text = std::fs::read_to_string(json)?;
        let df: DumpFile = serde_json::from_str(&text)?;
        if df.version != SCHEMA_FILE_VERSION {
            return Err(MetaError::BadFile(format!(
                "unsupported schema file version {} (expected {SCHEMA_FILE_VERSION})",
                df.version
            )));
        }
        let mut cache = BTreeMap::new();
        for t in df.tables {
            let key = abs_key(&t.db, &t.table);
            if cache.insert(key.clone(), t).is_some() {
                warn!("duplicate `{key}` in schema file, last occurrence wins");
            }
        }
        Ok(Self {
            source: Source::Offline,
            cache,
        })
    }

    /// `mysql://user:pass@host:port` URI 建连一次（裁定 2；后续 get 懒查）。
    pub fn online(uri: &str) -> Result<Self, MetaError> {
        Ok(Self {
            source: Source::Online(Box::new(Conn::new(uri)?)),
            cache: BTreeMap::new(),
        })
    }

    /// 取表结构；online 未缓存时懒查 SHOW FULL COLUMNS + SHOW INDEX 并缓存。
    pub fn get(&mut self, db: &str, tb: &str) -> Result<&TableSchema, MetaError> {
        if db.is_empty() || tb.is_empty() {
            return Err(MetaError::EmptyIdent);
        }
        let key = abs_key(db, tb);
        if !self.cache.contains_key(&key) {
            let s = match &mut self.source {
                Source::Offline => return Err(MetaError::NotFound(key)),
                Source::Online(conn) => fetch_online(conn, db, tb)?,
            };
            self.cache.insert(key, s);
        }
        Ok(&self.cache[&abs_key(db, tb)])
    }

    /// 已缓存表全集写 `--schema-dump` JSON（版本包装 + 稳定排序）。
    pub fn dump(&self, json: &Path) -> Result<(), MetaError> {
        let tables = self.cache.values().cloned().collect::<Vec<_>>();
        let text = serde_json::to_string_pretty(&DumpFile {
            version: SCHEMA_FILE_VERSION,
            tables,
        })?;
        std::fs::write(json, text)?;
        Ok(())
    }
}

/// SHOW INDEX 单行的解析输入（Non_unique / Key_name / Column_name(NULL=表达式索引)）。
#[derive(Debug, Clone)]
pub(crate) struct IdxRow {
    pub non_unique: i64,
    pub key_name: String,
    pub col: Option<String>,
}

/// 从 SHOW INDEX 行集提炼 (pk, uks)。
///
/// 权威 mysqlFuncs.go:159-238：
/// - `:172-173` 仅收 Non_unique == 0 的行；
/// - `:184-192` 按 Key_name 分组、组内按输出行序（即 Seq_in_index 序）保序、
///   ContainsString 去重；
/// - `:189` 表达式索引的 NULL Column_name 经 `string(data[4])` 变空串 ""
///   （后续按名匹配必然失配）——本层同样以空串入组，交由 key_indexes 整键降级；
/// - `:194` 键名小写后含 "primary" 即视为主键（已核实，非简报杜撰）；
///   `:221-238` 多个这样的键时 Go 在 map 随机序下「最后一个生效」，其余
///   primary 名键既不保留为 pk 也不入 uks（直接丢失）——本层按确定序复刻
///   「最后生效 + 前者丢弃」。
pub(crate) fn parse_keys(
    rows: impl IntoIterator<Item = IdxRow>,
) -> (Vec<String>, Vec<Vec<String>>) {
    let mut keys: Vec<(String, Vec<String>)> = Vec::new();
    for r in rows {
        if r.non_unique != 0 {
            continue;
        }
        if !keys.iter().any(|(n, _)| *n == r.key_name) {
            keys.push((r.key_name.clone(), Vec::new()));
        }
        let entry = keys.iter_mut().find(|(n, _)| *n == r.key_name).unwrap();
        let col = r.col.unwrap_or_default();
        if !entry.1.contains(&col) {
            entry.1.push(col);
        }
    }
    let mut pk: Vec<String> = Vec::new();
    let mut uks: Vec<Vec<String>> = Vec::new();
    for (name, cols) in keys {
        if name.to_lowercase().contains("primary") {
            pk = cols;
        } else {
            uks.push(cols);
        }
    }
    (pk, uks)
}

/// SHOW FULL COLUMNS 行集 → cols（(Field, Type) 输入已由调用方取列）。
pub(crate) fn parse_columns(rows: impl IntoIterator<Item = (String, String)>) -> Vec<SchemaCol> {
    rows.into_iter()
        .map(|(name, raw_type)| {
            let (type_name, unsigned) = norm_type(&raw_type);
            SchemaCol {
                name,
                type_name,
                unsigned,
            }
        })
        .collect()
}

/// 单元格取字符串（NULL → None；Bytes 走 utf8-lossy，其余按显示值字符串化）。
/// 镜像 Go 的 `string(data[i])`（RawBytes 对 NULL 也是空串——差异：本层
/// 返回 None，调用处对 Column_name 用 unwrap_or_default 归零，行为一致）。
fn cell_str(row: &Row, col: &str) -> Option<String> {
    match row.get::<Value, _>(col)? {
        Value::NULL => None,
        Value::Bytes(b) => Some(String::from_utf8_lossy(&b).into_owned()),
        Value::Int(i) => Some(i.to_string()),
        Value::UInt(u) => Some(u.to_string()),
        // Float/Decimal/Date 等：本层仅 Field/Type/Key_name/Column_name/
        // Non_unique 五格消费，其余形态兜底 Debug 文本。
        other => Some(format!("{other:?}")),
    }
}

/// online 单表懒查：SHOW FULL COLUMNS + SHOW INDEX（backtick 包裹标识符，
/// 同 mysqlFuncs.go:132/254 的 `%s`.`%s` 形态）。
fn fetch_online(conn: &mut Conn, db: &str, tb: &str) -> Result<TableSchema, MetaError> {
    let cols_sql = format!("SHOW FULL COLUMNS FROM `{db}`.`{tb}`");
    let raw_cols: Vec<(String, String)> = conn
        .query_map(cols_sql, |row: Row| {
            (
                cell_str(&row, "Field").unwrap_or_default(),
                cell_str(&row, "Type").unwrap_or_default(),
            )
        })?
        .into_iter()
        .collect();
    let idx_sql = format!("SHOW INDEX FROM `{db}`.`{tb}`");
    let idx_rows: Vec<IdxRow> = conn
        .query_map(idx_sql, |row: Row| IdxRow {
            non_unique: cell_str(&row, "Non_unique")
                .and_then(|s| s.parse().ok())
                .unwrap_or(-1),
            key_name: cell_str(&row, "Key_name").unwrap_or_default(),
            col: cell_str(&row, "Column_name"),
        })?
        .into_iter()
        .collect();
    let (pk, uks) = parse_keys(idx_rows);
    Ok(TableSchema {
        db: db.to_string(),
        table: tb.to_string(),
        cols: parse_columns(raw_cols),
        pk,
        uks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::schema::Align;
    use std::path::PathBuf;

    fn sample_json(tables: &str) -> String {
        format!(r#"{{"version":1,"tables":{tables}}}"#)
    }

    fn tmp_path(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let mut p = std::env::temp_dir();
        p.push(format!("my2sql-t11-{tag}-{}.{nanos}", std::process::id()));
        p.set_extension("json");
        p
    }

    #[test]
    fn offline_dump_roundtrip() {
        let src = tmp_path("rt-in");
        let dst = tmp_path("rt-out");
        std::fs::write(
            &src,
            sample_json(
                r#"[
  {"db":"d1","table":"b_tab","cols":[{"name":"x","type_name":"int","unsigned":true}],"pk":["x"],"uks":[]},
  {"db":"d1","table":"a_tab","cols":[
     {"name":"id","type_name":"bigint","unsigned":true},
     {"name":"nm","type_name":"varchar","unsigned":false}],
   "pk":["id"],"uks":[["nm"],["id","nm"]]}
]"#,
            ),
        )
        .unwrap();
        let store = SchemaStore::offline(&src).unwrap();
        assert_eq!(store.cache.len(), 2);
        store.dump(&dst).unwrap();
        let store2 = SchemaStore::offline(&dst).unwrap();
        assert_eq!(store.cache, store2.cache);
        // dump 稳定序：BTreeMap 键字典序（d1.a_tab 在前）+ 版本包装
        let text = std::fs::read_to_string(&dst).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(v["tables"][0]["table"], "a_tab");
        assert_eq!(v["tables"][1]["table"], "b_tab");
        std::fs::remove_file(&src).ok();
        std::fs::remove_file(&dst).ok();
    }

    #[test]
    fn offline_duplicate_last_wins() {
        let src = tmp_path("dup");
        std::fs::write(
            &src,
            sample_json(
                r#"[
  {"db":"d","table":"t","cols":[{"name":"old","type_name":"int","unsigned":false}],"pk":[],"uks":[]},
  {"db":"d","table":"t","cols":[{"name":"new","type_name":"text","unsigned":false}],"pk":["new"],"uks":[]}
]"#,
            ),
        )
        .unwrap();
        let mut store = SchemaStore::offline(&src).unwrap();
        let s = store.get("d", "t").unwrap();
        assert_eq!(s.cols[0].name, "new");
        assert_eq!(s.pk, vec!["new".to_string()]);
        std::fs::remove_file(&src).ok();
    }

    #[test]
    fn offline_rejects_unknown_version() {
        let src = tmp_path("ver");
        std::fs::write(
            &src,
            r#"{"version":2,"tables":[{"db":"d","table":"t","cols":[],"pk":[],"uks":[]}]}"#,
        )
        .unwrap();
        let e = SchemaStore::offline(&src).unwrap_err();
        assert!(matches!(e, MetaError::BadFile(_)), "{e}");
        std::fs::remove_file(&src).ok();
    }

    #[test]
    fn offline_missing_table_not_found() {
        let src = tmp_path("miss");
        std::fs::write(&src, sample_json("[]")).unwrap();
        let mut store = SchemaStore::offline(&src).unwrap();
        let e = store.get("d", "nope").unwrap_err();
        assert!(matches!(e, MetaError::NotFound(_)), "{e}");
        // 空库/表名 → EmptyIdent（mysqlFuncs.go:126-130）
        assert!(matches!(
            store.get("", "t").unwrap_err(),
            MetaError::EmptyIdent
        ));
        std::fs::remove_file(&src).ok();
    }

    fn ir(non_unique: i64, key_name: &str, col: Option<&str>) -> IdxRow {
        IdxRow {
            non_unique,
            key_name: key_name.into(),
            col: col.map(Into::into),
        }
    }

    #[test]
    fn parse_keys_primary_and_unique() {
        let (pk, uks) = parse_keys(vec![
            ir(0, "PRIMARY", Some("id")),
            ir(0, "uq_mail", Some("mail")),
            ir(1, "i_name", Some("name")), // 非唯一 → 忽略（:172-173）
        ]);
        assert_eq!(pk, vec!["id".to_string()]);
        assert_eq!(uks, vec![vec!["mail".to_string()]]);
    }

    #[test]
    fn parse_keys_multi_column_ordered_and_dedup() {
        // Seq_in_index 保序（:184-192）；同名重复列去重（ContainsString :190）
        let (pk, uks) = parse_keys(vec![
            ir(0, "PRIMARY", Some("b")),
            ir(0, "PRIMARY", Some("a")),
            ir(0, "uq", Some("x")),
            ir(0, "uq", Some("x")),
            ir(0, "uq", Some("y")),
        ]);
        assert_eq!(pk, vec!["b".to_string(), "a".to_string()]);
        assert_eq!(uks, vec![vec!["x".to_string(), "y".to_string()]]);
    }

    #[test]
    fn parse_keys_name_containing_primary_is_pk() {
        // mysqlFuncs.go:194 实锤：名字小写含 "primary" 的唯一键被当主键
        let (pk, uks) = parse_keys(vec![
            ir(0, "fake_primary_idx", Some("x")),
            ir(0, "uq_other", Some("y")),
        ]);
        assert_eq!(pk, vec!["x".to_string()]);
        assert_eq!(uks, vec![vec!["y".to_string()]]);
        // 多个 primary 名键：最后生效、前者丢弃（Go map 随机序下的有效行为，:221-238）
        let (pk, uks) = parse_keys(vec![
            ir(0, "primary_like", Some("a")),
            ir(0, "the_primary", Some("b")),
        ]);
        assert_eq!(pk, vec!["b".to_string()]);
        assert!(uks.is_empty());
    }

    #[test]
    fn parse_keys_expression_index_null_column_becomes_empty_name() {
        // 表达式索引 Column_name NULL → Go string(data[4])=""（:189）；
        // 该 uk 带着空名进列表，key_indexes 侧整键降级
        let (pk, uks) = parse_keys(vec![ir(0, "PRIMARY", Some("id")), ir(0, "uq_expr", None)]);
        assert_eq!(pk, vec!["id".to_string()]);
        assert_eq!(uks, vec![vec!["".to_string()]]);
        let s = TableSchema {
            db: "d".into(),
            table: "t".into(),
            cols: vec![SchemaCol {
                name: "id".into(),
                type_name: "int".into(),
                unsigned: false,
            }],
            pk: pk.clone(),
            uks,
        };
        let tm = crate::binlog::table_map::TableMapEvent {
            table_id: 1,
            schema: "d".into(),
            table: "t".into(),
            n_cols: 1,
            column_type: vec![],
            column_meta: vec![],
            null_bits: vec![],
            charset: vec![],
        };
        let (pk_idx, uk_idx) = crate::metadata::schema::key_indexes(&s, &tm);
        assert_eq!(pk_idx, vec![0]);
        assert!(uk_idx.is_empty());
    }

    #[test]
    fn parse_columns_maps_type_normalization() {
        let cols = parse_columns(vec![
            ("a".into(), "int(11)".into()),
            ("b".into(), "decimal(10,2) unsigned".into()),
            ("c".into(), "text".into()),
        ]);
        assert_eq!(cols[0].type_name, "int");
        assert!(!cols[0].unsigned);
        assert_eq!(cols[1].type_name, "decimal");
        assert!(cols[1].unsigned);
        assert_eq!(cols[2].type_name, "text");
    }

    // 兜底：Align 在 store 侧可达（T13 消费路径冒烟）
    #[test]
    fn align_import_reachable() {
        let s = TableSchema {
            db: "d".into(),
            table: "t".into(),
            cols: vec![],
            pk: vec![],
            uks: vec![],
        };
        assert_eq!(
            crate::metadata::schema::align_cols(0, &s, false).unwrap(),
            Align::Ok
        );
    }

    /// 真库在线路径证明（裁定 3）：连 `MYSQL_TEST_URI` 指定实例，
    /// 自建自删测试库表，验证 SHOW FULL COLUMNS / SHOW INDEX 解析、
    /// 「primary」子串判定、dump→offline 闭环。docker 常备 harness 在 T15，
    /// 本测试对任何可达实例可跑（生成列 5.7+、不可见列 8.0.23+，
    /// ALTER 失败即降级跳过对应断言）。
    #[test]
    #[ignore = "requires a reachable MySQL server via MYSQL_TEST_URI"]
    fn online_store_live() {
        let uri = std::env::var("MYSQL_TEST_URI").expect("MYSQL_TEST_URI must be set");
        let mut admin = Conn::new(uri.as_str()).unwrap();
        let version: String = admin.query_first("SELECT VERSION()").unwrap().unwrap();
        println!("server version: {version}");

        admin
            .query_drop("DROP DATABASE IF EXISTS my2sql_t11")
            .unwrap();
        admin.query_drop("CREATE DATABASE my2sql_t11").unwrap();
        admin
            .query_drop(
                r#"CREATE TABLE my2sql_t11.t1 (
  id bigint unsigned NOT NULL AUTO_INCREMENT,
  a int NOT NULL,
  b varchar(50) NOT NULL,
  amt decimal(10,2) unsigned NOT NULL,
  note tinytext,
  email varchar(64),
  c1 int NOT NULL,
  c2 int NOT NULL,
  PRIMARY KEY (id),
  UNIQUE KEY uq_mail (email),
  UNIQUE KEY uq_c (c1, c2),
  KEY k_a (a)
)"#,
            )
            .unwrap();
        // 生成/不可见列在 SHOW COLUMNS 中出现与否 = 服务器口径，本层零过滤；
        // 低版本 ALTER 报错即跳过（query_drop 失败不污染库）。
        let has_gen = admin
            .query_drop("ALTER TABLE my2sql_t11.t1 ADD COLUMN gen_col int GENERATED ALWAYS AS (a + 1) STORED")
            .is_ok();
        let has_invis = admin
            .query_drop("ALTER TABLE my2sql_t11.t1 ADD COLUMN hid int INVISIBLE DEFAULT 3")
            .is_ok();
        println!("generated col added: {has_gen}, invisible col added: {has_invis}");
        // 「primary」子串判定实测：无真主键、唯一键名含 primary
        admin
            .query_drop(
                r#"CREATE TABLE my2sql_t11.odd (
  x int NOT NULL,
  y int,
  UNIQUE KEY fake_primary_idx (x),
  KEY k_y (y)
)"#,
            )
            .unwrap();

        let mut store = SchemaStore::online(&uri).unwrap();
        let s = store.get("my2sql_t11", "t1").unwrap().clone();
        let find = |name: &str| s.cols.iter().find(|c| c.name == name).cloned();
        let id = find("id").expect("id col");
        assert_eq!(id.type_name, "bigint");
        assert!(id.unsigned);
        let a = find("a").unwrap();
        assert_eq!((a.type_name.as_str(), a.unsigned), ("int", false));
        let amt = find("amt").unwrap();
        assert_eq!((amt.type_name.as_str(), amt.unsigned), ("decimal", true));
        assert_eq!(find("note").unwrap().type_name, "tinytext");
        // 生成列上游不过滤（SHOW COLUMNS 给什么吃什么）——本侧同口径；
        // 低版本未建成生成列时由「与服务器自报列面一致性」断言兜底。
        assert_eq!(find("gen_col").is_some(), has_gen);
        assert_eq!(find("hid").is_some(), has_invis);
        // 键：PRIMARY=[id]，uks=[uq_c?, uq_mail?]（SHOW INDEX 按键名分组序）
        assert_eq!(s.pk, vec!["id".to_string()]);
        assert_eq!(s.uks.len(), 2);
        assert!(s.uks.iter().any(|k| k == &vec!["email".to_string()]));
        assert!(
            s.uks
                .iter()
                .any(|k| k == &vec!["c1".to_string(), "c2".to_string()])
        );
        // 缓存命中：二次 get 不再发查询（同指针即证）
        let p1 = store.get("my2sql_t11", "t1").unwrap() as *const TableSchema;
        let p2 = store.get("my2sql_t11", "t1").unwrap() as *const TableSchema;
        assert_eq!(p1, p2);

        let odd = store.get("my2sql_t11", "odd").unwrap().clone();
        assert_eq!(
            odd.pk,
            vec!["x".to_string()],
            "index name containing 'primary' must be treated as PK (mysqlFuncs.go:194)"
        );
        assert!(odd.uks.is_empty(), "non-unique KEY must be ignored");

        // 与服务器自报列面一致性（证明本层零过滤）：SHOW COLUMNS 的 Field 序
        let server_cols: Vec<String> = admin
            .query_map("SHOW COLUMNS FROM my2sql_t11.t1", |row: Row| {
                cell_str(&row, "Field").unwrap_or_default()
            })
            .unwrap()
            .into_iter()
            .collect();
        let mine: Vec<&str> = s.cols.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            mine,
            server_cols.iter().map(String::as_str).collect::<Vec<_>>()
        );

        // dump → offline 闭环（online 拉取的真结构过一遍 JSON）
        let dst = tmp_path("live");
        store.dump(&dst).unwrap();
        let mut back = SchemaStore::offline(&dst).unwrap();
        assert_eq!(back.get("my2sql_t11", "t1").unwrap(), &s);
        std::fs::remove_file(&dst).ok();

        admin.query_drop("DROP DATABASE my2sql_t11").unwrap();
    }
}
