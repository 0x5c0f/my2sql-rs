//! CLI 定义与全局配置。
//!
//! Task 1 只装配骨架：`Cli::parse()` → 校验 → `Config`。
//! 后续任务（解码/过滤/SQL 生成/元数据）从 `Config` 读取参数。

use std::path::PathBuf;
use std::process::exit;
use std::str::FromStr;
use std::thread;

use chrono::{DateTime, FixedOffset, Local, NaiveDateTime};
use clap::{Args, Parser, Subcommand, ValueEnum};

/// binlog 转 SQL 的行类型过滤器；空 = 全部。
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Dml {
    Insert,
    Update,
    Delete,
}

/// 工作模式（P2 T3；上游 `-work-type` 的库层对应物，CLI 子命令面归 T5）。
/// `Stats` 由 P2 T4 消费，本任务仅立枚举位。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkType {
    ToSql,
    Flashback,
    Stats,
}

/// 逐事件错误策略（P2 T3）：`Stop` = 首错即整跑 Err + 清场（flashback 专用
/// 语义；to-sql 路径恒 `SkipBadEvent` 行为不变），`SkipBadEvent` = 计数跳过
/// （P1 robust-continue 既定默认）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnError {
    Stop,
    SkipBadEvent,
}

#[derive(Parser)]
#[command(name = "my2sql-rs", version)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// 解析 binlog 并输出还原 SQL（一期核心子命令）
    ToSql(ToSqlArgs),
}

#[derive(Args, Debug, Clone)]
pub struct ToSqlArgs {
    /// 服务端 binlog 目录（离线模式）或远程连接时的源库 binlog 目录
    #[arg(long)]
    pub binlog_dir: PathBuf,
    /// 起始 binlog 文件名，如 mysql-bin.000001
    #[arg(long)]
    pub start_file: String,
    /// 起始位点
    #[arg(long, default_value_t = 4)]
    pub start_pos: u32,
    /// 结束 binlog 文件名（默认到扫描到的最后一个）
    #[arg(long)]
    pub stop_file: Option<String>,
    /// 结束位点
    #[arg(long)]
    pub stop_pos: Option<u32>,
    /// 起始时间 "YYYY-MM-DD HH:MM:SS"
    #[arg(long)]
    pub start_datetime: Option<String>,
    /// 结束时间 "YYYY-MM-DD HH:MM:SS"
    #[arg(long)]
    pub stop_datetime: Option<String>,
    /// 库白名单，逗号分隔
    #[arg(long, value_delimiter = ',')]
    pub db: Vec<String>,
    /// 表白名单 db.table，逗号分隔
    #[arg(long, value_delimiter = ',')]
    pub table: Vec<String>,
    /// 库黑名单，逗号分隔
    #[arg(long, value_delimiter = ',')]
    pub ignore_db: Vec<String>,
    /// 表黑名单 db.table，逗号分隔
    #[arg(long, value_delimiter = ',')]
    pub ignore_table: Vec<String>,
    /// 只处理指定行类型（空 = 全部），逗号分隔
    #[arg(long, value_delimiter = ',')]
    pub dml: Vec<Dml>,
    /// 表结构来源：mysql://user:pass@host:port
    #[arg(long)]
    pub uri: Option<String>,
    /// 表结构来源：离线 schema 文件（mysqldump --no-data）
    #[arg(long)]
    pub schema_file: Option<PathBuf>,
    /// 解析完成后把表结构导出到该文件
    #[arg(long)]
    pub schema_dump: Option<PathBuf>,
    /// SQL 输出目录
    #[arg(long)]
    pub output_dir: Option<PathBuf>,
    /// 输出到标准输出
    #[arg(long)]
    pub to_stdout: bool,
    /// 每张表一个输出文件
    #[arg(long)]
    pub file_per_table: bool,
    /// 在 SQL 前后附加位置/时间注释
    #[arg(long)]
    pub add_extra_info: bool,
    /// 输出 SQL 不带库名前缀
    #[arg(long)]
    pub no_db_prefix: bool,
    /// INSERT 输出全列、UPDATE 输出全列（默认只输出被改列）
    #[arg(long)]
    pub full_columns: bool,
    /// UPDATE 的 WHERE 优先用唯一键
    #[arg(long)]
    pub unique_key_first: bool,
    /// INSERT 时忽略主键列
    #[arg(long)]
    pub ignore_primary_key_for_insert: bool,
    /// 表结构与 binlog 不匹配时直接报错（默认跳过并告警）
    #[arg(long)]
    pub strict_schema: bool,
    /// 多条 INSERT 合并为批量 INSERT 的最大行数
    #[arg(long)]
    pub insert_batch: Option<usize>,
    /// binlog 时间戳解释时区，如 +08:00 或 SYSTEM
    #[arg(long)]
    pub time_zone: Option<String>,
    /// 并行线程数
    #[arg(long, default_value_t = thread::available_parallelism().map(|n| n.get()).unwrap_or(8))]
    pub threads: usize,
}

/// 校验并归一化后的运行配置。后续所有任务从这里取参数。
#[derive(Debug, Clone)]
pub struct Config {
    pub binlog_dir: PathBuf,
    pub start_file: String,
    pub start_pos: u32,
    pub stop_file: Option<String>,
    pub stop_pos: Option<u32>,
    pub start_datetime: Option<DateTime<FixedOffset>>,
    pub stop_datetime: Option<DateTime<FixedOffset>>,
    pub db: Vec<String>,
    pub table: Vec<String>,
    pub ignore_db: Vec<String>,
    pub ignore_table: Vec<String>,
    /// 空 = 全部 DML
    pub dml: Vec<Dml>,
    pub uri: Option<String>,
    pub schema_file: Option<PathBuf>,
    pub schema_dump: Option<PathBuf>,
    pub output_dir: Option<PathBuf>,
    pub to_stdout: bool,
    pub file_per_table: bool,
    pub add_extra_info: bool,
    pub no_db_prefix: bool,
    pub full_columns: bool,
    pub unique_key_first: bool,
    pub ignore_primary_key_for_insert: bool,
    pub strict_schema: bool,
    pub insert_batch: Option<usize>,
    /// --time-zone 解析结果（默认 UTC+00）
    pub time_zone: FixedOffset,
    pub threads: usize,
    /// P2 T3：工作模式（`validate(ToSqlArgs)` 恒 `ToSql`；flashback 测试/
    /// T5 子命令在库层覆写）。
    pub work_type: WorkType,
    /// 回滚脚本逐事务注入 `commit;\nbegin;\n`（上游 rollback_process.go 口径，
    /// P2 T3；to-sql 默认 true 无消费）。
    pub keep_trx: bool,
    /// 逐事件错误策略（P2 T3；to-sql 恒默认 `SkipBadEvent` = P1 行为）。
    pub on_error: OnError,
    /// stats 窗口落盘间隔秒（P2 T4；上游 PrintInterval 默认 30、范围 1..600
    /// 的校验归 T5 `validate_stats`，此处仅字段+中性默认）。
    pub print_interval: u32,
    /// 大事务行数阈值（上游 BigTrxRowLimit 默认 10、范围 1..30000）。
    pub big_trx_rows: u32,
    /// 长事务秒阈值（上游 LongTrxSeconds 默认 1、范围 0..3600）。
    pub long_trx_seconds: u32,
    /// `--stats-json`：两份报表同步输出 JSONL 版（P2 T4 消费；默认 false）。
    pub stats_json: bool,
}

/// 解析 `--time-zone`：支持 "+08:00"/"-06:00" 数字偏移、UTC、SYSTEM（本机时区）。
fn parse_time_zone(raw: Option<&str>) -> Result<FixedOffset, String> {
    let Some(s) = raw else {
        return Ok(FixedOffset::east_opt(0).expect("zero offset is valid"));
    };
    match s.to_ascii_uppercase().as_str() {
        "UTC" | "GMT" | "Z" => Ok(FixedOffset::east_opt(0).expect("zero offset is valid")),
        "SYSTEM" | "LOCAL" => Ok(*Local::now().offset()),
        other => FixedOffset::from_str(other)
            .map_err(|_| format!("invalid --time-zone {raw:?} (want +HH:MM, SYSTEM or UTC)")),
    }
}

/// 按 "YYYY-MM-DD HH:MM:SS" 在给定偏移下解析时间戳。
fn parse_datetime(raw: &str, tz: FixedOffset) -> Result<DateTime<FixedOffset>, String> {
    let naive = NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S")
        .map_err(|_| format!(r#"invalid datetime {raw:?} (want "YYYY-MM-DD HH:MM:SS")"#))?;
    Ok(naive
        .and_local_timezone(tz)
        .single()
        .expect("fixed offset has no ambiguity"))
}

impl Config {
    /// 解析命令行 → 校验 → 产出 `Config`。**进程级失败出口唯一**：校验错误
    /// 打印 `error: …` 并 `exit(2)`（T14 Step-0 重构：`validate` 本身返回
    /// `Result`，退出决策留在这里，测试不再被 `die` 连坐）。
    pub fn from_args() -> Config {
        let cli = Cli::parse();
        match cli.cmd {
            Command::ToSql(args) => Config::validate(args).unwrap_or_else(|e| {
                eprintln!("error: {e}");
                exit(2)
            }),
        }
    }

    /// 校验并归一化 `to-sql` 参数 → `Config`；失败返回人类可读错误串
    /// （由调用方决定展示/退出——`from_args` 走 `exit(2)`，测试直接 `unwrap_err`）。
    pub fn validate(args: ToSqlArgs) -> Result<Config, String> {
        if args.threads == 0 {
            return Err("--threads must be >= 1".into());
        }
        if args.uri.is_none() && args.schema_file.is_none() {
            return Err("table schema source required: pass --uri or --schema-file".into());
        }
        let time_zone = parse_time_zone(args.time_zone.as_deref())?;
        let start_datetime = args
            .start_datetime
            .as_deref()
            .map(|s| parse_datetime(s, time_zone))
            .transpose()?;
        let stop_datetime = args
            .stop_datetime
            .as_deref()
            .map(|s| parse_datetime(s, time_zone))
            .transpose()?;
        if let (Some(s), Some(e)) = (start_datetime, stop_datetime)
            && s >= e
        {
            return Err(format!(
                "start_datetime ({s}) must be earlier than stop_datetime ({e})"
            ));
        }
        // 结束位点只在同一文件的结束条件下才有可比性；
        // stop_file 缺省或与 start_file 同名时要求 stop_pos > start_pos。
        if let Some(stop_pos) = args.stop_pos {
            let same_file = args
                .stop_file
                .as_deref()
                .is_none_or(|f| f == args.start_file);
            if same_file && stop_pos <= args.start_pos {
                return Err(format!(
                    "stop_pos ({stop_pos}) must be greater than start_pos ({})",
                    args.start_pos
                ));
            }
        }
        Ok(Config {
            binlog_dir: args.binlog_dir,
            start_file: args.start_file,
            start_pos: args.start_pos,
            stop_file: args.stop_file,
            stop_pos: args.stop_pos,
            start_datetime,
            stop_datetime,
            db: args.db,
            table: args.table,
            ignore_db: args.ignore_db,
            ignore_table: args.ignore_table,
            dml: args.dml,
            uri: args.uri,
            schema_file: args.schema_file,
            schema_dump: args.schema_dump,
            output_dir: args.output_dir,
            to_stdout: args.to_stdout,
            file_per_table: args.file_per_table,
            add_extra_info: args.add_extra_info,
            no_db_prefix: args.no_db_prefix,
            full_columns: args.full_columns,
            unique_key_first: args.unique_key_first,
            ignore_primary_key_for_insert: args.ignore_primary_key_for_insert,
            strict_schema: args.strict_schema,
            insert_batch: args.insert_batch,
            time_zone,
            threads: args.threads,
            // P2 T3 默认：to-sql 恒 ToSql/keep_trx=true/Skip（robust-continue 不变）
            work_type: WorkType::ToSql,
            keep_trx: true,
            on_error: OnError::SkipBadEvent,
            // P2 T4 默认：stats 阈值取上游缺省（30/10/1），JSONL 关（T5 接 CLI）
            print_interval: 30,
            big_trx_rows: 10,
            long_trx_seconds: 1,
            stats_json: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn args(extra: &[&str]) -> ToSqlArgs {
        let mut v = vec![
            "my2sql-rs",
            "to-sql",
            "--binlog-dir",
            "/d",
            "--start-file",
            "f.000001",
        ];
        v.extend_from_slice(extra);
        let cli = Cli::try_parse_from(v).unwrap();
        match cli.cmd {
            Command::ToSql(a) => a,
        }
    }

    #[test]
    fn validate_returns_err_instead_of_killing_process() {
        // T14 Step-0 账载（HANDOVER T12「地雷」）：缺 schema 源必须是 Err 值，
        // 旧 die()→exit(2) 形态下本测试会杀掉整个测试进程。
        let e = Config::validate(args(&[])).unwrap_err();
        assert!(e.contains("schema source"), "{e}");
        assert!(
            Config::validate(args(&["--uri", "mysql://x@y"])).is_ok(),
            "带 --uri 应通过"
        );
        // 其余校验分支同样走 Result
        let e = Config::validate(args(&["--uri", "mysql://x@y", "--threads", "0"])).unwrap_err();
        assert!(e.contains("threads"), "{e}");
        let e = Config::validate(args(&["--uri", "mysql://x@y", "--time-zone", "Kathmandu"]))
            .unwrap_err();
        assert!(e.contains("time-zone"), "{e}");
        let e = Config::validate(args(&["--uri", "mysql://x@y", "--stop-pos", "3"])).unwrap_err();
        assert!(e.contains("stop_pos"), "{e}");
    }
}
