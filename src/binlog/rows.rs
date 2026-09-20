//! ROWS 事件（WRITE/UPDATE/DELETE_ROWS v0-v2）行解码。
//!
//! 权威对照 go-mysql `replication/row_event.go`（vendor）：
//! - 头部布局 `RowsEvent.Decode` :860-897 —— table_id 6B LE + flags 2B +
//!   [v2: extra_info_len u16 LE（**含自身 2B**，payload = len-2，整段按长度跳过）]
//!   + n_cols LNE + cols_present bitmap1（+ update 的 bitmap2），每段宽 bit_width(n_cols)；
//! - 行迭代 `Decode` :918-930 + `decodeRows` :939-986 —— 重复到事件末尾；
//!   **每个行镜像各自独立、字节对齐的 null-bits 区**，宽度
//!   `bit_width(present_cols)`（present_cols = 该镜像 cols bitmap 中 1 的个数，
//!   只数前 n_cols 位——真机 8.0.46 抓包证实 padding 位为 1，必须按 n_cols 掩蔽），
//!   位序号按 **present 列的序数** 推进（非 present 列不消耗 null 位，
//!   `decodeRows` :961-968 的 nullbitIndex 只在 isBitSet 分支 ++）。
//!
//! ## 简报口径勘正（authority 裁决，T6-T9 先例）
//!
//! - 简报/控制器备忘称「UPDATE 双镜像共用一个 null-bits 区（单游标跨
//!   before+after 推进、位不重置）」——与两处权威均不符：
//!   1. vendored `Decode` :918-929 对 before/after 各调一次 `decodeRows`，每次
//!      从数据区**新起字节对齐切片**（`nullBitmap := data[pos:pos+count]`）；
//!   2. 真机 docker mysql:8.0.46 抓包（fixture `tests/fixtures/capture_8.0_rows/`
//!      binlog.000002，9 列表全镜像 UPDATE）按「每镜像独立区」布局逐字节精确
//!      消费（70B 行数据 0 剩余），且双镜像 NULL 模式不同（before 0x48/after 0x94）；
//!   3. 上游 modern go-mysql（master）同样 per-image 独立区（`decodeImage`）。
//!
//!   照权威实现：行对布局 = [nb1][cols1][nb2][cols2]，测试逐字节钉死。
//! - 真机 8.0.46：`binlog_rows_query_log_events=ON` 时查询文本走**独立
//!   ROWS_QUERY 事件（type 29）**，rows 事件 extra-info 实测恒为 len=2（空段）；
//!   简报设想的「extra-info 内 default_metadata 段」在本矩阵版本（5.6/5.7/8.0
//!   实测 + 8.4 预期）未观测到。P1 裁定（D5 不猜测）：extra-info 整段按
//!   extra_info_len 跳过（与 go-mysql 一致，modern 仅解读 typecode 1=NDB/
//!   2=PARTITION）；typecode 首字节 ∉ {0(ROWS_QUERY),1,2} →
//!   [`BinlogError::PartialNotSupported`]（未知附加语义可能改变体布局，拒）。
//! - PARTIAL_UPDATE_ROWS_V2（事件码 **39**，binlog_row_value_options=PARTIAL_JSON）
//!   after 镜像前多「value_options LNE + partial-json bitmap」，且无法从 body
//!   内容判别（事件码在公共头里）——**T12 路由不得把 39 送进本函数**；
//!   若误送，最终消费长度断言会把它变成 TooShort/InvalidData 硬错误
//!   （真机 39 抓包 fixture 已做防脏测试）。vendored 裁判（const.go 无 39）
//!   对 39 同样不支持，my2sql-go 行为等价。
//!
//! ## schema ↔ table-map 配对（控制器裁定 3）
//!
//! - `schema.cols[i]` ↔ `tm.column_type[i]`；binlog 列多于 schema（DROP COLUMN
//!   历史行，P2 flashback 领域）→ 越界槽位用统一占位 [`dropped_col`]
//!   （name=`dropped_column`、type_name=`unknown_type` 对齐 my2sql-go
//!   `C_unknownColType`，base/context.go:27；per-index 名字由 T13 生成 SQL
//!   时按位补 `dropped_column_{i-cnt}`，裁判同口径
//!   sqlgen.go:19-34），**解码仍按 tm 的 type+meta 推进**（宽度自 table-map，
//!   unknown 类型自然由 decode_value 报 InvalidData，与上游一致）；
//! - binlog 列少于 schema（ALTER ADD COLUMN 之后的旧行，to-sql 方向）→
//!   本函数只迭代 tm.n_cols 列，缺失列**不出现**在 Row.cols 中，
//!   **T13 SQL 生成侧负责按 schema 全列宽补位**（接口接缝）。
//!
//! ## 其余口径
//!
//! - `body` 为**已剥 CRC** 的事件体（调用方用 `event::strip_checksum`，T2）；
//!   签名不带 `with_crc`——长度自洽校验在逐行消费中天然完成，参数无真实用途
//!   （控制器裁定 2 之「否则去掉并记录」分支，报告已备评审）。
//! - UPDATE 返回 `2n` 行交错 `[before1, after1, before2, after2, …]`（简报绑定）。
//! - cols_present 位为 0 的槽位 → [`ColumnValue::Missing`]（简报绑定；
//!   go-mysql 记 nil/NULL、my2sql 不感知——P1 采 Missing 保真「缺列≠NULL」，
//!   T13/T15 裁判比对时按接缝处理）。
//! - 位图后行区为空：go-mysql 静默返回 0 行；MySQL 从不发 0 行 rows 事件，
//!   本层按 T3「调用方须校验」纪律收紧为 [`BinlogError::TooShort`]。
//! - 时间列会话时区：本层 `tz_offset_secs` 恒 0（brief 签名无该参数；
//!   T14 若需 `--time-zone` 驱动，经扩展入口或参数注入，接缝记录在案）。

use super::error::BinlogError;
use super::int::ColumnValue;
use super::proto::{BitmapCursor, bit_width, read_lne};
use super::table_map::TableMapEvent;
use super::value::{ColCtx, decode_value};
use crate::metadata::schema::{SchemaCol, TableSchema};

/// binlog 列名/类型名占位（binlog 比当前 schema 宽的 dropped 列，裁定 3）。
/// `type_name="unknown_type"` 复刻 my2sql-go `C_unknownColType`
/// （context.go:27）：不含 "text" → BLOB 槽位按真 blob 走 `Bytes`，与其
/// 「unknown type → BytesColumn」裁判默认（sqlgen.go:50-58）一致。
const DROPPED_COL_NAME: &str = "dropped_column";
const DROPPED_COL_TYPE: &str = "unknown_type";

/// 构造 dropped 列占位（每次 `decode_rows` 建一份、多个越界槽位共用同一引用；
/// `SchemaCol` 含 `String` 字段无法做 `const`，构造开销可忽略）。
fn dropped_col() -> SchemaCol {
    SchemaCol {
        name: DROPPED_COL_NAME.to_string(),
        type_name: DROPPED_COL_TYPE.to_string(),
        unsigned: false,
    }
}

/// 行事件种类（简报绑定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowsKind {
    Write,
    Update,
    Delete,
}

/// 一行列值（长度 = tm.n_cols；UPDATE 时 before/after 各为一行，交错存放）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub cols: Vec<ColumnValue>,
}

/// 解码 ROWS 事件体（19B 公共头与 CRC 均已剥除）为行序列。
///
/// - `v2`：事件码 30/31/32 → true（多一段 2B 自含长度的 extra-info）；
///   23/24/25（V1）→ false。**V0（20/21/22，4B table_id）不支持**（D6），
///   路由层（T12）不得送入。
/// - `kind`：Update 时每个行对产出 before/after 两行（2n 交错）。
/// - 错误：任一截断 → [`BinlogError::TooShort`]；n_cols 与 table_map 不符、
///   table_id 不符、extra_info_len<2 → `InvalidData`；extra-info 未知
///   typecode → `PartialNotSupported`。行区迭代到事件体末尾（go-mysql
///   `decodeRows` 同口径），畸形尾部若自洽成「额外行」则按同规则继续解，
///   真实场景（如误路由的 39）表现为 TooShort/InvalidData 硬错误。
pub fn decode_rows(
    body: &[u8],
    tm: &TableMapEvent,
    schema: &TableSchema,
    kind: RowsKind,
    v2: bool,
) -> Result<Vec<Row>, BinlogError> {
    let mut pos = 0usize;
    // ---- table_id 6B LE + flags 2B ----
    if body.len() < 8 {
        return Err(BinlogError::TooShort);
    }
    let table_id = u64::from_le_bytes([body[0], body[1], body[2], body[3], body[4], body[5], 0, 0]);
    if table_id != tm.table_id {
        return Err(BinlogError::InvalidData(format!(
            "rows table_id {table_id} mismatches table_map {}",
            tm.table_id
        )));
    }
    pos += 8; // flags 2B：P1 不消费（STMT_END_F 等归 T12 事务机按需读取）
    // ---- v2 extra-info（extra_info_len 含自身 2B，整段按长度跳过）----
    if v2 {
        if body.len() < pos + 2 {
            return Err(BinlogError::TooShort);
        }
        let elen = u16::from_le_bytes([body[pos], body[pos + 1]]) as usize;
        if elen < 2 {
            return Err(BinlogError::InvalidData(
                "extra_info_len must include its own 2 bytes".into(),
            ));
        }
        let end = pos + elen;
        if body.len() < end {
            return Err(BinlogError::TooShort);
        }
        // P1 裁定（D5，见模块注释）：仅甄别首字节 typecode，整段跳过。
        let payload = &body[pos + 2..end];
        if let Some(&tc) = payload.first()
            && !matches!(tc, 0..=2)
        {
            return Err(BinlogError::PartialNotSupported);
        }
        pos = end;
    }
    // ---- n_cols（LNE）必须与 table_map 一致 ----
    let n_cols = read_lne(body, &mut pos)? as usize;
    if n_cols != tm.n_cols {
        return Err(BinlogError::InvalidData(format!(
            "rows n_cols {n_cols} mismatches table_map {}",
            tm.n_cols
        )));
    }
    let bm_bytes = bit_width(n_cols);
    let take_bitmap = |body: &[u8], p: &mut usize| -> Result<Vec<u8>, BinlogError> {
        let s = body.get(*p..*p + bm_bytes).ok_or(BinlogError::TooShort)?;
        *p += bm_bytes;
        Ok(s.to_vec())
    };
    let bm1 = take_bitmap(body, &mut pos)?;
    let bm2 = if kind == RowsKind::Update {
        Some(take_bitmap(body, &mut pos)?)
    } else {
        None
    };
    // ---- 行区：迭代到事件体末尾（对照 go-mysql decodeRows :939-986）。
    // 空行区非法：MySQL 从不发 0 行的 rows 事件（go-mysql 静默返回 0 行，
    // 本层按 T3「调用方须校验」纪律收紧为硬错误）。----
    if pos >= body.len() {
        return Err(BinlogError::TooShort);
    }
    let dropped = dropped_col();
    let mut rows = Vec::new();
    while pos < body.len() {
        rows.push(decode_image(body, &mut pos, &bm1, tm, schema, &dropped)?);
        if let Some(bm) = &bm2 {
            rows.push(decode_image(body, &mut pos, bm, tm, schema, &dropped)?);
        }
    }
    Ok(rows)
}

/// 单个行镜像：`[null 区 (bit_width(present))][逐 present 列值]`。
/// null 位按 **present 序数** 推进；present 位图取前 n_cols 位（padding 位
/// 真机恒为 1，必须掩蔽——见模块注释）。
fn decode_image(
    body: &[u8],
    pos: &mut usize,
    present_bits: &[u8],
    tm: &TableMapEvent,
    schema: &TableSchema,
    dropped: &SchemaCol,
) -> Result<Row, BinlogError> {
    let n = tm.n_cols;
    let present = (0..n).filter(|&i| bit_at(present_bits, i)).count();
    // 活锁防护（T12 Step-0）：present==0（位图全 0 或 n_cols==0）时 null 区宽
    // bit_width(0)=0，行镜像消耗 0 字节，外层 while 永不推进 → 敌意输入挂死。
    // MySQL 从不发 0 列镜像的行，硬错误处理（D5）。
    if present == 0 {
        return Err(BinlogError::InvalidData(
            "rows image has zero present columns (livelock guard)".into(),
        ));
    }
    let nb_bytes = bit_width(present);
    let null_bits = body
        .get(*pos..*pos + nb_bytes)
        .ok_or(BinlogError::TooShort)?;
    *pos += nb_bytes;
    let mut cur = BitmapCursor::new(null_bits, nb_bytes);
    let mut cols = Vec::with_capacity(n);
    for i in 0..n {
        if !bit_at(present_bits, i) {
            // 列裁剪（MINIMAL/partial 镜像）：缺列 ≠ NULL（简报绑定）
            cols.push(ColumnValue::Missing);
            continue;
        }
        if cur.next_is_null() {
            cols.push(ColumnValue::Null);
            continue;
        }
        let sc = schema.cols.get(i).unwrap_or(dropped);
        // TableMapEvent 是 pub struct，字段间不互相约束（n_cols 可比数组长），
        // 直接索引会把敌意构造变成 panic → 越界一律 TooShort（T12 Step-0）。
        let tp = *tm.column_type.get(i).ok_or(BinlogError::TooShort)?;
        let meta = *tm.column_meta.get(i).ok_or(BinlogError::TooShort)?;
        cols.push(decode_value(
            body,
            pos,
            &ColCtx {
                tp,
                meta,
                schema: sc,
                tz_offset_secs: 0, // 本层无时区语义；T14 扩展接缝（见模块注释）
            },
        )?);
    }
    Ok(Row { cols })
}

/// 位图第 `i` 位（LSB first，对照 go-mysql `isBitSet` row_event.go:899-902）。
fn bit_at(bits: &[u8], i: usize) -> bool {
    bits[i / 8] >> (i % 8) & 1 == 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binlog::event::{EVENT_HEADER_SIZE, crc32_ok, parse_header, strip_checksum};
    use crate::binlog::table_map::parse_table_map;

    // ---------- 手搭 fixture 辅助 ----------

    fn iv(v: i32) -> ColumnValue {
        ColumnValue::Int(v as i64)
    }
    fn uv(v: u64) -> ColumnValue {
        ColumnValue::UInt(v)
    }
    fn sv(t: &str) -> ColumnValue {
        ColumnValue::Str(t.as_bytes().to_vec())
    }
    fn bv(b: &[u8]) -> ColumnValue {
        ColumnValue::Bytes(b.to_vec())
    }
    fn jv(t: &str) -> ColumnValue {
        ColumnValue::Json(t.to_string())
    }
    /// 9 列 int/varchar 混合表（真机 t10.u 同构：a INT, b VARCHAR(8), c..i2 INT）。
    const U9_TYPES: [u8; 9] = [3, 15, 3, 3, 3, 3, 3, 3, 3];
    const U9_META: [u16; 9] = [0, 32, 0, 0, 0, 0, 0, 0, 0];

    fn tm_u9(table_id: u64) -> TableMapEvent {
        TableMapEvent {
            table_id,
            schema: "t10".into(),
            table: "u".into(),
            n_cols: 9,
            column_type: U9_TYPES.to_vec(),
            column_meta: U9_META.to_vec(),
            null_bits: vec![0, 0],
            charset: vec![],
        }
    }
    fn scol(name: &str, ty: &str) -> SchemaCol {
        SchemaCol {
            name: name.into(),
            type_name: ty.into(),
            unsigned: false,
        }
    }
    fn schema_u9() -> TableSchema {
        let names = ["a", "b", "c", "d", "e", "f", "g", "h", "i2"];
        TableSchema {
            db: "t10".into(),
            table: "u".into(),
            cols: names
                .iter()
                .map(|n| scol(n, if *n == "b" { "varchar" } else { "int" }))
                .collect(),
            pk: vec![],
            uks: vec![],
        }
    }
    /// schema 截短到 `k` 列（构造 binlog 比 schema 宽的 dropped 场景）。
    fn schema_u9_trunc(k: usize) -> TableSchema {
        let mut s = schema_u9();
        s.cols.truncate(k);
        s
    }
    /// schema 加长到 12 列（构造 schema 比 binlog 宽的 ALTER ADD 场景）。
    fn schema_u9_wider() -> TableSchema {
        let mut s = schema_u9();
        for i in 0..3 {
            s.cols.push(scol(&format!("new{i}"), "int"));
        }
        s
    }

    fn ints(vals: &[i32]) -> Vec<u8> {
        let mut b = Vec::new();
        for v in vals {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b
    }
    fn var(t: &str) -> Vec<u8> {
        let mut b = vec![t.len() as u8];
        b.extend_from_slice(t.as_bytes());
        b
    }
    /// v2 事件体：tid + flags2 + extra_info_len(含自身) + extra + n_cols + bitmaps + rows。
    fn body_v2(tid: u64, extra: &[u8], n_cols: u8, bms: &[&[u8]], rows: &[&[u8]]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&tid.to_le_bytes()[..6]);
        b.extend_from_slice(&[0u8; 2]); // flags
        let elen = (2 + extra.len()) as u16;
        b.extend_from_slice(&elen.to_le_bytes());
        b.extend_from_slice(extra);
        b.push(n_cols);
        for x in bms {
            b.extend_from_slice(x);
        }
        for r in rows {
            b.extend_from_slice(r);
        }
        b
    }
    /// v1（无 extra-info 段）事件体。
    fn body_v1(tid: u64, n_cols: u8, bms: &[&[u8]], rows: &[&[u8]]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&tid.to_le_bytes()[..6]);
        b.extend_from_slice(&[0u8; 2]);
        b.push(n_cols);
        for x in bms {
            b.extend_from_slice(x);
        }
        for r in rows {
            b.extend_from_slice(r);
        }
        b
    }

    // ---------- 手搭：Write 多行 + null 位跨字节 ----------

    #[test]
    fn write_two_rows_null_bits_across_bytes() {
        let tm = tm_u9(7);
        // 行1：present 全 9（padding 位 1 亦真机形态），null 位 3、6 → 跨第 0/1 字节
        let mut r1 = vec![0x48, 0x00];
        r1.extend_from_slice(&ints(&[5]));
        r1.extend_from_slice(&var("hey"));
        r1.extend_from_slice(&ints(&[7, 8, 9, 10, 11]));
        // 行2：null 位 0、7（位 7 在 byte0 高位、位 8 起进入 byte1）
        let mut r2 = vec![0x81, 0x00];
        r2.extend_from_slice(&var("ok"));
        r2.extend_from_slice(&ints(&[1, 2, 3, 4, 5, 6]));
        let body = body_v2(7, &[], 9, &[&[0xff, 0xff]], &[&r1, &r2]);
        let rows = decode_rows(&body, &tm, &schema_u9(), RowsKind::Write, true).unwrap();
        assert_eq!(
            rows,
            vec![
                Row {
                    cols: vec![
                        iv(5),
                        sv("hey"),
                        iv(7),
                        ColumnValue::Null,
                        iv(8),
                        iv(9),
                        ColumnValue::Null,
                        iv(10),
                        iv(11),
                    ],
                },
                Row {
                    cols: vec![
                        ColumnValue::Null,
                        sv("ok"),
                        iv(1),
                        iv(2),
                        iv(3),
                        iv(4),
                        iv(5),
                        ColumnValue::Null,
                        iv(6),
                    ],
                },
            ],
        );
    }

    // ---------- 手搭：Update 每镜像独立 null 区 ----------

    #[test]
    fn update_rows_have_per_image_null_regions() {
        let tm = tm_u9(7);
        let mut pair = vec![0x48, 0x00]; // before：NULL@3,6
        pair.extend_from_slice(&ints(&[1]));
        pair.extend_from_slice(&var("hi"));
        pair.extend_from_slice(&ints(&[10, 30, 40, 60, 70]));
        pair.extend_from_slice(&[0x94, 0x00]); // after：NULL@2,4,7（若共用游标必错位）
        pair.extend_from_slice(&ints(&[2]));
        pair.extend_from_slice(&var("yo"));
        pair.extend_from_slice(&ints(&[11, 0, 99]));
        pair.extend_from_slice(&(-5i32).to_le_bytes());
        let body = body_v2(7, &[], 9, &[&[0xff, 0xff], &[0xff, 0xff]], &[&pair]);
        let rows = decode_rows(&body, &tm, &schema_u9(), RowsKind::Update, true).unwrap();
        assert_eq!(
            rows,
            vec![
                Row {
                    cols: vec![
                        iv(1),
                        sv("hi"),
                        iv(10),
                        ColumnValue::Null,
                        iv(30),
                        iv(40),
                        ColumnValue::Null,
                        iv(60),
                        iv(70),
                    ],
                },
                Row {
                    cols: vec![
                        iv(2),
                        sv("yo"),
                        ColumnValue::Null,
                        iv(11),
                        ColumnValue::Null,
                        iv(0),
                        iv(99),
                        ColumnValue::Null,
                        iv(-5),
                    ],
                },
            ],
        );
    }

    // ---------- 手搭：present 位图空洞 → Missing ----------

    #[test]
    fn minimal_image_update_yields_missing_and_1b_null_region() {
        let tm = tm_u9(7);
        // before 全镜像（bm1=01ff）
        let mut img = vec![0x94, 0x00];
        img.extend_from_slice(&ints(&[2]));
        img.extend_from_slice(&var("xyz"));
        img.extend_from_slice(&ints(&[11, 0, 99]));
        img.extend_from_slice(&(-5i32).to_le_bytes());
        // after 仅 col5 present（bm2=2000）→ null 区宽 bit_width(1)=1 字节
        let mut after = vec![0x00];
        after.extend_from_slice(&ints(&[123]));
        img.extend_from_slice(&after);
        let body = body_v2(7, &[], 9, &[&[0xff, 0xff], &[0x20, 0x00]], &[&img]);
        let rows = decode_rows(&body, &tm, &schema_u9(), RowsKind::Update, true).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[1].cols,
            vec![
                ColumnValue::Missing,
                ColumnValue::Missing,
                ColumnValue::Missing,
                ColumnValue::Missing,
                ColumnValue::Missing,
                iv(123),
                ColumnValue::Missing,
                ColumnValue::Missing,
                ColumnValue::Missing,
            ],
        );
    }

    #[test]
    fn present_holes_map_null_bits_by_present_ordinal() {
        // bm = cols{0,1,2,4,8} present → present=5，null 区 1 字节；
        // nb bit1=1 → 第 2 个 present 列（col1）NULL——位按序数推进而非列号。
        let tm = tm_u9(7);
        let mut r = vec![0x02];
        r.extend_from_slice(&ints(&[7])); // col0
        // col1 NULL（varchar 无字节）
        r.extend_from_slice(&ints(&[9, 11, 13])); // col2, col4, col8
        let body = body_v2(7, &[], 9, &[&[0x17, 0x01]], &[&r]);
        let rows = decode_rows(&body, &tm, &schema_u9(), RowsKind::Write, true).unwrap();
        assert_eq!(
            rows[0].cols,
            vec![
                iv(7),
                ColumnValue::Null,
                iv(9),
                ColumnValue::Missing,
                iv(11),
                ColumnValue::Missing,
                ColumnValue::Missing,
                ColumnValue::Missing,
                iv(13),
            ],
        );
    }

    // ---------- 手搭：截断纪律 ----------

    #[test]
    fn truncated_bodies_are_too_short() {
        let tm = tm_u9(7);
        let sc = schema_u9();
        assert_eq!(
            decode_rows(&[], &tm, &sc, RowsKind::Write, true).unwrap_err(),
            BinlogError::TooShort
        );
        // 头部后无位图
        let b = body_v2(7, &[], 9, &[], &[]);
        assert_eq!(
            decode_rows(&b, &tm, &sc, RowsKind::Write, true).unwrap_err(),
            BinlogError::TooShort
        );
        // 行区完全缺失
        let b = body_v2(7, &[], 9, &[&[0xff, 0xff]], &[]);
        assert_eq!(
            decode_rows(&b, &tm, &sc, RowsKind::Write, true).unwrap_err(),
            BinlogError::TooShort
        );
        // null 区只剩 1 字节（需 bit_width(9)=2）
        let b = body_v2(7, &[], 9, &[&[0xff, 0xff]], &[&[0x00]]);
        assert_eq!(
            decode_rows(&b, &tm, &sc, RowsKind::Write, true).unwrap_err(),
            BinlogError::TooShort
        );
        // 第 1 列 int 截断（2/4 字节）
        let b = body_v2(7, &[], 9, &[&[0xff, 0xff]], &[&[0x00, 0x00, 1, 2]]);
        assert_eq!(
            decode_rows(&b, &tm, &sc, RowsKind::Write, true).unwrap_err(),
            BinlogError::TooShort
        );
        // varchar 前缀声称 5 字节但只剩 2
        let mut r = vec![0x00, 0x00];
        r.extend_from_slice(&ints(&[1]));
        r.extend_from_slice(&[5, b'a', b'b']);
        let b = body_v2(7, &[], 9, &[&[0xff, 0xff]], &[&r]);
        assert_eq!(
            decode_rows(&b, &tm, &sc, RowsKind::Write, true).unwrap_err(),
            BinlogError::TooShort
        );
        // UPDATE：第二个位图缺失
        let b = body_v2(7, &[], 9, &[&[0xff, 0xff]], &[]);
        assert_eq!(
            decode_rows(&b, &tm, &sc, RowsKind::Update, true).unwrap_err(),
            BinlogError::TooShort
        );
    }

    // ---------- 手搭：头部合法性 ----------

    #[test]
    fn header_field_mismatches_are_invalid() {
        let tm = tm_u9(7);
        let sc = schema_u9();
        let good_r = {
            let mut r = vec![0x00, 0x00];
            r.extend_from_slice(&ints(&[1, 2, 3, 4, 5, 6, 7, 8, 9]));
            r
        };
        // n_cols ≠ table_map.n_cols
        let b = body_v2(7, &[], 10, &[&[0xff, 0xff]], &[&good_r]);
        assert!(matches!(
            decode_rows(&b, &tm, &sc, RowsKind::Write, true),
            Err(BinlogError::InvalidData(_))
        ));
        // table_id 与 tm 不符
        let b = body_v2(8, &[], 9, &[&[0xff, 0xff]], &[&good_r]);
        assert!(matches!(
            decode_rows(&b, &tm, &sc, RowsKind::Write, true),
            Err(BinlogError::InvalidData(_))
        ));
        // extra_info_len < 2（不含自身长度非法）
        let mut b = body_v2(7, &[], 9, &[&[0xff, 0xff]], &[&good_r]);
        b[8] = 1;
        assert!(matches!(
            decode_rows(&b, &tm, &sc, RowsKind::Write, true),
            Err(BinlogError::InvalidData(_))
        ));
    }

    // ---------- 手搭：extra-info 跳过 / PartialNotSupported ----------

    fn one_full_row() -> Vec<u8> {
        let mut r = vec![0x00, 0x00]; // 全 9 列 present → null 区 bit_width(9)=2B，模式 0
        r.extend_from_slice(&ints(&[1]));
        r.extend_from_slice(&var("ok")); // col1 = varchar 槽
        r.extend_from_slice(&ints(&[3, 4, 5, 6, 7, 8, 9]));
        r
    }
    fn expect_full_nine(rows: &[Row]) {
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].cols,
            vec![
                iv(1),
                sv("ok"),
                iv(3),
                iv(4),
                iv(5),
                iv(6),
                iv(7),
                iv(8),
                iv(9),
            ],
        );
    }

    #[test]
    fn extra_info_known_typecodes_are_skipped() {
        let tm = tm_u9(7);
        let sc = schema_u9();
        // 空段（8.0.46 实测恒此形态）
        let b = body_v2(7, &[], 9, &[&[0xff, 0xff]], &[&one_full_row()]);
        let rows = decode_rows(&b, &tm, &sc, RowsKind::Write, true).unwrap();
        expect_full_nine(&rows);
        // typecode 0 = ROWS_QUERY（5.6/5.7 时代的段内形态，合成）
        let b = body_v2(
            7,
            &[0x00, b'S', b'E', b'L'],
            9,
            &[&[0xff, 0xff]],
            &[&one_full_row()],
        );
        let rows = decode_rows(&b, &tm, &sc, RowsKind::Write, true).unwrap();
        expect_full_nine(&rows);
        // typecode 1 = NDB / 2 = PARTITION：跳过
        let b = body_v2(
            7,
            &[0x02, 0x01, 0x00],
            9,
            &[&[0xff, 0xff]],
            &[&one_full_row()],
        );
        assert!(decode_rows(&b, &tm, &sc, RowsKind::Write, true).is_ok());
    }

    #[test]
    fn extra_info_unknown_typecode_is_partial_not_supported() {
        let tm = tm_u9(7);
        let b = body_v2(
            7,
            &[0x03, 0x01, 0x00],
            9,
            &[&[0xff, 0xff]],
            &[&one_full_row()],
        );
        assert_eq!(
            decode_rows(&b, &tm, &schema_u9(), RowsKind::Write, true).unwrap_err(),
            BinlogError::PartialNotSupported,
        );
    }

    #[test]
    fn v1_body_without_extra_info_decodes() {
        let tm = tm_u9(7);
        let b = body_v1(7, 9, &[&[0xff, 0xff]], &[&one_full_row()]);
        let rows = decode_rows(&b, &tm, &schema_u9(), RowsKind::Write, false).unwrap();
        expect_full_nine(&rows);
    }

    // ---------- 裁定 3 接缝：dropped / 更宽 schema ----------

    #[test]
    fn dropped_columns_beyond_schema_still_decode() {
        // schema 只剩前 3 列（binlog 更宽：DROP COLUMN 历史行）→
        // 越界槽位用占位、解码按 tm 的 type+meta 照常推进。
        let tm = tm_u9(7);
        let b = body_v2(7, &[], 9, &[&[0xff, 0xff]], &[&one_full_row()]);
        let rows = decode_rows(&b, &tm, &schema_u9_trunc(3), RowsKind::Write, true).unwrap();
        expect_full_nine(&rows);
    }

    #[test]
    fn wider_schema_only_iterates_tm_columns() {
        // schema 12 列 > tm 9 列（ADD COLUMN 后的旧行）→ Row.cols 仅 9，
        // 补齐是 T13 SQL 生成侧的接缝责任。
        let tm = tm_u9(7);
        let b = body_v2(7, &[], 9, &[&[0xff, 0xff]], &[&one_full_row()]);
        let rows = decode_rows(&b, &tm, &schema_u9_wider(), RowsKind::Write, true).unwrap();
        expect_full_nine(&rows);
    }

    /// T12 Step-0（敌意输入）：cols-present 位图全 0（或 tm.n_cols==0）时
    /// present=0 → null 区宽 bit_width(0)=0 → 行镜像消耗 0 字节 → while 循环
    /// 永不推进（活锁）。必须报错而非空转。
    #[test]
    fn all_zero_present_bitmap_errors_instead_of_hanging() {
        let tm = tm_u9(7);
        // 位图 2 字节全 0（无任何 present 列），行区塞 1 字节垃圾保证进入循环
        let b = body_v2(7, &[], 9, &[&[0x00, 0x00]], &[&[0xAB]]);
        assert!(matches!(
            decode_rows(&b, &tm, &schema_u9(), RowsKind::Write, true),
            Err(BinlogError::InvalidData(_))
        ));
    }

    /// T12 Step-0：pub struct `TableMapEvent` 字段不互相约束（敌意构造），
    /// column_type/column_meta 短于 n_cols 时不得 panic（越界索引），
    /// 必须报 TooShort。
    #[test]
    fn short_tm_arrays_error_not_panic() {
        let mut tm = tm_u9(7);
        tm.column_type.truncate(1); // n_cols 仍为 9，数组只剩 1 项
        let mut r = vec![0x00, 0x00];
        r.extend_from_slice(&ints(&[1, 2, 3, 4, 5, 6, 7, 8, 9]));
        let b = body_v2(7, &[], 9, &[&[0xff, 0xff]], &[&r]);
        assert_eq!(
            decode_rows(&b, &tm, &schema_u9(), RowsKind::Write, true).unwrap_err(),
            BinlogError::TooShort
        );
        let mut tm2 = tm_u9(7);
        tm2.column_meta.truncate(1);
        let b2 = body_v2(7, &[], 9, &[&[0xff, 0xff]], &[&r]);
        assert_eq!(
            decode_rows(&b2, &tm2, &schema_u9(), RowsKind::Write, true).unwrap_err(),
            BinlogError::TooShort
        );
    }

    // ---------- 真机 fixture 端到端 ----------

    /// 逐事件走读 8.0 抓包（CRC 恒开）：返回 (事件类型, 已剥 CRC 的 body)。
    fn walk_events(path: &str) -> Vec<(u8, Vec<u8>)> {
        let data = std::fs::read(path).expect("fixture must be committed");
        assert_eq!(&data[..4], b"\xfebin");
        let mut pos = 4usize;
        let mut out = Vec::new();
        while pos < data.len() {
            let h = parse_header(&data[pos..]).unwrap();
            let size = h.event_size as usize;
            assert!(pos + size <= data.len(), "event overruns file");
            let full = &data[pos..pos + size];
            if h.event_type.0 != 15 {
                // 8.0 默认 binlog_checksum=CRC32；FDE 校验区另有特例（T2 注）
                assert!(crc32_ok(full), "event type {} crc", h.event_type.0);
            }
            let mut body = full[EVENT_HEADER_SIZE..].to_vec();
            strip_checksum(&mut body, true);
            out.push((h.event_type.0, body));
            pos += size;
        }
        assert_eq!(pos, data.len());
        out
    }

    fn fixture(path: &str) -> String {
        format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), path)
    }

    /// 从事件流提取 TABLE_MAP（按出现顺序覆盖式建表）。
    fn table_map_of(events: &[(u8, Vec<u8>)], tid: u64) -> TableMapEvent {
        let mut last = None;
        for (t, body) in events {
            if *t == 19 {
                let tm = parse_table_map(body, false).unwrap();
                if tm.table_id == tid {
                    last = Some(tm);
                }
            }
        }
        last.expect("table map present")
    }

    #[test]
    fn fixture_rows_000002_real_write_and_update() {
        let events = walk_events(&fixture("capture_8.0_rows/binlog.000002"));
        let types: Vec<u8> = events.iter().map(|(t, _)| *t).collect();
        assert_eq!(types.iter().filter(|t| **t == 31).count(), 1);
        let n30 = events.iter().filter(|(t, _)| *t == 30).count();
        assert!(n30 >= 1, "capture has at least one WRITE_ROWS_V2");
        let tm = table_map_of(&events, 85);
        assert_eq!(tm.column_type, U9_TYPES.to_vec());
        assert_eq!(tm.column_meta, U9_META.to_vec());
        let sc = schema_u9();
        for (t, body) in &events {
            match *t {
                30 => {
                    let rows = decode_rows(body, &tm, &sc, RowsKind::Write, true).unwrap();
                    assert_eq!(
                        rows[0].cols,
                        vec![
                            iv(1),
                            sv("ab"),
                            iv(10),
                            ColumnValue::Null,
                            iv(30),
                            iv(40),
                            ColumnValue::Null,
                            iv(60),
                            iv(70),
                        ],
                    );
                }
                31 => {
                    let rows = decode_rows(body, &tm, &sc, RowsKind::Update, true).unwrap();
                    assert_eq!(rows.len(), 2);
                    assert_eq!(rows[0].cols, rows_before_u());
                    assert_eq!(rows[1].cols, rows_after_u());
                }
                _ => {}
            }
        }
    }
    fn rows_before_u() -> Vec<ColumnValue> {
        vec![
            iv(1),
            sv("ab"),
            iv(10),
            ColumnValue::Null,
            iv(30),
            iv(40),
            ColumnValue::Null,
            iv(60),
            iv(70),
        ]
    }
    fn rows_after_u() -> Vec<ColumnValue> {
        vec![
            iv(2),
            sv("xyz"),
            ColumnValue::Null,
            iv(11),
            ColumnValue::Null,
            iv(0),
            iv(99),
            ColumnValue::Null,
            iv(-5),
        ]
    }

    #[test]
    fn fixture_rows_000003_real_minimal_and_full_update() {
        let events = walk_events(&fixture("capture_8.0_rows/binlog.000003"));
        let updates: Vec<&Vec<u8>> = events
            .iter()
            .filter(|(t, _)| *t == 31)
            .map(|(_, b)| b)
            .collect();
        assert_eq!(updates.len(), 2, "1 个 MINIMAL + 1 个 FULL 镜像 UPDATE");
        let tm = table_map_of(&events, 85);
        let sc = schema_u9();
        // 事件 1（session binlog_row_image=MINIMAL）：after 仅 col5 present，
        // 其 null 区 1 字节——共用游标/固定 2 字节布局都会在此错位报错。
        let rows = decode_rows(updates[0], &tm, &sc, RowsKind::Update, true).unwrap();
        assert_eq!(rows[1].cols.len(), 9);
        assert_eq!(rows[0].cols, rows_after_u()); // 承接 000002 的 after 态
        assert_eq!(
            rows[1].cols,
            vec![
                ColumnValue::Missing,
                ColumnValue::Missing,
                ColumnValue::Missing,
                ColumnValue::Missing,
                ColumnValue::Missing,
                iv(123),
                ColumnValue::Missing,
                ColumnValue::Missing,
                ColumnValue::Missing,
            ],
        );
        // 事件 2（FULL）：before 含 f=123，after c=55。
        let rows = decode_rows(updates[1], &tm, &sc, RowsKind::Update, true).unwrap();
        assert_eq!(
            rows[0].cols,
            vec![
                iv(2),
                sv("xyz"),
                ColumnValue::Null,
                iv(11),
                ColumnValue::Null,
                iv(123),
                iv(99),
                ColumnValue::Null,
                iv(-5),
            ],
        );
        assert_eq!(
            rows[1].cols,
            vec![
                iv(2),
                sv("xyz"),
                iv(55),
                iv(11),
                ColumnValue::Null,
                iv(123),
                iv(99),
                ColumnValue::Null,
                iv(-5),
            ],
        );
    }

    fn schema_j() -> TableSchema {
        TableSchema {
            db: "t10".into(),
            table: "j".into(),
            cols: vec![
                scol("id", "int"),
                scol("doc", "json"),
                scol("tag", "varchar"),
            ],
            pk: vec!["id".into()],
            uks: vec![],
        }
    }

    #[test]
    fn fixture_rows_000004_real_json_write_delete() {
        let events = walk_events(&fixture("capture_8.0_rows/binlog.000004"));
        let tm = table_map_of(&events, 87);
        assert_eq!(tm.column_type, vec![3, 245, 15]);
        assert_eq!(tm.column_meta, vec![0, 4, 16]);
        let sc = schema_j();
        let mut seen = [0usize; 3]; // write / partial39-err / delete
        for (t, body) in &events {
            match *t {
                30 => {
                    let rows = decode_rows(body, &tm, &sc, RowsKind::Write, true).unwrap();
                    assert_eq!(
                        rows[0].cols,
                        vec![iv(1), jv(r#"{"a":1,"b":[1,2]}"#), ColumnValue::Null],
                    );
                    seen[0] += 1;
                }
                39 => {
                    // PARTIAL_UPDATE_ROWS_V2：T12 路由禁止送入本函数；若误送
                    // （按 Update 全镜像解），必须硬错误而非假性成功（D5）。
                    let r = decode_rows(body, &tm, &sc, RowsKind::Update, true);
                    assert!(
                        matches!(
                            r,
                            Err(BinlogError::TooShort) | Err(BinlogError::InvalidData(_))
                        ),
                        "event-39 body must not decode as full-image rows, got {r:?}",
                    );
                    seen[1] += 1;
                }
                32 => {
                    let rows = decode_rows(body, &tm, &sc, RowsKind::Delete, true).unwrap();
                    assert_eq!(
                        rows[0].cols,
                        vec![iv(1), jv(r#"{"a":9,"b":[1,2]}"#), sv("tt")],
                    );
                    seen[2] += 1;
                }
                _ => {}
            }
        }
        assert_eq!(seen, [1, 2, 1], "抓包应含 1×30、2×39、1×32");
    }

    /// P1 首个「真实 8.0 binlog 全链路行解码」测试：capture_8.0_minimal 的
    /// 26 列 probe 表单行 INSERT（task-9 报告逐列字节账 2186B），
    /// 断言值与 T5-T9 各模块真机 fixture 结论逐列一致。
    #[test]
    fn fixture_8_0_minimal_26col_row_end_to_end() {
        let events = walk_events(&fixture("capture_8.0_minimal/mysql-bin.000003"));
        let tms: Vec<TableMapEvent> = events
            .iter()
            .filter(|(t, _)| *t == 19)
            .map(|(_, b)| parse_table_map(b, false).unwrap())
            .collect();
        assert_eq!(tms.len(), 1);
        let tm = &tms[0];
        assert_eq!(tm.n_cols, 26);
        let type_names = [
            "char",
            "char",
            "varchar",
            "varchar",
            "enum",
            "set",
            "decimal",
            "timestamp",
            "timestamp",
            "datetime",
            "time",
            "year",
            "float",
            "double",
            "bit",
            "tinytext",
            "text",
            "mediumtext",
            "longtext",
            "tinyblob",
            "blob",
            "mediumblob",
            "longblob",
            "json",
            "geometry",
            "varchar",
        ];
        let sc = TableSchema {
            db: tm.schema.clone(),
            table: tm.table.clone(),
            cols: type_names
                .iter()
                .enumerate()
                .map(|(i, t)| scol(&format!("c{i}"), t))
                .collect(),
            pk: vec![],
            uks: vec![],
        };
        let writes: Vec<&Vec<u8>> = events
            .iter()
            .filter(|(t, _)| *t == 30)
            .map(|(_, b)| b)
            .collect();
        assert_eq!(writes.len(), 1);
        let rows = decode_rows(writes[0], tm, &sc, RowsKind::Write, true).unwrap();
        assert_eq!(rows.len(), 1);
        let gy: Vec<u8> = vec![
            0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0xF0, 0x3F, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40,
        ];
        let want = vec![
            sv("abc"),
            sv("ab"),
            sv("vs"),
            sv(&"L".repeat(2000)),
            uv(2),
            uv(5),
            ColumnValue::Decimal("-12.34".into()),
            sv("2020-07-16 10:44:09"),
            sv("2020-07-16 10:44:09.120"),
            sv("2020-07-16 10:44:09"),
            sv("10:44:09"),
            uv(2026),
            ColumnValue::Double("1.5".into()),
            ColumnValue::Double("3.25".into()),
            uv(384),
            sv("tinytext!"),
            sv("sample text"),
            sv("medium"),
            sv("long"),
            bv(&[0x00, 0xFF]),
            bv(&[0x01, 0x02, 0x03]),
            bv(&[0x04, 0x05]),
            bv(&[0x06, 0x07]),
            jv(r#"{"k":1,"bb":"xy"}"#),
            bv(&gy),
            ColumnValue::Null, // col25 nul='nul' 值本身为 NULL
        ];
        assert_eq!(rows[0].cols, want);
    }
}
