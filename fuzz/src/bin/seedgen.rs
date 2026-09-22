//! P4a Lane A Step 5：确定性种子语料生成器（禁随机入仓，spec §1）。
//!
//! 复用 `tests/fuzz_seed.rs` 的 builder（仅因其 4 个 seed_* +
//! `event_bytes`/`tm_body`/`rows_head` 已 `pub`，零逻辑改动）。
//! 普通 bin 构建不注册内建 `#[test]` 属性；下面的 `macro_rules! test` 在本
//! crate 作用域内将其屏蔽为 no-op——主 crate 的 `cargo test --test
//! fuzz_seed` 在另一个 crate 编译，测试注册完全不受影响。
//!
//! 产出（每枚 payload × with_crc 选择器 ∈ {0,1} 双件，靶以 `data[0]&1`
//! 读选择器，故语料文件 = 1 字节选择器 + payload）写 argv 指定的两个目录：
//! 1. 7 枚畸形种子（tests/fuzz_seed.rs SEEDS 同源：P1 4 枚 + P4a T1 fuzz
//!    真发现溢出件 3 枚）；
//! 2. 合法基线事件 ≥3 枚：合法 2×INT TABLE_MAP 单事件；tm + 只含
//!    present=0 空行区 WRITE_ROWS_V2 的事件对（seed2 的 tm 件口径）；
//!    seed1 的 tm 修全形（声明 4B 实给 4B + null_bits）。
//!
//! **crc1 件的 CRC 形态（评审修复轮 2）**：选择器 =1 时靶会先
//! `strip_checksum(body, true)` 剥 4B——payload 若无 CRC 尾，剥掉的就是真实
//! body 尾部，合法基线在 crc1 腿全军覆没（selector 腿形同未跑）。现 crc1
//! payload 逐事件在 `event_size` 内补 4B 占位 CRC 尾（同步 +4 event_size），
//! 剥一次后即还原与 crc0 完全相同的 body。main() 对 `legal_*` 件双态自检
//! （每个 TABLE_MAP 必须 parse Ok），非法基线直接让 seedgen 非零退出。

#![allow(dead_code, unused_imports)]

// lint 误报：宏确被 `#[test]`（属性位、跨 #[path] 模块）消费，仍报 unused。
#[allow(unused_macros)]
macro_rules! test {
    ($($tt:tt)*) => {};
}

#[path = "../../../tests/fuzz_seed.rs"]
mod seedsrc;

use my2sql_rs::binlog::event::{EVENT_HEADER_SIZE, parse_header, strip_checksum};
use my2sql_rs::binlog::table_map::parse_table_map;

const TABLE_MAP_TYPE: u8 = 19;
const WRITE_ROWS_V2_TYPE: u8 = 30;
/// CRC32 尾长度（与 `event::strip_checksum` 的剥除宽度同源）。
const CRC_LEN: usize = 4;

/// (语料名, payload 字节)。payload 前会再拼 1 字节 with_crc 选择器。
fn payloads() -> Vec<(String, Vec<u8>)> {
    vec![
        // ---- 畸形种子 ×4（tests/fuzz_seed.rs SEEDS 同源，原样） ----
        (
            "seed1_table_map_truncated_meta".into(),
            seedsrc::seed_table_map_truncated_meta(),
        ),
        (
            "seed2_rows_cols_present_zero".into(),
            seedsrc::seed_rows_cols_present_zero(),
        ),
        (
            "seed3_json_depth_bomb".into(),
            seedsrc::seed_json_depth_bomb(),
        ),
        (
            "seed4_decimal_full_group_overflow".into(),
            seedsrc::seed_decimal_full_group_overflow(),
        ),
        // P4a T1 fuzz 首轮真发现（tmin 等价精简件，同 tests/fuzz_seed.rs 5–7）
        (
            "seed5_tm_ncols_overflow".into(),
            seedsrc::seed_tm_ncols_overflow(),
        ),
        (
            "seed6_tm_meta_len_overflow".into(),
            seedsrc::seed_tm_meta_len_overflow(),
        ),
        (
            "seed7_tm_tlv_len_overflow".into(),
            seedsrc::seed_tm_tlv_len_overflow(),
        ),
        // ---- 合法基线 ≥3 ----
        // 合法 2×INT TABLE_MAP 单事件。
        (
            "legal_tm_only".into(),
            seedsrc::event_bytes(
                TABLE_MAP_TYPE,
                &seedsrc::tm_body(7, "fz", "t_zero_bm", &[3, 3], 0, &[], &[0]),
            ),
        ),
        // 合法 tm + 一条 WRITE_ROWS_V2（present=0 的空行区版，即 seed2 的
        // tm 件 + 只含 tm 头段的 rows 事件，行区为空）。
        ("legal_tm_plus_empty_write_rows_v2".into(), {
            let mut out = seedsrc::event_bytes(
                TABLE_MAP_TYPE,
                &seedsrc::tm_body(7, "fz", "t_zero_bm", &[3, 3], 0, &[], &[0]),
            );
            out.extend_from_slice(&seedsrc::event_bytes(
                WRITE_ROWS_V2_TYPE,
                &seedsrc::rows_head(7, 2, &[0x00]),
            ));
            out
        }),
        // seed1 的 tm 修全形：声明 meta 总长 4B、实给 4B（2×VAR_STRING 各 2B）
        // + null_bits，应为合法 TABLE_MAP。
        (
            "legal_tm_full_meta_varstring".into(),
            seedsrc::event_bytes(
                TABLE_MAP_TYPE,
                &seedsrc::tm_body(
                    7,
                    "fz",
                    "t_trunc",
                    &[0xFD, 0xFD],
                    4,
                    &[0x0a, 0x00, 0x0a, 0x00],
                    &[0x00],
                ),
            ),
        ),
    ]
}

/// with_crc=1 腿的 payload 变换：逐事件在帧尾补 4B 占位 CRC 并同步
/// `event_size += 4`（占位在 event_size **内**，靶 `strip_checksum(_, true)`
/// 恰好剥掉它，还原与 crc0 逐字节相同的 body）。头非法/越界时原样拷贝
/// 剩余字节（当前全部 payload 由 `event_bytes` 构造、恒走不到该分支，
/// 仅防 builder 漂移时 seedgen 自己 panic）。确定性：同输入同输出。
fn append_crc_filler(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 2 * CRC_LEN);
    let mut off = 0usize;
    while off < payload.len() {
        if payload.len() - off < EVENT_HEADER_SIZE {
            out.extend_from_slice(&payload[off..]);
            return out;
        }
        let size = u32::from_le_bytes(payload[off + 9..off + 13].try_into().unwrap()) as usize;
        if size < EVENT_HEADER_SIZE || off + size > payload.len() {
            out.extend_from_slice(&payload[off..]);
            return out;
        }
        out.extend_from_slice(&payload[off..off + size]);
        out.extend_from_slice(&[0u8; CRC_LEN]); // 占位 CRC32（靶不做 crc32 校验）
        let at = out.len() - size - CRC_LEN + 9;
        out[at..at + 4].copy_from_slice(&((size + CRC_LEN) as u32).to_le_bytes());
        off += size;
    }
    out
}

/// 双态自检：`legal_*` payload 在选择器 sel∈{0,1} 下逐事件走靶同款口径
/// （parse_header → 尺寸闸 → strip_checksum(sel) → TABLE_MAP 必须 parse Ok）。
/// 任一不满足即 panic——seedgen 非零退出，坏语料出不了门。
fn self_check_legal(name: &str, payload: &[u8]) {
    if !name.starts_with("legal_") {
        return;
    }
    for sel in [0u8, 1u8] {
        let bytes = if sel == 1 {
            append_crc_filler(payload)
        } else {
            payload.to_vec()
        };
        let mut off = 0usize;
        let mut n_tm = 0usize;
        while off < bytes.len() {
            let h = parse_header(&bytes[off..])
                .unwrap_or_else(|e| panic!("{name}_crc{sel}: header 自检失败: {e:?}"));
            let size = h.event_size as usize;
            assert!(
                size >= EVENT_HEADER_SIZE && off + size <= bytes.len(),
                "{name}_crc{sel}: event_size {size} 越界（off={off} len={})",
                bytes.len()
            );
            let mut body = bytes[off + EVENT_HEADER_SIZE..off + size].to_vec();
            strip_checksum(&mut body, sel == 1);
            if h.event_type.0 == TABLE_MAP_TYPE {
                parse_table_map(&body, false)
                    .unwrap_or_else(|e| panic!("{name}_crc{sel}: 合法 TABLE_MAP 解失败: {e:?}"));
                n_tm += 1;
            }
            off += size;
        }
        assert!(n_tm >= 1, "{name}_crc{sel}: 合法件却无 TABLE_MAP 事件");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    assert_eq!(
        args.len(),
        2,
        "usage: seedgen <decode_event_corpus_dir> <event_stream_corpus_dir>"
    );
    let mut total = 0usize;
    for dir in &args {
        std::fs::create_dir_all(dir).expect("create corpus dir");
        for (name, payload) in payloads() {
            self_check_legal(&name, &payload);
            let crc1 = append_crc_filler(&payload);
            for sel in [0u8, 1u8] {
                let mut b = vec![sel];
                b.extend_from_slice(if sel == 1 { &crc1 } else { &payload }.as_slice());
                let path = std::path::Path::new(dir).join(format!("{name}_crc{sel}.bin"));
                std::fs::write(&path, &b).expect("write corpus file");
                total += 1;
                println!("wrote {} ({} bytes)", path.display(), b.len());
            }
        }
    }
    println!("seedgen: {total} corpus files");
}
