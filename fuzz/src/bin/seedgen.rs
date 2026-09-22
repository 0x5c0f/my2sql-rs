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

#![allow(dead_code, unused_imports)]

// lint 误报：宏确被 `#[test]`（属性位、跨 #[path] 模块）消费，仍报 unused。
#[allow(unused_macros)]
macro_rules! test {
    ($($tt:tt)*) => {};
}

#[path = "../../../tests/fuzz_seed.rs"]
mod seedsrc;

const TABLE_MAP_TYPE: u8 = 19;
const WRITE_ROWS_V2_TYPE: u8 = 30;

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
            for sel in [0u8, 1u8] {
                let mut b = vec![sel];
                b.extend_from_slice(&payload);
                let path = std::path::Path::new(dir).join(format!("{name}_crc{sel}.bin"));
                std::fs::write(&path, &b).expect("write corpus file");
                total += 1;
                println!("wrote {} ({} bytes)", path.display(), b.len());
            }
        }
    }
    println!("seedgen: {total} corpus files");
}
