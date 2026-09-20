//! JSON 二进制（JSONB，MySQL binlog row 事件 JSON 列的内部编码）→ 紧凑 JSON 文本。
//!
//! 权威参照（依简报顺序）：
//! 1. go-mysql `replication/json_binary.go`（vendor 全量镜像：类型字节、
//!    small/large 对象与数组、inline 规则、字符串/整型/双精度/opaque 解码，
//!    本实现逐函数对照其行号，见 task-8 报告）；
//! 2. MySQL Internals “JSON Binary Layout”文档（字段宽度/键排序口径校验）；
//! 3. 真机抓包（docker mysql:8.0.46 / 5.7.44，ROW 事件 JSON 列 blob，
//!    两版本 12 个公共行逐字节一致）作为渲染 oracle——凡 go-mysql 与真机
//!    文本不一致处（键序、双精度 `12.0`、TIME/DATE 零值、转义集），以真机
//!    为准并在下表备案。
//!
//! 支持类型字节（全量，无静默子集）：0x00 small object / 0x01 large object /
//! 0x02 small array / 0x03 large array / 0x04 literal(null,true,false) /
//! 0x05 i16 / 0x06 u16 / 0x07 i32 / 0x08 u32 / 0x09 i64 / 0x0a u64 /
//! 0x0b double / 0x0c string / 0x0f opaque（NEWDECIMAL=246、TIME=11、
//! DATE=10、DATETIME=12、TIMESTAMP=14、其余按 UTF-8 字符串兜底）。
//!
//! 相对 go-mysql 的加固偏差（真机/文档不冲突处从严）：
//! - 显式栈迭代解析，嵌套深度上限 [`MAX_DEPTH`]，超限 [`BinlogError::InvalidData`]
//!   （go-mysql 递归下降，深文档可栈溢出）；
//! - 所有切片访问带边界检查，任何截断/越界/非法头部返回
//!   [`TooShort`](BinlogError::TooShort)/[`InvalidData`](BinlogError::InvalidData)，
//!   绝不 panic（go-mysql `decodeDecimal` 直接索引 data[0..2] 可 panic）；
//! - 字符串与对象键执行严格 UTF-8 校验（go-mysql `hack.String` 零拷贝不校验，
//!   坏字节会透传进输出）。
//!
//! 真机 oracle 备案（go-mysql DOM+json.Marshal 口径与之不同处，本项目从真机）：
//! - 对象键按存储序输出；MySQL 存储键按（字节长度，memcmp）排序，故
//!   `{"b":1,"aa":"x"}` 输出 b 在 aa 前（简报示例的字母序不成立）；
//! - 整值 double 渲染为 `12.0`/`100.0`（Go marshal 输出 `12`；MySQL 恒带 `.0`），
//!   e 记号阈值 = 小数点位移 `pp>15 || pp<-14`（30 组真机位型↔文本全验，
//!   见测试 `double_rendering_captures`：`1e14`→`100000000000000.0`、
//!   `1e-15`→`0.000000000000001`、`1.5e-16`→e 记号）；
//! - TIME 零值输出 `"00:00:00.000000"`、DATE 零值 `"0000-00-00"`、
//!   DATETIME 零值 `"0000-00-00 00:00:00.000000"`（go 特判为 `"00:00:00"` /
//!   `"0000-00-00 00:00:00"`，无 6 位秒，真机恒 6 位）；
//! - 字符串转义仅 `\" \\` 与 <0x20 控制符（\b \f \n \r \t，其余 `\u00xx` 小写
//!   十六进制）；DEL(0x7f)、`<>&`、UTF-8 多字节全部原样输出（Go 会
//!   `\u003c` 化，真机原样）；
//! - 空输入返回空字符串（go-mysql `decodeJsonBinary` 对 0 长 data 返回空 slice，
//!   对应 NULL-JSON-in-not-null-column 的历史场景，逐字镜像）。

// 生产消费者在 Task 9（decode_value 分发），骨架阶段参照同级模块允许死代码。
#![allow(dead_code)]

use super::decimal::decode_decimal;
use super::error::BinlogError;

/// JSONB 类型字节（go-mysql json_binary.go:13-28 逐一对应）。
const SMALL_OBJECT: u8 = 0x00;
const LARGE_OBJECT: u8 = 0x01;
const SMALL_ARRAY: u8 = 0x02;
const LARGE_ARRAY: u8 = 0x03;
const LITERAL: u8 = 0x04;
const INT16: u8 = 0x05;
const UINT16: u8 = 0x06;
const INT32: u8 = 0x07;
const UINT32: u8 = 0x08;
const INT64: u8 = 0x09;
const UINT64: u8 = 0x0a;
const DOUBLE: u8 = 0x0b;
const STRING: u8 = 0x0c;
const OPAQUE: u8 = 0x0f;

/// literal 子字节（go-mysql json_binary.go:30-34）。
const LIT_NULL: u8 = 0x00;
const LIT_TRUE: u8 = 0x01;
const LIT_FALSE: u8 = 0x02;

/// opaque 内部 MySQL 字段类型（mysql_const.go / docs）。
const MYSQL_TYPE_DATE: u8 = 10;
const MYSQL_TYPE_TIME: u8 = 11;
const MYSQL_TYPE_DATETIME: u8 = 12;
const MYSQL_TYPE_TIMESTAMP: u8 = 14;
const MYSQL_TYPE_NEWDECIMAL: u8 = 246;

/// 加固：嵌套容器（对象/数组）最大深度，超过返回 InvalidData。
const MAX_DEPTH: usize = 100;

/// JSONB 缓冲区 → 紧凑 JSON 文本（无多余空格）。
pub fn json_binary_to_text(buf: &[u8]) -> Result<String, BinlogError> {
    // 镜像 go-mysql decodeJsonBinary:76-78：空 data（NULL 写入 NOT NULL JSON 列
    // 的历史场景）返回空文本。
    if buf.is_empty() {
        return Ok(String::new());
    }
    let mut out = String::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut current: Option<(u8, &[u8])> = Some((buf[0], &buf[1..]));

    while let Some((ty, data)) = current.take() {
        match ty {
            SMALL_OBJECT | LARGE_OBJECT | SMALL_ARRAY | LARGE_ARRAY => {
                // 加固：显式栈 + 深度上限（go-mysql 递归无上限）。
                if stack.len() >= MAX_DEPTH {
                    return Err(BinlogError::InvalidData(format!(
                        "max JSON nesting depth ({MAX_DEPTH}) exceeded"
                    )));
                }
                let frame = Frame::parse(data, ty)?;
                out.push(if frame.is_object { '{' } else { '[' });
                stack.push(frame);
            }
            _ => out.push_str(&decode_scalar(ty, data)?),
        }
        // 栈驱动：把刚完成子节点之后所有可推进的容器逐个吐出
        loop {
            let Some(frame) = stack.last_mut() else {
                return Ok(out);
            };
            match frame.next(&mut out)? {
                Some(child) => {
                    current = Some(child);
                    break;
                }
                None => {
                    out.push(frame.close_char());
                    stack.pop();
                }
            }
        }
    }
    Ok(out)
}

/// 容器解析帧：持有该容器的 data 区（不含其类型字节）与游标。
struct Frame<'a> {
    data: &'a [u8],
    is_object: bool,
    is_small: bool,
    count: usize,
    idx: usize,
    header_size: usize,
    /// 对象键（已严格 UTF-8 校验），下标与值 entry 一一对应。
    keys: Vec<&'a str>,
}

impl<'a> Frame<'a> {
    /// 解析容器头 + 键区，对照 go-mysql decodeObjectOrArray:143-198。
    fn parse(data: &'a [u8], ty: u8) -> Result<Self, BinlogError> {
        let is_small = ty == SMALL_OBJECT || ty == SMALL_ARRAY;
        let is_object = ty == SMALL_OBJECT || ty == LARGE_OBJECT;
        let os = if is_small { 2 } else { 4 }; // offsetSize:47-53
        let count = read_uint(data, 0, os)?;
        let size = read_uint(data, os, os)?;
        if size > data.len() {
            // go:152 isDataShort(data, size)——本项目不回吞坏 size，直接报错
            return Err(BinlogError::TooShort);
        }
        let value_entry_size = 1 + os; // jsonbGetValueEntrySize:63-69
        let key_entry_size = 2 + os; // jsonbGetKeyEntrySize:55-61（偏移 os + 长度恒 2B）
        let mut header_size = 2 * os + count * value_entry_size;
        if is_object {
            header_size += count * key_entry_size;
        }
        if header_size > size {
            return Err(BinlogError::InvalidData(format!(
                "header size {header_size} > size {size}"
            )));
        }

        let mut keys = Vec::new();
        if is_object {
            keys = Vec::with_capacity(count);
            for i in 0..count {
                let entry = 2 * os + key_entry_size * i;
                let key_offset = read_uint(data, entry, os)?;
                let key_length = read_uint(data, entry + os, 2)?; // go:184 恒 u16
                if key_offset < header_size {
                    // go:187-190 键必须起始于值 entry 之后
                    return Err(BinlogError::InvalidData(format!(
                        "invalid key offset {key_offset}, must >= {header_size}"
                    )));
                }
                let key_end = key_offset
                    .checked_add(key_length)
                    .ok_or(BinlogError::TooShort)?;
                let raw = data.get(key_offset..key_end).ok_or(BinlogError::TooShort)?;
                // 加固：键严格 UTF-8（go hack.String 不校验）
                keys.push(
                    std::str::from_utf8(raw).map_err(|_| {
                        BinlogError::InvalidData("invalid UTF-8 in object key".into())
                    })?,
                );
            }
        }
        Ok(Self {
            data,
            is_object,
            is_small,
            count,
            idx: 0,
            header_size,
            keys,
        })
    }

    fn close_char(&self) -> char {
        if self.is_object { '}' } else { ']' }
    }

    /// 推进一个元素：向 out 写分隔符/键/冒号，返回子节点 (类型, 数据区)。
    /// 对照 go-mysql decodeObjectOrArray:204-226 的循环体。
    fn next(&mut self, out: &mut String) -> Result<Option<(u8, &'a [u8])>, BinlogError> {
        if self.idx == self.count {
            return Ok(None);
        }
        let os = if self.is_small { 2 } else { 4 };
        let value_entry_size = 1 + os;
        let key_entry_size = 2 + os;

        if self.is_object {
            if self.idx > 0 {
                out.push(',');
            }
            out.push('"');
            push_escaped_str(self.keys[self.idx], out);
            out.push_str("\":");
        } else if self.idx > 0 {
            out.push(',');
        }

        let mut entry = 2 * os + value_entry_size * self.idx;
        if self.is_object {
            entry += key_entry_size * self.count; // go:207-210
        }
        let ty = *self.data.get(entry).ok_or(BinlogError::TooShort)?;
        self.idx += 1;

        if is_inline_value(ty, self.is_small) {
            // go:214-217 内联值直接借用 entry 的 os 字节
            let inline = self
                .data
                .get(entry + 1..entry + value_entry_size)
                .ok_or(BinlogError::TooShort)?;
            return Ok(Some((ty, inline)));
        }
        let offset = read_uint(self.data, entry + 1, os)?;
        if offset > self.data.len() {
            return Err(BinlogError::TooShort); // go:221
        }
        Ok(Some((ty, &self.data[offset..])))
    }
}

/// inline 规则，go-mysql isInlineValue:244-253 逐字镜像：
/// i16/u16/literal 恒内联；i32/u32 仅 large 容器内联（small 的 2B 偏移场装不下）。
fn is_inline_value(ty: u8, is_small: bool) -> bool {
    match ty {
        INT16 | UINT16 | LITERAL => true,
        INT32 | UINT32 => !is_small,
        _ => false,
    }
}

/// 读 os（2/4）字节小端无符号偏移/计数（go decodeCount:455-462）。
fn read_uint(data: &[u8], at: usize, os: usize) -> Result<usize, BinlogError> {
    let s = slice(data, at, os)?;
    Ok(match os {
        2 => u16::from_le_bytes(s.try_into().unwrap()) as usize,
        _ => u32::from_le_bytes(s.try_into().unwrap()) as usize,
    })
}

/// 定长切片取出（越界 → TooShort），数组转换交由已定长的 try_into。
fn slice(data: &[u8], at: usize, len: usize) -> Result<&[u8], BinlogError> {
    data.get(at..at + len).ok_or(BinlogError::TooShort)
}

/// 7 位组变长整数，go-mysql decodeVariableLength:464-493 镜像（≤5 字节、
/// ≤ u32::MAX），截断加固为 [`TooShort`](BinlogError::TooShort)。
fn read_varint(data: &[u8]) -> Result<(usize, usize), BinlogError> {
    let mut value: u64 = 0;
    let mut pos = 0usize;
    loop {
        let Some(&b) = data.get(pos) else {
            return Err(BinlogError::TooShort); // go 侧此处进 "variable length failed"
        };
        if pos >= 5 {
            return Err(BinlogError::InvalidData(
                "variable length field too long".into(),
            ));
        }
        value |= u64::from(b & 0x7f) << (7 * pos as u32);
        pos += 1;
        if b & 0x80 == 0 {
            if value > u64::from(u32::MAX) {
                return Err(BinlogError::InvalidData(format!(
                    "variable length {value} must <= {}",
                    u32::MAX
                )));
            }
            return Ok((value as usize, pos));
        }
    }
}

/// 标量（非容器）值 → JSON 文本片段。data 为该值的完整数据区（含变长前缀）。
fn decode_scalar(ty: u8, data: &[u8]) -> Result<String, BinlogError> {
    let text = match ty {
        LITERAL => match *data.first().ok_or(BinlogError::TooShort)? {
            // go decodeLiteral:255-274
            LIT_NULL => "null".to_string(),
            LIT_TRUE => "true".to_string(),
            LIT_FALSE => "false".to_string(),
            b => {
                return Err(BinlogError::InvalidData(format!("invalid literal {b}")));
            }
        },
        INT16 => i16::from_le_bytes(two(data)?).to_string(),
        UINT16 => u16::from_le_bytes(two(data)?).to_string(),
        INT32 => i32::from_le_bytes(four(data)?).to_string(),
        UINT32 => u32::from_le_bytes(four(data)?).to_string(),
        INT64 => i64::from_le_bytes(eight(data)?).to_string(),
        UINT64 => u64::from_le_bytes(eight(data)?).to_string(),
        DOUBLE => {
            let v = f64::from_le_bytes(eight(data)?);
            format_json_double(v)?
        }
        STRING => {
            let (len, n) = read_varint(data)?; // go decodeString:351-366
            let raw = data
                .get(n..n.checked_add(len).ok_or(BinlogError::TooShort)?)
                .ok_or(BinlogError::TooShort)?;
            let s = std::str::from_utf8(raw)
                .map_err(|_| BinlogError::InvalidData("invalid UTF-8 in JSON string".into()))?;
            quoted(s)
        }
        OPAQUE => decode_opaque(data)?,
        _ => {
            return Err(BinlogError::InvalidData(format!("invalid json type {ty}"))); // go decodeValue:136-138
        }
    };
    Ok(text)
}

fn two(data: &[u8]) -> Result<[u8; 2], BinlogError> {
    Ok(slice(data, 0, 2)?.try_into().unwrap())
}
fn four(data: &[u8]) -> Result<[u8; 4], BinlogError> {
    Ok(slice(data, 0, 4)?.try_into().unwrap())
}
fn eight(data: &[u8]) -> Result<[u8; 8], BinlogError> {
    Ok(slice(data, 0, 8)?.try_into().unwrap())
}

/// opaque = [MySQL 类型字节][varint 长度][payload]，go decodeOpaque:368-394。
fn decode_opaque(data: &[u8]) -> Result<String, BinlogError> {
    let inner = *data.first().ok_or(BinlogError::TooShort)?;
    let (len, n) = read_varint(&data[1..])?;
    let start = 1 + n;
    let payload = data
        .get(start..start.checked_add(len).ok_or(BinlogError::TooShort)?)
        .ok_or(BinlogError::TooShort)?;
    match inner {
        MYSQL_TYPE_NEWDECIMAL => {
            // go decodeDecimal:396-404 直接索引 data[0..2]——加固为显式检查
            if payload.len() < 2 {
                return Err(BinlogError::TooShort);
            }
            let precision = u16::from(payload[0]);
            let scale = u16::from(payload[1]);
            let mut pos = 2usize;
            decode_decimal(payload, &mut pos, precision, scale)
        }
        MYSQL_TYPE_TIME => Ok(quoted(&format_time(eight(payload)?))),
        MYSQL_TYPE_DATE => Ok(quoted(&format_datetime(eight(payload)?, true))),
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_TIMESTAMP => {
            Ok(quoted(&format_datetime(eight(payload)?, false)))
        }
        // 其余（YEAR/BINARY 等）：真机未见，按 go:392 兜底口径输出字符串，
        // 但从严校验 UTF-8（go 不校验）。
        _ => {
            let s = std::str::from_utf8(payload).map_err(|_| {
                BinlogError::InvalidData(format!(
                    "unsupported opaque type {inner} with non-UTF-8 payload"
                ))
            })?;
            Ok(quoted(s))
        }
    }
}

/// TIME packed i64：intPart=v>>24（h 10b | m 6b | s 6b），frac=v&(2^24-1)；
/// 负值取绝对值加 '-'。算式同 go decodeTime:406-426，唯零值按真机渲染
/// "00:00:00.000000"（go 特判 "00:00:00" 为偏差）。
fn format_time(bytes: [u8; 8]) -> String {
    let v = i64::from_le_bytes(bytes);
    if v == 0 {
        return "00:00:00.000000".to_string();
    }
    let neg = v < 0;
    let u = v.unsigned_abs();
    let int_part = u >> 24;
    let frac = u & ((1 << 24) - 1);
    format!(
        "{}{:02}:{:02}:{:02}.{:06}",
        if neg { "-" } else { "" },
        (int_part >> 12) % 1024,
        (int_part >> 6) % 64,
        int_part % 64,
        frac
    )
}

/// DATE/DATETIME/TIMESTAMP packed i64：intPart=v>>24 = ymd<<17|hms、
/// ymd=(year*13+month)<<5|day；算式同 go decodeDateTime:428-453。零值与
/// 位数按真机：datetime 恒 6 位小数秒（go 零值特判 "0000-00-00 00:00:00"
/// 为偏差）；DATE 只渲染日期部分（go 统一渲染 datetime 为偏差）。
fn format_datetime(bytes: [u8; 8], date_only: bool) -> String {
    let v = i64::from_le_bytes(bytes);
    let u = v.unsigned_abs();
    let int_part = u >> 24;
    let frac = u & ((1 << 24) - 1);
    let ymd = int_part >> 17;
    let ym = ymd >> 5;
    let (year, month, day) = (ym / 13, ym % 13, ymd % 32);
    let hms = int_part % (1 << 17);
    let (hour, minute, second) = (hms >> 12, (hms >> 6) % 64, hms % 64);
    if date_only {
        format!("{year:04}-{month:02}-{day:02}")
    } else {
        format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}.{frac:06}")
    }
}

/// MySQL JSON 文本 double 渲染（真机 oracle 30 组位型全验，见测试）：
/// 最短往返数字；小数点位移 pp（value = 0.d1d2…×10^pp）满足
/// pp>15 || pp<-14 用 e 记号（无加号、指数不补零），否则定点；
/// 定点整数尾随 ".0"；±0 → "0.0"/"-0.0"；非有限值 MySQL 不落盘 → 报错。
fn format_json_double(v: f64) -> Result<String, BinlogError> {
    if !v.is_finite() {
        return Err(BinlogError::InvalidData(format!(
            "non-finite double in JSON binary: {v}"
        )));
    }
    if v == 0.0 {
        return Ok(if v.is_sign_negative() { "-0.0" } else { "0.0" }.to_string());
    }
    let neg = v < 0.0;
    let a = v.abs();
    // Rust {:e} = 最短科学计数法 "d[.ddd]eE"，value = d.ddd×10^E → pp = E+1
    let sci = format!("{a:e}");
    let (mant, exps) = sci
        .split_once('e')
        .ok_or_else(|| BinlogError::InvalidData("double format unexpected".into()))?;
    let exp: i32 = exps
        .parse()
        .map_err(|_| BinlogError::InvalidData("double format unexpected".into()))?;
    let (ip, fp) = mant.split_once('.').unwrap_or((mant, ""));
    let digits = format!("{ip}{fp}");
    let pp = exp + 1;

    let mut s = String::new();
    if neg {
        s.push('-');
    }
    if !(-14..=15).contains(&pp) {
        s.push_str(&digits[..1]);
        if digits.len() > 1 {
            s.push('.');
            s.push_str(&digits[1..]);
        }
        s.push('e');
        s.push_str(&(pp - 1).to_string());
    } else if pp <= 0 {
        s.push_str("0.");
        for _ in 0..(-pp) {
            s.push('0');
        }
        s.push_str(&digits);
    } else if (pp as usize) >= digits.len() {
        s.push_str(&digits);
        for _ in 0..(pp as usize - digits.len()) {
            s.push('0');
        }
        s.push_str(".0");
    } else {
        s.push_str(&digits[..pp as usize]);
        s.push('.');
        s.push_str(&digits[pp as usize..]);
    }
    Ok(s)
}

fn quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    push_escaped_str(s, &mut out);
    out.push('"');
    out
}

/// MySQL JSON 字符串转义集（真机 id7 验证）：仅 `"` `\` 与 <0x20 控制符
/// （\b \f \n \r \t，其余 `\u00xx` 小写）；DEL、`<>&`、非 ASCII UTF-8 原样。
fn push_escaped_str(s: &str, out: &mut String) {
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hx(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn dec(hex: &str) -> Result<String, BinlogError> {
        json_binary_to_text(&hx(hex))
    }

    /// 真机抓包 fixture（docker mysql:8.0.46 cap.j_probe，ROW 事件 blob 提取；
    /// 与 5.7.44 公共行逐字节一致）。期望文本 = MySQL CONVERT(j USING utf8mb4)
    /// 输出去除 MySQL 展示层空格后的紧凑形态。
    #[test]
    fn captured_top_level_scalars() {
        assert_eq!(dec("050b00").unwrap(), "11"); // i16
        assert_eq!(dec("0c03616263").unwrap(), "\"abc\""); // string
    }

    #[test]
    fn captured_objects_and_arrays() {
        assert_eq!(
            dec("000200170012000100130002000501000c15006261610178").unwrap(),
            r#"{"b":1,"aa":"x"}"#
        );
        assert_eq!(
            dec("0204002b000501000502000210000c290003001900050300000d0004000001000c000b0001000401006b0173").unwrap(),
            r#"[1,2,[3,{"k":true},null],"s"]"#
        );
        assert_eq!(
            dec("0003001c00190001001a0001001b000100040200040000040100666e74").unwrap(),
            r#"{"f":false,"n":null,"t":true}"#
        );
        assert_eq!(
            dec("020700230004010004020004000005000005ffff0b19000c2100000000000000f43f0178")
                .unwrap(),
            r#"[true,false,null,0,-1,1.25,"x"]"#
        );
        // inline 规则验证：literal/int16 恒 inline；string/double 走偏移
        assert_eq!(
            dec("0006004d002e0001002f0002003100030034000400380005003d0006000401000402000400000501000c43000b45006b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b0173000000000000f83f").unwrap(),
            r#"{"k":true,"kk":false,"kkk":null,"kkkk":1,"kkkkk":"s","kkkkkk":1.5}"#
        );
    }

    #[test]
    fn captured_int_edges() {
        assert_eq!(
            dec("00060060002e0003003100030034000300370003003a0003003d0003000500800740000a4400094c000754000958006931366933326936346e656775313675333200000080ffffffffffffffff0000000000000080ffff0000ffffffff00000000").unwrap(),
            r#"{"i16":-32768,"i32":-2147483648,"i64":18446744073709551615,"neg":-9223372036854775808,"u16":65535,"u32":4294967295}"#
        );
    }

    #[test]
    fn captured_string_escapes_and_utf8() {
        let exp = "{\"e\":\"a\\\"b\\\\c\\nd\",\"d1\":\"x\\u0001y\",\"del\":\"a\u{7f}b\",\"你好\":\"世界\u{1f30d}\"}";
        assert_eq!(
            dec("0004004700200001002100020023000300260006000c2c000c34000c38000c3c0065643164656ce4bda0e5a5bd076122625c630a640378017903617f620ae4b896e7958cf09f8c8d").unwrap(),
            exp
        );
    }

    /// d_probe（cap.d_probe，8.0.46）：30 组 IEEE754 位型 ↔ MySQL 渲染文本；
    /// 对象前缀 `00010014000b0001000b0c0061` = {"a":<double>}，`…000a0c0061` =
    /// {"a":<u64>}。验证 `.0` 后缀、e 记号阈值 pp>15||pp<-14、-0.0、±DBL_MIN/MAX。
    #[test]
    fn double_rendering_captures() {
        const P: &str = "00010014000b0001000b0c0061";
        const PU: &str = "00010014000b0001000a0c0061";
        let cases: &[(&str, &str)] = &[
            ("00003426f56b0c43", "1e15"),
            ("0080e03779c34143", "1e16"),
            ("00a0d88557347643", "1e17"),
            ("00c84e676dc1ab43", "1e18"),
            ("8dedb5a0f7c6b03e", "0.000001"),
            ("3a8c30e28e79453e", "0.00000001"),
            ("95d626e80b2e113e", "0.000000001"),
            ("ffffffffffffef7f", "1.7976931348623157e308"),
            ("0100000000000000", "5e-324"),
            ("0000000000000080", "-0.0"),
            ("0000000000000000", "0.0"),
            ("0000000000005940", "100.0"),
            ("95d626e80b2ef13d", "0.00000000025"),
            ("0000000000000840", "3.0"),
            ("92d54d06cff08044", "1e22"),
            ("9a9999999999b93f", "0.1"),
            ("0000901ec4bcd642", "100000000000000.0"),
            ("0000a7dcf7501543", "1.5e15"),
            ("fdff3326f56b0c43", "999999999999999.6"),
            ("40de77832112dc42", "123456789012345.0"),
            ("f168e388b5f8e43e", "0.00001"),
            ("bbbdd7d9df7cdb3d", "0.0000000001"),
            ("1656e79eaf03d23c", "0.000000000000001"),
            ("4d67e2f1059ea53c", "1.5e-16"),
            ("97d44646f50e673c", "1e-17"),
            ("0a00000000000000", "5e-323"),
            ("408cb5781daf1544", "1e20"),
            ("0180e03779c34143", "1.0000000000000002e16"),
        ];
        for (bits, want) in cases {
            let got = dec(&(P.to_string() + bits)).unwrap();
            assert_eq!(got, format!("{{\"a\":{want}}}"), "bits {bits}");
        }
        // u64 大整数不走 double：逐位精确输出
        assert_eq!(
            dec(&(PU.to_string() + "1581e97df4102211")).unwrap(),
            r#"{"a":1234567890123456789}"#
        );
        assert_eq!(
            dec(&(PU.to_string() + "0100000000002000")).unwrap(),
            r#"{"a":9007199254740993}"#
        );
    }

    #[test]
    fn captured_double_in_document() {
        assert_eq!(
            dec("00080084003c0001003d0001003e0001003f000100400001004100010042000100430001000b44000b4c000b54000b5c000b64000b6c000b74000b7c00737475767778797a182d4454fb21094050efe2d6e41a4b4480dbd9905605ea4348afbc9af2d77a3e7dc39425ad49b25400000000000029400000000000002840000000000000e0bf").unwrap(),
            r#"{"s":3.141592653589793,"t":1e21,"u":1.5e19,"v":0.0000001,"w":1e100,"x":12.5,"y":12.0,"z":-0.5}"#
        );
    }

    /// tm_probe（cap.tm_probe）：16 组 TIME(6) 位型 ↔ MySQL 文本，验证
    /// intPart=v>>24、h=(ip>>12)%1024、负值取绝对值加 '-'、frac 恒 6 位、
    /// 零值 "00:00:00.000000"（go-mysql 特判 "00:00:00" 为偏差，从真机）。
    #[test]
    fn time_rendering_captures() {
        const P: &str = "00010016000b0001000f0c00740b08";
        let cases: &[(&str, &str)] = &[
            ("0000008310000000", "01:02:03.000000"),
            ("0000000100000000", "00:00:01.000000"),
            ("000000fb6e270000", "630:59:59.000000"),
            ("20a1071ea5000000", "10:20:30.500000"),
            ("0000007defffffff", "-01:02:03.000000"),
            ("0000000591d8ffff", "-630:59:59.000000"),
            ("ffffffffffffffff", "-00:00:00.000001"),
            ("85ffff6bbddeffff", "-532:10:20.000123"),
            ("000000fb7e010000", "23:59:59.000000"),
            ("c1bdf00481feffff", "-23:59:59.999999"),
            ("7b00001ea5000000", "10:20:30.000123"),
            ("85ffffe15affffff", "-10:20:30.000123"),
            ("000000e25affffff", "-10:20:30.000000"),
            ("7b00009442190000", "404:10:20.000123"),
            ("0000000000000000", "00:00:00.000000"),
            ("000000ffffffffff", "-00:00:01.000000"),
        ];
        for (bits, want) in cases {
            assert_eq!(
                dec(&(P.to_string() + bits)).unwrap(),
                format!("{{\"t\":\"{want}\"}}"),
                "bits {bits}"
            );
        }
    }

    #[test]
    fn captured_opaque_datetime_and_zero_forms() {
        assert_eq!(
            dec("0004005000200001002100020023000200250003000f28000f32000f3c000f4600646474746d6e74730a080000000000c8a5190c0840e2018751c8a5190b08050000b8c80000000c080000008751c8a519").unwrap(),
            r#"{"d":"2020-03-04","dt":"2020-03-04 05:06:07.123456","tm":"12:34:56.000005","nts":"2020-03-04 05:06:07.000000"}"#
        );
        assert_eq!(
            dec("0002002c0012000300150003000f18000f22006e746d7a64740b080000008038ffffff0c080000000000820300").unwrap(),
            r#"{"ntm":"-12:30:00.000000","zdt":"0001-01-01 00:00:00.000000"}"#
        );
        // 顶层 opaque（CAST(x AS JSON) 于标量位置）与零值
        assert_eq!(
            dec("0f0c080000008751c8a519").unwrap(),
            "\"2020-03-04 05:06:07.000000\""
        );
        assert_eq!(dec("0f0a080000000000000000").unwrap(), "\"0000-00-00\"");
        assert_eq!(
            dec("0f0c080000000000000000").unwrap(),
            "\"0000-00-00 00:00:00.000000\""
        );
    }

    #[test]
    fn captured_opaque_decimal() {
        assert_eq!(
            dec("000200350012000300150004000f19000f220064656364656332f6070b028000000119f61120057ffffff3eb655bcaf204c72dffcfc6").unwrap(),
            r#"{"dec":1.25,"dec2":-12345678901234567890.12345}"#
        );
    }

    #[test]
    fn captured_large_object() {
        let mut blob = hx("0101000000b98601001300000003000c16000000626967a08d06");
        blob.extend(std::iter::repeat_n(b'q', 100_000));
        let mut want = String::from(r#"{"big":""#);
        want.push_str(&"q".repeat(100_000));
        want.push('"');
        want.push('}');
        assert_eq!(json_binary_to_text(&blob).unwrap(), want);
    }

    #[test]
    fn empty_input_yields_empty_text() {
        // go-mysql decodeJsonBinary:76-78 对空 data 返回空（NULL-into-NOT-NULL 历史场景）
        assert_eq!(json_binary_to_text(&[]).unwrap(), "");
    }

    /// 合成用例（格式按文档/权威构造，非抓包）：large 容器内 int32/uint32
    /// inline（go-mysql isInlineValue:244-253 规则，真机 id17 间接证实 small
    /// 容器下 i32 不 inline）。
    #[test]
    fn synthetic_large_container_inline_int32() {
        // large array [ -8(i32), 9(u32) ]，value entry = type + 4B inline
        let b = "03020000001200000007f8ffffff0809000000";
        assert_eq!(dec(b).unwrap(), "[-8,9]");
    }

    #[test]
    fn synthetic_empty_containers() {
        assert_eq!(dec("0000000400").unwrap(), "{}");
        assert_eq!(dec("0200000400").unwrap(), "[]");
    }

    #[test]
    fn synthetic_top_level_literals_and_int32() {
        assert_eq!(dec("0401").unwrap(), "true");
        assert_eq!(dec("0400").unwrap(), "null");
        assert_eq!(dec("0701000000").unwrap(), "1");
        assert_eq!(dec("08ffffffff").unwrap(), "4294967295");
    }

    /// 加固：嵌套容器深度上限。合成 100 层数组合法、101 层拒绝
    /// （go-mysql 递归下降、无深度限制，深文档可栈溢出）。
    #[test]
    fn depth_limit() {
        // 最内层 data：[count=1][size=8][entry: type=04 offset=00 00 → inline null] + 1B 填充
        fn wrap(depth: usize) -> Vec<u8> {
            let mut child: Vec<u8> = vec![0x01, 0x00, 0x08, 0x00, LITERAL, 0x00, 0x00, 0x00];
            for _ in 0..depth {
                let size = (7 + child.len()) as u16;
                let mut next = Vec::with_capacity(size as usize);
                next.extend_from_slice(&1u16.to_le_bytes());
                next.extend_from_slice(&size.to_le_bytes());
                next.push(SMALL_ARRAY);
                next.extend_from_slice(&7u16.to_le_bytes());
                next.extend_from_slice(&child);
                child = next;
            }
            let mut buf = vec![SMALL_ARRAY];
            buf.extend_from_slice(&child);
            buf
        }
        // 顶层容器 + 99 层包装 = 100 层 → 允许，输出 100 个 '['
        let ok = json_binary_to_text(&wrap(99)).unwrap();
        assert_eq!(ok, format!("{}null{}", "[".repeat(100), "]".repeat(100)));
        // 101 层 → 拒绝
        let err = json_binary_to_text(&wrap(100)).unwrap_err();
        assert!(matches!(err, BinlogError::InvalidData(_)), "{err:?}");
    }

    /// 加固：一切截断/坏头部/未知类型返回错误而非 panic。
    #[test]
    fn corrupt_inputs_never_panic() {
        assert_eq!(dec("00"), Err(BinlogError::TooShort)); // 截断头
        // 未知顶层类型字节 0x10
        assert!(matches!(dec("10"), Err(BinlogError::InvalidData(_))));
        // 坏 literal（0x07）
        assert!(matches!(dec("0407"), Err(BinlogError::InvalidData(_))));
        // header 尺寸超声明 size：count=0xffff、size=4
        assert!(matches!(
            dec("00ffff0400"),
            Err(BinlogError::InvalidData(_))
        ));
        // varint 长度超 u32::MAX（ff ff ff ff 10 → 0x1_0FFFFFFF）
        assert!(matches!(
            dec("0cffffffff10"),
            Err(BinlogError::InvalidData(_))
        ));
        // varint 截断（首字节 continuation 但无后继）
        assert_eq!(dec("0c80"), Err(BinlogError::TooShort));
        // 非 UTF-8 字符串
        assert!(matches!(dec("0c01ff"), Err(BinlogError::InvalidData(_))));
        // 键偏移 < header_size（header=11，key off=2）
        assert!(matches!(
            dec("0001000c000200010004010000"),
            Err(BinlogError::InvalidData(_))
        ));
        // 非有限 double（NaN=0x7FF8… / +Inf=0x7FF0…，LE 位型）
        assert!(matches!(
            dec("0b000000000000f87f"),
            Err(BinlogError::InvalidData(_))
        ));
        assert!(matches!(
            dec("0b000000000000f07f"),
            Err(BinlogError::InvalidData(_))
        ));
        // 值偏移越界（off=0x20 > data len 8）
        assert_eq!(dec("02010008000c200001"), Err(BinlogError::TooShort));
    }

    #[test]
    fn invalid_utf8_in_container_key_and_string() {
        // string：len=3、尾字节 C3 截断序列
        assert!(matches!(
            dec("0c034142c3"),
            Err(BinlogError::InvalidData(_))
        ));
        // 对象键非法 UTF-8：{"\xff":true}（header=11，key@11 len1）
        assert!(matches!(
            dec("0001000c000b000100040100ff"),
            Err(BinlogError::InvalidData(_))
        ));
    }
}
