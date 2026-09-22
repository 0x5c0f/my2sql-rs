//! 靶 2（P4a Lane A / spec §1）：多事件流 + 事务状态机 fuzz。
//!
//! 走查器口径源 = `tests/fuzz_seed.rs:run_decode_layer`（fuzz 包无法 import
//! 主 crate tests/，本文件为同构重写，口径漂移以彼为准并回改此处注释）：
//! 逐事件 `parse_header` → 尺寸闸 → `strip_checksum(body, with_crc)` →
//! `parse_table_map`/`decode_rows`；`with_crc` 取首字节选择器（语料两态都跑，
//! 见 seedgen 的 crc0/crc1 双件）。
//!
//! 流级不变量：遇 QUERY(2)/XID_EVENT(15,16)（简报绑定值；15 在 MySQL 官方
//! 序为 FORMAT_DESC——按简报逐字，两值都喂 Xid 只影响状态机语义分类，不触
//! 解码器）或 QUERY 文本为 BEGIN 的语义体，构造 `RawEvent` 喂
//! `TrxStateMachine::feed`，断言 `trx_id` 不回退（assert 触发即 crash 报告）。
//!
//! 表→schema 映射默认 = 2×INT 合成（同靶 1，禁 panic：`decode_rows` 对
//! schema 越位列走 `cols.get(i).unwrap_or(dropped)`，无索引 panic 面）。
//! 不 catch_unwind：panic = crash。

#![no_main]

use libfuzzer_sys::fuzz_target;
use my2sql_rs::binlog::event::{EVENT_HEADER_SIZE, parse_header, strip_checksum};
use my2sql_rs::binlog::rows::{RowsKind, decode_rows};
use my2sql_rs::binlog::table_map::{TableMapEvent, parse_table_map};
use my2sql_rs::metadata::schema::{SchemaCol, TableSchema};
use my2sql_rs::pipeline::source::{RawEvent, RawKind, TrxStateMachine};
use std::sync::Arc;

/// 2×INT 合成 schema（表名随流内 tm，钉死无查找表 = 无 panic 路径）。
fn synth_schema(tm: &TableMapEvent) -> TableSchema {
    TableSchema {
        db: tm.schema.clone(),
        table: tm.table.clone(),
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

/// QUERY 事件体（已剥 19B 头与 CRC）取 SQL 文本：
/// `[4 thread_id][4 exec_time][1 db_len][2 err_code][2 sv_len][sv..][db\0][sql..]`。
/// 畸形 → 空串（状态机对 "" 为 Process no-op），全程无 panic。
fn query_text(body: &[u8]) -> String {
    let Some(&db_len) = body.get(8) else {
        return String::new();
    };
    let Some(sv) = body.get(9..11) else {
        return String::new();
    };
    let sv_len = u16::from_le_bytes([sv[0], sv[1]]) as usize;
    let start = 13usize + sv_len + db_len as usize + 1;
    match body.get(start..) {
        Some(rest) => String::from_utf8_lossy(rest).into_owned(),
        None => String::new(),
    }
}

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    let with_crc = data[0] & 1 != 0;
    let bytes = &data[1..];
    let mut sm = TrxStateMachine::new();
    let mut last_trx: u64 = 0;
    let mut tm: Option<TableMapEvent> = None;
    let mut off = 0usize;
    while off < bytes.len() {
        let Ok(h) = parse_header(&bytes[off..]) else {
            return;
        };
        let size = h.event_size as usize;
        if size < EVENT_HEADER_SIZE || off + size > bytes.len() {
            return;
        }
        let mut body = bytes[off + EVENT_HEADER_SIZE..off + size].to_vec();
        strip_checksum(&mut body, with_crc);
        match h.event_type.0 {
            19 => {
                if let Ok(t) = parse_table_map(&body, with_crc) {
                    tm = Some(t);
                }
            }
            20 | 21 | 22 | 23 | 24 | 25 | 30 | 31 | 32 => {
                if let Some(t) = tm.as_ref() {
                    let kind = match h.event_type.0 {
                        20 | 23 | 30 => RowsKind::Write,
                        21 | 24 | 31 => RowsKind::Update,
                        _ => RowsKind::Delete,
                    };
                    let v2 = matches!(h.event_type.0, 30 | 31 | 32);
                    let _ = decode_rows(&body, t, &synth_schema(t), kind, v2);
                }
            }
            2 | 15 | 16 => {
                let kind = if h.event_type.0 == 2 {
                    RawKind::Query(query_text(&body))
                } else {
                    RawKind::Xid
                };
                let raw = RawEvent {
                    binlog: "fuzz".into(),
                    start_pos: h.log_pos.saturating_sub(h.event_size),
                    end_pos: h.log_pos,
                    timestamp: h.timestamp,
                    kind,
                    body: body.clone(),
                    tm: tm.clone().map(Arc::new),
                };
                let (id, _status) = sm.feed(&raw);
                assert!(id >= last_trx, "trx_id regressed: {last_trx} -> {id}");
                last_trx = id;
            }
            _ => {}
        }
        off += size;
    }
});
