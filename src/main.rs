//! my2sql-rs：MySQL binlog 解析 / 还原 SQL 工具（入口装配骨架）。

mod binlog;
mod config;
mod metadata;
mod pipeline;
mod sqlopen;

use std::process::exit;

use config::Config;

fn main() {
    let cfg = Config::from_args();
    // 占位：Task 12/13/14 将用 读取→解码→过滤→SQL 生成 管道替换此处。
    eprintln!(
        "to-sql: not wired yet (start_file={}, start_pos={}, threads={})",
        cfg.start_file, cfg.start_pos, cfg.threads
    );
    exit(1);
}
