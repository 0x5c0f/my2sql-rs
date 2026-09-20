//! 值 → SQL 字面量文本编码。
//!
//! ## 权威转义集（逐字节复刻，非标准 MySQL 全集）
//!
//! my2sql-go 字面量渲染链：`SQL.Literal(v)` →
//! `sqltypes.BuildValue`（sqltypes.go:217-278，string→utf8 String、
//! []byte→非 utf8 String）→ `String.encodeSql`（:548-566）按 `SqlEncodeMap`
//! 逐字节转义，映射表由 `encodeRef`（:611-621）定义，**恰好**为：
//!
//! | 字节 | 输出 | | 字节 | 输出 |
//! |---|---|---|---|---|
//! | 0x00 | `\0` | | 0x0A | `\n` |
//! | `'`  | `\'` | | 0x0D | `\r` |
//! | `"`  | `\"` | | 0x1A | `\Z` |
//! | 0x08 | `\b` | | `\`  | `\\` |
//! | 0x09 | `\t` | |      |      |
//!
//! 其余字节（含 ≥0x80 的 UTF-8 续字节、emoji）原样透传。**特殊例外**
//! （:556-561，LIKE 通配保真）：`\` 后随 `%` 或 `_` 时**不**双写
//! （Go 原文注释引 MySQL 5.7 string-literals 文档，为使 `\%`/`\_` 在
//! LIKE RHS 语义下往返不变）。本层逐字节镜像该集合（含例外），达成
//! my2sql-go 输出对等；与 MySQL 默认 NO_BACKSLASH_ESCAPES 关闭形态一致。
//!
//! ## 与控制器的输出重设计（裁定 1，T15 白名单已挂账）
//!
//! - `Bytes` → `0xUPPERHEX`。上游非 utf8 String 走 `X'lowerhex'`
//!   （sqltypes.go:567-570 + hex.go:19 `%02x`）——前缀（`0x` vs `X'..'`）与
//!   大小写双重分歧，SQL 语义等价，比较器需双解；
//! - `Str` 理论上是 utf8-safe 后的合法 UTF-8（T9 `utf8_safe` 闸）；若构造
//!   输入违约携带非法 UTF-8，整值降级 `0xHEX`（不 lossy 重写，D3 值保真），
//!   与 Bytes 同形态；
//! - `Json` → T8 紧凑文本按同集转义后单引号包裹（上游 json 列亦 Str 化，
//!   events.go:102-117 text 分支的引号语义）；
//! - `Null` → `NULL`（上游渲染 `null`，sqltypes.go:27 `nullstr`，大小写无语义）；
//! - `Double`/`Decimal` → 原样文本（T5-T8 预渲染最短往返/精确十进制，
//!   上游 float64 走 `Fractional`、decimal 走 Numeric 亦原文直出）。

use crate::binlog::error::BinlogError;
use crate::binlog::int::ColumnValue;

use super::SqlError;

/// 标识符反引号引用：内部 `` ` `` 双写（MySQL 引号规则）。任何拼进 SQL 的
/// 库/表/列名必须过此函数（裁定 8）。
pub fn quote_ident(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('`');
    for c in s.chars() {
        if c == '`' {
            out.push('`');
        }
        out.push(c);
    }
    out.push('`');
    out
}

/// 大写 `0xHEX` 字面量（`Bytes` 与非法 UTF-8 降级共用）。
fn hex_literal(data: &[u8]) -> String {
    let mut out = String::with_capacity(2 + data.len() * 2);
    out.push_str("0x");
    for b in data {
        out.push_str(&format!("{b:02X}"));
    }
    out
}

/// `Str`/`Json` 的引号字面量：逐字节镜像上游 SqlEncodeMap（见模块头表），
/// 末尾做 UTF-8 合法性兜底（违约输入 → 0xHEX，不重写字节）。
fn quoted_literal(data: &[u8]) -> String {
    let mut buf: Vec<u8> = Vec::with_capacity(data.len());
    for (i, &ch) in data.iter().enumerate() {
        match ch {
            0x00 => buf.extend_from_slice(b"\\0"),
            b'\'' => buf.extend_from_slice(b"\\'"),
            b'"' => buf.extend_from_slice(b"\\\""),
            0x08 => buf.extend_from_slice(b"\\b"),
            0x0A => buf.extend_from_slice(b"\\n"),
            0x0D => buf.extend_from_slice(b"\\r"),
            0x09 => buf.extend_from_slice(b"\\t"),
            0x1A => buf.extend_from_slice(b"\\Z"),
            b'\\' => {
                // 上游例外（sqltypes.go:556-561）：`\%` `\_` 不双写反斜杠
                if matches!(data.get(i + 1), Some(b'%') | Some(b'_')) {
                    buf.push(b'\\');
                } else {
                    buf.extend_from_slice(b"\\\\");
                }
            }
            _ => buf.push(ch),
        }
    }
    match String::from_utf8(buf) {
        // 转义只增 ASCII，内层合法 ⇔ 原始 data 合法
        Ok(inner) => format!("'{inner}'"),
        // 合法输入（utf8_safe 过后）永不可达；违约输入降级 hex、不含引号（D3）
        Err(_) => hex_literal(data),
    }
}

/// 单列值 → SQL 字面量文本。`Missing`（仅 8.0.1 partial rows 可达，T10
/// decode_rows 已拒收该形态）按裁定 3 视为逐事件硬错误。
pub fn encode_value(v: &ColumnValue) -> Result<String, SqlError> {
    Ok(match v {
        ColumnValue::Null => "NULL".to_string(),
        ColumnValue::Int(i) => i.to_string(),
        ColumnValue::UInt(u) => u.to_string(),
        ColumnValue::Double(s) | ColumnValue::Decimal(s) => s.clone(),
        ColumnValue::Str(b) => quoted_literal(b),
        ColumnValue::Bytes(b) => hex_literal(b),
        ColumnValue::Json(s) => quoted_literal(s.as_bytes()),
        ColumnValue::Missing => {
            return Err(BinlogError::InvalidData(
                "ColumnValue::Missing reached SQL encoding (partial rows are rejected at decode; \
                 this is a decoder bug, not a shipping path)"
                    .into(),
            )
            .into());
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(t: &str) -> ColumnValue {
        ColumnValue::Str(t.as_bytes().to_vec())
    }
    fn enc(v: &ColumnValue) -> String {
        encode_value(v).unwrap()
    }

    #[test]
    fn quote_ident_wraps_and_doubles_backticks() {
        assert_eq!(quote_ident("a"), "`a`");
        assert_eq!(quote_ident("a`b"), "`a``b`");
        assert_eq!(quote_ident(""), "``");
        // 反引号外的特殊字符原样（DDL 名域合法性非本层职责）
        assert_eq!(quote_ident("my tbl'x"), "`my tbl'x`");
    }

    #[test]
    fn scalar_variants_basic() {
        assert_eq!(enc(&ColumnValue::Null), "NULL");
        assert_eq!(enc(&ColumnValue::Int(-42)), "-42");
        assert_eq!(enc(&ColumnValue::UInt(u64::MAX)), "18446744073709551615");
        // Double/Decimal 预渲染文本原样透传（T5-T7 契约）
        assert_eq!(enc(&ColumnValue::Double("1.5".into())), "1.5");
        assert_eq!(enc(&ColumnValue::Decimal("-12.34".into())), "-12.34");
    }

    /// 转义集穷举：encodeRef 的 9 个字符逐一钉死（sqltypes.go:611-621）。
    #[test]
    fn escape_set_full_sweep() {
        assert_eq!(enc(&s("a\0b")), r"'a\0b'"); // 0x00
        assert_eq!(enc(&s("it's")), r"'it\'s'"); // ' → \'（encodeRef 原样，非 ''）
        assert_eq!(enc(&s("say\"hi\"")), r#"'say\"hi\"'"#); // "
        assert_eq!(enc(&s("a\u{8}b")), r"'a\bb'"); // 0x08 backspace
        assert_eq!(enc(&s("l1\nl2")), r"'l1\nl2'"); // 0x0A
        assert_eq!(enc(&s("c1\rc2")), r"'c1\rc2'"); // 0x0D
        assert_eq!(enc(&s("t\tt")), r"'t\tt'"); // 0x09
        assert_eq!(enc(&s("a\u{1a}b")), r"'a\Zb'"); // 0x1A ctrl-Z
        assert_eq!(enc(&s("back\\slash")), r"'back\\slash'"); // \
        // 不在表内的字节原样（% _ 本身不转义）
        assert_eq!(enc(&s("100%_x")), "'100%_x'");
    }

    /// 上游 LIKE 例外：`\` 后随 % 或 _ 不双写（sqltypes.go:556-561）。
    #[test]
    fn backslash_before_wildcard_not_doubled() {
        assert_eq!(enc(&s(r"a\%b")), r"'a\%b'");
        assert_eq!(enc(&s(r"a\_b")), r"'a\_b'");
        // 反斜杠连缀：第 1 个 \ 后跟 \（非通配）→ 双写；第 2 个 \ 后跟 % → 保留
        assert_eq!(enc(&s(r"a\\%b")), r"'a\\\%b'");
        // 行尾孤立 \ 必须双写（防止引号被吞）
        assert_eq!(enc(&s("tail\\")), r"'tail\\'");
    }

    #[test]
    fn emoji_and_cjk_passthrough_verbatim() {
        assert_eq!(enc(&s("中文🎉")), "'中文🎉'");
        assert_eq!(
            enc(&ColumnValue::Json(r#"{"k":"🎉"}"#.into())),
            r#"'{\"k\":\"🎉\"}'"#
        );
    }

    #[test]
    fn json_string_escapes_quotes_and_backslash() {
        // JSON 文本内嵌 " 与转义 \ 走上游同集
        assert_eq!(
            enc(&ColumnValue::Json(r#"{"a":"b\"c"}"#.into())),
            r#"'{\"a\":\"b\\\"c\"}'"#
        );
    }

    /// 裁定 1：Bytes → 0x 大写十六进制（上游 X'小写' —— T15 白名单挂账）。
    #[test]
    fn bytes_to_uppercase_hex_literal() {
        assert_eq!(enc(&ColumnValue::Bytes(vec![0xFF, 0x00])), "0xFF00");
        assert_eq!(enc(&ColumnValue::Bytes(vec![0x0a, 0x1B, 0x7F])), "0x0A1B7F");
        assert_eq!(enc(&ColumnValue::Bytes(vec![])), "0x");
    }

    /// D3 值保真证明：非法 UTF-8 的 Str（decode 链路上被 utf8_safe 降为
    /// Bytes；此处构造违约输入验证编码层自身兜底）→ 同一 0xHEX 形态。
    #[test]
    fn invalid_utf8_str_falls_back_to_hex() {
        let mut raw = vec![0xFF, 0xFE];
        let v = {
            utf8_downgrade(&mut raw);
            ColumnValue::Str(raw.clone())
        };
        assert_eq!(enc(&v), "0xFFFE");
    }

    /// 链路实证：T9 `utf8_safe` 把非法 UTF-8 的 Str 降级 Bytes，编码即 hex
    /// （简报 Step 1「非法 UTF-8 Str 已被 Task 9 降级 Bytes → hex」）。
    fn utf8_downgrade(bytes: &mut Vec<u8>) {
        let mut v = ColumnValue::Str(std::mem::take(bytes));
        crate::binlog::value::utf8_safe(&mut v);
        match v {
            ColumnValue::Bytes(b) => *bytes = b,
            other => panic!("expected Bytes, got {other:?}"),
        }
    }

    #[test]
    fn missing_is_hard_error() {
        // 裁定 3：Missing 值位置 = 逐事件 InvalidData（P1 非出货路径）
        let e = encode_value(&ColumnValue::Missing).unwrap_err();
        assert!(
            matches!(e, SqlError::Value(BinlogError::InvalidData(_))),
            "{e:?}"
        );
    }
}
