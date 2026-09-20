//! 值分发：`decode_value(buf, pos, ctx)` —— schema × (tp, meta) → [`ColumnValue`]。
//!
//! 分支图逐条镜像 go-mysql `replication/row_event.go` 的
//! `RowsEvent.decodeValue`（vendor 行号 :1004-1170，含 STRING 前奏 :1007-1022），
//! 类型码统一取 [`super::field_types`]（官方 iota：FLOAT=4/DOUBLE=5、
//! TIMESTAMP2=17/DATETIME2=18/TIME2=19、JSON=245…GEOMETRY=255；
//! T10 Step 0 起原私有 `mod tp` 与 table_map.rs 的误名副本一并并入该表）。
//!
//! 与简报/控制器的对接口径：
//! - `ColCtx` 在简报 3 字段之外增 `tz_offset_secs`（TIMESTAMP2/V1 TIMESTAMP
//!   的会话时区换算，T6 裁定；T14 由 `--time-zone` 喂值，测试默认 0）。
//! - unsigned 判定不在 TABLE_MAP（T4 结论），由 schema 层
//!   [`SchemaCol::unsigned`] 提供，整型族据此走 `decode_int` 的 UInt 分支。
//! - BLOB(252) 是 binlog 对 TINYTEXT/TEXT/MEDIUMTEXT/LONGTEXT/TINYBLOB/…/
//!   LONGBLOB 的统一存储类型；TEXT/BLOB 判别谓词照 my2sql-go 裁判：
//!   `strings.Contains(strings.ToLower(tpDef), "text")`
//!   （base/sqlgen.go:114-118、base/events.go:102-104）→ 本侧
//!   `schema.type_name`（小写无括号）含 `text` → `Str`（再经 [`utf8_safe`]
//!   校验，非法降级 `Bytes`），否则真 blob → `Bytes`。
//! - JSON/GEOMETRY/BLOB 长度前缀均为 **meta 字节宽的定宽小端**（对照
//!   `decodeBlob`(row_event.go:1539-1563) 与 JSON 分支 `FixedLengthInt`
//!   （=小端，mysql/util.go:121-127）；真机 8.0.46 抓包（/tmp/t9probe，
//!   probe 表 26 列，行 2186B 逐列精确消费）证实 JSON meta=4、LE u32。
//!   简报/控制器备忘中「JSON=LNE(read_lns)」与权威不符：meta 是定宽前缀的
//!   宽度而非 LNE 哨兵编码，按权威实现（差异详见 task-9 报告）。
//! - VARCHAR/VAR_STRING/STRING 前缀宽度 = max_len<256 ? 1B : 2B LE
//!   （`decodeString` row_event.go:1173-1185；真机 VARCHAR(50 utf8mb4)
//!   meta=200→1B、VARCHAR(3000)→meta=12000→2B；CHAR(5) ascii meta=0xFE05
//!   走 else 分支 length=5→1B 前缀，均已实测）。
//! - ENUM/SET：真机以 STRING 伪装入 table_map（meta 高字节 b0=0xF7/0xF8，
//!   经 `real_string_type` 还原；真机抓取 `fe..f701`/`fe..f801` 证实），
//!   值 = 1B/2B(ENUM)、nB(SET) 序号/位图 → `UInt`（D4 裁定，名称留 P2）。
//! - 旧族（V1 row 事件，type 码 7/12/11，非打包）：TIMESTAMP=4B LE 秒、
//!   DATETIME=8B LE `YYYYMMDDHHMMSS`（**无**微秒——微秒只在 5.6+ 的 *2 族
//!   出现，简报「micro*1e6」猜测不成立）、TIME=3B LE `HHMMSS`（hour 可达
//!   838）。按 go-mysql 分支实现；V1 事件仅 5.5 及以下产生（D6 不支持），
//!   保留为尽职实现。
//! - TIMESTAMP 零值（秒=0）：go-mysql `formatZeroTime` 输出
//!   "0000-00-00 00:00:00"，T6 控制器裁定改判为 epoch+tz（1970-01-01）；
//!   本模块旧 TIMESTAMP 分支沿用同一裁定，保持 V1/V2 行为一致（T15 白名单
//!   已含该条）。
//! - 未知/不支持类型码：go-mysql default 分支返回 error（无法知道列宽，
//!   行同步必断）——本侧同口径返回 `InvalidData` + `tracing::warn!`，
//!   **不**做 Bytes 兜底（简报「兜底 unknown→Bytes」仅对宽度可知类型安全，
//!   而所有宽度已知的官方类型均已覆盖；详见 task-9 报告裁定链）。

use super::decimal::decode_decimal;
use super::error::BinlogError;
use super::int::{ColumnValue, decode_float, decode_int};
use super::json::json_binary_to_text;
use super::time::{
    decode_date2, decode_datetime2, decode_time2, decode_timestamp_v1, decode_timestamp2,
};
use crate::metadata::schema::SchemaCol;

// 类型码统一取自 super::field_types（T10 Step 0 合并，原私有 `mod tp` 删除）。
use super::field_types as tp;

/// 单列解码上下文：binlog 类型码 + TABLE_MAP 列 metadata + schema 交叉信息。
///
/// `tz_offset_secs` 为简报 3 字段之外的新增（控制器裁定 1）：TIMESTAMP 族
/// 需会话时区偏移，来源 `--time-zone`（T14），无时区语义的类型忽略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColCtx<'a> {
    pub tp: u8,
    pub meta: u16,
    pub schema: &'a SchemaCol,
    pub tz_offset_secs: i32,
}

impl<'a> ColCtx<'a> {
    /// 便捷构造（tz 默认 0；测试密集使用，T10/T14 用完整结构体字面量）。
    // 生产侧行解码走完整字面量（需注入 tz），本构造暂仅测试消费（T12+ 或移除豁免）。
    #[allow(dead_code)]
    pub fn new(tp: u8, meta: u16, schema: &'a SchemaCol) -> Self {
        Self {
            tp,
            meta,
            schema,
            tz_offset_secs: 0,
        }
    }
}

/// 从 `buf[at..]` 读 `w` 字节小端定宽整数，不推进任何游标；不足 → TooShort。
fn read_fixed_le(buf: &[u8], at: usize, w: usize) -> Result<u64, BinlogError> {
    let s = buf.get(at..at + w).ok_or(BinlogError::TooShort)?;
    let mut v = 0u64;
    for (i, &b) in s.iter().enumerate() {
        v |= (b as u64) << (8 * i);
    }
    Ok(v)
}

/// `w` 字节 LE 定宽长度前缀 + 该长度 payload（BLOB 家族 row_event.go:1539-1563
/// 与 JSON/GEOMETRY 的 `FixedLengthInt`（=小端，util.go:121-127）同形态）。
/// `w ∉ 1..=4` → InvalidData（go-mysql "invalid blob packlen"）。
fn read_len_payload(buf: &[u8], pos: &mut usize, w: usize) -> Result<Vec<u8>, BinlogError> {
    if !(1..=4).contains(&w) {
        return Err(BinlogError::InvalidData(format!(
            "invalid blob packlen: {w}"
        )));
    }
    let n = read_fixed_le(buf, *pos, w)? as usize;
    let at = *pos + w;
    let s = buf.get(at..at + n).ok_or(BinlogError::TooShort)?;
    *pos = at + n;
    Ok(s.to_vec())
}

/// VARCHAR/VAR_STRING/STRING 的解码字符串：前缀宽 = max_len < 256 ? 1B : 2B LE
/// （`decodeString` row_event.go:1173-1185；真机 meta=200→1B、12000→2B）。
fn read_str_payload(buf: &[u8], pos: &mut usize, max_len: usize) -> Result<Vec<u8>, BinlogError> {
    let w = if max_len < 256 { 1 } else { 2 };
    let n = read_fixed_le(buf, *pos, w)? as usize;
    let at = *pos + w;
    let s = buf.get(at..at + n).ok_or(BinlogError::TooShort)?;
    *pos = at + n;
    Ok(s.to_vec())
}

/// BLOB(252) 槽位的 TEXT vs BLOB 判别谓词，照 my2sql-go 裁判原样：
/// `strings.Contains(strings.ToLower(tpDef), "text")`（base/sqlgen.go:114-118、
/// base/events.go:102-104）。`SchemaCol::type_name` 已小写无括号（T11 归一化，
/// 对照 base/funcs.go:86-96），此处仍 to_lowercase 双保险。
fn is_text_col(ctx: &ColCtx) -> bool {
    ctx.schema.type_name.to_lowercase().contains("text")
}

/// V1 DATETIME（type 码 12）：8B LE 整数 `YYYYMMDDHHMMSS`，**无**微秒段
/// （微秒只在 5.6+ 的 DATETIME2 出现——简报「micro*1e6」猜测与 go-mysql
/// :1073-1092 均否定）。零值 → "0000-00-00 00:00:00"（go-mysql 同口径）。
fn datetime_v1_text(v: u64) -> String {
    if v == 0 {
        return "0000-00-00 00:00:00".into();
    }
    let (d, t) = (v / 1_000_000, v % 1_000_000);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        d / 10_000,
        d / 100 % 100,
        d % 100,
        t / 10_000,
        t / 100 % 100,
        t % 100,
    )
}

/// V1 TIME（type 码 11）：3B LE 整数 `HHMMSS`（go-mysql :1098-1106），
/// hour 可达 838（>24h），零值 → "00:00:00"。
fn time_v1_text(v: u64) -> String {
    if v == 0 {
        return "00:00:00".into();
    }
    // T9 校准：Go `%02d:%02d:%02d`（row_event.go:1105）——hour 也零位左补。
    format!("{:02}:{:02}:{:02}", v / 10_000, v / 100 % 100, v % 100)
}

/// 值分发主入口：从 `buf[*pos..]` 按 `ctx` 解码一列，成功时 `pos` 前进到
/// 该列末尾；任何错误路径不污染 `pos`（T5-T8 口径）。分支图镜像
/// go-mysql `decodeValue`（row_event.go:1004-1170）。
pub fn decode_value(buf: &[u8], pos: &mut usize, ctx: &ColCtx) -> Result<ColumnValue, BinlogError> {
    // ---- STRING 前奏（row_event.go:1007-1022）：CHAR/ENUM/SET 等在
    // table_map 里伪装成 type 254，真类型码藏在 meta 高字节 b0：
    // b0&0x30==0x30（0xFE CHAR 与 0xF5..0xF8 均满足）→ 长度=b1、tp=b0；
    // 否则（旧形态）长度=b1|((b0&0x30)^0x30)<<4、tp=b0|0x30。
    let mut tp = ctx.tp;
    let mut m = ctx.meta;
    if tp == tp::STRING && m >= 256 {
        let b0 = (m >> 8) as u8;
        let b1 = (m & 0xFF) as u8;
        if b0 & 0x30 != 0x30 {
            // T9 校准：对照 go-mysql :1013 `uint16(b1) | (uint16((b0&0x30)^0x30) << 4)`
            // ——**先转 u16 再移位**；旧写法在 u8 域移位使 {0x10,0x20,0x30}<<4 全截为 0。
            m = b1 as u16 | (((b0 & 0x30) ^ 0x30) as u16) << 4;
            tp = b0 | 0x30;
        } else {
            m = b1 as u16;
            tp = b0;
        }
    }

    let mut v = match tp {
        // ---- 定宽整型族 + BIT/YEAR（unsigned 判定来自 schema，T4 结论）----
        tp::TINY | tp::SHORT | tp::LONG | tp::INT24 | tp::LONGLONG | tp::YEAR | tp::BIT => {
            decode_int(buf, pos, tp, ctx.schema.unsigned, ctx.meta)?
        }
        tp::FLOAT | tp::DOUBLE => decode_float(buf, pos, tp)?,
        tp::NULL => ColumnValue::Null,
        // ---- 精确小数：meta = precision<<8 | scale（真机直存 246，T4 实测）----
        tp::NEWDECIMAL => {
            ColumnValue::Decimal(decode_decimal(buf, pos, ctx.meta >> 8, ctx.meta & 0xFF)?)
        }
        // ---- 变长二进制槽位：BLOB(252) 统一承载 TEXT/BLOB 全家族，
        // Str/Bytes 由 schema 谓词判别（裁判口径，见模块注释）----
        tp::BLOB => {
            let payload = read_len_payload(buf, pos, m as usize)?;
            if is_text_col(ctx) {
                ColumnValue::Str(payload)
            } else {
                ColumnValue::Bytes(payload)
            }
        }
        // ---- 变长字符族：前缀宽 = 256 门槛（decodeString）----
        tp::VAR_STRING | tp::VARCHAR | tp::STRING => {
            ColumnValue::Str(read_str_payload(buf, pos, m as usize)?)
        }
        // ---- JSON：meta(=4) 字节 LE 定宽前缀 + JSONB payload。
        // 简报/控制器备忘「LNE(read_lns)」与权威不符：go-mysql 用
        // FixedLengthInt（小端定宽，util.go:121），真机 8.0.46 实测 LE u32。----
        tp::JSON => {
            let payload = read_len_payload(buf, pos, m as usize)?;
            ColumnValue::Json(json_binary_to_text(&payload)?)
        }
        // ---- GEOMETRY：同 blob 形态前缀，payload（SRID+WKB）原样保真（裁定 7）----
        tp::GEOMETRY => ColumnValue::Bytes(read_len_payload(buf, pos, m as usize)?),
        // ---- ENUM/SET：序号/位图 → UInt（D4/裁定 7，名称映射留 P2）。
        // packlen 藏在（前奏还原后的）meta 低字节。----
        tp::ENUM => {
            let n = (m & 0xFF) as usize;
            let ord = match n {
                1 => buf.get(*pos).copied().ok_or(BinlogError::TooShort)? as u64,
                2 => read_fixed_le(buf, *pos, 2)?,
                _ => {
                    return Err(BinlogError::InvalidData(format!(
                        "unknown ENUM packlen: {n}"
                    )));
                }
            };
            *pos += n;
            ColumnValue::UInt(ord)
        }
        tp::SET => {
            let n = (m & 0xFF) as usize;
            if !(1..=8).contains(&n) {
                return Err(BinlogError::InvalidData(format!(
                    "unknown SET packlen: {n}"
                )));
            }
            let bits = read_fixed_le(buf, *pos, n)?;
            *pos += n;
            ColumnValue::UInt(bits)
        }
        // ---- 时间族 V2 打包形态（5.6+，fsp = meta）----
        tp::DATE => ColumnValue::Str(decode_date2(buf, pos)?.into_bytes()),
        tp::DATETIME2 => ColumnValue::Str(decode_datetime2(buf, pos, m as u8)?.into_bytes()),
        tp::TIMESTAMP2 => {
            ColumnValue::Str(decode_timestamp2(buf, pos, m as u8, ctx.tz_offset_secs)?.into_bytes())
        }
        tp::TIME2 => ColumnValue::Str(decode_time2(buf, pos, m as u8)?.into_bytes()),
        // ---- 时间族 V1 旧形态（仅 <5.6 V1 事件；D6 不支持，尽职实现）----
        tp::TIMESTAMP => {
            ColumnValue::Str(decode_timestamp_v1(buf, pos, ctx.tz_offset_secs)?.into_bytes())
        }
        tp::DATETIME => {
            let raw = read_fixed_le(buf, *pos, 8)?;
            *pos += 8;
            ColumnValue::Str(datetime_v1_text(raw).into_bytes())
        }
        tp::TIME => {
            let raw = read_fixed_le(buf, *pos, 3)?;
            *pos += 3;
            ColumnValue::Str(time_v1_text(raw).into_bytes())
        }
        // ---- 其余（legacy DECIMAL=0、NEWDATE=14、TINY/MEDIUM/LONG_BLOB=249-251、
        // 未知码）：go-mysql default 分支 → error。宽度不可知时静默 Bytes 兜底
        // 会导致整行错位，比报错更糟——拒绝并告警（裁定链见 task-9 报告）。----
        _ => {
            tracing::warn!(
                tp,
                column = ctx.schema.name,
                "decode_value: unsupported column type"
            );
            return Err(BinlogError::InvalidData(format!(
                "unsupport decode raw type: {tp}"
            )));
        }
    };
    utf8_safe(&mut v);
    Ok(v)
}

/// UTF-8 安全阀：`Str` 且 simdutf8 校验失败 → 原地降级为 `Bytes`
/// （D3 值保真原则：绝不 lossy 重写字节；其余变体不动）。
/// decode_value 的所有 Str 产生分支末尾统一过闸。
pub fn utf8_safe(v: &mut ColumnValue) {
    if let ColumnValue::Str(bytes) = v
        && simdutf8::basic::from_utf8(bytes).is_err()
    {
        *v = ColumnValue::Bytes(std::mem::take(bytes));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::schema::SchemaCol;

    fn sc(type_name: &str) -> SchemaCol {
        SchemaCol {
            name: "c".into(),
            type_name: type_name.into(),
            unsigned: false,
        }
    }
    fn scu(type_name: &str) -> SchemaCol {
        SchemaCol {
            unsigned: true,
            ..sc(type_name)
        }
    }

    /// 辅助：从偏移 0 解码一列，断言恰好读完全部 fixture 字节。
    fn dv(buf: &[u8], ctx: &ColCtx) -> ColumnValue {
        let mut pos = 0usize;
        let v = decode_value(buf, &mut pos, ctx).unwrap();
        assert_eq!(pos, buf.len(), "decode_value must consume all bytes");
        v
    }
    /// 辅助：带会话时区偏移的解码（借用要求 SchemaCol 具名）。
    fn dv_tz(buf: &[u8], tpc: u8, meta: u16, t: &str, tz: i32) -> ColumnValue {
        let s = sc(t);
        let c = ColCtx {
            tp: tpc,
            meta,
            schema: &s,
            tz_offset_secs: tz,
        };
        dv(buf, &c)
    }
    fn s(t: &str) -> ColumnValue {
        ColumnValue::Str(t.as_bytes().to_vec())
    }

    // ---- 以下 fixture 除注明外取自 docker mysql:8.0.46 真机 ROW binlog
    // （/tmp/t9probe probe 表 26 列单行 INSERT，行体 2186B，go-mysql 裁判
    // 逐列解析比对通过，见 task-9 报告）。----

    #[test]
    fn null_type_code_consumes_zero_bytes() {
        let c = sc("int");
        let mut pos = 3usize;
        assert_eq!(
            decode_value(b"xxx", &mut pos, &ColCtx::new(tp::NULL, 0, &c)).unwrap(),
            ColumnValue::Null
        );
        assert_eq!(pos, 3);
    }

    #[test]
    fn int_family_routes_unsigned_from_schema() {
        // LONG 0xFFFFFFFF：signed → -1；schema unsigned → UInt(4294967295)
        let b = [0xFF, 0xFF, 0xFF, 0xFF];
        assert_eq!(
            dv(&b, &ColCtx::new(tp::LONG, 0, &sc("int"))),
            ColumnValue::Int(-1)
        );
        assert_eq!(
            dv(&b, &ColCtx::new(tp::LONG, 0, &scu("int unsigned"))),
            ColumnValue::UInt(u32::MAX as u64)
        );
        // TINY/SHORT/INT24/LONGLONG 冒烟（全域在 int.rs 已测）
        assert_eq!(
            dv(&[0x2A], &ColCtx::new(tp::TINY, 0, &sc("tinyint"))),
            ColumnValue::Int(42)
        );
        assert_eq!(
            dv(&[0x34, 0x12], &ColCtx::new(tp::SHORT, 0, &sc("smallint"))),
            ColumnValue::Int(0x1234)
        );
        assert_eq!(
            dv(
                &[0x00, 0x00, 0x80],
                &ColCtx::new(tp::INT24, 0, &scu("mediumint unsigned"))
            ),
            ColumnValue::UInt(8_388_608)
        );
        assert_eq!(
            dv(&[0xFF; 8], &ColCtx::new(tp::LONGLONG, 0, &sc("bigint"))),
            ColumnValue::Int(-1)
        );
    }

    #[test]
    fn newdecimal_precision_scale_from_meta() {
        // 真机 DECIMAL(5,2) −12.34：meta=0x0502（高字节 precision），字节 7f f3 dd
        assert_eq!(
            dv(
                &[0x7F, 0xF3, 0xDD],
                &ColCtx::new(tp::NEWDECIMAL, 0x0502, &sc("decimal"))
            ),
            ColumnValue::Decimal("-12.34".into())
        );
    }

    #[test]
    fn float_double_dispatch() {
        // 真机 f=1.5 → 00 00 c0 3f；d=3.25 → 00…0a 40（meta 8.0.17 起为 4/8，分发忽略）
        assert_eq!(
            dv(
                &[0x00, 0x00, 0xC0, 0x3F],
                &ColCtx::new(tp::FLOAT, 4, &sc("float"))
            ),
            ColumnValue::Double("1.5".into())
        );
        assert_eq!(
            dv(
                &[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0A, 0x40],
                &ColCtx::new(tp::DOUBLE, 8, &sc("double"))
            ),
            ColumnValue::Double("3.25".into())
        );
    }

    #[test]
    fn bit_year_dispatch_via_int() {
        // 真机 b'110000000'（=384）：meta=0x0101，字节 01 80（大端）
        assert_eq!(
            dv(&[0x01, 0x80], &ColCtx::new(tp::BIT, 0x0101, &sc("bit"))),
            ColumnValue::UInt(384)
        );
        // 真机 YEAR 2026 → 1B 0x7E
        assert_eq!(
            dv(&[0x7E], &ColCtx::new(tp::YEAR, 0, &sc("year"))),
            ColumnValue::UInt(2026)
        );
    }

    #[test]
    fn temporal_v2_family_to_str() {
        // 真机行：DATE 2020-07-16（f0 c8 0f，T6 fixture）
        assert_eq!(
            dv(&[0xF0, 0xC8, 0x0F], &ColCtx::new(tp::DATE, 0, &sc("date"))),
            s("2020-07-16")
        );
        // DATETIME2 fsp0：99 a6 e0 ab 09
        assert_eq!(
            dv(
                &[0x99, 0xA6, 0xE0, 0xAB, 0x09],
                &ColCtx::new(tp::DATETIME2, 0, &sc("datetime"))
            ),
            s("2020-07-16 10:44:09")
        );
        // TIMESTAMP2 fsp3（真机 ts3）：5f 10 2f 79 04 b0；+08:00 → 裁判 col8 同值
        assert_eq!(
            dv_tz(
                &[0x5F, 0x10, 0x2F, 0x79, 0x04, 0xB0],
                tp::TIMESTAMP2,
                3,
                "timestamp",
                8 * 3600
            ),
            s("2020-07-16 18:44:09.120")
        );
        // TIME2 fsp0（真机 t）：80 ab 09
        assert_eq!(
            dv(&[0x80, 0xAB, 0x09], &ColCtx::new(tp::TIME2, 0, &sc("time"))),
            s("10:44:09")
        );
    }

    #[test]
    fn legacy_timestamp_v1_plain_le_seconds() {
        // 真机 ts=1594896249(2020-07-16 10:44:09 UTC) 的 V1 形态：4B **小端**秒
        let sec: u32 = 0x5F10_2F79;
        assert_eq!(
            dv(
                &sec.to_le_bytes(),
                &ColCtx::new(tp::TIMESTAMP, 0, &sc("timestamp"))
            ),
            s("2020-07-16 10:44:09")
        );
        // 零值：T6 裁定同口径 → epoch（go-mysql formatZeroTime 差异见模块注释）
        assert_eq!(
            dv(
                &[0, 0, 0, 0],
                &ColCtx::new(tp::TIMESTAMP, 0, &sc("timestamp"))
            ),
            s("1970-01-01 00:00:00")
        );
    }

    #[test]
    fn legacy_datetime_v1_decimal_packed_8b() {
        // V1 DATETIME：8B LE，值 = YYYYMMDDHHMMSS（无微秒，微秒属 *2 族——
        // 简报 micro*1e6 猜测不成立，对照 go-mysql :1073-1092）
        let v: u64 = 20_200_716_104_409;
        assert_eq!(
            dv(
                &v.to_le_bytes(),
                &ColCtx::new(tp::DATETIME, 0, &sc("datetime"))
            ),
            s("2020-07-16 10:44:09")
        );
        assert_eq!(
            dv(&[0u8; 8], &ColCtx::new(tp::DATETIME, 0, &sc("datetime"))),
            s("0000-00-00 00:00:00")
        );
    }

    #[test]
    fn legacy_time_v1_hhmmss() {
        // V1 TIME：3B LE 整数 HHMMSS（go-mysql :1098-1106），0 → 00:00:00
        assert_eq!(
            dv(
                &[0xD9, 0x97, 0x01], // 104409
                &ColCtx::new(tp::TIME, 0, &sc("time"))
            ),
            s("10:44:09")
        );
        // T9 校准：hour<10 必须零位左补（Go `%02d:%02d:%02d`）——90506 → "09:05:06"
        assert_eq!(
            dv(
                &[0x8A, 0x61, 0x01], // 90506
                &ColCtx::new(tp::TIME, 0, &sc("time"))
            ),
            s("09:05:06")
        );
        // 838:59:59（TIME 上限，>24h）
        assert_eq!(
            dv(
                &[0xA7, 0xF5, 0x7F], // 8385959
                &ColCtx::new(tp::TIME, 0, &sc("time"))
            ),
            s("838:59:59")
        );
        assert_eq!(
            dv(&[0, 0, 0], &ColCtx::new(tp::TIME, 0, &sc("time"))),
            s("00:00:00")
        );
    }

    /// T9 校准：STRING 前奏 if 分支（b0&0x30 != 0x30，旧伪装形态）的还原长度
    /// 高位必须按 go-mysql :1013 `uint16((b0&0x30)^0x30) << 4` 先转宽再移位——
    /// u8 域移位会把 {0x10,0x20,0x30}<<4 全部截成 0。
    /// 用例（合成，前奏规则穷举 b0&0x30 的三个非 0x30 值）：b0∈{0xCD,0xDD,0xED}
    /// 均还原 tp=b0|0x30=0xFD(VAR_STRING)，length=b1|{0x300,0x200,0x100} ≥256
    /// → 行内 2B LE 前缀（decodeString :1173-1185 用还原后的 length 判前缀宽）。
    #[test]
    fn string_preamble_if_branch_restores_high_bits() {
        for (b0, hi) in [(0xCDu8, 0x300u16), (0xDD, 0x200), (0xED, 0x100)] {
            let meta = ((b0 as u16) << 8) | 0x01; // b1 = 1
            // 2B LE 前缀 = 1，payload 'a'；u8 截断 bug 会按 1B 前缀少读 1 字节、
            // 且 payload 变成 [0x00]——两侧夹击，pos 与值都能钉死。
            assert_eq!(
                dv(
                    &[0x01, 0x00, b'a'],
                    &ColCtx::new(tp::STRING, meta, &sc("varbinary"))
                ),
                s("a"),
                "b0={b0:#04X} 应还原 length={:#X}",
                hi | 0x01,
            );
        }
    }

    #[test]
    fn char_real_machine_one_byte_prefix() {
        // 真机 CHAR(5) ascii：meta=0xFE05（b0=0xFE，0xFE&0x30==0x30 → else 分支
        // length=b1=5 <256）→ 行内 1B 前缀：03 'abc'
        assert_eq!(
            dv(
                &[0x03, b'a', b'b', b'c'],
                &ColCtx::new(tp::STRING, 0xFE05, &sc("char"))
            ),
            ColumnValue::Str(b"abc".to_vec())
        );
        // 真机 CHAR(5) utf8mb4：meta=0xFE14（pack_length=20）→ 1B 前缀 02 'ab'
        assert_eq!(
            dv(
                &[0x02, b'a', b'b'],
                &ColCtx::new(tp::STRING, 0xFE14, &sc("char"))
            ),
            ColumnValue::Str(b"ab".to_vec())
        );
        // STRING meta<256（旧形态 char_length 直存）：length=meta
        assert_eq!(
            dv(
                &[0x02, b'h', b'i'],
                &ColCtx::new(tp::STRING, 5, &sc("char"))
            ),
            ColumnValue::Str(b"hi".to_vec())
        );
    }

    #[test]
    fn varchar_varstring_prefix_widths() {
        // 真机 VARCHAR(50) utf8mb4：meta=200 → 1B 前缀：02 'vs'
        assert_eq!(
            dv(
                &[0x02, b'v', b's'],
                &ColCtx::new(tp::VARCHAR, 200, &sc("varchar"))
            ),
            ColumnValue::Str(b"vs".to_vec())
        );
        // 简报指定 width-2 用例：meta=300 → 2B LE 前缀（真机 v_long=12000 同形态）
        assert_eq!(
            dv(
                &[0x02, 0x00, b'h', b'i'],
                &ColCtx::new(tp::VARCHAR, 300, &sc("varchar"))
            ),
            ColumnValue::Str(b"hi".to_vec())
        );
        assert_eq!(
            dv(
                &[0x03, 0x00, b'a', b'b', b'c'],
                &ColCtx::new(tp::VAR_STRING, 300, &sc("varbinary"))
            ),
            ColumnValue::Str(b"abc".to_vec())
        );
        // VAR_STRING（老 varchar/5.0 CHAR 等）meta=250 → 1B 前缀
        assert_eq!(
            dv(
                &[0x01, b'x'],
                &ColCtx::new(tp::VAR_STRING, 250, &sc("varchar"))
            ),
            ColumnValue::Str(b"x".to_vec())
        );
    }

    #[test]
    fn enum_set_ordinals_to_uint() {
        // 真机 ENUM('a','bb','ccc') 值 'bb'：STRING 伪装 meta=0xF701，1B 序号 02
        assert_eq!(
            dv(&[0x02], &ColCtx::new(tp::STRING, 0xF701, &sc("enum"))),
            ColumnValue::UInt(2)
        );
        // ENUM 2B 形态（>255 成员；合成）：meta=0xF702，LE
        assert_eq!(
            dv(&[0x34, 0x12], &ColCtx::new(tp::STRING, 0xF702, &sc("enum"))),
            ColumnValue::UInt(0x1234)
        );
        // 真机 SET('x','y','z') 值 'x,z'：meta=0xF801，位图 05
        assert_eq!(
            dv(&[0x05], &ColCtx::new(tp::STRING, 0xF801, &sc("set"))),
            ColumnValue::UInt(5)
        );
        // SET 64bit 形态（合成）
        assert_eq!(
            dv(
                &[0, 0, 0, 0, 0, 0, 0, 0x80],
                &ColCtx::new(tp::STRING, 0xF808, &sc("set"))
            ),
            ColumnValue::UInt(1 << 63)
        );
        // 非法 packlen（go-mysql: "Unknown ENUM packlen"）→ InvalidData，pos 不动
        let mut pos = 0usize;
        assert!(matches!(
            decode_value(
                &[0x01, 0x02, 0x03],
                &mut pos,
                &ColCtx::new(tp::STRING, 0xF703, &sc("enum"))
            ),
            Err(BinlogError::InvalidData(_))
        ));
        assert_eq!(pos, 0);
    }

    #[test]
    fn blob_text_discrimination_by_schema_typename() {
        // 真机 TEXT：tp=252 meta=2，0b 00 "sample text"；type_name="text" → Str
        let mut buf = vec![0x0B, 0x00];
        buf.extend_from_slice(b"sample text");
        assert_eq!(
            dv(&buf, &ColCtx::new(tp::BLOB, 2, &sc("text"))),
            ColumnValue::Str(b"sample text".to_vec())
        );
        // TINY/MEDIUM/LONGTEXT：谓词 = type_name 含 "text"（sqlgen.go:116）；
        // 真机 tinytext 存储形态 = tp252 meta1，前缀 09 + "tinytext!"
        assert_eq!(
            dv(
                &[0x09, b't', b'i', b'n', b'y', b't', b'e', b'x', b't', b'!'],
                &ColCtx::new(tp::BLOB, 1, &sc("tinytext"))
            ),
            ColumnValue::Str(b"tinytext!".to_vec())
        );
        // 真 blob（type_name="blob"）：0x03 0x00 01 02 03 → Bytes
        assert_eq!(
            dv(
                &[0x03, 0x00, 0x01, 0x02, 0x03],
                &ColCtx::new(tp::BLOB, 2, &sc("blob"))
            ),
            ColumnValue::Bytes(vec![1, 2, 3])
        );
        // 真机 MEDIUMBLOB（meta=3 LE）：02 00 00 04 05
        assert_eq!(
            dv(
                &[0x02, 0x00, 0x00, 0x04, 0x05],
                &ColCtx::new(tp::BLOB, 3, &sc("mediumblob"))
            ),
            ColumnValue::Bytes(vec![4, 5])
        );
        // 真机 LONGBLOB（meta=4 LE）：04 00 00 00 "long"
        assert_eq!(
            dv(
                &[0x04, 0x00, 0x00, 0x00, b'l', b'o', b'n', b'g'],
                &ColCtx::new(tp::BLOB, 4, &sc("longblob"))
            ),
            ColumnValue::Bytes(b"long".to_vec())
        );
        // 非法 blob packlen（go-mysql: invalid blob packlen）
        let mut pos = 1usize;
        assert!(matches!(
            decode_value(b"xx", &mut pos, &ColCtx::new(tp::BLOB, 0, &sc("blob"))),
            Err(BinlogError::InvalidData(_))
        ));
        assert_eq!(pos, 1);
    }

    #[test]
    fn text_column_invalid_utf8_downgrades_to_bytes() {
        // TEXT 列存非法 UTF-8 → utf8_safe 降级 Bytes（D3）
        let mut buf = vec![0x02, 0x00];
        buf.extend_from_slice(&[0xFF, 0xFE]);
        assert_eq!(
            dv(&buf, &ColCtx::new(tp::BLOB, 2, &sc("text"))),
            ColumnValue::Bytes(vec![0xFF, 0xFE])
        );
        // varchar 非法 UTF-8 同样降级
        assert_eq!(
            dv(&[0x01, 0xFF], &ColCtx::new(tp::VARCHAR, 50, &sc("varchar"))),
            ColumnValue::Bytes(vec![0xFF])
        );
    }

    #[test]
    fn json_dispatch_to_json_variant() {
        // 真机 JSON {"k":1,"bb":"xy"}：meta=4，前缀 19 00 00 00(=25 LE) + JSONB
        // （25B payload 为 T8 fixture 原样，见 json.rs captured_objects_and_arrays）
        let mut buf = vec![0x19, 0x00, 0x00, 0x00];
        buf.extend_from_slice(&[
            0x00, 0x02, 0x00, 0x18, 0x00, 0x12, 0x00, 0x01, 0x00, 0x13, 0x00, 0x02, 0x00, 0x05,
            0x01, 0x00, 0x0C, 0x15, 0x00, 0x6B, 0x62, 0x62, 0x02, 0x78, 0x79,
        ]);
        let v = dv(&buf, &ColCtx::new(tp::JSON, 4, &sc("json")));
        // T8 按存储序渲染（键序 (长度,memcmp)：k 先于 bb）；go-mysql 裁判输出
        // 纯字典序 {"bb":"xy","k":1} —— T15 白名单①
        assert_eq!(v, ColumnValue::Json(r#"{"k":1,"bb":"xy"}"#.into()));
    }

    #[test]
    fn geometry_dispatch_to_bytes() {
        // 真机 POINT(1 2)：meta=4，25B payload（SRID 0 + WKB）→ Bytes 原样
        let mut buf = vec![0x19, 0x00, 0x00, 0x00];
        let payload: Vec<u8> = vec![
            0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0xF0, 0x3F, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40,
        ];
        buf.extend_from_slice(&payload);
        assert_eq!(
            dv(&buf, &ColCtx::new(tp::GEOMETRY, 4, &sc("geometry"))),
            ColumnValue::Bytes(payload)
        );
    }

    #[test]
    fn unknown_type_code_is_invalid_not_bytes_fallback() {
        // go-mysql default 分支 → error（宽度不可知，不允许静默错行）
        for t in [
            tp::DECIMAL,
            14u8,  /*NEWDATE*/
            249u8, /*TINY_BLOB*/
            200u8,
        ] {
            let mut pos = 7usize;
            let err = decode_value(&[0u8; 16], &mut pos, &ColCtx::new(t, 0, &sc("whatever")))
                .unwrap_err();
            assert!(
                matches!(err, BinlogError::InvalidData(_)),
                "tp {t} → {err:?}"
            );
            assert_eq!(pos, 7, "tp {t}: pos must stay untouched");
        }
    }

    #[test]
    fn too_short_leaves_pos_untouched() {
        let c = sc("varchar");
        let mut pos = 0usize;
        // 前缀声称 10 字节但只剩 3
        assert_eq!(
            decode_value(
                &[0x0A, b'a', b'b', b'c'],
                &mut pos,
                &ColCtx::new(tp::VARCHAR, 50, &c)
            )
            .unwrap_err(),
            BinlogError::TooShort
        );
        // blob 长度前缀本身截断
        assert_eq!(
            decode_value(&[0x01], &mut pos, &ColCtx::new(tp::BLOB, 2, &sc("blob"))).unwrap_err(),
            BinlogError::TooShort
        );
        // 定宽类型截断
        assert_eq!(
            decode_value(&[0x01; 3], &mut pos, &ColCtx::new(tp::LONGLONG, 0, &c)).unwrap_err(),
            BinlogError::TooShort
        );
        assert_eq!(pos, 0);
    }

    #[test]
    fn utf8_safe_direct() {
        let mut v = ColumnValue::Str("中文".as_bytes().to_vec());
        utf8_safe(&mut v);
        assert_eq!(v, ColumnValue::Str("中文".as_bytes().to_vec()));
        let mut bad = ColumnValue::Str(vec![0xC3, 0x28]);
        utf8_safe(&mut bad);
        assert_eq!(bad, ColumnValue::Bytes(vec![0xC3, 0x28]));
        let mut i = ColumnValue::Int(7);
        utf8_safe(&mut i);
        assert_eq!(i, ColumnValue::Int(7));
    }
}
