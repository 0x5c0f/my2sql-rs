//! 定宽列值解码：整型族（TINY/SHORT/LONG/INT24/LONGLONG）+ BIT/YEAR/FLOAT/DOUBLE
//! 特例，以及全链路共用的 [`ColumnValue`] 枚举。
//!
//! 行为对照 go-mysql `replication/row_event.go`：整型走 `ParseBinaryInt*`（LE +
//! 符号扩展）、BIT 走 `decodeBit`（按 meta 推字节数、**大端**取值——等价于简报
//! 所说「读出后字节逆序再按 LE 解释」）、FLOAT/DOUBLE 走 `ParseBinaryFloat*`
//! （IEEE754 LE）。unsigned 不在 TABLE_MAP 中（Task 4 结论），由调用方（Task 9
//! 结合 schema）以 `unsigned: bool` 传入。
//!
//! YEAR 布局：简报称「2 字节 LE 直存年份」，但 go-mysql 与真机 binlog 实测
//! （docker mysql:8.0.46 / mysql:5.7.44，含 BIT(9)/FLOAT/DOUBLE 同表探针）均为
//! **1 字节（年份−1900）**，2 字节是 MariaDB 变体（D5 不支持）——按权威实现 1B。

// 骨架阶段本模块尚无生产消费者（Task 9/10 接入），参照 Task 1-4 允许死代码。
#![allow(dead_code)]

use super::error::BinlogError;
// 类型码统一取自 super::field_types（T10 Step 0 合并，原私有 `mod tp` 删除）。
use super::field_types as tp;

/// 单列解码结果的全链路载体（后续任务按此精确匹配/消费）。
///
/// - `Null`：NULL bitmap 命中或 MYSQL_TYPE_NULL；
/// - `Int`/`UInt`：整型族 + BIT/YEAR（unsigned 列与 BIT 一律 `UInt`）；
/// - `Double`：FLOAT/DOUBLE 的最短往返十进制文本（Task 8 JSON 的数值也走此形态）；
/// - `Decimal`：NEWDECIMAL 精确文本（Task 7）；
/// - `Str`：字符集列原始字节（utf8 校验后保留，Task 9）；
/// - `Bytes`：二进制列（BLOB/GEOMETRY/降级后的 Str）；
/// - `Json`：json_binary 解码后的紧凑文本（Task 8）；
/// - `Missing`：rows 事件列裁剪（partial/cropped）产生的缺列（Task 10）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnValue {
    Null,
    Int(i64),
    UInt(u64),
    Double(String),
    Decimal(String),
    Str(Vec<u8>),
    Bytes(Vec<u8>),
    Json(String),
    Missing,
}

/// 定宽整型 + BIT/YEAR 解码。`tp` 为 MYSQL_TYPE_* 列类型码；`unsigned` 来自
/// schema 层；`meta` 为 TABLE_MAP 该列 metadata（BIT 的位宽信息只存在于 meta，
/// 故本入口带 meta 参数——简报签名未列，属必要偏差，详见模块注释与交接文档）。
///
/// 读长：TINY 1B、SHORT 2B、INT24 3B、LONG 4B、LONGLONG 8B，均小端；signed 用
/// `^ (1<<(w*8-1))` 翻回符号位后按 i64 解释；YEAR 1B，输出 `年份+1900`（0 原样）；
/// BIT 长度 `n=ceil(nbits/8)`、`nbits=(meta>>8)*8+(meta&0xFF)`，大端取值输出 `UInt`。
pub fn decode_int(
    buf: &[u8],
    pos: &mut usize,
    tp: u8,
    unsigned: bool,
    meta: u16,
) -> Result<ColumnValue, BinlogError> {
    // 定宽整型：TINY 1B / SHORT 2B / INT24 3B / LONG 4B / LONGLONG 8B，均小端
    // （对照 go-mysql ParseBinaryInt8/16/24/32/64）。
    let w = match tp {
        tp::TINY => 1usize,
        tp::SHORT => 2,
        tp::INT24 => 3,
        tp::LONG => 4,
        tp::LONGLONG => 8,
        tp::YEAR => {
            // 真机实测（8.0.46/5.7.44 binlog + go-mysql `decodeValue` 一致）：
            // YEAR 为 **1 字节**，存「年份 − 1900」，0 表示 '0000'。
            // 简报「2 字节 LE 直存年份」与两处权威均不符（2B 是 MariaDB 变体，
            // D5 不支持 MariaDB），按实测实现——见 task-5 报告与 HANDOVER。
            let y = read_le(buf, *pos, 1)?;
            *pos += 1;
            return Ok(ColumnValue::UInt(if y == 0 { 0 } else { y + 1900 }));
        }
        tp::BIT => {
            // meta 编码 (满字节数<<8 | 不足一字节的余 bit 数)：
            // nbits=(meta>>8)*8+(meta&0xFF)，存储长 n=ceil(nbits/8)，**大端**取值
            // （等价简报口径「字节逆序后按 LE 解释」；对照 go-mysql `decodeBit`）。
            let nbits = ((meta >> 8) as usize) * 8 + (meta & 0xFF) as usize;
            let n = nbits.div_ceil(8);
            if n == 0 || n > 8 {
                return Err(BinlogError::InvalidData(format!(
                    "invalid BIT metadata: meta=0x{meta:04X} implies {n} storage bytes"
                )));
            }
            let s = buf.get(*pos..*pos + n).ok_or(BinlogError::TooShort)?;
            let mut v = 0u64;
            for &b in s {
                v = (v << 8) | b as u64;
            }
            *pos += n;
            return Ok(ColumnValue::UInt(v));
        }
        _ => {
            return Err(BinlogError::InvalidData(format!(
                "decode_int: unsupported column type {tp}"
            )));
        }
    };
    let v = read_le(buf, *pos, w)?;
    *pos += w;
    if unsigned {
        Ok(ColumnValue::UInt(v))
    } else {
        // 简报口径：`^ (1<<(w*8-1))` 还原符号位后按 i 解释（等价 w 字节符号扩展）。
        let mask = 1u64 << (w * 8 - 1);
        Ok(ColumnValue::Int(
            ((v ^ mask) as i64).wrapping_sub(mask as i64),
        ))
    }
}

/// 从 `buf[pos..]` 读 `w` 字节小端整数；不足则 [`BinlogError::TooShort`]（不推进 pos）。
fn read_le(buf: &[u8], pos: usize, w: usize) -> Result<u64, BinlogError> {
    let s = buf.get(pos..pos + w).ok_or(BinlogError::TooShort)?;
    let mut v = 0u64;
    for (i, &b) in s.iter().enumerate() {
        v |= (b as u64) << (8 * i);
    }
    Ok(v)
}

/// FLOAT（4B f32）/ DOUBLE（8B f64）定宽解码，IEEE754 小端；输出
/// [`ColumnValue::Double`]，文本为「可往返的最短十进制表示」（Rust float Display
/// 即 shortest-roundtrip），不补零、不用科学计数（与 MySQL 常规显示一致）。
pub fn decode_float(buf: &[u8], pos: &mut usize, tp: u8) -> Result<ColumnValue, BinlogError> {
    match tp {
        tp::FLOAT => {
            let s = buf.get(*pos..*pos + 4).ok_or(BinlogError::TooShort)?;
            *pos += 4;
            Ok(ColumnValue::Double(
                f32::from_le_bytes(s.try_into().unwrap()).to_string(),
            ))
        }
        tp::DOUBLE => {
            let s = buf.get(*pos..*pos + 8).ok_or(BinlogError::TooShort)?;
            *pos += 8;
            Ok(ColumnValue::Double(
                f64::from_le_bytes(s.try_into().unwrap()).to_string(),
            ))
        }
        _ => Err(BinlogError::InvalidData(format!(
            "decode_float: unsupported column type {tp}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::tp::*;
    use super::*;
    use proptest::prelude::*;

    /// 辅助：从偏移 0 解码，断言读完全部字节。
    fn di(buf: &[u8], tp: u8, unsigned: bool, meta: u16) -> ColumnValue {
        let mut pos = 0usize;
        let v = decode_int(buf, &mut pos, tp, unsigned, meta).unwrap();
        assert_eq!(pos, buf.len(), "decode_int must consume exactly {tp} bytes");
        v
    }

    fn df(buf: &[u8], tp: u8) -> ColumnValue {
        let mut pos = 0usize;
        let v = decode_float(buf, &mut pos, tp).unwrap();
        assert_eq!(
            pos,
            buf.len(),
            "decode_float must consume exactly {tp} bytes"
        );
        v
    }

    // ---- Step 1（简报指定用例） ----

    #[test]
    fn tiny_signed_sign_extends() {
        // [0x80] → -128；[0xFF] → -1；[0x7F] → 127
        assert_eq!(di(&[0x80], TINY, false, 0), ColumnValue::Int(-128));
        assert_eq!(di(&[0xFF], TINY, false, 0), ColumnValue::Int(-1));
        assert_eq!(di(&[0x7F], TINY, false, 0), ColumnValue::Int(127));
        assert_eq!(di(&[0x00], TINY, false, 0), ColumnValue::Int(0));
    }

    #[test]
    fn tiny_unsigned_is_u_int() {
        assert_eq!(di(&[0xFF], TINY, true, 0), ColumnValue::UInt(255));
        assert_eq!(di(&[0x80], TINY, true, 0), ColumnValue::UInt(128));
    }

    #[test]
    fn short_le_two_bytes() {
        // 0x1234 LE = [0x34,0x12]；signed 负值 0xFFFF → -1
        assert_eq!(di(&[0x34, 0x12], SHORT, false, 0), ColumnValue::Int(0x1234));
        assert_eq!(di(&[0xFF, 0xFF], SHORT, false, 0), ColumnValue::Int(-1));
        assert_eq!(di(&[0xFF, 0xFF], SHORT, true, 0), ColumnValue::UInt(65535));
    }

    #[test]
    fn long_le_four_bytes() {
        assert_eq!(
            di(&[0x01, 0x02, 0x03, 0x04], LONG, false, 0),
            ColumnValue::Int(0x0403_0201)
        );
        assert_eq!(
            di(&[0xFF, 0xFF, 0xFF, 0x7F], LONG, false, 0),
            ColumnValue::Int(i32::MAX as i64)
        );
        assert_eq!(
            di(&[0xFF, 0xFF, 0xFF, 0xFF], LONG, true, 0),
            ColumnValue::UInt(u32::MAX as u64)
        );
    }

    #[test]
    fn int24_signed_and_unsigned_extremes() {
        // [0x00,0x00,0x80] signed → -8388608；unsigned → 8388608
        assert_eq!(
            di(&[0x00, 0x00, 0x80], INT24, false, 0),
            ColumnValue::Int(-8_388_608)
        );
        assert_eq!(
            di(&[0x00, 0x00, 0x80], INT24, true, 0),
            ColumnValue::UInt(8_388_608)
        );
        // mediumint-unsigned 边界 [0xFF,0xFF,0xFF] → UInt(16777215)
        assert_eq!(
            di(&[0xFF, 0xFF, 0xFF], INT24, true, 0),
            ColumnValue::UInt(16_777_215)
        );
        assert_eq!(
            di(&[0xFF, 0xFF, 0x7F], INT24, false, 0),
            ColumnValue::Int(8_388_607)
        );
    }

    #[test]
    fn longlong_eight_bytes() {
        // LONGLONG signed [0u8;8] → 0
        assert_eq!(di(&[0u8; 8], LONGLONG, false, 0), ColumnValue::Int(0));
        assert_eq!(di(&[0xFF; 8], LONGLONG, false, 0), ColumnValue::Int(-1));
        assert_eq!(
            di(&[0xFF; 8], LONGLONG, true, 0),
            ColumnValue::UInt(u64::MAX)
        );
        assert_eq!(
            di(&1i64.to_le_bytes(), LONGLONG, false, 0),
            ColumnValue::Int(1)
        );
    }

    #[test]
    fn bit_reads_big_endian_after_byte_reversal() {
        // 简报用例：BIT(9bits, 2B) [0x01,0x02]——字节逆序后 [0x02,0x01] 按 LE
        // 解释 = 大端直读 = 0x0102，断言 UInt(0x0102)。
        // meta 编码：(满字节数)<<8 | 余数 bit：bit(9) → (1<<8)|1 = 0x0101。
        assert_eq!(
            di(&[0x01, 0x02], BIT, false, (1 << 8) | 1),
            ColumnValue::UInt(0x0102)
        );
        // bit(1)：meta=(0<<8)|1 → 1 字节
        assert_eq!(di(&[0x01], BIT, false, 1), ColumnValue::UInt(1));
        // bit(8)：meta=(1<<8)|0 → 1 字节
        assert_eq!(di(&[0xAB], BIT, false, 1 << 8), ColumnValue::UInt(0xAB));
        // bit(64) 全 1：meta=(8<<8)|0 → 8 字节大端
        assert_eq!(
            di(&[0xFF; 8], BIT, false, 8 << 8),
            ColumnValue::UInt(u64::MAX)
        );
    }

    #[test]
    fn float_four_bytes_ieee754_le() {
        assert_eq!(
            df(&1.5f32.to_le_bytes(), FLOAT),
            ColumnValue::Double("1.5".into())
        );
        assert_eq!(
            df(&0.0f32.to_le_bytes(), FLOAT),
            ColumnValue::Double("0".into())
        );
        // 最短往返：0.1_f32 Display 为 "0.1"
        assert_eq!(
            df(&0.1f32.to_le_bytes(), FLOAT),
            ColumnValue::Double("0.1".into())
        );
    }

    #[test]
    fn double_eight_bytes_ieee754_le() {
        assert_eq!(
            df(&3.25f64.to_le_bytes(), DOUBLE),
            ColumnValue::Double("3.25".into())
        );
        assert_eq!(
            df(&1e10f64.to_le_bytes(), DOUBLE),
            ColumnValue::Double("10000000000".into())
        );
    }

    #[test]
    fn too_short_buffer_errors() {
        let mut pos = 0usize;
        assert_eq!(
            decode_int(&[0x01, 0x02], &mut pos, LONGLONG, false, 0).unwrap_err(),
            BinlogError::TooShort
        );
        assert_eq!(
            decode_float(&[0x01, 0x02, 0x03], &mut pos, DOUBLE).unwrap_err(),
            BinlogError::TooShort
        );
        // pos 未被污染
        assert_eq!(pos, 0);
    }

    #[test]
    fn unknown_type_code_is_invalid() {
        let mut pos = 0usize;
        assert!(matches!(
            decode_int(&[0u8; 8], &mut pos, 200, false, 0),
            Err(BinlogError::InvalidData(_))
        ));
        assert!(matches!(
            decode_float(&[0u8; 8], &mut pos, LONGLONG),
            Err(BinlogError::InvalidData(_))
        ));
    }

    // ---- YEAR：真机实测用例（8.0.46/5.7.44 docker binlog 字节为 1B「年−1900」，
    // 与 go-mysql `decodeValue` 一致；简报的「2B LE 直存年份」不成立，见报告） ----

    /// 实测行（5.7.44，`INSERT … VALUES (2026, …)`）row 数据首字节 `0x7E`=126。
    #[test]
    fn year_one_byte_plus_1900() {
        assert_eq!(di(&[0x7E], YEAR, false, 0), ColumnValue::UInt(2026));
        assert_eq!(di(&[0x64], YEAR, false, 0), ColumnValue::UInt(2000));
        // 零值 = '0000'：存 0x00，输出 0（对照 go-mysql：year==0 不加 1900）
        assert_eq!(di(&[0x00], YEAR, false, 0), ColumnValue::UInt(0));
        // unsigned 标志不影响 YEAR（YEAR 恒为 UInt，与 go-mysql 恒正一致）
        assert_eq!(di(&[0x01], YEAR, true, 0), ColumnValue::UInt(1901));
    }

    // ---- Step 3：proptest 往返 ----

    proptest! {
        #[test]
        fn longlong_signed_roundtrip(v in any::<i64>()) {
            let b = v.to_le_bytes();
            let mut pos = 0usize;
            let got = decode_int(&b, &mut pos, LONGLONG, false, 0).unwrap();
            prop_assert_eq!(got, ColumnValue::Int(v));
            prop_assert_eq!(pos, 8);
        }

        #[test]
        fn longlong_unsigned_roundtrip(v in any::<u64>()) {
            let b = v.to_le_bytes();
            let mut pos = 0usize;
            let got = decode_int(&b, &mut pos, LONGLONG, true, 0).unwrap();
            prop_assert_eq!(got, ColumnValue::UInt(v));
        }

        #[test]
        fn narrow_signed_roundtrip(x in any::<i32>()) {
            // LONG：i32 全域往返
            let b = x.to_le_bytes();
            let mut pos = 0usize;
            let got = decode_int(&b, &mut pos, LONG, false, 0).unwrap();
            prop_assert_eq!(got, ColumnValue::Int(x as i64));
            // SHORT / TINY 用窄域值
            let s = x as i16;
            let b2 = s.to_le_bytes();
            let mut pos2 = 0usize;
            let got2 = decode_int(&b2, &mut pos2, SHORT, false, 0).unwrap();
            prop_assert_eq!(got2, ColumnValue::Int(s as i64));
            let t = x as i8;
            let mut pos3 = 0usize;
            let got3 = decode_int(&[t as u8], &mut pos3, TINY, false, 0).unwrap();
            prop_assert_eq!(got3, ColumnValue::Int(t as i64));
        }

        #[test]
        fn int24_signed_matches_sign_flip(v in -8_388_608i64..8_388_607i64) {
            let b = [(v as u8), ((v >> 8) as u8), ((v >> 16) as u8)];
            let mut pos = 0usize;
            let got = decode_int(&b, &mut pos, INT24, false, 0).unwrap();
            prop_assert_eq!(got, ColumnValue::Int(v));
        }
    }
}
