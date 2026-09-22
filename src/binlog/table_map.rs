//! TABLE_MAP 事件 body 解码（含 STRING 列真实类型还原）。
//!
//! 行为对照 go-mysql `replication/row_event.go` 的 `TableMapEvent.Decode`
//! （布局：table_id 6B LE + flags 2B + schema_len(1B)+schema+0x00 +
//! table_len(1B)+table+0x00 + n_cols(LNE) + column_types(n_cols×1B) +
//! metadata(LNE 长度 + 每列 meta) + null_bits bitmap + 可选尾部（两形态：
//! 5.6.3 WL#6494 charset 数组 = 1B 长度前缀、255 转义为后随 2B LE、逐列 LNE
//! 列表；8.0 optional-metadata TLV 流 = 无前导总长、逐条 type+len+value）。
//! 注：`signed` 不属于本事件——unsigned 判定在 Task 11 由 metadata 层提供。

// 骨架阶段本模块尚无生产消费者（Task 10/12 接入），参照 Task 1-3 允许死代码。

use super::error::BinlogError;
use super::field_types as tp;
use super::proto::{bit_width, read_lne, read_lns};

// 类型码统一取自 super::field_types（T10 Step 0 合并）。原私有 `mod tp`
// 把 FLOAT/DOUBLE 与时间 2 族命名相对官方值互换（三族在本模块 meta 长度表
// 同走 1B 分支，值集合一致、行为从未出错，但命名系误，T5/T9 评审挂账至今）。

/// TABLE_MAP 事件（19B 公共头之后的 body 解码结果）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableMapEvent {
    pub table_id: u64,
    pub schema: String,
    pub table: String,
    pub n_cols: usize,
    pub column_type: Vec<u8>,
    pub column_meta: Vec<u16>,
    pub null_bits: Vec<u8>,
    pub charset: Vec<u64>,
}

/// 解析 TABLE_MAP body（不含 19B 事件头）；`with_crc` 时先剥尾部 4 字节 CRC 再解字段。
pub fn parse_table_map(body: &[u8], with_crc: bool) -> Result<TableMapEvent, BinlogError> {
    let body = if with_crc {
        if body.len() < 4 {
            return Err(BinlogError::TooShort);
        }
        &body[..body.len() - 4]
    } else {
        body
    };
    let mut pos = 0usize;
    // table_id：6 字节小端（5.0 时代的 4 字节变体不支持，目标版本 5.6+）
    if body.len() < 6 {
        return Err(BinlogError::TooShort);
    }
    let table_id = u64::from_le_bytes([body[0], body[1], body[2], body[3], body[4], body[5], 0, 0]);
    pos += 6;
    // flags 2B（P1 不消费）
    if body.len() < pos + 2 {
        return Err(BinlogError::TooShort);
    }
    pos += 2;
    // schema: 1B 长度 + 内容 + 0x00 终止符（终止符仅跳过，不校验，对照 go-mysql）
    let slen = *body.get(pos).ok_or(BinlogError::TooShort)? as usize;
    pos += 1;
    let schema = String::from_utf8(
        body.get(pos..pos + slen)
            .ok_or(BinlogError::TooShort)?
            .to_vec(),
    )
    .map_err(|e| BinlogError::InvalidData(format!("schema utf8: {e}")))?;
    pos += slen + 1; // 跳过终止符
    // table：同 schema
    let tlen = *body.get(pos).ok_or(BinlogError::TooShort)? as usize;
    pos += 1;
    let table = String::from_utf8(
        body.get(pos..pos + tlen)
            .ok_or(BinlogError::TooShort)?
            .to_vec(),
    )
    .map_err(|e| BinlogError::InvalidData(format!("table utf8: {e}")))?;
    pos += tlen + 1;
    // n_cols（LNE）+ 列类型数组
    let n_cols = read_lne(body, &mut pos)? as usize;
    // P4a T1 fuzz 红钉（种子 tm_ncols_overflow）：0xFE 8B LNE 可声明
    // n_cols = u64::MAX，`pos + n_cols` usize 加溢出即 panic（debug/fuzz
    // profile）/ 回绕成错误切片（release）。溢出 ⇒ types 区必然越界，同 TooShort。
    let types_end = pos.checked_add(n_cols).ok_or(BinlogError::TooShort)?;
    let column_type = body
        .get(pos..types_end)
        .ok_or(BinlogError::TooShort)?
        .to_vec();
    pos = types_end;
    // metadata：LNE 总长 + 逐列 meta（对照 go-mysql LengthEncodedString + decodeMeta）
    let meta_bytes = read_lns(body, &mut pos)?;
    let column_meta = decode_meta(meta_bytes, &column_type)?;
    // null_bits bitmap
    let bw = bit_width(n_cols);
    let null_bits = body
        .get(pos..pos + bw)
        .ok_or(BinlogError::TooShort)?
        .to_vec();
    pos += bw;
    // 可选尾部：legacy WL#6494 charset 数组或 8.0 TLV optional metadata（见 parse_charset）
    let charset = parse_charset(&body[pos..], n_cols)?;
    Ok(TableMapEvent {
        table_id,
        schema,
        table,
        n_cols,
        column_type,
        column_meta,
        null_bits,
        charset,
    })
}

/// 逐列 metadata 解码，长度表对照 go-mysql `TableMapEvent.decodeMeta`：
/// STRING/NEWDECIMAL 为 2B「高字节在前」对（真实类型/精度 + 长度）；
/// VAR_STRING/VARCHAR/BIT 为 2B 小端；BLOB/FLOAT/DOUBLE/GEOMETRY/JSON/时间2 族为 1B；
/// 其余整型等 0 字节。参考实现对 ENUM/SET/TINY_BLOB 等报 unsupported，本项目按
/// 任务简报「rest 0」处理（binlog 实际不会以这些类型码出现在 column_type 中）。
fn decode_meta(data: &[u8], types: &[u8]) -> Result<Vec<u16>, BinlogError> {
    let mut out = Vec::with_capacity(types.len());
    let mut p = 0usize;
    for &t in types {
        let m: u16 = match t {
            tp::STRING | tp::NEWDECIMAL => {
                let s = data.get(p..p + 2).ok_or(BinlogError::TooShort)?;
                p += 2;
                ((s[0] as u16) << 8) | s[1] as u16
            }
            tp::VAR_STRING | tp::VARCHAR | tp::BIT => {
                let s = data.get(p..p + 2).ok_or(BinlogError::TooShort)?;
                p += 2;
                u16::from_le_bytes([s[0], s[1]])
            }
            tp::BLOB
            | tp::FLOAT
            | tp::DOUBLE
            | tp::GEOMETRY
            | tp::JSON
            | tp::TIME2
            | tp::DATETIME2
            | tp::TIMESTAMP2 => {
                let v = *data.get(p).ok_or(BinlogError::TooShort)?;
                p += 1;
                v as u16
            }
            _ => 0,
        };
        out.push(m);
    }
    Ok(out)
}

/// 解析 null_bits 之后的可选尾部，两形态分流（T9 校准，fixture 真机件回归钉死）：
///
/// 1. **legacy WL#6494 字符集数组**（5.6.3~5.7 简报形态）：1B 内容总长（=255
///    转义为后随 2B LE）且**恰好覆盖全部剩余字节**、内容为恰好 `n_cols` 条
///    完整 LNE——命中即按此解析（覆盖恰好但条数/截断不符 → 仍按 legacy 报错，
///    不回退）；零字节 → `Ok(空)`（pre-8.0 或无 metadata）。
/// 2. **8.0 optional metadata TLV 流**：其余一律按 TLV 迭代（见
///    [`decode_optional_meta`]）。注意：**无前导 2B total_length**——旧本文档
///    及 Task 4 台账「2B LE total_length + TLV」的描述有误；vendored fork
///    `decodeOptionalMeta`（row_event.go:241-321）直接从 null_bits 后迭代
///    `[type 1B][len LNE][payload]` 直到 body 末尾，真机 8.0.46 fixture 证实。
fn parse_charset(rest: &[u8], n_cols: usize) -> Result<Vec<u64>, BinlogError> {
    // 零尾随字节：合法（pre-8.0 或 metadata 段缺失）。
    if rest.is_empty() {
        return Ok(Vec::new());
    }
    let invalid = |why: String| {
        BinlogError::InvalidData(format!(
            "unsupported table_map optional metadata / charset section: {why}"
        ))
    };
    // legacy 形态判定：长度前缀声明的总长恰好覆盖 rest。
    if rest[0] != 255 {
        let total = rest[0] as usize;
        if total + 1 == rest.len() {
            return legacy_charset_values(rest, 1, total, n_cols, invalid);
        }
    } else if rest.len() >= 3 {
        let total = u16::from_le_bytes([rest[1], rest[2]]) as usize;
        if total + 3 == rest.len() {
            return legacy_charset_values(rest, 3, total, n_cols, invalid);
        }
    }
    // 未命中 legacy 精确覆盖 → 8.0 TLV 流（含 255 转义头不足 3B 的畸形件）。
    decode_optional_meta(rest, n_cols, invalid)
}

/// legacy 精确覆盖段的逐列 LNE 解码：条数必须恰为 `n_cols`，截断报错。
fn legacy_charset_values(
    rest: &[u8],
    hdr: usize,
    total: usize,
    n_cols: usize,
    invalid: impl Fn(String) -> BinlogError,
) -> Result<Vec<u64>, BinlogError> {
    let vals = &rest[hdr..hdr + total];
    let mut out = Vec::with_capacity(n_cols);
    let mut q = 0usize;
    while q < vals.len() {
        let v = read_lne(vals, &mut q)
            .map_err(|_| invalid(format!("truncated length-encoded integer at byte {q}")))?;
        out.push(v);
    }
    if out.len() != n_cols {
        return Err(invalid(format!(
            "charset holds {} collation ids, expected n_cols = {n_cols}",
            out.len()
        )));
    }
    Ok(out)
}

/// optional-metadata 的 LNE 整数序列（fork `decodeIntSeq`）：截断即错。
fn tlv_int_seq(v: &[u8]) -> Result<Vec<u64>, BinlogError> {
    let mut out = Vec::new();
    let mut q = 0usize;
    while q < v.len() {
        out.push(read_lne(v, &mut q)?);
    }
    Ok(out)
}

/// fork `decodeStrValue` 形态：若干「(LNE 值个数 + 该数个 LNE 字符串)」组；仅校验结构。
fn tlv_str_values(v: &[u8]) -> Result<(), BinlogError> {
    let mut q = 0usize;
    while q < v.len() {
        let n = read_lne(v, &mut q)?;
        for _ in 0..n {
            read_lns(v, &mut q)?;
        }
    }
    Ok(())
}

/// fork `decodeColumnNames` 形态：逐列「1B 长度 + 名字」，条数必须等于 n_cols。
fn tlv_column_names(v: &[u8], n_cols: usize) -> Result<(), BinlogError> {
    let mut q = 0usize;
    let mut count = 0usize;
    while q < v.len() {
        let l = *v.get(q).ok_or(BinlogError::TooShort)? as usize;
        q += 1;
        v.get(q..q + l).ok_or(BinlogError::TooShort)?;
        q += l;
        count += 1;
    }
    if count != n_cols {
        return Err(BinlogError::InvalidData(format!(
            "expect {n_cols} column names but got {count}"
        )));
    }
    Ok(())
}

/// fork `decodePrimaryKeyWithPrefix` 形态：成对 LNE（列号+前缀长）；截断即错。
fn tlv_pk_pairs(v: &[u8]) -> Result<(), BinlogError> {
    let mut q = 0usize;
    while q < v.len() {
        read_lne(v, &mut q)?;
        read_lne(v, &mut q)?;
    }
    Ok(())
}

/// 8.0 TABLE_MAP optional metadata TLV 循环，镜像 vendored fork
/// `TableMapEvent.decodeOptionalMeta`（row_event.go:241-321）。逐条
/// `[type 1B][len LNE][payload]` 直到 rest 末尾：
/// - **#1 signedness**：仅消费、不存储——P1 裁定 unsigned 判定来自 schema DDL
///   （Task 11），结构体保持无该字段（真机 fixture #1=`01 01 40` 已验证消费；P2 复议）；
/// - **#2/#10 default charset**：LNE 序列且项数须为奇数（fork decodeDefaultCharset
///   的表默认+成对校验），校验后丢弃；**#3 column charset** 同 legacy 语义存入
///   `charset` 返回；#7/#8/#11 仅结构校验（decodeIntSeq），#9 成对校验，
///   #4/#5/#6 结构校验（列名条数 = n_cols / str-value 组）；
/// - **未知 type**：跳过——vendored fork 无 `ignoreUnknownOptMetadata` 开关，
///   default 臂即 "Ignore for future extension"（:317），且 my2sql-go 从未调用
///   任何 ignore/opt-meta 相关设置（SetIgnoreJSONDecodeError 亦未用），参考工具
///   有效行为 = 未知条目消费忽略，本侧照此裁定；
/// - **任何截断**（长度前缀或 payload 越出末尾、已知字段畸形）→ InvalidData：
///   fork 对越界同样崩溃/报错，D5 禁止对残段猜测。
fn decode_optional_meta(
    rest: &[u8],
    n_cols: usize,
    invalid: impl Fn(String) -> BinlogError,
) -> Result<Vec<u64>, BinlogError> {
    const SIGNEDNESS: u8 = 1;
    const DEFAULT_CHARSET: u8 = 2;
    const COLUMN_CHARSET: u8 = 3;
    const COLUMN_NAME: u8 = 4;
    const SET_STR_VALUE: u8 = 5;
    const ENUM_STR_VALUE: u8 = 6;
    const GEOMETRY_TYPE: u8 = 7;
    const SIMPLE_PRIMARY_KEY: u8 = 8;
    const PRIMARY_KEY_WITH_PREFIX: u8 = 9;
    const ENUM_SET_DEFAULT_CHARSET: u8 = 10;
    const ENUM_SET_COLUMN_CHARSET: u8 = 11;

    let mut pos = 0usize;
    let mut charset = Vec::new();
    while pos < rest.len() {
        let t = rest[pos];
        pos += 1;
        let l = read_lne(rest, &mut pos)
            .map_err(|_| invalid(format!("TLV type {t}: truncated length prefix")))?
            as usize;
        // P4a T1 fuzz 红钉（种子 tm_tlv_len_overflow）：payload 长 0xFE 8B
        // 可声明 u64::MAX，`pos + l` usize 加溢出 panic——溢出 ⇒ payload
        // 必然越出 rest 末尾，与截断同口径报 InvalidData（D5 不猜残段）。
        let end = pos
            .checked_add(l)
            .ok_or_else(|| invalid(format!("TLV type {t}: payload length {l} out of bounds")))?;
        let v = rest.get(pos..end).ok_or_else(|| {
            invalid(format!(
                "TLV type {t}: truncated payload, need {l} have {}",
                rest.len() - pos
            ))
        })?;
        pos = end;
        let checked: Result<(), BinlogError> = match t {
            SIGNEDNESS => Ok(()), // 消费即弃（P1 裁定，见函数文档）
            DEFAULT_CHARSET | ENUM_SET_DEFAULT_CHARSET => {
                let seq = tlv_int_seq(v)?;
                if seq.len() % 2 != 1 {
                    return Err(invalid(format!(
                        "default charset (type {t}) expects odd items, got {}",
                        seq.len()
                    )));
                }
                Ok(())
            }
            COLUMN_CHARSET => {
                charset = tlv_int_seq(v)?;
                Ok(())
            }
            COLUMN_NAME => tlv_column_names(v, n_cols),
            SET_STR_VALUE | ENUM_STR_VALUE => tlv_str_values(v),
            GEOMETRY_TYPE | SIMPLE_PRIMARY_KEY | ENUM_SET_COLUMN_CHARSET => {
                tlv_int_seq(v).map(|_| ())
            }
            PRIMARY_KEY_WITH_PREFIX => tlv_pk_pairs(v),
            // 未知类型：消费并忽略（fork default 臂，参考工具有效行为）。
            _ => Ok(()),
        };
        checked.map_err(|e| match e {
            BinlogError::InvalidData(msg) => invalid(msg),
            other => invalid(format!("TLV type {t}: {other}")),
        })?;
    }
    Ok(charset)
}

/// MYSQL_TYPE_STRING(0xFE) 的“真实类型”还原（对齐 go-mysql/sqlgen 行为）：
/// binlog 把 ENUM/SET/NEWDECIMAL 伪装成 STRING 写入，真实类型存于 meta 高字节；
/// `b0 & 0x30 != 0x30` 时补 `| 0x30`（如 ENUM meta 高字节 0x06 → 0x36），否则原样。
/// 非 STRING 类型或 meta < 256（无高字节）时原样返回 `tp`。
pub fn real_string_type(tp: u8, meta: u16) -> u8 {
    if tp == tp::STRING && meta >= 256 {
        let b0 = (meta >> 8) as u8;
        if b0 & 0x30 != 0x30 { b0 | 0x30 } else { b0 }
    } else {
        tp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 最小 3 列 body：LONG(0x03) + VAR_STRING(0xFD,meta 20 LE) + BLOB(0xFC,meta 1)。
    fn body_minimal() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&[0x41, 0, 0, 0, 0, 0]); // table_id = 65（6B LE）
        b.extend_from_slice(&[0x00, 0x00]); // flags
        b.push(4);
        b.extend_from_slice(b"test");
        b.push(0x00); // schema 结束 0
        b.push(3);
        b.extend_from_slice(b"tb1");
        b.push(0x00); // table 结束 0
        b.push(3); // n_cols LNE
        b.extend_from_slice(&[0x03, tp::VAR_STRING, tp::BLOB]); // column_types
        b.push(3); // metadata 长度 LNE
        b.extend_from_slice(&[20, 0x00, 1]); // VAR_STRING 2B LE=20；BLOB 1B=1
        b.push(0b0000_0000); // null_bits bitmap（3 列 → 1 字节）
        b
    }

    #[test]
    fn parses_minimal_body() {
        let e = parse_table_map(&body_minimal(), false).unwrap();
        assert_eq!(e.table_id, 0x41);
        assert_eq!((e.schema.as_str(), e.table.as_str()), ("test", "tb1"));
        assert_eq!(e.n_cols, 3);
        assert_eq!(e.column_type, vec![0x03, tp::VAR_STRING, tp::BLOB]);
        assert_eq!(e.column_meta, vec![0, 20, 1]);
        assert_eq!(e.null_bits, vec![0]);
        assert_eq!(e.charset, Vec::<u64>::new());
    }

    #[test]
    fn with_crc_strips_4b_tail_before_fields() {
        let mut b = body_minimal();
        b.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]); // 假 CRC 尾
        let e = parse_table_map(&b, true).unwrap();
        assert_eq!(e, parse_table_map(&body_minimal(), false).unwrap());
    }

    #[test]
    fn string_meta_is_big_endian_pair_and_decodable() {
        // 2 列：STRING(0xFE) meta 高字节真实类型 0xF6(NEWDECIMAL) + 长度 6；TIMESTAMP2 1B meta=3
        let mut b = Vec::new();
        b.extend_from_slice(&[0x01, 0, 0, 0, 0, 0]);
        b.extend_from_slice(&[0, 0]);
        b.push(2);
        b.extend_from_slice(b"d2");
        b.push(0);
        b.push(1);
        b.extend_from_slice(b"t");
        b.push(0);
        b.push(2); // n_cols
        b.extend_from_slice(&[tp::STRING, tp::TIMESTAMP2]);
        b.push(3); // meta 长 LNE：2B STRING + 1B TS2
        b.extend_from_slice(&[0xF6, 0x06, 3]);
        b.push(0); // null_bits
        let e = parse_table_map(&b, false).unwrap();
        assert_eq!(e.column_meta, vec![0xF606, 3]);
    }

    #[test]
    fn short_body_is_too_short() {
        assert_eq!(
            parse_table_map(&[0u8; 6], false).unwrap_err(),
            BinlogError::TooShort
        );
        // 声称 3 列但 types 区不足
        let mut b = body_minimal();
        b.truncate(b.len() - 3);
        assert!(parse_table_map(&b, false).is_err());
        // with_crc 但总长 < 4
        assert_eq!(
            parse_table_map(&[1, 2, 3], true).unwrap_err(),
            BinlogError::TooShort
        );
    }

    /// 字符集段形态 1：1B 长度前缀 + 恰好 n_cols(3) 条 LNE（255 以 0xFC 前缀编码）。
    #[test]
    fn charset_section_one_byte_length() {
        let mut b = body_minimal();
        b.push(5); // charset 内容总长 5B：1B(45) + 3B(0xFC 255) + 1B(46)
        b.push(45); // LNE 45
        b.extend_from_slice(&[0xFC, 0xFF, 0x00]); // LNE 255（2 字节前缀）
        b.push(46); // LNE 46
        let e = parse_table_map(&b, false).unwrap();
        assert_eq!(e.charset, vec![45, 255, 46]);
    }

    /// 字符集段形态 2：前缀字节 ==255 转义 → 后随 2B LE 总长；恰好 n_cols(3) 条 LNE。
    #[test]
    fn charset_section_two_byte_escaped_length() {
        let mut b = body_minimal();
        b.push(255); // 转义标记
        b.extend_from_slice(&[3, 0x00]); // 总长 3B LE
        b.extend_from_slice(&[45, 46, 250]); // 三条 LNE
        let e = parse_table_map(&b, false).unwrap();
        assert_eq!(e.charset, vec![45, 46, 250]);
    }

    /// D5 约束：不支持的元数据必须报错而非猜测。声称总长 9 但只剩 2 字节——
    /// 截断的 WL#6494 段（也可能是 8.0 TLV 被误读），必须 Err 而非静默置空。
    #[test]
    fn charset_truncated_is_error() {
        let mut b = body_minimal();
        b.push(9); // 声称 9 字节内容，实际只剩 2
        b.extend_from_slice(&[45, 46]);
        let err = parse_table_map(&b, false).unwrap_err();
        match &err {
            BinlogError::InvalidData(msg) => {
                assert!(
                    msg.contains("unsupported table_map"),
                    "error message must name the unrecognized section, got: {msg}"
                );
            }
            other => panic!("expected InvalidData, got {other:?}"),
        }
    }

    /// 段完整但 LNE 条数 != n_cols：不是合法的 WL#6494 charset，报错。
    #[test]
    fn charset_wrong_collation_count_is_error() {
        let mut b = body_minimal();
        b.push(2); // 总长 2B，但只有 2 条，n_cols=3
        b.extend_from_slice(&[45, 46]);
        assert!(parse_table_map(&b, false).is_err());
    }

    /// legacy 声明总长未覆盖全部尾部、且尾部也无法按 TLV 规则消费（如截断的
    /// 8.0 opt-meta 条目/垃圾字节）：宽松读法会静默吞掉尾随字节，必须报错。
    #[test]
    fn charset_trailing_bytes_is_error() {
        let mut b = body_minimal();
        b.push(3); // 恰好 3 条 LNE 的合法 charset……
        b.extend_from_slice(&[45, 46, 250]);
        b.extend_from_slice(&[0x06, 0x00]); // ……后面又跟 2 字节：legacy 覆盖判定失败，
        // TLV 路径把首字节 3 当 type#3、45 当 payload 长 → 截断报错，尾部不被吞掉
        assert!(parse_table_map(&b, false).is_err());
    }

    /// 段内某条 LNE 中途截断（0xFC 两字节前缀只剩 1 字节）：报错，不返回残缺前缀。
    #[test]
    fn charset_truncated_lne_inside_is_error() {
        let mut b = body_minimal();
        b.push(3); // 总长 3B
        b.extend_from_slice(&[45, 46, 0xFC]); // 第三条 0xFC 需要 2B 值，但段已结束
        assert!(parse_table_map(&b, false).is_err());
    }

    /// 255 转义标记后不足 3 字节头：报错。
    #[test]
    fn charset_escape_marker_without_length_is_error() {
        let mut b = body_minimal();
        b.push(255); // 转义标记，但没有后随 2B 总长
        assert!(parse_table_map(&b, false).is_err());
    }

    /// T9 校准（推翻原「8.0 TLV 必须拒绝」测试）：fork `decodeOptionalMeta`
    /// 实测**无**前导 2B total_length，TLV 一直迭代到 body 末尾；合法流必须
    /// Ok。用例覆盖：#1 signedness（消费不存储）、#2 default charset（奇数项
    /// 校验通过）、#7 geometry type、未知类型 #12（fork default 臂 ignore）。
    #[test]
    fn eight_zero_opt_meta_tlv_stream_parses_ok() {
        let mut b = body_minimal();
        b.extend_from_slice(&[
            0x01, 0x01, 0x40, // #1 signedness，1B 位图 → 消费（P1 裁定不存）
            0x02, 0x03, 0x2d, 0x2d, 0x00, // #2 default charset：3 项（奇数 ✓）
            0x07, 0x01, 0x00, // #7 geometry type：[0]
            0x0c, 0x01, 0x2a, // 未知类型 12 → ignore（fork:317）
        ]);
        let e = parse_table_map(&b, false).unwrap();
        // 非字符列语义字段一律不落位；charset 仅 #3 填充
        assert_eq!(e.charset, Vec::<u64>::new());
        assert_eq!(e.n_cols, 3);
    }

    /// #3 column charset 是逐列排序表，与 legacy WL#6494 数组同语义 →
    /// 存入 `charset` 字段。
    #[test]
    fn eight_zero_opt_meta_column_charset_stored() {
        let mut b = body_minimal();
        b.extend_from_slice(&[0x03, 0x03, 0x2d, 0x2e, 0xfa]); // #3：LNE 序列 [45,46,250]
        let e = parse_table_map(&b, false).unwrap();
        assert_eq!(e.charset, vec![45, 46, 250]);
    }

    /// fork decodeDefaultCharset：#2 项数必须为奇数（表默认 + 逐列成对），
    /// 偶数 → 报错（已知字段畸形不得静默）。
    #[test]
    fn eight_zero_opt_meta_even_default_charset_is_error() {
        let mut b = body_minimal();
        b.extend_from_slice(&[0x02, 0x02, 0x2d, 0x2e]); // 2 项，偶数
        assert!(parse_table_map(&b, false).is_err());
    }

    /// 截断 TLV（payload 越出 body 末尾 / 长度前缀不完整）：报错——
    /// 对应 fork 的越界 panic/err 口径（D5：截断必须报错）。
    #[test]
    fn eight_zero_opt_meta_truncated_is_error() {
        let mut b = body_minimal();
        b.extend_from_slice(&[0x01, 0x04, 0x40]); // #1 声称 4B payload，只剩 1B
        assert!(parse_table_map(&b, false).is_err());
        let mut c = body_minimal();
        c.extend_from_slice(&[0x02]); // 连长度前缀都没有
        assert!(parse_table_map(&c, false).is_err());
    }

    /// T9 校准回归（fixture 真机件）：8.0.46 MINIMAL 真实 TABLE_MAP body——
    /// null_bits 后 TLV `01 01 40 | 02 0d fc ff 00 00 0b 08 3f 09 3f 0a 3f 0b 3f | 07 01 00`
    /// （#1 有符号性 / #2 默认字符集 11 项奇数 / #7 几何类型）解析为 Ok 且逐字段
    /// 与审阅核实值一致。此件是旧严格 charset 解析器直接拒绝的真机流。
    #[test]
    fn fixture_8_0_table_map_parses_with_real_tlv_opt_meta() {
        use crate::binlog::event::parse_header;
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/capture_8.0_minimal/mysql-bin.000003"
        );
        let data = std::fs::read(path).expect("fixture must be committed");
        let mut pos = 4usize; // 跳过 \xfebin magic
        let mut body: Option<&[u8]> = None;
        while pos < data.len() {
            let h = parse_header(&data[pos..]).unwrap();
            let size = h.event_size as usize;
            if h.event_type.0 == 19 {
                body = Some(&data[pos + super::super::event::EVENT_HEADER_SIZE..pos + size]);
                break;
            }
            pos += size;
        }
        let e = parse_table_map(body.expect("fixture must contain TABLE_MAP"), true).unwrap();
        assert_eq!(e.table_id, 0x55);
        assert_eq!((e.schema.as_str(), e.table.as_str()), ("t9", "probe"));
        assert_eq!(e.n_cols, 26);
        assert_eq!(
            e.column_type,
            vec![
                0xFE, 0xFE, 0x0F, 0x0F, 0xFE, 0xFE,
                0xF6, // char×2, varchar×2, char(enum/set 伪装), decimal
                0x11, 0x11, 0x12, 0x13, // ts, ts3, dt, t（*2 族）
                0x0D, 0x04, 0x05, 0x10, // year, float, double, bit
                0xFC, 0xFC, 0xFC, 0xFC, 0xFC, 0xFC, 0xFC, 0xFC, // text×4 + blob×4
                0xF5, 0xFF, 0x0F, // json, geometry, varchar(nul)
            ]
        );
        // 审阅核实块：fe 05 / fe 14 / c8 00 / e0 2e / f7 01 / f8 01 / 05 02 …
        assert_eq!(
            e.column_meta,
            vec![
                0xFE05, 0xFE14, 200, 12_000, 0xF701, 0xF801, 0x0502, // 字符串族+decimal
                0, 3, 0, 0, // ts0, ts3, dt, t
                0, 4, 8, 0x0101, // year, float, double, bit(9)
                1, 2, 3, 4, 1, 2, 3, 4, // tinytext..longtext, tinyblob..longblob
                4, 4, 40, // json, geometry, varchar(10)→40
            ]
        );
        assert_eq!(e.null_bits, vec![0xFF, 0xFF, 0xFF, 0x03]);
        // MINIMAL 只带 #1/#2/#7，无 #3 column charset → charset 保持空
        assert_eq!(e.charset, Vec::<u64>::new());
    }

    /// brief Step2 指定两例：0x06 高字节补成 0x36（ENUM）；0xF6 原样透传（NEWDECIMAL）。
    #[test]
    fn real_string_type_known_pairs() {
        assert_eq!(real_string_type(0xFE, (0x06 << 8) | 2), 0x36);
        assert_eq!(real_string_type(0xFE, (0xF6 << 8) | 6), 0xF6);
        // 非 STRING 类型不改动；STRING 但 meta<256（无高字节）原样返回
        assert_eq!(real_string_type(0x03, (0x06 << 8) | 2), 0x03);
        assert_eq!(real_string_type(0xFE, 6), 0xFE);
    }
}
