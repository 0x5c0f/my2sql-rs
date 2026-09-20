//! 时间族列值解码：DATE2 / DATETIME2 / TIMESTAMP2 / TIME2 → ASCII 文本。
//!
//! 位级算式逐项对照 go-mysql `replication/row_event.go`
//! （`decodeDatetime2` / `decodeTimestamp2` / `decodeTime2` / `timeFormat` 及
//! `MYSQL_TYPE_DATE` 分支），并以 docker 真机 binlog（mysql:8.0.46 与
//! mysql:5.7.x，ROW 格式，含 DATE/DATETIME(3)/DATETIME(2)/TIMESTAMP(3)/
//! TIME(0)/TIME(3)/TIME(6) 探针表）抓包字节回验，见各测试的 fixture 注释。
//!
//! 简报偏差（以权威为准，证据见 task-6 报告）：
//! - DATE2 实为 **3 字节小端**位域 `(year<<9)|(month<<5)|day`（go-mysql
//!   `MYSQL_TYPE_DATE` 分支 `i/(16*32), i/32%16, i%32` + 真机字节一致），
//!   简报「3B 大端、高位减 0x800000」两处均不成立（0x800000 是 TIME2 的
//!   `TIMEF_INT_OFS`，DATE 无任何 bias）。
//! - TIMESTAMP2 增加 `tz_offset_secs` 参数（简报签名无法换算会话时区；
//!   上游 my2sql 将 UTC 秒换算到 --time-zone，本层保持 chrono-free）。
//! - 秒=0 的 TIMESTAMP2 按控制器裁定输出 `1970-01-01 00:00:00`（+tz），
//!   而非 go-mysql 的 `formatZeroTime` 零值特例（裁定优先，见报告）。

use super::error::BinlogError;

/// DATETIME2 整数部分 bias（go-mysql `DATETIMEF_INT_OFS`）。
const DATETIMEF_INT_OFS: i64 = 0x0080_0000_0000;
/// TIME2 小数秒 ≤4 位时整数部分 bias（go-mysql `TIMEF_INT_OFS`）。
const TIMEF_INT_OFS: i64 = 0x80_0000;
/// TIME2 小数秒 5/6 位时整体 bias（go-mysql `TIMEF_OFS`）。
const TIMEF_OFS: i64 = 0x8000_0000_0000;

/// 校验 fsp ∈ 0..=6（MySQL DATETIME/TIME/TIMESTAMP 小数位上限）。
fn check_fsp(fsp: u8) -> Result<(), BinlogError> {
    if fsp > 6 {
        return Err(BinlogError::InvalidData(format!(
            "invalid fractional seconds precision: {fsp}"
        )));
    }
    Ok(())
}

/// 大端读 `s` 的字节拼 u64（go-mysql `BFixedLengthInt`）。
fn be_uint(s: &[u8]) -> u64 {
    s.iter().fold(0u64, |acc, &b| (acc << 8) | b as u64)
}

/// DATETIME2/TIMESTAMP2 的小数段 → 微秒（go-mysql：fsp1,2: `b0*10000`；
/// fsp3,4: `BE16*100`；fsp5,6: `BE24`）。`s` 长度须 ≥ `(fsp+1)/2`。
fn frac_usec(fsp: u8, s: &[u8]) -> i64 {
    match fsp {
        1 | 2 => s[0] as i64 * 10_000,
        3 | 4 => be_uint(&s[0..2]) as i64 * 100,
        5 | 6 => be_uint(&s[0..3]) as i64,
        _ => 0,
    }
}

/// 微秒 → 小数文本：`.` + `%06d` 截前 fsp 位（go-mysql
/// `formatZeroTime`/`fracTimeFormat` 的 `s[0:len-(6-dec)]` 口径）。
fn frac_text(usec: i64, fsp: u8) -> String {
    let s = format!("{usec:06}");
    format!(".{}", &s[..fsp as usize])
}

/// DATE2（binlog type 10）：3 字节**小端**位域 `[year:15][month:4][day:5]`
/// （= `year*512 + month*32 + day`；go-mysql `MYSQL_TYPE_DATE` 分支，真机
/// 8.0.46/5.7 抓包一致）。零值（0x000000）→ `"0000-00-00"`。读 3 字节。
pub fn decode_date2(buf: &[u8], pos: &mut usize) -> Result<String, BinlogError> {
    let s = buf.get(*pos..*pos + 3).ok_or(BinlogError::TooShort)?;
    // go-mysql FixedLengthInt = 小端
    let v = (s[0] as u32) | ((s[1] as u32) << 8) | ((s[2] as u32) << 16);
    *pos += 3;
    if v == 0 {
        return Ok("0000-00-00".to_string());
    }
    // go-mysql: fmt.Sprintf("%04d-%02d-%02d", i32/(16*32), i32/32%16, i32%32)
    Ok(format!(
        "{:04}-{:02}-{:02}",
        v / (16 * 32),
        v / 32 % 16,
        v % 32
    ))
}

/// DATETIME2：`5 + (fsp+1)/2` 字节大端；整数部分 = `BE(5B) − 0x8000000000`，
/// 位拆解 `ymd = v>>17; ym = ymd>>5; hms = v % 2^17; year = ym/13; month = ym%13;
/// day = ymd%32; hour = hms>>12; minute = (hms>>6)%64; second = hms%64`
/// （go-mysql `decodeDatetime2` 逐式）。整数部分为 0 → 零值
/// `"0000-00-00 00:00:00[.frac]"`；fsp>0 恒输出 fsp 位小数。
pub fn decode_datetime2(buf: &[u8], pos: &mut usize, fsp: u8) -> Result<String, BinlogError> {
    check_fsp(fsp)?;
    let nb = 5 + usize::from(fsp).div_ceil(2);
    let s = buf.get(*pos..*pos + nb).ok_or(BinlogError::TooShort)?;
    let int_part = be_uint(&s[..5]) as i64 - DATETIMEF_INT_OFS;
    let frac = frac_usec(fsp, &s[5..]);
    *pos += nb;
    let text = if int_part == 0 {
        // go-mysql formatZeroTime：零值也带小数段（按 fsp 位）
        "0000-00-00 00:00:00".to_string()
    } else {
        // go-mysql decodeDatetime2 逐式：tmp = intPart<<24 + frac（本函数中
        // frac 为微秒 < 2^24，仅用于符号翻转后取整），ymdhms = |tmp| >> 24
        let mut tmp = (int_part << 24) + frac;
        if tmp < 0 {
            tmp = tmp.wrapping_neg();
        }
        let ymdhms = tmp >> 24;
        let ymd = ymdhms >> 17;
        let ym = ymd >> 5;
        let hms = ymdhms % (1 << 17);
        let (year, month, day) = (ym / 13, ym % 13, ymd % (1 << 5));
        let (hour, minute, second) = (hms >> 12, (hms >> 6) % (1 << 6), hms % (1 << 6));
        format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
    };
    Ok(if fsp > 0 {
        format!("{text}{}", frac_text(frac, fsp))
    } else {
        text
    })
}

/// TIMESTAMP2：`4 + (fsp+1)/2` 字节大端（4B 无符号 UTC 秒 + 小数段），
/// 输出按 `秒 + tz_offset_secs` 手工换算的本地时间文本（不依赖 chrono）。
/// 秒=0 不再走 go-mysql 零值特例，按控制器裁定输出 `1970-01-01 00:00:00`
/// （+tz）。fsp>0 恒输出 fsp 位小数。
pub fn decode_timestamp2(
    buf: &[u8],
    pos: &mut usize,
    fsp: u8,
    tz_offset_secs: i32,
) -> Result<String, BinlogError> {
    check_fsp(fsp)?;
    let nb = 4 + usize::from(fsp).div_ceil(2);
    let s = buf.get(*pos..*pos + nb).ok_or(BinlogError::TooShort)?;
    let sec = be_uint(&s[..4]) as i64;
    let usec = frac_usec(fsp, &s[4..]);
    *pos += nb;
    // 控制器裁定：秒=0 不再走 go-mysql 的 formatZeroTime 特例，统一按
    // UTC 秒 + 会话时区偏移做历法换算（1970-01-01 00:00:00 + tz）。
    let (y, mo, d, h, mi, se) = civil_from_epoch_secs(sec + tz_offset_secs as i64);
    let text = format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{se:02}");
    Ok(if fsp > 0 {
        format!("{text}{}", frac_text(usec, fsp))
    } else {
        text
    })
}

/// V1 TIMESTAMP（type 码 7，非打包旧形态）：4 字节 **小端** unsigned UTC 秒
/// （go-mysql `decodeValue` :1065-1072 `ParseBinaryUint32`），按
/// `秒 + tz_offset_secs` 输出本地时间文本。零秒与 TIMESTAMP2 同裁定输出
/// epoch（T6 裁定在 V1/V2 间保持一致，go-mysql formatZeroTime 差异见
/// task-9 报告）。仅 5.5 及以下 V1 事件产生（D6 不支持，尽职实现）。
pub fn decode_timestamp_v1(
    buf: &[u8],
    pos: &mut usize,
    tz_offset_secs: i32,
) -> Result<String, BinlogError> {
    let s = buf.get(*pos..*pos + 4).ok_or(BinlogError::TooShort)?;
    let sec = u32::from_le_bytes(s.try_into().unwrap()) as i64;
    *pos += 4;
    let (y, mo, d, h, mi, se) = civil_from_epoch_secs(sec + tz_offset_secs as i64);
    Ok(format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{se:02}"))
}

/// UTC 秒（可为负）→ (年, 月, 日, 时, 分, 秒)。手工历法换算（Hinnant
/// days_from_civil 逆算法），不依赖 chrono（全局约束：decode 层 chrono-free）。
fn civil_from_epoch_secs(secs: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    (y, m, d, sod / 3600, sod / 60 % 60, sod % 60)
}

/// 自 1970-01-01 的天数（可为负）→ 公历年月日（Howard Hinnant 算法）。
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468; // 移至 0000-03-01 历元
    let era = z.div_euclid(146_097); // 400 年历元
    let doe = z.rem_euclid(146_097); // day-of-era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // era 内年 [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day-of-year [0, 365]
    let mp = (5 * doy + 2) / 153; // 自 3 月起月 [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // day [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// TIME2：`3 + (fsp+1)/2` 字节。fsp≤4：整数部分 = `BE(3B) − 0x800000` +
/// 独立小数段（负值小数段按 go-mysql 反向补偿 `intPart++ / frac−=0x100..`）；
/// fsp5/6：整体 = `BE(6B) − 0x800000000000`，微秒在低 24 位。符号在整数值
/// 最低位（`0x800000` bias 的另一半，真机 `-00:00:01` → `7F FF FF`）。
/// 输出 `[-]HH:MM:SS[.frac]`，hour 为 10 bit（最大 838，支持 >24h）；
/// 仅当小数非 0 且 fsp>0 才输出小数（go-mysql `timeFormat` 口径，与
/// DATETIME 恒输出不同）。
pub fn decode_time2(buf: &[u8], pos: &mut usize, fsp: u8) -> Result<String, BinlogError> {
    check_fsp(fsp)?;
    let nb = 3 + usize::from(fsp).div_ceil(2);
    let s = buf.get(*pos..*pos + nb).ok_or(BinlogError::TooShort)?;
    *pos += nb;
    // go-mysql decodeTime2 逐分支
    if fsp >= 5 {
        // 5/6 位小数：整体 6 字节，微秒在低 24 位，负值即 TIMEF_OFS − |packed|
        let tmp = be_uint(s) as i64 - TIMEF_OFS;
        return Ok(time_format(tmp, fsp));
    }
    let mut int_part = be_uint(&s[..3]) as i64 - TIMEF_INT_OFS;
    let mut frac = 0i64;
    let tmp;
    match fsp {
        1 | 2 => {
            frac = s[3] as i64;
            if int_part < 0 && frac != 0 {
                // 负值小数段反向存储（二进制排序兼容）：进位 + 翻转小数
                int_part += 1;
                frac -= 0x100;
            }
            tmp = (int_part << 24) + frac * 10_000;
        }
        3 | 4 => {
            frac = be_uint(&s[3..5]) as i64;
            if int_part < 0 && frac != 0 {
                int_part += 1;
                frac -= 0x1_0000;
            }
            tmp = (int_part << 24) + frac * 100;
        }
        _ => {
            tmp = int_part << 24;
        }
    }
    if int_part == 0 && frac == 0 {
        return Ok("00:00:00".to_string());
    }
    Ok(time_format(tmp, fsp))
}

/// go-mysql `timeFormat`：符号取绝对值后拆 `hms = tmp>>24`，
/// `hour = (hms>>12)%1024`（10 bit，支持 >24h、上限 838）、min/sec 各 6 bit；
/// 低 24 位为微秒，**仅当非 0** 才输出小数（fsp=0 时恒为 0）。
fn time_format(mut tmp: i64, fsp: u8) -> String {
    let sign = if tmp < 0 {
        tmp = tmp.wrapping_neg();
        "-"
    } else {
        ""
    };
    let hms = tmp >> 24;
    let hour = (hms >> 12) % (1 << 10);
    let minute = (hms >> 6) % (1 << 6);
    let second = hms % (1 << 6);
    let sec_part = tmp % (1 << 24);
    if sec_part != 0 && fsp > 0 {
        format!(
            "{sign}{hour:02}:{minute:02}:{second:02}{}",
            frac_text(sec_part, fsp)
        )
    } else {
        format!("{sign}{hour:02}:{minute:02}:{second:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 辅助：从偏移 0 解码，断言恰好读完全部字节。
    fn dd(buf: &[u8]) -> String {
        let mut pos = 0usize;
        let v = decode_date2(buf, &mut pos).unwrap();
        assert_eq!(pos, buf.len(), "decode_date2 must consume all bytes");
        v
    }
    fn ddt(buf: &[u8], fsp: u8) -> String {
        let mut pos = 0usize;
        let v = decode_datetime2(buf, &mut pos, fsp).unwrap();
        assert_eq!(pos, buf.len(), "decode_datetime2 must consume all bytes");
        v
    }
    fn dts(buf: &[u8], fsp: u8, tz: i32) -> String {
        let mut pos = 0usize;
        let v = decode_timestamp2(buf, &mut pos, fsp, tz).unwrap();
        assert_eq!(pos, buf.len(), "decode_timestamp2 must consume all bytes");
        v
    }
    fn dt(buf: &[u8], fsp: u8) -> String {
        let mut pos = 0usize;
        let v = decode_time2(buf, &mut pos, fsp).unwrap();
        assert_eq!(pos, buf.len(), "decode_time2 must consume all bytes");
        v
    }

    // 以下 fixture 全部取自 docker mysql:8.0.46 真机 ROW binlog 抓包字节
    // （t.t6 探针表，见 task-6 报告；5.7 抓包逐字节一致）。

    // ---- DATE2 ----

    #[test]
    fn date2_real_bytes() {
        // '2020-07-16' → LE f0 c8 0f（=2020<<9|7<<5|16=1034480）
        assert_eq!(dd(&[0xf0, 0xc8, 0x0f]), "2020-07-16");
        // '0001-01-01' → 21 02 00；'9999-12-31' → 9f 1f 4e
        assert_eq!(dd(&[0x21, 0x02, 0x00]), "0001-01-01");
        assert_eq!(dd(&[0x9f, 0x1f, 0x4e]), "9999-12-31");
    }

    #[test]
    fn date2_zero_is_not_error() {
        // 简报指定：零值 → "0000-00-00"（真机 '0000-00-00' 存 00 00 00）
        assert_eq!(dd(&[0x00, 0x00, 0x00]), "0000-00-00");
    }

    // ---- DATETIME2 ----

    #[test]
    fn datetime2_binding_case() {
        // 简报/裁定指定绑定用例：2020-07-16 10:44:09 →
        // 整数部分 99 a6 e0 ab 09（= v+0x8000000000，v=(ym<<17)|hms）
        assert_eq!(
            ddt(&[0x99, 0xa6, 0xe0, 0xab, 0x09], 0),
            "2020-07-16 10:44:09"
        );
        // fsp=3 小数 120ms → frac=120000usec → 存 BE16 0x04B0=1200（*100）
        assert_eq!(
            ddt(&[0x99, 0xa6, 0xe0, 0xab, 0x09, 0x04, 0xb0], 3),
            "2020-07-16 10:44:09.120"
        );
        // fsp=6（简报所称 8 字节形态）：123456usec → BE24 0x01E240
        assert_eq!(
            ddt(&[0x99, 0xa6, 0xe0, 0xab, 0x09, 0x01, 0xe2, 0x40], 6),
            "2020-07-16 10:44:09.123456"
        );
    }

    #[test]
    fn datetime2_zero_dates() {
        // 真机 '0000-00-00 00:00:00.000' → 80 00 00 00 00 | 00 00
        assert_eq!(
            ddt(&[0x80, 0x00, 0x00, 0x00, 0x00], 0),
            "0000-00-00 00:00:00"
        );
        assert_eq!(
            ddt(&[0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], 3),
            "0000-00-00 00:00:00.000"
        );
    }

    #[test]
    fn datetime2_range_and_fsp2() {
        // '1970-01-01 00:00:00.000' 真机字节（go-mysql 注释整数 107420450816）
        assert_eq!(
            ddt(&[0x99, 0x02, 0xc2, 0x00, 0x00, 0x00, 0x00], 3),
            "1970-01-01 00:00:00.000"
        );
        // '1000-01-01 00:00:00.00' fsp=2：frac 1 字节
        assert_eq!(
            ddt(&[0x8c, 0xb2, 0x42, 0x00, 0x00, 0x00], 2),
            "1000-01-01 00:00:00.00"
        );
        // '9999-12-31 23:59:59.999'
        assert_eq!(
            ddt(&[0xfe, 0xf3, 0xff, 0x7e, 0xfb, 0x27, 0x06], 3),
            "9999-12-31 23:59:59.999"
        );
        // fsp=2 真机 '.12' → frac 字节 0x0C=12 → 12*10000=120000usec → ".12"
        assert_eq!(
            ddt(&[0x99, 0xa6, 0xe0, 0xab, 0x09, 0x0c], 2),
            "2020-07-16 10:44:09.12"
        );
    }

    // ---- TIMESTAMP2 ----

    #[test]
    fn timestamp2_epoch_zero_per_ruling() {
        // 裁定指定：秒=0 + tz_offset=0 → 1970-01-01 00:00:00；+08:00 → 08:00:00
        assert_eq!(dts(&[0x00, 0x00, 0x00, 0x00], 0, 0), "1970-01-01 00:00:00");
        assert_eq!(
            dts(&[0x00, 0x00, 0x00, 0x00], 0, 8 * 3600),
            "1970-01-01 08:00:00"
        );
    }

    #[test]
    fn timestamp2_real_bytes_and_tz() {
        // 真机 '1970-01-01 00:00:10.000'（UTC 会话）→ 00 00 00 0a | 00 00
        assert_eq!(
            dts(&[0x00, 0x00, 0x00, 0x0a, 0x00, 0x00], 3, 0),
            "1970-01-01 00:00:10.000"
        );
        // 2038 上界（真机 >03:14:07 溢出存 0）：0x7FFFFFFF
        assert_eq!(dts(&[0x7f, 0xff, 0xff, 0xff], 0, 0), "2038-01-19 03:14:07");
        // 负偏移跨日：sec=10, tz=-01:00 → 1969-12-31 23:00:10
        assert_eq!(
            dts(&[0x00, 0x00, 0x00, 0x0a], 0, -3600),
            "1969-12-31 23:00:10"
        );
        // fsp=1：frac 字节 0x1E=30 → 300000usec → ".3"（1 字节按百分之一秒
        // 粒度存储，文本取 %06d 首位，与 go-mysql 截断口径一致）
        assert_eq!(
            dts(&[0x00, 0x00, 0x00, 0x0a, 0x1e], 1, 0),
            "1970-01-01 00:00:10.3"
        );
    }

    // ---- TIME2 ----

    #[test]
    fn time2_negative_one_second() {
        // 简报/裁定指定：负 1 秒 fsp=0 → "-00:00:01"；真机字节 7f ff ff
        assert_eq!(dt(&[0x7f, 0xff, 0xff], 0), "-00:00:01");
        // 正 1 秒真机字节 80 00 01
        assert_eq!(dt(&[0x80, 0x00, 0x01], 0), "00:00:01");
    }

    #[test]
    fn time2_zero_and_over_24h() {
        assert_eq!(dt(&[0x80, 0x00, 0x00], 0), "00:00:00");
        // '838:59:59' → b4 6e fb；'−838:59:59' → 4b 91 05（TIME 上限非 24h）
        assert_eq!(dt(&[0xb4, 0x6e, 0xfb], 0), "838:59:59");
        assert_eq!(dt(&[0x4b, 0x91, 0x05], 0), "-838:59:59");
        // fsp=6 '100:00:00.000001' → 86 40 00 00 00 01
        assert_eq!(
            dt(&[0x86, 0x40, 0x00, 0x00, 0x00, 0x01], 6),
            "100:00:00.000001"
        );
    }

    #[test]
    fn time2_fractional_cases() {
        // fsp=3 '00:00:00.999' → 80 00 00 | 27 06
        assert_eq!(dt(&[0x80, 0x00, 0x00, 0x27, 0x06], 3), "00:00:00.999");
        // fsp=3 '-00:00:00.999' → 7f ff ff | d8 fa（反向小数补偿）
        assert_eq!(dt(&[0x7f, 0xff, 0xff, 0xd8, 0xfa], 3), "-00:00:00.999");
        // fsp=3 '-00:00:00.001' → 7f ff ff | ff f6
        assert_eq!(dt(&[0x7f, 0xff, 0xff, 0xff, 0xf6], 3), "-00:00:00.001");
        // fsp=6 '12:34:56.123456' → 80 c8 b8 01 e2 40
        assert_eq!(
            dt(&[0x80, 0xc8, 0xb8, 0x01, 0xe2, 0x40], 6),
            "12:34:56.123456"
        );
        // fsp=6 '-12:34:56.123456' → 7f 37 47 fe 1d c0
        assert_eq!(
            dt(&[0x7f, 0x37, 0x47, 0xfe, 0x1d, 0xc0], 6),
            "-12:34:56.123456"
        );
        // fsp=6 零值 80 00 00 00 00 00 → "00:00:00"（小数段为 0 不输出）
        assert_eq!(dt(&[0x80, 0x00, 0x00, 0x00, 0x00, 0x00], 6), "00:00:00");
    }

    // ---- 错误路径 ----

    #[test]
    fn too_short_and_bad_fsp() {
        let mut pos = 0usize;
        assert_eq!(
            decode_date2(&[0x01, 0x02], &mut pos).unwrap_err(),
            BinlogError::TooShort
        );
        assert_eq!(
            decode_datetime2(&[0x99; 5], &mut pos, 3).unwrap_err(),
            BinlogError::TooShort
        );
        assert_eq!(
            decode_timestamp2(&[0x00; 5], &mut pos, 6, 0).unwrap_err(),
            BinlogError::TooShort
        );
        assert_eq!(
            decode_time2(&[0x80, 0x00], &mut pos, 0).unwrap_err(),
            BinlogError::TooShort
        );
        // pos 未被污染
        assert_eq!(pos, 0);
        // 非法 fsp
        assert!(matches!(
            decode_datetime2(&[0x80; 9], &mut pos, 7),
            Err(BinlogError::InvalidData(_))
        ));
        assert!(matches!(
            decode_timestamp2(&[0x80; 8], &mut pos, 7, 0),
            Err(BinlogError::InvalidData(_))
        ));
        assert!(matches!(
            decode_time2(&[0x80; 9], &mut pos, 7),
            Err(BinlogError::InvalidData(_))
        ));
    }
}
