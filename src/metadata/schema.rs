//! 表结构列元数据 + 列数对账（align_cols）+ 键名→binlog 序号映射（key_indexes）。
//!
//! 字段口径对照 my2sql-go `base/mysqlFuncs.go:298`（SHOW COLUMNS 的 Type 列经
//! `GetFiledType`（base/funcs.go:86-92）按 `(` 分割取首段得 `type_name`（小写、
//! 无括号/无 unsigned 后缀），`IsUnsigned`（funcs.go:94-96）按含 "unsigned"
//! 判 `unsigned`）。
//!
//! 列数对账与键映射的权威行为（引用行号见 docs/HANDOVER.md Task 11 对照表）：
//! - `sqlgen.go:23-33`（GetAllFieldNamesWithDroppedFields）：binlog 宽 > schema
//!   宽时以 `dropped_column_{i}`/`unknown_type` 补位（`sqlgen.go:19-21`），
//!   但 `events.go:87` 随后无条件 `log.Fatalf`——补位在参考实现中从未真正产出
//!   SQL。本层将「补位」作为非 strict 分支返回（Padded），strict=true 复现
//!   fatal，语义差异与理由见 HANDOVER Task 11 节点。
//! - `events.go:83`（rowLen <= len(colNames) 分支）：binlog 宽 < schema 宽时
//!   静默取前 binlog 列（Truncated）。
//! - `mysqlFuncs.go:337-348`（GetColIndexFromKey）：键列名在列表中找不到时，
//!   Go 把该序号静默置 0（arr 零值，错误列风险）；`events.go:139-143` 仅在
//!   主键名列表本身为空时才降级 pk=[]。本层按简报绑定改用「整键丢弃」降级，
//!   属对上游已知序号-0 缺陷的刻意偏离（差异记录见报告/HANDOVER）。

// T11 起生产消费者为 store.rs 与 T13/T14（本层 align_cols/key_indexes/TableMap
// 相关项在 pipeline 接入前仅测试消费）。

use serde::{Deserialize, Serialize};

use super::store::MetaError;
use crate::binlog::table_map::TableMapEvent;

/// 单列 schema 元数据（来自 SHOW COLUMNS / information_schema）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaCol {
    pub name: String,
    /// 小写、无括号、无 unsigned 后缀的类型名（如 `int`、`tinytext`、`enum`）。
    pub type_name: String,
    pub unsigned: bool,
}

/// 表结构（库名 + 表名 + 列 + 键）。JSON 序列化键面即字段名本身
/// （`{db,table,cols[{name,type_name,unsigned}],pk,uks}`，controller 裁定 1）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableSchema {
    pub db: String,
    pub table: String,
    pub cols: Vec<SchemaCol>,
    pub pk: Vec<String>,
    pub uks: Vec<Vec<String>>,
}

/// Type 列（如 `int(11)`、`int(10) unsigned`、8.0.19+ 的 `int unsigned`）归一化：
/// 返回 `(type_name, unsigned)`。
///
/// 权威：funcs.go:86-92（`(` 前首段）+ funcs.go:94-96（含 "unsigned" 判符号）。
/// 8.0.19+ 的无括号形态（`int unsigned`）在 GetFiledType 下会把 " unsigned"
/// 留在 type_name 里（上游写作时仅有带括号形态）；此处一并剥掉尾部
/// `unsigned`/`signed`/`zerofill` 词，命中 T9 契约「type_name 小写无后缀」，
/// 与 5.7 带括号形态下的上游有效输出一致（差异记录见报告）。
pub(crate) fn norm_type(raw: &str) -> (String, bool) {
    let lower = raw.to_lowercase();
    let head = lower.split('(').next().unwrap_or(&lower);
    let type_name = head
        .split_whitespace()
        .filter(|w| !matches!(*w, "unsigned" | "signed" | "zerofill"))
        .collect::<Vec<_>>()
        .join(" ");
    (type_name, lower.contains("unsigned"))
}

/// 列数对账结果（bind: 简报 Step 1 三态）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Align {
    /// binlog 列数 == 当前 schema 列数。
    Ok,
    /// schema 比 binlog 宽（binlog 行里没有这些尾列）。
    /// 载荷 = 被截断的 schema 尾列个数（目标宽度 = schema.cols.len() - n，
    /// 即 binlog_cols；对齐 events.go:83 的静默前缀行为）。
    Truncated(usize),
    /// binlog 比 schema 宽（schema 之后加过列又…典型为 DDL 居中）。
    /// 载荷 = 依序补位的合成列名 `dropped_column_0..`（对齐 sqlgen.go:19-21
    /// 的命名；类型一律 `unknown_type`，同 context.go:27）。
    /// 目标宽度 = schema.cols.len() + dropped.len()，即 binlog_cols。
    Padded { dropped: Vec<String> },
}

/// `dropped_column_{idx}` 合成列名（sqlgen.go:19-21 GetDroppedFieldName）。
pub fn dropped_col_name(idx: usize) -> String {
    format!("dropped_column_{idx}")
}

/// binlog 列数 vs 当前 schema 列数对账；T13 在生成 SQL 前调用决定 pad/
/// truncate/fatal。
///
/// - 相等 → `Ok`；
/// - `strict=true` 且任何不等 → `Err(MetaError::ColCountFatal)`（对应
///   events.go:87 在扩宽方向上的无条件 fatal；收窄方向上游从不 fatal，是本层
///   为 strict 语义补充的扩展，见 HANDOVER 差异表）；
/// - 非 strict：binlog 窄 → `Truncated`（events.go:83 静默前缀），
///   binlog 宽 → `Padded`（sqlgen.go:23-33 的补位命名，上游该分支随后 fatal）。
pub fn align_cols(
    binlog_cols: usize,
    schema: &TableSchema,
    strict: bool,
) -> Result<Align, MetaError> {
    let sc = schema.cols.len();
    if binlog_cols == sc {
        return Ok(Align::Ok);
    }
    if strict {
        return Err(MetaError::ColCountFatal {
            table: format!("`{}`.`{}`", schema.db, schema.table),
            binlog_cols,
            schema_cols: sc,
        });
    }
    if binlog_cols < sc {
        Ok(Align::Truncated(sc - binlog_cols))
    } else {
        let dropped = (0..binlog_cols - sc)
            .map(dropped_col_name)
            .collect::<Vec<_>>();
        Ok(Align::Padded { dropped })
    }
}

/// 把 schema 的主键/唯一键列名映射到 binlog 行内序号（0 基，按
/// `schema.cols` 顺序——binlog 行与 schema 列按序数配对，T10 口径）。
///
/// 降级规则（与上游差异已在模块头注明）：
/// - 键内任一列名在 `schema.cols` 找不到，或其序号 ≥ `tm.n_cols`
///   （binlog 行里没有该列，如 schema 后加列 / 表达式索引空列名）→
///   整键丢弃：pk 降级为 `[]`，uk 从列表剔除；
/// - 空键列表不产出（pk 自然为 `[]`；空 uk 条目剔除）。
pub fn key_indexes(s: &TableSchema, tm: &TableMapEvent) -> (Vec<usize>, Vec<Vec<usize>>) {
    let resolve = |names: &[String]| -> Option<Vec<usize>> {
        let mut out = Vec::with_capacity(names.len());
        for n in names {
            let idx = s.cols.iter().position(|c| &c.name == n)?;
            if idx >= tm.n_cols {
                return None;
            }
            out.push(idx);
        }
        Some(out)
    };
    let pk = if s.pk.is_empty() {
        Vec::new()
    } else {
        resolve(&s.pk).unwrap_or_default()
    };
    let uks = s
        .uks
        .iter()
        .filter(|k| !k.is_empty())
        .filter_map(|k| resolve(k))
        .collect();
    (pk, uks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scol(name: &str, ty: &str) -> SchemaCol {
        SchemaCol {
            name: name.into(),
            type_name: ty.into(),
            unsigned: false,
        }
    }

    fn schema(names: &[&str], pk: &[&str], uks: &[&[&str]]) -> TableSchema {
        TableSchema {
            db: "d".into(),
            table: "t".into(),
            cols: names.iter().map(|n| scol(n, "int")).collect(),
            pk: pk.iter().map(|s| s.to_string()).collect(),
            uks: uks
                .iter()
                .map(|k| k.iter().map(|s| s.to_string()).collect())
                .collect(),
        }
    }

    fn tm_with_n_cols(n: usize) -> TableMapEvent {
        TableMapEvent {
            table_id: 1,
            schema: "d".into(),
            table: "t".into(),
            n_cols: n,
            column_type: vec![],
            column_meta: vec![],
            null_bits: vec![],
            charset: vec![],
        }
    }

    #[test]
    fn norm_type_basic() {
        // 5.7 形态：带显示宽度
        assert_eq!(norm_type("int(11)"), ("int".into(), false));
        assert_eq!(norm_type("int(10) unsigned"), ("int".into(), true));
        assert_eq!(
            norm_type("decimal(10,2) unsigned"),
            ("decimal".into(), true)
        );
        assert_eq!(norm_type("varchar(50)"), ("varchar".into(), false));
        assert_eq!(norm_type("tinytext"), ("tinytext".into(), false));
        // enum 值里含括号也只取 `(` 前首段（funcs.go:86-92）
        assert_eq!(norm_type("enum('a(b','c')"), ("enum".into(), false));
        // 8.0.19+ 形态：无显示宽度、带后缀词
        assert_eq!(norm_type("int unsigned"), ("int".into(), true));
        assert_eq!(
            norm_type("bigint(20) unsigned zerofill"),
            ("bigint".into(), true)
        );
        assert_eq!(norm_type("int zerofill unsigned"), ("int".into(), true));
    }

    // ---- align_cols：三态 + strict fatal ----

    #[test]
    fn align_cols_equal_ok() {
        let s = schema(&["a", "b", "c"], &[], &[]);
        assert_eq!(align_cols(3, &s, false).unwrap(), Align::Ok);
        assert_eq!(align_cols(3, &s, true).unwrap(), Align::Ok);
    }

    #[test]
    fn align_cols_binlog_narrower_truncated() {
        let s = schema(&["a", "b", "c", "d", "e"], &[], &[]);
        assert_eq!(align_cols(3, &s, false).unwrap(), Align::Truncated(2));
    }

    #[test]
    fn align_cols_binlog_wider_padded() {
        let s = schema(&["a", "b"], &[], &[]);
        let got = align_cols(4, &s, false).unwrap();
        assert_eq!(
            got,
            Align::Padded {
                dropped: vec!["dropped_column_0".into(), "dropped_column_1".into()]
            }
        );
    }

    #[test]
    fn align_cols_strict_fatal_both_directions() {
        let s = schema(&["a", "b", "c"], &[], &[]);
        // 宽出（上游 events.go:87 的 fatal 方向）
        let e = align_cols(5, &s, true).unwrap_err();
        assert!(
            matches!(
                e,
                MetaError::ColCountFatal {
                    binlog_cols: 5,
                    schema_cols: 3,
                    ..
                }
            ),
            "{e}"
        );
        // 窄出（上游静默；strict 下本层同样 fatal，语义扩展见 HANDOVER）
        assert!(align_cols(1, &s, true).is_err());
        // 非 strict 两向都不 fatal
        assert!(align_cols(5, &s, false).is_ok());
        assert!(align_cols(1, &s, false).is_ok());
    }

    // ---- key_indexes ----

    #[test]
    fn key_indexes_resolves_all_ordinals() {
        let s = schema(&["a", "b", "c", "d"], &["b", "c"], &[&["a"], &["d"]]);
        let (pk, uks) = key_indexes(&s, &tm_with_n_cols(4));
        assert_eq!(pk, vec![1, 2]);
        assert_eq!(uks, vec![vec![0usize], vec![3usize]]);
    }

    #[test]
    fn key_indexes_pk_with_missing_col_degrades_to_empty() {
        // dropped 列在 key 中 → pk=[]（简报绑定；上游此处是序号-0 缺陷，见差异表）
        let s = schema(&["a", "b"], &["a", "gone"], &[&["b"]]);
        let (pk, uks) = key_indexes(&s, &tm_with_n_cols(2));
        assert!(pk.is_empty());
        assert_eq!(uks, vec![vec![1usize]]);
    }

    #[test]
    fn key_indexes_key_beyond_binlog_width_dropped() {
        // schema 5 列、binlog 只宽 3：尾列 d 的 uk 与超出宽度的 pk 均整键丢弃
        let s = schema(&["a", "b", "c", "d", "e"], &["c", "d"], &[&["a"], &["d"]]);
        let (pk, uks) = key_indexes(&s, &tm_with_n_cols(3));
        assert!(pk.is_empty()); // c 在行内、d 不在 → 整键降级
        assert_eq!(uks, vec![vec![0usize]]);
    }

    #[test]
    fn key_indexes_empty_key_lists() {
        let s = schema(&["a"], &[], &[vec![].as_slice()]);
        let (pk, uks) = key_indexes(&s, &tm_with_n_cols(1));
        assert!(pk.is_empty());
        assert!(uks.is_empty());
    }
}
