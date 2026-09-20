//! Task 16 / DoD-4：畸形 binlog 事件语料（fuzz seed）→ 解码层必须返回
//! `Err` 且**绝不 panic**。
//!
//! 四个种子文件落在 `tests/fuzz_seed/*.bin`，由本文件的 `builders` 从
//! `tests/fixtures/events.rs` 的合法事件头常量 + 合法事件体构造器
//! （e2e.rs Synth 同款布局）对字节流施加畸形而来：
//!   1. `table_map_truncated_meta.bin` —— TABLE_MAP 在 metadata 段中途截断
//!      （声明 meta 总长 4B，实体只给 2B，null_bits/charset 缺失）；
//!   2. `rows_cols_present_zero.bin` —— WRITE_ROWS_V2 位图全 0（present==0
//!      活锁形态，T12 Step-0 守卫的敌意输入形状，行区刻意非空）；
//!   3. `json_depth_bomb.bin` —— WRITE_ROWS_V2 的 JSON 列携带 151 层嵌套
//!      数组（> MAX_DEPTH=100 的深度炸弹，同时把每层 size 字段保持自洽，
//!      防止在深度闸口之前先撞长度校验）；
//!   4. `decimal_full_group_overflow.bin` —— WRITE_ROWS_V2 的 DECIMAL(19,9)
//!      列携带终审 #1 repro 字节 `81 00 00 00 01 FF FF FF FF`（小数满组
//!      XOR 还原为 10 位 u32，`9 − 10` 下溢点，修复前 debug/release 双 panic）。
//!
//! 用途注记：**这些文件是 P4 正式 fuzz（cargo-fuzz/libfuzzer）的起始语料
//! 种子**（spec §挂账 P4）。P4 接入时直接以 tests/fuzz_seed/ 为 corpus 目录；
//! 本测试即语料的回归闸——任何让种子变为 panic / Ok 的解码层改动都在这里红。
//!
//! 重生成（builder 调整后钉文件）：`FUZZ_SEED_REGEN=1 cargo test --test fuzz_seed`

use std::panic::{AssertUnwindSafe, catch_unwind};

use my2sql_rs::binlog::error::BinlogError;
use my2sql_rs::binlog::event::{EVENT_HEADER_SIZE, parse_header, strip_checksum};
use my2sql_rs::binlog::rows::{RowsKind, decode_rows};
use my2sql_rs::binlog::table_map::{TableMapEvent, parse_table_map};
use my2sql_rs::metadata::schema::{SchemaCol, TableSchema};

#[path = "fixtures/events.rs"]
#[allow(dead_code)] // fixture 常量全集共享（KNOWN_TS/FLAGS 等由 known_header_bytes 内部消费）
mod fixtures;

const TABLE_MAP_TYPE: u8 = fixtures::KNOWN_TYPE; // 19
const WRITE_ROWS_V2_TYPE: u8 = 30;

// ---------- 事件字节构造器（合法形态基线，随后按需畸形） ----------

/// 19B 公共头：以 fixtures 的**合法** `known_header_bytes()` 为底（TABLE_MAP、
/// size=100），原位覆写 type/event_size 成为合法新头；log_pos 保留 fixture 常量
/// （解码层不校验 log_pos 自洽性）。
fn event_bytes(evtype: u8, body: &[u8]) -> Vec<u8> {
    let size = (fixtures::EVENT_HEADER_SIZE + body.len()) as u32;
    let mut b = fixtures::known_header_bytes();
    b[4] = evtype;
    b[9..13].copy_from_slice(&size.to_le_bytes());
    b.extend_from_slice(body);
    b
}

/// TABLE_MAP 事件体（对照 e2e.rs Synth::table_map 的合法布局）。
/// `types`/`meta_len_declared` 分离声明，支持制造「声明与实给不符」畸形。
fn tm_body(
    tid: u64,
    db: &str,
    tb: &str,
    types: &[u8],
    meta_len_declared: usize,
    meta_given: &[u8],
    null_bits: &[u8],
) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&tid.to_le_bytes()[..6]);
    b.extend_from_slice(&0u16.to_le_bytes()); // flags
    b.push(db.len() as u8);
    b.extend_from_slice(db.as_bytes());
    b.push(0);
    b.push(tb.len() as u8);
    b.extend_from_slice(tb.as_bytes());
    b.push(0);
    b.push(types.len() as u8); // n_cols（LNE < 251）
    b.extend_from_slice(types);
    b.push(meta_len_declared as u8); // metadata 总长 LNE
    b.extend_from_slice(meta_given);
    b.extend_from_slice(null_bits);
    b
}

/// WRITE_ROWS_V2 事件体头段：tid + flags + extra_info_len(=2 自含) + n_cols + bm1。
fn rows_head(tid: u64, n_cols: usize, present_bitmap: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&tid.to_le_bytes()[..6]);
    b.extend_from_slice(&0u16.to_le_bytes()); // flags
    b.extend_from_slice(&2u16.to_le_bytes()); // extra_info_len = 2（空段，真机 8.0.46 恒此形）
    b.push(n_cols as u8);
    b.extend_from_slice(present_bitmap);
    b
}

/// JSON 二进制深度炸弹：镜像 `src/binlog/json.rs` 测试 `depth_limit::wrap`
/// 的合法小数组逐层包装（count=1、size 自洽），depth 层 > MAX_DEPTH。
fn jsonb_wrapped(depth: usize) -> Vec<u8> {
    const SMALL_ARRAY: u8 = 0x02;
    const LITERAL: u8 = 0x04;
    // 最内层：[count=1][size=8][entry: type=04 offset=00 00 → inline null] + 1B 填充
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

// ---------- 四个种子：合法事件流 + 一处畸形 ----------

/// 种子 1：TABLE_MAP 在 metadata 中途截断（声明 4B、实给 2B、无 null_bits）。
fn seed_table_map_truncated_meta() -> Vec<u8> {
    let body = tm_body(
        7,
        "fz",
        "t_trunc",
        &[0xFD, 0xFD], // 2×VAR_STRING，每列 meta 2B ⇒ 声明总长 4
        4,
        &[0x0a, 0x00], // 只给第一列的 meta，第二列 2B 缺失 → read_lns TooShort
        &[],           // 连带 null_bits 也没有（截断即截在这里）
    );
    event_bytes(TABLE_MAP_TYPE, &body)
}

/// 种子 2：合法 TABLE_MAP（2×INT）+ WRITE_ROWS_V2 位图全 0 + 非空行区。
fn seed_rows_cols_present_zero() -> Vec<u8> {
    let tm = tm_body(7, "fz", "t_zero_bm", &[0x03, 0x03], 0, &[], &[0x00]);
    let mut rows = rows_head(7, 2, &[0x00]); // present==0（活锁形态）
    rows.extend_from_slice(&[0x00, 0x01, 0x02, 0x03, 0x04]); // 行区刻意非空
    let mut out = event_bytes(TABLE_MAP_TYPE, &tm);
    out.extend_from_slice(&event_bytes(WRITE_ROWS_V2_TYPE, &rows));
    out
}

/// 种子 3：合法 TABLE_MAP（1×JSON，meta=4 前缀宽）+ WRITE_ROWS_V2 载 151 层
/// 嵌套数组（json.rs MAX_DEPTH=100 拒绝；各层 size 自洽，先撞的是深度闸口）。
fn seed_json_depth_bomb() -> Vec<u8> {
    let tm = tm_body(7, "fz", "t_json_bomb", &[0xF5], 1, &[0x04], &[0x00]);
    let payload = jsonb_wrapped(150);
    let mut rows = rows_head(7, 1, &[0x01]); // present = 列 0
    rows.push(0x00); // 行 null 区（1 列 → 1B，非 NULL）
    rows.extend_from_slice(&(payload.len() as u32).to_le_bytes()); // meta 宽 LE 前缀
    rows.extend_from_slice(&payload);
    let mut out = event_bytes(TABLE_MAP_TYPE, &tm);
    out.extend_from_slice(&event_bytes(WRITE_ROWS_V2_TYPE, &rows));
    out
}

/// 种子 4：合法 TABLE_MAP（1×NEWDECIMAL(19,9)，meta=2B [19,9]）+ WRITE_ROWS_V2
/// 载终审 #1 repro 字节——小数满组 0xFFFFFFFF 还原为 10 位值，`9 − t.len()`
/// 下溢（修复前 debug subtract-overflow / release repeat-capacity 双 panic）。
fn seed_decimal_full_group_overflow() -> Vec<u8> {
    let tm = tm_body(7, "fz", "t_dec_bomb", &[0xF6], 2, &[19, 9], &[0x00]);
    let mut rows = rows_head(7, 1, &[0x01]); // present = 列 0
    rows.push(0x00); // 行 null 区（1 列 → 1B，非 NULL）
    rows.extend_from_slice(&[0x81, 0x00, 0x00, 0x00, 0x01, 0xFF, 0xFF, 0xFF, 0xFF]);
    let mut out = event_bytes(TABLE_MAP_TYPE, &tm);
    out.extend_from_slice(&event_bytes(WRITE_ROWS_V2_TYPE, &rows));
    out
}

// ---------- 解码层走读：任一事件出错即整体 Err ----------

fn schema_for(tm: &TableMapEvent) -> TableSchema {
    let cols = match tm.table.as_str() {
        "t_zero_bm" => vec![
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
        "t_json_bomb" => vec![SchemaCol {
            name: "j".into(),
            type_name: "json".into(),
            unsigned: false,
        }],
        "t_dec_bomb" => vec![SchemaCol {
            name: "d".into(),
            type_name: "decimal".into(),
            unsigned: false,
        }],
        other => panic!("unexpected table in seed: {other}"),
    };
    TableSchema {
        db: tm.schema.clone(),
        table: tm.table.clone(),
        cols,
        pk: vec![],
        uks: vec![],
    }
}

/// 逐事件走过解码层链：parse_header → parse_table_map → decode_rows
/// （含 decode_value → json_binary_to_text）。无 CRC 形态（5.6 语义）。
fn run_decode_layer(bytes: &[u8]) -> Result<(), BinlogError> {
    let mut off = 0usize;
    let mut tm: Option<TableMapEvent> = None;
    while off < bytes.len() {
        let h = parse_header(&bytes[off..])?;
        let size = h.event_size as usize;
        if size < EVENT_HEADER_SIZE || off + size > bytes.len() {
            return Err(BinlogError::InvalidData(format!(
                "event_size {size} out of bounds at {off}"
            )));
        }
        let mut body = bytes[off + EVENT_HEADER_SIZE..off + size].to_vec();
        strip_checksum(&mut body, false);
        match h.event_type.0 {
            TABLE_MAP_TYPE => tm = Some(parse_table_map(&body, false)?),
            WRITE_ROWS_V2_TYPE => {
                let t = tm
                    .as_ref()
                    .ok_or_else(|| BinlogError::InvalidData("rows without table_map".into()))?;
                decode_rows(&body, t, &schema_for(t), RowsKind::Write, true)?;
            }
            _ => {}
        }
        off += size;
    }
    Ok(())
}

type SeedBuilder = fn() -> Vec<u8>;

const SEEDS: &[(&str, SeedBuilder)] = &[
    (
        "table_map_truncated_meta.bin",
        seed_table_map_truncated_meta,
    ),
    ("rows_cols_present_zero.bin", seed_rows_cols_present_zero),
    ("json_depth_bomb.bin", seed_json_depth_bomb),
    (
        "decimal_full_group_overflow.bin",
        seed_decimal_full_group_overflow,
    ),
];

#[test]
fn fuzz_seeds_return_err_and_never_panic() {
    let mut checked = 0;
    for (name, build) in SEEDS {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fuzz_seed")
            .join(name);
        let disk = std::fs::read(&path).unwrap_or_else(|e| {
            panic!(
                "seed {} 缺失（跑 FUZZ_SEED_REGEN=1 cargo test --test fuzz_seed）: {e}",
                path.display()
            )
        });
        // 磁盘种子必须与 builder 输出逐字节一致（防漂移：P4 corpus 同源可再生）
        assert_eq!(disk, build(), "{name}: 磁盘种子与 builder 输出漂移");
        // 关键闸：Err 而非 panic、也非 Ok。catch_unwind 把 panic 变成可断言值。
        let res = catch_unwind(AssertUnwindSafe(|| run_decode_layer(&disk)));
        match res {
            Err(_) => panic!("{name}: 解码层 panic（DoD-4 违例）"),
            Ok(Ok(())) => panic!("{name}: 畸形输入被解码层接受（应 Err，实 Ok）"),
            Ok(Err(e)) => {
                eprintln!("{name}: Err as expected: {e:?}");
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 4, "四个种子全部检查");
}

/// 非默认路径：`FUZZ_SEED_REGEN=1 cargo test --test fuzz_seed` 重生成磁盘种子。
#[test]
fn regenerate_fuzz_seeds_when_requested() {
    if std::env::var("FUZZ_SEED_REGEN").is_err() {
        return;
    }
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fuzz_seed");
    std::fs::create_dir_all(&dir).unwrap();
    for (name, build) in SEEDS {
        std::fs::write(dir.join(name), build()).unwrap();
        eprintln!("wrote {}", dir.join(name).display());
    }
}
