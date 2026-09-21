//! my2sql-rs：MySQL binlog 解析 / 还原 SQL 工具（入口装配，逻辑在库层）。

use std::process::exit;

use my2sql_rs::config::{Config, WorkType};
use my2sql_rs::pipeline::{run_flashback, run_repl, run_stats, run_to_sql};

fn main() {
    let cfg = Config::from_args();
    // 进度/告警走 tracing（stderr）；默认全收（无 env-filter 特性），
    // 摘要行单独 println 到 stdout。
    tracing_subscriber::fmt::init();
    // P2 T5：三子命令按 work_type 分派（validate_* 已保证与子命令一致）。
    // 摘要文案：to-sql 走 Display（"to-sql done:"，P1 逐字节不变）；
    // flashback 复用同字段面换前缀（display_with）；stats 由 StatsRun 自带 Display。
    let rc = match cfg.work_type {
        WorkType::ToSql => run_to_sql(&cfg).map(|s| println!("{s}")),
        WorkType::Flashback => {
            run_flashback(&cfg).map(|s| println!("{}", s.display_with("flashback done")))
        }
        WorkType::Stats => run_stats(&cfg).map(|s| println!("{s}")),
        // P3 T1：repl dispatch 面定稿（run_repl 现为真实 Err 空壳 → 下方统一
        // 打印 `error: …` 并退 1；T5 仅换函数体）。
        WorkType::Repl => run_repl(&cfg).map(|s| println!("{}", s.display_with("repl done"))),
    };
    match rc {
        Ok(()) => {}
        Err(e) => {
            eprintln!("error: {e}");
            exit(1);
        }
    }
}
