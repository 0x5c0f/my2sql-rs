//! 靶 1（P4a Lane A / spec §1）：单事件字节流 fuzz。
//!
//! 输入格式：`data[0] & 1` = with_crc 选择器，剥掉首字节后余下当**单事件**流：
//! `parse_header` → 尺寸自洽闸（`size < EVENT_HEADER_SIZE || size > buf.len()`
//! → return）→ `strip_checksum` → 按 `h.event_type.0` 路由：
//!   - 19（TABLE_MAP）→ `parse_table_map`；
//!   - 20/21/22/23/24/25/30/31/32（rows 家族）→ `decode_rows`，配**固定合成
//!     tm**（先 `parse_table_map(合法 tm 字节)` 得 `TableMapEvent`，schema
//!     固定 2×INT）；
//!   - 其余类型仅要求 header 解析不 panic。
//!
//! 纪律（简报 Step 3 陷阱钉死）：
//! - **不 catch_unwind**——panic = crash 信号，正是本靶目的；
//! - schema 查找路径**禁止 panic**：`tests/fuzz_seed.rs` 的 `schema_for` 对
//!   未知表 `panic!`，那是 fuzz 语境的假阳性 crash 源，本靶用固定合成表，
//!   绝不复制那张表。

#![no_main]

use libfuzzer_sys::fuzz_target;
use my2sql_rs::binlog::event::{EVENT_HEADER_SIZE, parse_header, strip_checksum};
use my2sql_rs::binlog::rows::{RowsKind, decode_rows};
use my2sql_rs::binlog::table_map::parse_table_map;
use my2sql_rs::metadata::schema::{SchemaCol, TableSchema};

/// 固定合成 TABLE_MAP body：与 `tests/fuzz_seed.rs` 种子 2 的
/// `tm_body(7, "fz", "t_zero_bm", &[3,3], 0, &[], &[0])` 逐字节同构
/// （tid=7、flags=0、db="fz"、table="t_zero_bm"、2×TYPE_INT24/INT(0x03)、
/// metadata 总长 0、null_bits=[0]）。
fn synth_tm_body() -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&7u64.to_le_bytes()[..6]);
    b.extend_from_slice(&0u16.to_le_bytes()); // flags
    b.push(2);
    b.extend_from_slice(b"fz");
    b.push(0);
    b.push(9);
    b.extend_from_slice(b"t_zero_bm");
    b.push(0);
    b.push(2); // n_cols
    b.extend_from_slice(&[0x03, 0x03]); // 2×INT
    b.push(0); // metadata 总长
    b.push(0x00); // null_bits
    b
}

/// 固定合成 schema（2×INT），配合 [`synth_tm_body`] 的 2 列表。
fn synth_schema() -> TableSchema {
    TableSchema {
        db: "fz".into(),
        table: "t_zero_bm".into(),
        cols: vec![
            SchemaCol {
                name: "a".into(),
                type_name: "int".into(),
                unsigned: false,
            },
            SchemaCol {
                name: "b".into(),
                type_name: "int".into(),
                unsigned: false,
            },
        ],
        pk: vec![],
        uks: vec![],
    }
}

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    let with_crc = data[0] & 1 != 0;
    let buf = &data[1..];
    let Ok(h) = parse_header(buf) else {
        return;
    };
    let size = h.event_size as usize;
    if size < EVENT_HEADER_SIZE || size > buf.len() {
        return;
    }
    let mut body = buf[EVENT_HEADER_SIZE..size].to_vec();
    strip_checksum(&mut body, with_crc);
    match h.event_type.0 {
        19 => {
            let _ = parse_table_map(&body, with_crc);
        }
        20 | 21 | 22 | 23 | 24 | 25 | 30 | 31 | 32 => {
            // 固定合成 tm：合法字节必解成功；理论不可达的 Err 也走 return，
            // 绝不 panic（解码器契约 = Err-不-panic，此处只加固不弱化）。
            let Ok(tm) = parse_table_map(&synth_tm_body(), false) else {
                return;
            };
            let kind = match h.event_type.0 {
                20 | 23 | 30 => RowsKind::Write,
                21 | 24 | 31 => RowsKind::Update,
                _ => RowsKind::Delete,
            };
            let v2 = matches!(h.event_type.0, 30 | 31 | 32);
            let _ = decode_rows(&body, &tm, &synth_schema(), kind, v2);
        }
        _ => {}
    }
});
