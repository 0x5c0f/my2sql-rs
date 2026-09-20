//! TABLE_MAP 事件 body 解码（含 STRING 列真实类型还原）。
//!
//! 行为对照 go-mysql `replication/row_event.go` 的 `TableMapEvent.Decode`
//! （布局：table_id 6B LE + flags 2B + schema_len(1B)+schema+0x00 +
//! table_len(1B)+table+0x00 + n_cols(LNE) + column_types(n_cols×1B) +
//! metadata(LNE 长度 + 每列 meta) + null_bits bitmap + 字符集段(5.6.3 WL#6494，
//! 1B 长度前缀、255 转义为后随 2B LE，内容为逐列 LNE 列表)）。
//! 注：`signed` 不属于本事件——unsigned 判定在 Task 11 由 metadata 层提供。

// 骨架阶段本模块尚无生产消费者（Task 10/12 接入），参照 Task 1-3 允许死代码。
#![allow(dead_code)]

use super::error::BinlogError;
use super::proto::{bit_width, read_lne, read_lns};

/// MySQL 列类型常量（仅本模块 meta 长度表 / real_string_type 所需子集）。
mod tp {
    pub const DOUBLE: u8 = 0x04;
    pub const FLOAT: u8 = 0x05;
    pub const VARCHAR: u8 = 0x0F;
    pub const BIT: u8 = 0x10;
    pub const TIME2: u8 = 0x11;
    pub const DATETIME2: u8 = 0x12;
    pub const TIMESTAMP2: u8 = 0x13;
    pub const NEWDECIMAL: u8 = 0xF6;
    pub const BLOB: u8 = 0xFC;
    pub const VAR_STRING: u8 = 0xFD;
    pub const STRING: u8 = 0xFE;
    pub const GEOMETRY: u8 = 0xFF;
    pub const JSON: u8 = 0xF5;
}

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
    let column_type = body
        .get(pos..pos + n_cols)
        .ok_or(BinlogError::TooShort)?
        .to_vec();
    pos += n_cols;
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
    // 字符集段（可选；宽松解析，EOF 时置空不影响前面字段）
    let charset = parse_charset_lenient(&body[pos..]);
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

/// 宽松解析字符集段（MySQL 5.6.3 WL#6494 布局）：首字节为内容总长，
/// 等于 255 时转义——真实总长取后随 2B 小端；内容为逐列 LNE 整数序列。
/// 字节不足或中途 EOF：停止并返回已解析部分/空表（可选段缺失是合法状态）。
fn parse_charset_lenient(rest: &[u8]) -> Vec<u64> {
    let Some(&first) = rest.first() else {
        return Vec::new();
    };
    let (total, hdr) = if first == 255 {
        match rest.get(1..3) {
            Some(s) => (u16::from_le_bytes([s[0], s[1]]) as usize, 3usize),
            None => return Vec::new(),
        }
    } else {
        (first as usize, 1usize)
    };
    let Some(vals) = rest.get(hdr..hdr + total) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut q = 0usize;
    while q < vals.len() {
        match read_lne(vals, &mut q) {
            Ok(v) => out.push(v),
            // 尾部残缺：保留已读出的 collation id，不视为错误
            Err(_) => break,
        }
    }
    out
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
        assert_eq!(e.charset, Vec::new());
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

    /// 字符集段形态 1：1B 长度前缀 + 逐列 LNE（值 45 与 255）。
    #[test]
    fn charset_section_one_byte_length() {
        let mut b = body_minimal();
        b.push(4); // charset 总长 4B
        b.push(45); // LNE 45
        b.extend_from_slice(&[0xFC, 0xFF, 0x00]); // LNE 255（2 字节前缀）
        let e = parse_table_map(&b, false).unwrap();
        assert_eq!(e.charset, vec![45, 255]);
    }

    /// 字符集段形态 2：前缀字节 ==255 转义 → 后随 2B LE 总长。
    #[test]
    fn charset_section_two_byte_escaped_length() {
        let mut b = body_minimal();
        b.push(255); // 转义标记
        b.extend_from_slice(&[2, 0x00]); // 总长 2B LE
        b.extend_from_slice(&[45, 46]); // 两条 LNE
        let e = parse_table_map(&b, false).unwrap();
        assert_eq!(e.charset, vec![45, 46]);
    }

    /// 可选段读到一半 EOF：优雅停止，先前字段仍有效，charset 置空。
    #[test]
    fn charset_truncated_stops_gracefully() {
        let mut b = body_minimal();
        b.push(9); // 声称 9 字节，但只剩 2 —— charset 放弃，其余字段有效
        b.extend_from_slice(&[45, 46]);
        let e = parse_table_map(&b, false).unwrap();
        assert_eq!(e.n_cols, 3);
        assert_eq!(e.charset, Vec::new());
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
