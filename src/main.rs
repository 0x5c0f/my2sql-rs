//! my2sql-rs：MySQL binlog 解析 / 还原 SQL 工具（入口装配，逻辑在库层）。

use std::process::exit;

use my2sql_rs::config::Config;
use my2sql_rs::pipeline::run_to_sql;

fn main() {
    let cfg = Config::from_args();
    // 进度/告警走 tracing（stderr）；默认全收（无 env-filter 特性），
    // 摘要行单独 println 到 stdout。
    tracing_subscriber::fmt::init();
    match run_to_sql(&cfg) {
        Ok(summary) => {
            println!("{summary}");
        }
        Err(e) => {
            eprintln!("error: {e}");
            exit(1);
        }
    }
}
