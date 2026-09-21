//! DML 语句文本构建：INSERT / UPDATE / DELETE。
//!
//! 权威对照 my2sql-go `base/sqlgen.go`（简报名 `GenerateInsertSql/
//! GenerateUpdateSql/GenerateDeleteSql` 不存在，实为
//! `GenInsertSqlsForOneRowsEvent`/`GenDeleteSqlsForOneRowsEvent`/
//! `GenUpdateSqlsForOneRowsEvent`——速记失真续例）与调用面 `base/events.go:119-163`。
//!
//! 语义镜像清单：
//! - **列清单**：全列 = schema 名可用前缀 `min(schema宽, binlog宽)`。binlog 宽出
//!   的 dropped 补位列（`Align::Padded`，`dropped_column_i`）**不**进列清单/
//!   WHERE（裁定 2：合成名不是真列，SQL 引用必错；其 binlog 序号保留仅作位置
//!   映射）并 warn 一次/事件。上游该分支 `events.go:87` 恒 Fatalf、补位从不
//!   出 SQL（T11 对照表已录），本层非 strict 出「可用子集」SQL，strict=true
//!   经 `align_cols` 复现 fatal（`SqlOpts::strict_schema`，裁定 2 定稿默认
//!   false=非 strict，与上游「有效出货行为」等价——上游 fatal 前只出过等宽
//!   场景的 SQL）。
//! - **WHERE 键选择**：镜像 `GetOneUniqueKey`（mysqlFuncs.go:322-335）+
//!   `GenEqualConditions`（sqlgen.go:269-282）：unique_first 且 uk 非空 →
//!   uk[0]；否则 pk；否则 uk[0]；皆空或 `full_columns=true` → 全列。
//!   无键全列 WHERE 的 NULL 值渲染 `col IS NULL`——镜像上游
//!   `sqlbuilder.Eq`（expression.go:441-447）对 NULL 右值改换 `IS` 算子
//!   （渲染面 `col IS null`，sqltypes.go:27 nullstr；大小写无语义）。
//!   **绝不**产出 `col = null`（恒假，行不可定位）。
//! - **UPDATE SET 差异**（GenUpdateSetPart sqlgen.go:336-381）：非 full 时仅
//!   「变化列」入 SET；上游比较 = 字节族 []byte 原字节比较、其余 Go `==`
//!   （解码值等值比较）。本层用 `ColumnValue` derive 的 PartialEq（int.rs:30，
//!   T10 已派生——裁定 6 核实无需新增）：Str/Bytes 按字节、Double/Decimal/
//!   Json 按预渲染文本严格比较。因 T5-T8 文本为确定性渲染，与上游「比较解码
//!   后值再原样输出」效果等价。
//! - **ignore_pk_for_insert**（sqlgen.go:159-164 + :204-217，裁定 7）：列清单
//!   与 VALUES **双位置**剔除 pk 列（灌回场景让目标库自增）；pk 为空时自动
//!   失效（:159-161）。仅作用于 INSERT——`GenUpdateSqlsForOneRowsEvent`
//!   （:288）与 DELETE 均不接收该参数，UPDATE 的 SET/WHERE 不受影响（已核实）。
//! - **批量 INSERT**：`insert_batch=Some(n)` 按 n 行切分多语句
//!   （sqlgen.go:165-187 的 rowsPerSql 循环；上游调用面 events.go:150 恒传 1，
//!   即默认一行一语句，`None`→1）。
//! - **Missing 值**（裁定 3）：唯一来源是 8.0.1 partial rows，T10 decode_rows
//!   已在解码层拒收——值位置命中即 InvalidData 硬错误（防御性，非出货路径）。
//!
//! **P2 反转口径**（`WorkKind::Flashback`，上游对照 `base/sqlgen.go` 的
//! `ifRollback` 形参：:137/:237/:288 与包装 :233/:284、调用面
//! `base/events.go` 的 rollback 分支）：
//! - `dml_for` 为事件→SQL 唯一分派入口：Flashback 下 Write↔Delete 互换
//!   （INSERT 事件出 DELETE、DELETE 事件出 INSERT），UPDATE 复用 `updates()`
//!   签名、镜像取位 SET=before / WHERE=after（正向 ToSql 与 P1 逐字节一致）。
//! - 硬规则 a（宁缺毋漏）：`Align::Padded`（dropped 列 → 旧镜像不可靠）在
//!   Flashback 下从 warn 升级为逐事件 InvalidData——对齐上游
//!   `events.go:87` fail-hard；正向分支不变。
//! - 硬规则 b：任意可引用列 `Missing`（MINIMAL row image/partial）在
//!   Flashback 下报 InvalidData 并提示 `binlog_row_image=FULL`——`cell()`
//!   闸门 + `deletes()` 整行预检（其 WHERE 仅触键列，须显式扫全行）。
//! - **不继承**上游「JSON 列恒进 SET」quirk（spec §3.1；上游 GenUpdateSetPart
//!   对 decoded JSON 的 []byte 断言失败 → 未变化 JSON 也入 SET，正反向着
//!   皆然）：本侧正/逆向均按实际 diff 省略未变化 JSON 列，差分白名单
//!   ALW-JSON-IN-SET 容忍裁判多出的 JSON 项。

use crate::binlog::error::BinlogError;
use crate::binlog::int::ColumnValue;
use crate::binlog::rows::{Row, RowsKind};
use crate::binlog::table_map::TableMapEvent;
use crate::config::Config;
use crate::metadata::schema::{Align, TableSchema, align_cols, key_indexes};

use super::SqlError;
use super::encode::{encode_value, quote_ident};

/// SQL 生成选项。**bind 口径**：镜像 `config.rs`（T1 实际暴露）重设计 flags，
/// 全部有 CLI 对应（无「字段有 flag 无」遗留）：
/// `db_prefix`←`--no-db-prefix` 取反、`full_columns`←`--full-columns`、
/// `insert_batch`←`--insert-batch`、`ignore_pk_for_insert`←
/// `--ignore-primary-key-for-insert`、`unique_first`←`--unique-key-first`、
/// `strict_schema`←`--strict-schema`（裁定 2 新增于 SqlOpts，T13 决策点已定稿
/// default false，见 HANDOVER）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlOpts {
    /// true = `` `db`.`tbl` ``；false = 裸表名（上游 SqlTblPrefixDb，events.go:225-227）。
    pub db_prefix: bool,
    /// UPDATE SET 出全列、UPDATE/DELETE WHERE 出全列（上游 --full-columns）。
    pub full_columns: bool,
    /// 批量 INSERT 每条语句最大行数；None/Some(0) → 1（上游恒 1）。
    pub insert_batch: Option<usize>,
    /// INSERT 剔除 pk 列（列清单+值双位置）；仅 INSERT 生效。
    pub ignore_pk_for_insert: bool,
    /// WHERE 键优先 uk[0]（上游 -U / GetOneUniqueKey(uniqueFirst)）。
    pub unique_first: bool,
    /// true：schema/binlog 列数不匹配经 align_cols 升为逐事件 ColCountFatal。
    pub strict_schema: bool,
}

impl Default for SqlOpts {
    fn default() -> Self {
        Self {
            db_prefix: true,
            full_columns: false,
            insert_batch: None,
            ignore_pk_for_insert: false,
            unique_first: false,
            strict_schema: false,
        }
    }
}

impl SqlOpts {
    /// T14 接线用：CLI `Config` → `SqlOpts`（裁定 4/5：字段与 flag 一一对应）。
    pub fn from_config(c: &Config) -> Self {
        Self {
            db_prefix: !c.no_db_prefix,
            full_columns: c.full_columns,
            insert_batch: c.insert_batch,
            ignore_pk_for_insert: c.ignore_primary_key_for_insert,
            unique_first: c.unique_key_first,
            strict_schema: c.strict_schema,
        }
    }
}

/// 工作模式：正向（P1）或回滚反转（P2）。上游对照 `-work-type`
/// （`base/context.go:186`；rollback 分派 `base/events.go:62`、出货面
/// `base/rollback_process.go`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WorkKind {
    #[default]
    ToSql,
    Flashback,
}

/// 无状态构建器（opts 之外零共享；`..,` 简写在简报接口块的含义 = 与
/// inserts/deletes 一致的三参数 tm/schema/rows，内部状态仅 opts + 每事件
/// 现算的 align/键计划，不跨事件缓存——文档化于报告）。
#[derive(Debug, Clone, Default)]
pub struct DmlBuilder {
    opts: SqlOpts,
    kind: WorkKind,
}

/// 每事件计算结果：可入 SQL 的列序号 + 键序号 + WHERE 序号 + 表名。
struct Plan<'a> {
    tbl: String,
    schema: &'a TableSchema,
    /// 可引用列序号（schema 有名 ∩ binlog 在场），dropped 位已剔除（裁定 2）。
    cols: Vec<usize>,
    pk: Vec<usize>,
    where_idx: Vec<usize>,
}

impl DmlBuilder {
    /// 正向构建器（P1 行为，零改动）。
    pub fn new(opts: SqlOpts) -> Self {
        Self {
            opts,
            kind: WorkKind::ToSql,
        }
    }
    /// P2 回滚构建器：语义反转（INSERT↔DELETE、UPDATE 的 SET/WHERE 镜像）。
    pub fn flashback(opts: SqlOpts) -> Self {
        Self {
            opts,
            kind: WorkKind::Flashback,
        }
    }
    pub fn kind(&self) -> WorkKind {
        self.kind
    }
    /// 事件种类 → 逆向/正向 SQL 的统一分派（T3 worker 唯一入口）。
    pub fn dml_for(
        &self,
        kind: RowsKind,
        tm: &TableMapEvent,
        s: &TableSchema,
        rows: &[Row],
    ) -> Result<Vec<String>, SqlError> {
        use RowsKind::*;
        match (kind, self.kind) {
            (Write, WorkKind::ToSql) | (Delete, WorkKind::Flashback) => self.inserts(tm, s, rows),
            (Delete, WorkKind::ToSql) | (Write, WorkKind::Flashback) => self.deletes(tm, s, rows),
            (Update, _) => self.updates(tm, s, rows),
        }
    }

    pub fn opts(&self) -> &SqlOpts {
        &self.opts
    }

    /// 事件级准备：对账（strict 透传 fatal / 非 strict Padded 告警）、键计划。
    fn plan<'a>(&self, tm: &TableMapEvent, s: &'a TableSchema) -> Result<Plan<'a>, SqlError> {
        // 裁定 2：strict=true 时任何宽度失配经 align_cols 升为 ColCountFatal；
        // 非 strict：Padded（dropped 位）跳过 + warn，Truncated 静默取前缀
        // （events.go:83 口径）。两形态下可入 SQL 的列集合相同：
        // 序号 < min(schema宽, binlog宽)。
        let align = align_cols(tm.n_cols, s, self.opts.strict_schema)?;
        if let Align::Padded { dropped } = &align {
            // 硬规则 a（P2）：Flashback 下 dropped 列 = 旧镜像不完整，回滚
            // 宁缺毋漏 → 错误而非告警（对齐上游 events.go:87 fail-hard）。
            if self.kind == WorkKind::Flashback {
                return Err(BinlogError::InvalidData(format!(
                    "flashback: dropped columns in `{}`.`{}` make old row images \
                     unreliable — refusing to emit partial rollback",
                    s.db, s.table
                ))
                .into());
            }
            tracing::warn!(
                table = %format!("`{}`.`{}`", s.db, s.table),
                dropped = ?dropped,
                "dropped columns present in binlog rows are omitted from generated SQL \
                 (values unreliable / names synthetic); use --strict-schema to hard-fail"
            );
        }
        let named = s.cols.len().min(tm.n_cols);
        let cols: Vec<usize> = (0..named).collect();
        if cols.is_empty() {
            return Err(BinlogError::InvalidData(format!(
                "no schema-named columns within binlog row width for `{}`.`{}`",
                tm.schema, tm.table
            ))
            .into());
        }
        let (pk, uks) = key_indexes(s, tm);
        // WHERE 键选择镜像 GetOneUniqueKey（mysqlFuncs.go:322-335）；
        // full_columns = 上游 ifFullImage=true → 恒全列（sqlgen.go:270 短路）。
        let where_idx = if self.opts.full_columns {
            cols.clone()
        } else if self.opts.unique_first && !uks.is_empty() {
            uks[0].clone()
        } else if !pk.is_empty() {
            pk.clone()
        } else if !uks.is_empty() {
            uks[0].clone()
        } else {
            cols.clone() // 无键：全列 WHERE（NULL 位走 IS NULL）
        };
        let tbl = if self.opts.db_prefix {
            format!("{}.{}", quote_ident(&tm.schema), quote_ident(&tm.table))
        } else {
            quote_ident(&tm.table)
        };
        Ok(Plan {
            tbl,
            schema: s,
            cols,
            pk,
            where_idx,
        })
    }

    /// 行值取位：decode_rows 不变式 `row.cols.len() == tm.n_cols`（T10），
    /// 违背视为调用方 bug → 硬错误；Missing 正向不在此拦截、由 encode_value
    /// 判定；Flashback 下 Missing 命中硬规则 b（见下）。
    fn cell<'r>(
        &self,
        row: &'r Row,
        tm: &TableMapEvent,
        ord: usize,
    ) -> Result<&'r ColumnValue, SqlError> {
        if row.cols.len() != tm.n_cols {
            return Err(BinlogError::InvalidData(format!(
                "row has {} columns but table_map declares {}",
                row.cols.len(),
                tm.n_cols
            ))
            .into());
        }
        let v = row.cols.get(ord).ok_or_else(|| {
            BinlogError::InvalidData(format!("column ordinal {ord} out of range"))
        })?;
        // 硬规则 b（P2）：Flashback 下 Missing（MINIMAL row image / partial）
        // 进 WHERE/VALUES 必产坏回滚 SQL → 报错并提示需 FULL 镜像。
        if self.kind == WorkKind::Flashback && matches!(v, ColumnValue::Missing) {
            return Err(BinlogError::InvalidData(format!(
                "flashback: column ordinal {ord} missing (MINIMAL row image); \
                 requires binlog_row_image=FULL"
            ))
            .into());
        }
        Ok(v)
    }

    /// WHERE 单项：NULL → `col IS NULL`（镜像 expression.go:441-447），否则 `col=lit`。
    fn cond(&self, p: &Plan, ord: usize, v: &ColumnValue) -> Result<String, SqlError> {
        let ident = quote_ident(&p.schema.cols[ord].name);
        if matches!(v, ColumnValue::Null) {
            // 权威：Eq 对 NULL 右值改用 ` IS ` 算子（sqlbuilder 输出 `col IS null`），
            // 绝不 `col = null`（恒假、行不可定位）。本层大写 NULL。
            Ok(format!("{ident} IS NULL"))
        } else {
            Ok(format!("{ident}={}", encode_value(v)?))
        }
    }

    /// WHERE 片段：`where_idx` 各列取 **before 镜像**（updates/deletes 语义）
    /// 的值，` AND ` 连接（镜像 sqlbuilder `And` 的结合子渲染，
    /// expression.go `conjunctExpression`；上游产物 `a=1 AND b='x'`）。
    fn where_part(&self, p: &Plan, row: &Row, tm: &TableMapEvent) -> Result<String, SqlError> {
        let mut conds = Vec::with_capacity(p.where_idx.len());
        for &ord in &p.where_idx {
            let v = self.cell(row, tm, ord)?;
            conds.push(self.cond(p, ord, v)?);
        }
        Ok(conds.join(" AND "))
    }

    /// INSERT（可批量）。rows = 单镜像行序（WRITE 事件 / 回滚 DELETE 复原）。
    pub fn inserts(
        &self,
        tm: &TableMapEvent,
        s: &TableSchema,
        rows: &[Row],
    ) -> Result<Vec<String>, SqlError> {
        let p = self.plan(tm, s)?;
        // 裁定 7（sqlgen.go:159-164 + :204-217 核实）：ignore_pk 同时剔除
        // 列清单与 VALUES 序号位；pk 为空自动失效；仅 INSERT 消费该开关。
        let skip_pk = self.opts.ignore_pk_for_insert && !p.pk.is_empty();
        let col_ords: Vec<usize> = if skip_pk {
            p.cols
                .iter()
                .copied()
                .filter(|i| !p.pk.contains(i))
                .collect()
        } else {
            p.cols.clone()
        };
        if col_ords.is_empty() {
            return Err(BinlogError::InvalidData(
                "insert: every column is a pk column (ignore_pk_for_insert with pk-only table)"
                    .into(),
            )
            .into());
        }
        let collist = col_ords
            .iter()
            .map(|&i| quote_ident(&p.schema.cols[i].name))
            .collect::<Vec<_>>()
            .join(",");
        // 批量 = 上游 rowsPerSql 循环（sqlgen.go:165-187）；上游调用面恒 1。
        let batch = self.opts.insert_batch.unwrap_or(1).max(1);
        let mut out = Vec::with_capacity(rows.len().div_ceil(batch));
        for chunk in rows.chunks(batch) {
            let mut tuples = Vec::with_capacity(chunk.len());
            for r in chunk {
                let mut vals = Vec::with_capacity(col_ords.len());
                for &ord in &col_ords {
                    vals.push(encode_value(self.cell(r, tm, ord)?)?);
                }
                tuples.push(format!("({})", vals.join(",")));
            }
            out.push(format!(
                "INSERT INTO {} ({}) VALUES {};",
                p.tbl,
                collist,
                tuples.join(",")
            ));
        }
        Ok(out)
    }

    /// DELETE：一行一语句（上游 sqlgen.go:254-265 无批量路径）。
    pub fn deletes(
        &self,
        tm: &TableMapEvent,
        s: &TableSchema,
        rows: &[Row],
    ) -> Result<Vec<String>, SqlError> {
        let p = self.plan(tm, s)?;
        // 硬规则 b 补全（P2，简报 Step 6 测试钉死）：Flashback 下 WHERE 仅触
        // 键列，但任意可引用列 Missing 即证行镜像不完整（MINIMAL/partial）→
        // 整行预检，宁缺毋漏。inserts/updates 天然逐列过 cell()，无需此扫描。
        if self.kind == WorkKind::Flashback {
            for r in rows {
                for &ord in &p.cols {
                    self.cell(r, tm, ord)?;
                }
            }
        }
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(format!(
                "DELETE FROM {} WHERE {};",
                p.tbl,
                self.where_part(&p, r, tm)?
            ));
        }
        Ok(out)
    }

    /// UPDATE：rows 为 decode_rows 的交错对 `[before,after,…]`；一行对一语句。
    /// 正向：SET = 变化列（full_columns 时全列）取 after、WHERE = before 镜像键值。
    /// Flashback：镜像反转 —— SET 取 before、WHERE 取 after（签名不变）。
    pub fn updates(
        &self,
        tm: &TableMapEvent,
        s: &TableSchema,
        rows: &[Row],
    ) -> Result<Vec<String>, SqlError> {
        let p = self.plan(tm, s)?;
        if !rows.len().is_multiple_of(2) {
            return Err(BinlogError::InvalidData(format!(
                "update rows length {} is not a before/after pair count",
                rows.len()
            ))
            .into());
        }
        let mut out = Vec::with_capacity(rows.len() / 2);
        for pair in rows.chunks_exact(2) {
            let (before, after) = (&pair[0], &pair[1]);
            // P2 反转：SET/WHERE 镜像取位（回滚 = 用旧镜像值覆写、按新镜像
            // 行定位）；正向路径的 (after, before) 与 P1 完全一致。
            let (set_row, where_row) = match self.kind {
                WorkKind::ToSql => (after, before),
                WorkKind::Flashback => (before, after),
            };
            // SET 差异（GenUpdateSetPart sqlgen.go:336-381）：非 full 时仅变化
            // 列入 SET；比较 = ColumnValue PartialEq（裁定 6：Str/Bytes 按字节、
            // 文本族按预渲染文本，与上游「比解码值」效果等价）。比较对称，
            // 两 kind 共用；assigns 值从 set_row 取。
            let mut assigns = Vec::new();
            for &ord in &p.cols {
                let b = self.cell(before, tm, ord)?;
                let a = self.cell(after, tm, ord)?;
                if self.opts.full_columns || b != a {
                    assigns.push(format!(
                        "{}={}",
                        quote_ident(&p.schema.cols[ord].name),
                        encode_value(self.cell(set_row, tm, ord)?)?
                    ));
                }
            }
            if assigns.is_empty() {
                // FULL 镜像可对无变化行记行对；上游空 SET → sqlbuilder 报错 →
                // log.Fatalf 进程死。本层跳语句 + warn（重放语义等价，偏差已录）。
                tracing::warn!(
                    table = %p.tbl,
                    "update row pair has no changed columns; statement skipped (upstream would abort)"
                );
                continue;
            }
            out.push(format!(
                "UPDATE {} SET {} WHERE {};",
                p.tbl,
                assigns.join(","),
                self.where_part(&p, where_row, tm)?
            ));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binlog::rows::RowsKind;
    use crate::metadata::schema::SchemaCol;

    fn opts(f: fn(&mut SqlOpts)) -> SqlOpts {
        let mut o = SqlOpts::default();
        f(&mut o);
        o
    }

    fn tm(n: usize) -> TableMapEvent {
        TableMapEvent {
            table_id: 7,
            schema: "db".into(),
            table: "t".into(),
            n_cols: n,
            column_type: vec![0x03; n],
            column_meta: vec![0; n],
            null_bits: vec![],
            charset: vec![],
        }
    }

    /// 3 列 a(int pk) b(varchar) c(int)；uk=[[b,c]] 由用例覆写。
    fn schema3() -> TableSchema {
        let col = |n: &str, t: &str| SchemaCol {
            name: n.into(),
            type_name: t.into(),
            unsigned: false,
        };
        TableSchema {
            db: "db".into(),
            table: "t".into(),
            cols: vec![col("a", "int"), col("b", "varchar"), col("c", "int")],
            pk: vec!["a".into()],
            uks: vec![],
        }
    }

    fn row(vals: &[ColumnValue]) -> Row {
        Row {
            cols: vals.to_vec(),
        }
    }
    fn i(v: i64) -> ColumnValue {
        ColumnValue::Int(v)
    }
    fn sv(v: &str) -> ColumnValue {
        ColumnValue::Str(v.as_bytes().to_vec())
    }

    // ---------- INSERT ----------

    #[test]
    fn insert_single_row_db_prefix() {
        let b = DmlBuilder::new(SqlOpts::default());
        let rows = [row(&[i(1), sv("x"), i(9)])];
        assert_eq!(
            b.inserts(&tm(3), &schema3(), &rows).unwrap(),
            vec![r#"INSERT INTO `db`.`t` (`a`,`b`,`c`) VALUES (1,'x',9);"#]
        );
    }

    #[test]
    fn insert_batch_splits_statements() {
        // 3 行 batch=2 → 2 条语句；简报例：VALUES (1,'x'),(2,'y')
        let b = DmlBuilder::new(opts(|o| o.insert_batch = Some(2)));
        let rows = [
            row(&[i(1), sv("x"), i(2)]),
            row(&[i(3), sv("y"), i(4)]),
            row(&[i(5), sv("z"), i(6)]),
        ];
        let got = b.inserts(&tm(3), &schema3(), &rows).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(
            got[0],
            r#"INSERT INTO `db`.`t` (`a`,`b`,`c`) VALUES (1,'x',2),(3,'y',4);"#
        );
        assert_eq!(
            got[1],
            r#"INSERT INTO `db`.`t` (`a`,`b`,`c`) VALUES (5,'z',6);"#
        );
    }

    #[test]
    fn insert_no_db_prefix_bare_table() {
        let b = DmlBuilder::new(opts(|o| o.db_prefix = false));
        let rows = [row(&[i(1), sv("x"), i(2)])];
        let got = b.inserts(&tm(3), &schema3(), &rows).unwrap();
        assert_eq!(got[0], r#"INSERT INTO `t` (`a`,`b`,`c`) VALUES (1,'x',2);"#);
    }

    #[test]
    fn insert_ignore_pk_drops_column_list_and_values() {
        // 裁定 7：列清单与 VALUES 双位置剔除 pk（sqlgen.go:163 + :204-217）
        let b = DmlBuilder::new(opts(|o| o.ignore_pk_for_insert = true));
        let rows = [row(&[i(1), sv("x"), i(2)])];
        let got = b.inserts(&tm(3), &schema3(), &rows).unwrap();
        assert_eq!(got[0], r#"INSERT INTO `db`.`t` (`b`,`c`) VALUES ('x',2);"#);
    }

    #[test]
    fn insert_ignore_pk_noop_when_no_pk() {
        // sqlgen.go:159-161：primaryIdx 空 → 开关自动失效，全列出列
        let mut s = schema3();
        s.pk.clear();
        let b = DmlBuilder::new(opts(|o| o.ignore_pk_for_insert = true));
        let rows = [row(&[i(1), sv("x"), i(2)])];
        let got = b.inserts(&tm(3), &s, &rows).unwrap();
        assert_eq!(
            got[0],
            r#"INSERT INTO `db`.`t` (`a`,`b`,`c`) VALUES (1,'x',2);"#
        );
    }

    // ---------- UPDATE ----------

    #[test]
    fn update_set_only_changed_columns() {
        let b = DmlBuilder::new(SqlOpts::default());
        // 行对 [before, after]：仅 b 变
        let rows = [row(&[i(1), sv("x"), i(2)]), row(&[i(1), sv("y"), i(2)])];
        let got = b.updates(&tm(3), &schema3(), &rows).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0], r#"UPDATE `db`.`t` SET `b`='y' WHERE `a`=1;"#,
            "SET 只含变化列、WHERE 用 before 镜像键值"
        );
    }

    #[test]
    fn update_full_columns_sets_and_wheres_all() {
        let b = DmlBuilder::new(opts(|o| o.full_columns = true));
        let rows = [row(&[i(1), sv("x"), i(2)]), row(&[i(1), sv("y"), i(2)])];
        let got = b.updates(&tm(3), &schema3(), &rows).unwrap();
        assert_eq!(
            got[0],
            r#"UPDATE `db`.`t` SET `a`=1,`b`='y',`c`=2 WHERE `a`=1 AND `b`='x' AND `c`=2;"#
        );
    }

    #[test]
    fn update_ignore_pk_for_insert_flag_does_not_touch_set() {
        // 裁定 7 核实：上游 GenUpdateSqls…(sqlgen.go:288) 无 ignorePrimary 参数
        // ——SET 差异列即便（理论上 pk 不会变也要出）不受该开关影响。
        // full_columns 强制 pk 入 SET：应出现 `a`=5。
        let b = DmlBuilder::new(opts(|o| {
            o.full_columns = true;
            o.ignore_pk_for_insert = true;
        }));
        let rows = [row(&[i(1), sv("x"), i(2)]), row(&[i(5), sv("x"), i(2)])];
        let got = b.updates(&tm(3), &schema3(), &rows).unwrap();
        assert!(
            got[0].starts_with(r#"UPDATE `db`.`t` SET `a`=5,`b`='x',`c`=2 "#),
            "{}",
            got[0]
        );
    }

    #[test]
    fn update_no_change_pair_skipped_with_warn() {
        // FULL 镜像下 MySQL 可对无变化行记行对（matched-only update）。上游
        // 空 SET 走 sqlbuilder 错误→log.Fatalf（进程死）；本层跳语句+warn
        // （无操作重放语义等价，偏差记录 HANDOVER/T15 白名单候选）。
        let b = DmlBuilder::new(SqlOpts::default());
        let rows = [row(&[i(1), sv("x"), i(2)]), row(&[i(1), sv("x"), i(2)])];
        assert!(b.updates(&tm(3), &schema3(), &rows).unwrap().is_empty());
    }

    #[test]
    fn update_odd_row_count_is_error() {
        let b = DmlBuilder::new(SqlOpts::default());
        let rows = [row(&[i(1), sv("x"), i(2)])];
        let e = b.updates(&tm(3), &schema3(), &rows).unwrap_err();
        assert!(
            matches!(e, SqlError::Value(BinlogError::InvalidData(_))),
            "{e:?}"
        );
    }

    // ---------- WHERE 形态（键选择 + IS NULL + 全列降级）----------

    #[test]
    fn delete_where_key_variants() {
        // pk 默认；unique_first → uk[0]；无 pk → uk[0]；皆无 → 全列
        let b = DmlBuilder::new(SqlOpts::default());
        let mut s = schema3();
        let rows = [row(&[i(1), sv("x"), i(2)])];
        assert_eq!(
            b.deletes(&tm(3), &s, &rows).unwrap()[0],
            "DELETE FROM `db`.`t` WHERE `a`=1;"
        );
        s.uks = vec![vec!["b".into()], vec!["c".into()]];
        let bu = DmlBuilder::new(opts(|o| o.unique_first = true));
        assert_eq!(
            bu.deletes(&tm(3), &s, &rows).unwrap()[0],
            "DELETE FROM `db`.`t` WHERE `b`='x';",
            "unique_first：uk[0] 优先于 pk（GetOneUniqueKey :323-326）"
        );
        assert_eq!(
            b.deletes(&tm(3), &s, &rows).unwrap()[0],
            "DELETE FROM `db`.`t` WHERE `a`=1;",
            "默认 pk 优先（:327-329）"
        );
        s.pk.clear();
        assert_eq!(
            b.deletes(&tm(3), &s, &rows).unwrap()[0],
            "DELETE FROM `db`.`t` WHERE `b`='x';",
            "无 pk → uk[0]（:330-331）"
        );
        s.uks.clear();
        assert_eq!(
            b.deletes(&tm(3), &s, &rows).unwrap()[0],
            "DELETE FROM `db`.`t` WHERE `a`=1 AND `b`='x' AND `c`=2;",
            "无键 → 全列 WHERE（GenEqualConditions :277-281 的 full 分支）"
        );
    }

    #[test]
    fn where_null_value_is_null_not_equals_null() {
        // 权威：sqlbuilder.Eq（expression.go:441-447）NULL 右值 → `IS` 算子
        // （输出 `col IS null`）；本层 `IS NULL`。绝不 `col = null`（恒假）。
        let b = DmlBuilder::new(SqlOpts::default());
        let mut s = schema3();
        s.pk.clear(); // 全列 WHERE
        let rows = [row(&[ColumnValue::Null, sv("x"), i(2)])];
        let got = b.deletes(&tm(3), &s, &rows).unwrap();
        assert_eq!(
            got[0],
            r#"DELETE FROM `db`.`t` WHERE `a` IS NULL AND `b`='x' AND `c`=2;"#
        );
        assert!(!got[0].contains("= NULL"), "{}", got[0]);
    }

    #[test]
    fn update_where_keys_on_before_image_with_null() {
        let b = DmlBuilder::new(SqlOpts::default());
        let mut s = schema3();
        s.pk.clear();
        let rows = [
            row(&[i(1), ColumnValue::Null, i(2)]),
            row(&[i(1), sv("y"), i(2)]),
        ];
        assert_eq!(
            b.updates(&tm(3), &s, &rows).unwrap()[0],
            r#"UPDATE `db`.`t` SET `b`='y' WHERE `a`=1 AND `b` IS NULL AND `c`=2;"#
        );
    }

    // ---------- 与 schema 对账的交界（裁定 2）----------

    #[test]
    fn dropped_trailing_cols_skipped_in_all_positions() {
        // binlog 宽 4 > schema 宽 3：第 4 列（dropped_column_0）不进列清单；
        // 无 pk 时全列 WHERE 也只 3 列；键永不落 dropped（key_indexes 构造保证）。
        let b = DmlBuilder::new(SqlOpts::default());
        let mut s = schema3();
        s.pk.clear();
        s.uks = vec![];
        let rows = [row(&[i(1), sv("x"), i(2), i(99)])];
        let ins = &b.inserts(&tm(4), &s, &rows).unwrap()[0];
        assert_eq!(
            ins, r#"INSERT INTO `db`.`t` (`a`,`b`,`c`) VALUES (1,'x',2);"#,
            "dropped 列被跳过且 warn（告警捕获非断言目标，行为钉死即可）"
        );
        let del = &b.deletes(&tm(4), &s, &rows).unwrap()[0];
        assert_eq!(
            del, "DELETE FROM `db`.`t` WHERE `a`=1 AND `b`='x' AND `c`=2;",
            "全列 WHERE 不键于 dropped"
        );
    }

    #[test]
    fn truncated_schema_tail_absent_from_rows() {
        // schema 宽 3 > binlog 宽 2：只出前 2 列（events.go:83 静默前缀）。
        let b = DmlBuilder::new(SqlOpts::default());
        let rows = [row(&[i(1), sv("x")])];
        assert_eq!(
            b.inserts(&tm(2), &schema3(), &rows).unwrap()[0],
            r#"INSERT INTO `db`.`t` (`a`,`b`) VALUES (1,'x');"#
        );
    }

    #[test]
    fn strict_schema_true_turns_mismatch_into_event_error() {
        // 裁定 2：strict=true → Truncated/ColCountFatal 逐事件错误传播。
        let b = DmlBuilder::new(opts(|o| o.strict_schema = true));
        let rows = [row(&[i(1), sv("x"), i(2)])];
        // 宽出
        let e = b.inserts(&tm(4), &schema3(), &rows).unwrap_err();
        assert!(
            matches!(
                e,
                SqlError::Meta(crate::metadata::store::MetaError::ColCountFatal { .. })
            ),
            "{e:?}"
        );
        // 窄出
        let e = b.deletes(&tm(2), &schema3(), &rows).unwrap_err();
        assert!(matches!(e, SqlError::Meta(_)), "{e:?}");
        // 等宽不受影响
        assert!(b.deletes(&tm(3), &schema3(), &rows).is_ok());
    }

    #[test]
    fn missing_value_in_sql_position_is_hard_error() {
        // 裁定 3：Missing（partial rows 专属，T10 已拒）→ InvalidData。
        let b = DmlBuilder::new(SqlOpts::default());
        let rows = [row(&[i(1), ColumnValue::Missing, i(2)])];
        let e = b.inserts(&tm(3), &schema3(), &rows).unwrap_err();
        assert!(
            matches!(e, SqlError::Value(BinlogError::InvalidData(_))),
            "{e:?}"
        );
    }

    #[test]
    fn row_width_mismatch_is_defensive_error() {
        // decode_rows 恒产出 len==n_cols（T10 不变式）；违背即硬错误，不静默
        let b = DmlBuilder::new(SqlOpts::default());
        let rows = [row(&[i(1), sv("x")])];
        let e = b.inserts(&tm(3), &schema3(), &rows).unwrap_err();
        assert!(
            matches!(e, SqlError::Value(BinlogError::InvalidData(_))),
            "{e:?}"
        );
    }

    #[test]
    fn identifiers_are_backtick_quoted_everywhere() {
        // 裁定 8：库/表/列名一律 quote_ident（内部反引号双写）
        let b = DmlBuilder::new(SqlOpts::default());
        let mut t = tm(2);
        t.schema = "d`b".into();
        t.table = "t`1".into();
        let mut s = schema3();
        s.cols.truncate(2);
        s.cols[0].name = "a`".into();
        let rows = [row(&[i(1), sv("x")])];
        let ins = &b.inserts(&t, &s, &rows).unwrap()[0];
        assert_eq!(
            ins, "INSERT INTO `d``b`.`t``1` (`a```,`b`) VALUES (1,'x');",
            "反引号双写贯通库/表/列位"
        );
    }

    // ---------- P2 Flashback（WorkKind 语义反转）----------

    #[test]
    fn flashback_dml_for_maps_events_to_reverse_statements() {
        let b = DmlBuilder::flashback(SqlOpts::default());
        let t = tm(3);
        let s = schema3(); // pk=a
        // INSERT 事件(after-image) → DELETE 键定位
        let w = [row(&[i(1), sv("x"), i(2)])];
        assert_eq!(
            b.dml_for(RowsKind::Write, &t, &s, &w).unwrap(),
            vec!["DELETE FROM `db`.`t` WHERE `a`=1;"]
        );
        // DELETE 事件(before-image) → INSERT 全列
        let d = [row(&[i(7), sv("z"), i(8)])];
        assert_eq!(
            b.dml_for(RowsKind::Delete, &t, &s, &d).unwrap(),
            vec![r#"INSERT INTO `db`.`t` (`a`,`b`,`c`) VALUES (7,'z',8);"#]
        );
        // UPDATE → SET=before, WHERE=after 键
        let u = [row(&[i(1), sv("x"), i(2)]), row(&[i(1), sv("y"), i(2)])];
        assert_eq!(
            b.dml_for(RowsKind::Update, &t, &s, &u).unwrap(),
            vec![r#"UPDATE `db`.`t` SET `b`='x' WHERE `a`=1;"#]
        );
        // 正向 kind 不受影响
        assert_eq!(
            DmlBuilder::new(SqlOpts::default())
                .dml_for(RowsKind::Write, &t, &s, &w)
                .unwrap()[0],
            r#"INSERT INTO `db`.`t` (`a`,`b`,`c`) VALUES (1,'x',2);"#
        );
    }

    #[test]
    fn flashback_update_full_columns_and_unchanged_pair() {
        let bf = DmlBuilder::flashback(opts(|o| o.full_columns = true));
        let rows = [row(&[i(1), sv("x"), i(2)]), row(&[i(1), sv("y"), i(2)])];
        assert_eq!(
            bf.updates(&tm(3), &schema3(), &rows).unwrap()[0],
            r#"UPDATE `db`.`t` SET `a`=1,`b`='x',`c`=2 WHERE `a`=1 AND `b`='y' AND `c`=2;"#,
            "full: SET 全列取 before、WHERE 全列取 after"
        );
        // 无变化行对：正向跳语句（上游 abort）；逆向同样跳（空 SET 同构）
        let same = [row(&[i(1), sv("x"), i(2)]), row(&[i(1), sv("x"), i(2)])];
        assert!(
            DmlBuilder::flashback(SqlOpts::default())
                .updates(&tm(3), &schema3(), &same)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn flashback_update_unchanged_json_not_in_set() {
        // 上游逆向 UPDATE 仍带「JSON 恒进 SET」quirk；本层不继承（spec §3.1）——
        // 未变化的 JSON 列不得出现在 SET。比较器 ALW-JSON-IN-SET 容忍裁判多出的
        // JSON 项，本测试钉死我方严格。
        let mut s = schema3();
        s.cols[1].type_name = "json".into();
        let mut t = tm(3);
        t.column_type[1] = 0xf6; // MYSQL_TYPE_JSON
        let rows = [
            row(&[i(1), ColumnValue::Json(r#"{"a":1}"#.into()), i(2)]),
            row(&[i(1), ColumnValue::Json(r#"{"a":1}"#.into()), i(9)]),
        ];
        let got = DmlBuilder::flashback(SqlOpts::default())
            .updates(&t, &s, &rows)
            .unwrap();
        assert_eq!(got[0], r#"UPDATE `db`.`t` SET `c`=2 WHERE `a`=1;"#);
    }

    #[test]
    fn flashback_rejects_padded_dropped_columns_as_event_error() {
        // 硬规则 a（对齐上游 events.go:87 fail-hard）：dropped 列 = 旧镜像不完整，
        // 回滚脚本宁缺毋漏 → Flashback 下 Padded 是错误而非告警。
        let b = DmlBuilder::flashback(SqlOpts::default());
        let rows = [row(&[i(1), sv("x"), i(2), i(99)])];
        let e = b.deletes(&tm(4), &schema3(), &rows).unwrap_err();
        assert!(
            matches!(e, SqlError::Value(BinlogError::InvalidData(_))),
            "{e:?}"
        );
        assert!(e.to_string().contains("flashback"), "{e}");
        // 正向不受影响（P1 行为回归守卫）
        assert!(
            DmlBuilder::new(SqlOpts::default())
                .inserts(&tm(4), &schema3(), &rows)
                .is_ok()
        );
    }

    #[test]
    fn flashback_missing_value_error_hints_row_image_full() {
        // 硬规则 b：Missing（MINIMAL row image / partial）进 WHERE/VALUES → 报错，
        // 提示需 binlog_row_image=FULL。
        let b = DmlBuilder::flashback(SqlOpts::default());
        let rows = [row(&[i(1), ColumnValue::Missing, i(2)])];
        let e = b.deletes(&tm(3), &schema3(), &rows).unwrap_err();
        assert!(e.to_string().contains("binlog_row_image=FULL"), "{e}");
        let e = b.inserts(&tm(3), &schema3(), &rows).unwrap_err();
        assert!(e.to_string().contains("binlog_row_image=FULL"), "{e}");
        // 正向维持 P1 既有 InvalidData（encode 路径），消息不要求 FULL 提示
        let e = DmlBuilder::new(SqlOpts::default())
            .inserts(&tm(3), &schema3(), &rows)
            .unwrap_err();
        assert!(
            matches!(e, SqlError::Value(BinlogError::InvalidData(_))),
            "{e:?}"
        );
    }
}
