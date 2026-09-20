//! 精确 DECIMAL（MYSQL_TYPE_NEWDECIMAL）列值解码 → 保真十进制文本。
//!
//! 算式逐项对照 go-mysql `replication/row_event.go` 的 `decodeDecimal` /
//! `decodeDecimalDecompressValue`（含 `compressedBytes` 余数字节表、首字节
//! 高 0x80 符号位、负值**按字节 one's complement**——无 +1 进位，真机抓包
//! 证实，见下），组内数值为大端 u32（满 9 位组）/ 1-4 字节（余数组，仅整数
//! 最高组与小数最低组各一）。纯 u32 数组算术 + 字符串拼接，无浮点、无依赖。
//!
//! 存储格式（MySQL Internals "Binary Format: Decimal And Numeric Types"）：
//! DECIMAL(p,s) 整数部分 `p−s` 位、小数 `s` 位，各按每 9 位一组存 4 字节，
//! 余数 0..8 位存 `digit_bin_len = [0,1,1,2,2,3,3,4,4(,4)]` 字节；符号折叠在
//! **整值首字节**高 0x80（正=1，负=0），负值其余全部字节取反。
//!
//! 输出规则（T15 与 go-mysql 差分比对口径）：小数恒输出 `scale` 位（尾零
//! 保留，"0.00"）；整数部分无多余前导零（"0.05"）；负值 '-' 前缀。
//!
//! 真机行为备案（docker mysql:8.0.46/5.7.44 ROW binlog 抓包，两版本逐字节
//! 一致，见 task-7 报告）：
//! - MySQL **不落盘 −0.00**：`CAST('-0.001' AS DECIMAL(10,2))` 舍入到零时
//!   丢弃符号，存储字节与 `0.00` 相同（`80 00 00 00 00`）；若上游真出现
//!   全取反的负零形态，本实现与 go-mysql 逐式一致输出 "-0.00"
//!   （见 decimal_negative_zero_synthetic 合成用例）。
//! - 简报口径修正：go-mysql 消费点 `prec = meta >> 8; scale = meta & 0xFF`
//!   与本项目 table_map.rs `decode_meta`（STRING/NEWDECIMAL 存 2B 大端对，
//!   高字节=precision）一致，接缝无缝隙；简报提示的 tiny_int_len 表
//!   `[0,1,1,2,2,3,3,4,4]` 为 9 项，权威 `compressedBytes` 为 10 项
//!   （索引 9=4 不可达，因余数恒 0..8），两处数值相同、长度取权威。

// 骨架阶段本模块尚无生产消费者（Task 9 接入），参照 Task 1-6 允许死代码。
#![allow(dead_code)]

use super::error::BinlogError;

/// 每「余数 0..9 位」所需字节数（go-mysql `compressedBytes` / MySQL
/// `digit_bin_len` 表：0→0,1→1,2→1,3→2,4→2,5→3,6→3,7→4,8→4,9→4）。
/// 余数 = 位数 % 9 恒 ∈ 0..8，索引 9 仅为逐字镜像权威表而保留。
const COMPRESSED_BYTES: [usize; 10] = [0, 1, 1, 2, 2, 3, 3, 4, 4, 4];

/// 余数组（1..4 字节）按**大端**逐字节 XOR `mask_byte` 还原（go-mysql
/// `decodeDecimalDecompressValue` 的 case 1..4 逐式）。`s` 长 = 表值。
fn decompress_small(s: &[u8], mask_byte: u8) -> u32 {
    s.iter()
        .fold(0u32, |acc, &b| (acc << 8) | (b ^ mask_byte) as u32)
}

/// DECIMAL 精确解码：`buf[*pos..]` 起读 `bin_size =
/// ints*4 + COMPRESSED_BYTES[intg%9] + fracs*4 + COMPRESSED_BYTES[scale%9]`
/// 字节（符号位折叠在首字节高 0x80，非独立字节），返回保真文本并推进 `pos`。
///
/// - 截断 → [`BinlogError::TooShort`]（`pos` 不动，T5/T6 口径）；
/// - `precision == 0 || precision > 65 || scale > precision`（meta 为 u8，
///   损坏值可达 254；go-mysql 在 precision=0 时越界 panic，此处按控制器
///   裁定报 [`BinlogError::InvalidData`]）。
pub fn decode_decimal(
    buf: &[u8],
    pos: &mut usize,
    precision: u16,
    scale: u16,
) -> Result<String, BinlogError> {
    if precision == 0 || precision > 65 {
        return Err(BinlogError::InvalidData(format!(
            "invalid decimal precision: {precision}"
        )));
    }
    if scale > precision {
        return Err(BinlogError::InvalidData(format!(
            "invalid decimal scale {scale} for precision {precision}"
        )));
    }
    // go-mysql decodeDecimal 逐式：integral = precision - decimals
    let integral = precision - scale;
    let uncomp_int = (integral / 9) as usize; // 满 9 位整数组数
    let comp_int = (integral % 9) as usize; // 整数最高余数组位数 0..8
    let uncomp_fr = (scale / 9) as usize; // 满 9 位小数组数
    let comp_fr = (scale % 9) as usize; // 小数最低余数组位数 0..8
    let bin_size =
        uncomp_int * 4 + COMPRESSED_BYTES[comp_int] + uncomp_fr * 4 + COMPRESSED_BYTES[comp_fr];
    let data = buf
        .get(*pos..*pos + bin_size)
        .ok_or(BinlogError::TooShort)?;

    // Support negative：go-mysql——符号看首字节高 0x80；负值掩码 = 全 1
    // （one's complement 逐字节取反，无 +1）；随后 data[0] ^= 0x80 清符号
    // （该位同样属于首组数值，无论首组是余数组还是满组）。
    let negative = data[0] & 0x80 == 0;
    let mask_word: u32 = if negative { u32::MAX } else { 0 };
    let mask_byte: u8 = if negative { 0xFF } else { 0 };
    let mut work = data.to_vec(); // go-mysql 亦 copy 后改 data[0]
    work[0] ^= 0x80;
    let data = &work[..];

    let mut res = String::with_capacity(precision as usize + 2);
    if negative {
        res.push('-');
    }
    let mut p = 0usize;
    let mut zero_leading = true;
    // 整数最高余数组（无组内左补零，直接输出）
    if COMPRESSED_BYTES[comp_int] > 0 {
        let n = COMPRESSED_BYTES[comp_int];
        let v = decompress_small(&data[p..p + n], mask_byte);
        p += n;
        if v != 0 {
            zero_leading = false;
            res.push_str(&v.to_string());
        }
    }
    // 整数满组：首个非零组前跳过全零组（去前导零），其后每组 9 位左补零
    for _ in 0..uncomp_int {
        let v = u32::from_be_bytes([data[p], data[p + 1], data[p + 2], data[p + 3]]) ^ mask_word;
        p += 4;
        let t = v.to_string();
        if zero_leading {
            if v != 0 {
                zero_leading = false;
                res.push_str(&t);
            }
        } else {
            res.push_str(&"0".repeat(9 - t.len()));
            res.push_str(&t);
        }
    }
    if zero_leading {
        res.push('0');
    }
    // 小数段（go-mysql `pos < len(data)` 守卫，等价于 scale > 0）：满组恒
    // 输出 9 位（含零组），末余数组左补零至 comp_fr 位 → 恒 scale 位。
    if uncomp_fr > 0 || comp_fr > 0 {
        res.push('.');
        for _ in 0..uncomp_fr {
            let v =
                u32::from_be_bytes([data[p], data[p + 1], data[p + 2], data[p + 3]]) ^ mask_word;
            p += 4;
            let t = v.to_string();
            res.push_str(&"0".repeat(9 - t.len()));
            res.push_str(&t);
        }
        if COMPRESSED_BYTES[comp_fr] > 0 {
            let n = COMPRESSED_BYTES[comp_fr];
            let v = decompress_small(&data[p..p + n], mask_byte);
            p += n;
            let t = v.to_string();
            if comp_fr > t.len() {
                res.push_str(&"0".repeat(comp_fr - t.len()));
            }
            res.push_str(&t);
        }
        debug_assert_eq!(p, data.len());
    }
    *pos += bin_size;
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 辅助：从偏移 0 解码，断言恰好读完全部字节。
    fn dd(buf: &[u8], precision: u16, scale: u16) -> String {
        let mut pos = 0usize;
        let v = decode_decimal(buf, &mut pos, precision, scale).unwrap();
        assert_eq!(pos, buf.len(), "decode_decimal must consume all bytes");
        v
    }

    // ---- 真机 fixture（docker mysql:8.0.46 + mysql:5.7.44 ROW binlog 抓包，
    // 单列 DECIMAL 探针表 t.p_a..p_k，两版本字节逐位一致；见 task-7 报告）----

    #[test]
    fn decimal_binding_case_neg_10_2() {
        // 简报绑定用例 DECIMAL(10,2) '-12345678.90'（真机 p_a 行 1）：
        // 手工推导：intg=8 位→1 个 8 位余数组(4B)：12345678=0x00BC614E，
        // 置符号位→80 bc 61 4e；frac=2 位→1B：90=0x5A；负值整体按字节取反
        // （one's complement，无 +1）→ 7f 43 9e b1 a5。
        assert_eq!(dd(&[0x7f, 0x43, 0x9e, 0xb1, 0xa5], 10, 2), "-12345678.90");
        // 正值对照（真机行 2）
        assert_eq!(dd(&[0x80, 0xbc, 0x61, 0x4e, 0x5a], 10, 2), "12345678.90");
    }

    #[test]
    fn decimal_zero_and_small_fractions() {
        // 真机 p_a 行 3：0.00（注意：CAST('-0.001' AS DECIMAL(10,2)) 舍入到
        // 零时符号被丢弃，MySQL 不落 -0.00，行 4 字节与行 3 完全相同）
        assert_eq!(dd(&[0x80, 0x00, 0x00, 0x00, 0x00], 10, 2), "0.00");
        // 真机 p_a 行 7：0.01；行 6/-0.01 取反 = 7f ff ff ff fe
        assert_eq!(dd(&[0x80, 0x00, 0x00, 0x00, 0x01], 10, 2), "0.01");
        assert_eq!(dd(&[0x7f, 0xff, 0xff, 0xff, 0xfe], 10, 2), "-0.01");
        // 手工推导：0.05 = 整数 0 + frac 5 → 80 00 00 00 05
        assert_eq!(dd(&[0x80, 0x00, 0x00, 0x00, 0x05], 10, 2), "0.05");
    }

    #[test]
    fn decimal_max_min_for_10_2() {
        // 真机 p_a 行 8/9：±99999999.99
        assert_eq!(dd(&[0x85, 0xf5, 0xe0, 0xff, 0x63], 10, 2), "99999999.99");
        assert_eq!(dd(&[0x7a, 0x0a, 0x1f, 0x00, 0x9c], 10, 2), "-99999999.99");
    }

    #[test]
    fn decimal_scale_zero_has_no_dot() {
        // 真机 p_b(3,0)：intg=3 位→2B 余数组
        assert_eq!(dd(&[0x80, 0x64], 3, 0), "100");
        assert_eq!(dd(&[0x7f, 0x9b], 3, 0), "-100");
        assert_eq!(dd(&[0x80, 0x00], 3, 0), "0");
        assert_eq!(dd(&[0x83, 0xe7], 3, 0), "999");
        assert_eq!(dd(&[0x7c, 0x18], 3, 0), "-999");
        assert_eq!(dd(&[0x80, 0x01], 3, 0), "1");
        assert_eq!(dd(&[0x7f, 0xfe], 3, 0), "-1");
        // 真机 p_h(1,0)：单字节 1 位组
        assert_eq!(dd(&[0x89], 1, 0), "9");
        assert_eq!(dd(&[0x76], 1, 0), "-9");
        assert_eq!(dd(&[0x80], 1, 0), "0");
    }

    #[test]
    fn decimal_full_4byte_group_9_0() {
        // 真机 p_c(9,0)：整 9 位 = 1 个 4B 满组、无余数组；
        // 999999999=0x3B9AC9FF 置符号位 bb 9a c9 ff；负值取反 44 65 36 00
        assert_eq!(dd(&[0xbb, 0x9a, 0xc9, 0xff], 9, 0), "999999999");
        assert_eq!(dd(&[0x44, 0x65, 0x36, 0x00], 9, 0), "-999999999");
        // 组内前导零输出时不补齐（最高非零组左对齐）：1 → 80 00 00 01
        assert_eq!(dd(&[0x80, 0x00, 0x00, 0x01], 9, 0), "1");
        assert_eq!(dd(&[0x7f, 0xff, 0xff, 0xfe], 9, 0), "-1");
    }

    #[test]
    fn decimal_pure_fraction_5_5() {
        // 真机 p_d(5,5)：intg=0 位、frac=5 位→3B，符号位在首字节高 0x80：
        // 0.12345 → 12345=0x3039 | 0x800000 = 80 30 39
        assert_eq!(dd(&[0x80, 0x30, 0x39], 5, 5), "0.12345");
        assert_eq!(dd(&[0x7f, 0xcf, 0xc6], 5, 5), "-0.12345");
        assert_eq!(dd(&[0x80, 0x00, 0x01], 5, 5), "0.00001");
        assert_eq!(dd(&[0x7f, 0xff, 0xfe], 5, 5), "-0.00001");
        assert_eq!(dd(&[0x81, 0x86, 0x9f], 5, 5), "0.99999");
        assert_eq!(dd(&[0x7e, 0x79, 0x60], 5, 5), "-0.99999");
    }

    #[test]
    fn decimal_leading_zero_groups_30_10() {
        // 真机 p_e(30,10)：intg=20 位 = 余 2 位(1B) + 2 满组；frac=10 位 =
        // 1 满组 + 余 1 位(1B)。12345.1234567890：整数前 11 位全 0 →
        // 输出无组内前导零；小数满组 123456789=0x075BCD15、余组 0
        assert_eq!(
            dd(
                &[
                    0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x30, 0x39, 0x07, 0x5b, 0xcd, 0x15,
                    0x00
                ],
                30,
                10
            ),
            "12345.1234567890"
        );
        assert_eq!(
            dd(
                &[
                    0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xcf, 0xc6, 0xf8, 0xa4, 0x32, 0xea,
                    0xff
                ],
                30,
                10
            ),
            "-12345.1234567890"
        );
        // 满组中间零补位：12345678901234567.1234567890（整数低两满组均非零）
        assert_eq!(
            dd(
                &[
                    0x80, 0x00, 0xbc, 0x61, 0x4e, 0x35, 0xb7, 0xbf, 0x87, 0x07, 0x5b, 0xcd, 0x15,
                    0x00
                ],
                30,
                10
            ),
            "12345678901234567.1234567890"
        );
        assert_eq!(
            dd(
                &[
                    0x7f, 0xff, 0x43, 0x9e, 0xb1, 0xca, 0x48, 0x40, 0x78, 0xf8, 0xa4, 0x32, 0xea,
                    0xff
                ],
                30,
                10
            ),
            "-12345678901234567.1234567890"
        );
    }

    #[test]
    fn decimal_frac_full_group_zero_padding() {
        // 真机 p_e：0.0000000001 → 小数满组=0 输出 9 个零 + 余组 1
        assert_eq!(
            dd(
                &[
                    0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                    0x01
                ],
                30,
                10
            ),
            "0.0000000001"
        );
        assert_eq!(
            dd(
                &[
                    0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                    0xfe
                ],
                30,
                10
            ),
            "-0.0000000001"
        );
        // 真机 p_e：±99999999999999999999.9999999999（全满组边界）
        assert_eq!(
            dd(
                &[
                    0xe3, 0x3b, 0x9a, 0xc9, 0xff, 0x3b, 0x9a, 0xc9, 0xff, 0x3b, 0x9a, 0xc9, 0xff,
                    0x09
                ],
                30,
                10
            ),
            "99999999999999999999.9999999999"
        );
        assert_eq!(
            dd(
                &[
                    0x1c, 0xc4, 0x65, 0x36, 0x00, 0xc4, 0x65, 0x36, 0x00, 0xc4, 0x65, 0x36, 0x00,
                    0xf6
                ],
                30,
                10
            ),
            "-99999999999999999999.9999999999"
        );
    }

    #[test]
    fn decimal_max_precision_65_30() {
        // 真机 p_f(65,30)：intg=35 位 = 余 8 位(4B) + 3 满组；frac=30 位 =
        // 3 满组 + 余 3 位(2B)。插入字面量 35 个 9.28 个 9 → 存储
        // （35 个 9.28 个 9 + 00，共 65 位）
        assert_eq!(
            dd(
                &[
                    0x85, 0xf5, 0xe0, 0xff, 0x3b, 0x9a, 0xc9, 0xff, 0x3b, 0x9a, 0xc9, 0xff, 0x3b,
                    0x9a, 0xc9, 0xff, 0x3b, 0x9a, 0xc9, 0xff, 0x3b, 0x9a, 0xc9, 0xff, 0x3b, 0x9a,
                    0xc9, 0xff, 0x03, 0x84
                ],
                65,
                30
            ),
            "99999999999999999999999999999999999.999999999999999999999999999900"
        );
        assert_eq!(
            dd(
                &[
                    0x7a, 0x0a, 0x1f, 0x00, 0xc4, 0x65, 0x36, 0x00, 0xc4, 0x65, 0x36, 0x00, 0xc4,
                    0x65, 0x36, 0x00, 0xc4, 0x65, 0x36, 0x00, 0xc4, 0x65, 0x36, 0x00, 0xc4, 0x65,
                    0x36, 0x00, 0xfc, 0x7b
                ],
                65,
                30
            ),
            "-99999999999999999999999999999999999.999999999999999999999999999900"
        );
        // 1e-30 与 −1e-30（真机）
        assert_eq!(
            dd(
                &[
                    0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                    0x00, 0x00, 0x00, 0x01
                ],
                65,
                30
            ),
            "0.000000000000000000000000000001"
        );
        assert_eq!(
            dd(
                &[
                    0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                    0xff, 0xff, 0xff, 0xfe
                ],
                65,
                30
            ),
            "-0.000000000000000000000000000001"
        );
        // 29 位整数 + 30 位小数（真机 p_f 行 5）
        assert_eq!(
            dd(
                &[
                    0x80, 0x00, 0x00, 0x0c, 0x14, 0x9a, 0xa4, 0x35, 0x0d, 0xfb, 0x38, 0xd2, 0x07,
                    0x5b, 0xcd, 0x15, 0x07, 0x5b, 0xcd, 0x15, 0x00, 0xbc, 0x61, 0x4e, 0x35, 0xb7,
                    0xbf, 0x87, 0x03, 0x7a
                ],
                65,
                30
            ),
            "12345678901234567890123456789.123456789012345678901234567890"
        );
        // 全零 (65,30)（真机 p_f 行 6）
        assert_eq!(
            dd(
                &[
                    0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                    0x00, 0x00, 0x00, 0x00
                ],
                65,
                30
            ),
            "0.000000000000000000000000000000"
        );
    }

    #[test]
    fn decimal_pure_fraction_20_20() {
        // 真机 p_g(20,20)：intg=0、frac=20 位 = 2 满组 + 余 2 位(1B)
        assert_eq!(
            dd(
                &[0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01],
                20,
                20
            ),
            "0.00000000000000000001"
        );
        assert_eq!(
            dd(
                &[0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe],
                20,
                20
            ),
            "-0.00000000000000000001"
        );
        assert_eq!(
            dd(
                &[0xbb, 0x9a, 0xc9, 0xff, 0x3b, 0x9a, 0xc9, 0xff, 0x63],
                20,
                20
            ),
            "0.99999999999999999999"
        );
        assert_eq!(
            dd(
                &[0x44, 0x65, 0x36, 0x00, 0xc4, 0x65, 0x36, 0x00, 0x9c],
                20,
                20
            ),
            "-0.99999999999999999999"
        );
    }

    #[test]
    fn decimal_small_two_byte_groups_4_2_and_2_1() {
        // 真机 p_i(4,2)：2+2 位 → 1B+1B（cb[2]=1）
        assert_eq!(dd(&[0x81, 0x05], 4, 2), "1.05");
        assert_eq!(dd(&[0x7e, 0xfa], 4, 2), "-1.05");
        assert_eq!(dd(&[0xe3, 0x63], 4, 2), "99.99");
        assert_eq!(dd(&[0x1c, 0x9c], 4, 2), "-99.99");
        assert_eq!(dd(&[0x80, 0x00], 4, 2), "0.00");
        // 真机 p_k(2,1)：1+1 位
        assert_eq!(dd(&[0x80, 0x05], 2, 1), "0.5");
        assert_eq!(dd(&[0x7f, 0xfa], 2, 1), "-0.5");
    }

    #[test]
    fn decimal_18_6_mixed() {
        // 真机 p_j(18,6)：intg=12 位 = 余 3 位(2B) + 1 满组；frac=6 位 =
        // 余 6 位(3B)：123456789012.123456
        assert_eq!(
            dd(
                &[0x80, 0x7b, 0x1b, 0x3a, 0x0c, 0x14, 0x01, 0xe2, 0x40],
                18,
                6
            ),
            "123456789012.123456"
        );
        assert_eq!(
            dd(
                &[0x7f, 0x84, 0xe4, 0xc5, 0xf3, 0xeb, 0xfe, 0x1d, 0xbf],
                18,
                6
            ),
            "-123456789012.123456"
        );
        assert_eq!(
            dd(
                &[0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01],
                18,
                6
            ),
            "0.000001"
        );
        assert_eq!(
            dd(
                &[0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe],
                18,
                6
            ),
            "-0.000001"
        );
    }

    #[test]
    fn decimal_negative_zero_synthetic() {
        // 合成形态（真机不可达：MySQL 把 -0.001→(10,2) 舍入为 +0.00 落盘）：
        // 若负值符号位为 0 且各组取反后为 0（即 0.00 的全字节取反），
        // go-mysql 输出 "-0.00"（符号只看首字节、零组照样输出）——逐式镜像。
        assert_eq!(dd(&[0x7f, 0xff, 0xff, 0xff, 0xff], 10, 2), "-0.00");
    }

    #[test]
    fn decimal_consumes_exactly_bin_size() {
        // 中段起始 + 尾部多余字节不被吞：pos 精确推进 bin_size=5
        let buf = [0xaa, 0xbb, 0x80, 0xbc, 0x61, 0x4e, 0x5a, 0xcc];
        let mut pos = 2usize;
        let v = decode_decimal(&buf, &mut pos, 10, 2).unwrap();
        assert_eq!(v, "12345678.90");
        assert_eq!(pos, 7);
    }

    #[test]
    fn decimal_too_short_leaves_pos_untouched() {
        // (10,2) 需 5 字节，只给 3 字节 + 起始 pos=1：报错且 pos 不动
        let mut pos = 1usize;
        assert_eq!(
            decode_decimal(&[0x00, 0x80, 0x00, 0x00], &mut pos, 10, 2).unwrap_err(),
            BinlogError::TooShort
        );
        assert_eq!(pos, 1);
    }

    #[test]
    fn decimal_invalid_meta() {
        let mut pos = 0usize;
        // scale > precision
        assert!(matches!(
            decode_decimal(&[0x80; 6], &mut pos, 4, 5),
            Err(BinlogError::InvalidData(_))
        ));
        // precision > 65
        assert!(matches!(
            decode_decimal(&[0x80; 6], &mut pos, 66, 2),
            Err(BinlogError::InvalidData(_))
        ));
        // precision = 0（binSize 0 字节，go-mysql 此处会越界 panic，我们报错）
        assert!(matches!(
            decode_decimal(&[0x80], &mut pos, 0, 0),
            Err(BinlogError::InvalidData(_))
        ));
        assert_eq!(pos, 0);
    }
}
