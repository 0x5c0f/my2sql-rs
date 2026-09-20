//! Task 16 / DoD-3：file 模式 to-sql **端到端吞吐基线**（criterion，harness=false）。
//!
//! 测量对象 = 发布态二进制的完整流水线（读 binlog → 解码 → 并行 worker →
//! 保序 → 写盘），通过子进程调用 `to-sql`；criterion 的 `Throughput::Bytes`
//! 以 **binlog 输入字节数** 计吞吐（MB/s，10^6 字节/秒口径）。
//!
//! 输入（≥500MB 合成 binlog）由 `tools/gen-bench-binlog.sh` 生成并缓存在
//! `data/bench/`（git-ignored）。**输入缺失或 debug 编译档时本 bench 跳过并
//! exit 0**——保证 `cargo test --all-targets` / `make test` 在洁净克隆上不受影响；
//! 正式基线跑法：
//!
//! ```text
//! bash tools/gen-bench-binlog.sh        # 一次性产数据（docker，约 5-10 分钟）
//! cargo bench --bench decode            # criterion 基线（bench profile = release 同档）
//! ```
//!
//! 编译档：`[profile.bench] inherits = "release"`（Cargo.toml），且 cargo bench
//! 的 `CARGO_BIN_EXE_my2sql-rs` 指向 bench-profile 产物——与 `--release` 同
//! opt-level/LTO，满足「基线必须 release 构建」约束。
//!
//! 通过标准（DoD-3）：threads=8 吞吐 ≥ 40MB/s。threads=1 组仅作扩展性参考
//! （低于阈值时定位串行点用）。

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput};

/// 与被测二进制约定的最小输入集；None = 未就绪（跳过）。
struct Input {
    binary: PathBuf,
    binlog_dir: PathBuf,
    start_file: String,
    schema_file: PathBuf,
    out_dir: PathBuf,
    bytes: u64,
}

fn load_input() -> Option<Input> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let bench = root.join("data/bench");
    let marker = bench.join(".bench-ready");
    let file = std::fs::read_to_string(&marker).ok()?;
    let start_file = file.trim().to_string();
    let bytes = std::fs::metadata(bench.join(&start_file)).ok()?.len();
    if bytes < 500_000_000 {
        return None;
    }
    let schema_file = bench.join("schema.json");
    if !schema_file.is_file() {
        return None;
    }
    let mut out_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    out_dir.push("bench-file-to-sql");
    Some(Input {
        // cargo bench 下为 bench profile（inherits=release）产物
        binary: PathBuf::from(env!("CARGO_BIN_EXE_my2sql-rs")),
        binlog_dir: bench,
        start_file,
        schema_file,
        out_dir,
        bytes,
    })
}

/// 一次完整 file-mode to-sql 运行；失败即 panic（criterion 会把迭代失败上抛）。
fn run_once(inp: &Input, threads: usize) {
    let st = Command::new(&inp.binary)
        .args([
            "to-sql",
            "--binlog-dir",
            inp.binlog_dir.to_str().unwrap(),
            "--start-file",
            &inp.start_file,
            "--schema-file",
            inp.schema_file.to_str().unwrap(),
            "--time-zone",
            "+00:00",
            "--threads",
            &threads.to_string(),
            "--output-dir",
            inp.out_dir.to_str().unwrap(),
        ])
        .status()
        .expect("spawn my2sql-rs (bench profile binary)");
    assert!(st.success(), "to-sql exited {st:?} (threads={threads})");
}

fn prepare_out_dir(inp: &Input) {
    if inp.out_dir.exists() {
        std::fs::remove_dir_all(&inp.out_dir).ok();
    }
    std::fs::create_dir_all(&inp.out_dir).unwrap();
}

fn bench_file_to_sql(c: &mut Criterion, inp: &Input) {
    let mut g = c.benchmark_group("file_to_sql");
    g.throughput(Throughput::Bytes(inp.bytes));
    // 单次迭代 ~10-30s（500MB+ 全流水线）：收缩 warmup、样本量 10（DoD 判定
    // 用 threads=8；threads=1 为扩展性参考组）。
    g.warm_up_time(Duration::from_secs(1));
    g.measurement_time(Duration::from_secs(30));
    g.sample_size(10);
    for threads in [8usize, 1usize] {
        let param = format!("{} MiB", inp.bytes / 1_048_576);
        let id = BenchmarkId::new(format!("threads={threads}"), param);
        g.bench_with_input(id, &(inp, threads), |b, &(inp, threads)| {
            prepare_out_dir(inp);
            b.iter(|| {
                run_once(inp, std::hint::black_box(threads));
            });
        });
    }
    g.finish();
}

fn main() {
    // dev 档保护：cargo test（dev）里 CARGO_BIN_EXE 指向 debug 二进制，测它没有
    // 意义且极耗时；吞吐基线只认 release 同档（cargo bench / --release）。
    if cfg!(debug_assertions) {
        eprintln!("SKIP bench: debug 编译档不做吞吐基线（请用 cargo bench，release 同档）");
        return;
    }
    let Some(inp) = load_input() else {
        eprintln!(
            "SKIP bench: data/bench 未就绪（≥500MB 缓存 binlog + schema.json）。\n\
             生成：bash tools/gen-bench-binlog.sh（跳过是为了不破坏 cargo test --all-targets）"
        );
        return;
    };
    println!(
        "bench input: {} ({} MiB) binary={}",
        inp.start_file,
        inp.bytes / 1_048_576,
        inp.binary.display()
    );
    let mut c = Criterion::default().configure_from_args();
    bench_file_to_sql(&mut c, &inp);
    c.final_summary();
}
