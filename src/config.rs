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
/// （P1 robust-continue 既定默认）。P2 T5 起兼作 `--on-error` 的 CLI 取值面
/// （kebab-case：`stop` / `skip-bad-event`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
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
    /// 反向生成回滚 SQL（flashback：INSERT↔DELETE 翻转、UPDATE 反写；回滚脚本宁缺毋漏）
    Flashback(FlashbackArgs),
    /// binlog 统计分析：输出 binlog_status.txt / biglong_trx.txt 报表，不生成 SQL
    Stats(StatsArgs),
}

/// 三子命令共享的定位/过滤/schema 源/输出目录/并发旗标（P2 T5 flatten 重构；
/// **不含** `--to-stdout`——flashback/stats 的产物必须是盘上文件）。
#[derive(Args, Debug, Clone)]
pub struct CommonArgs {
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
    /// 每张表一个输出文件
    #[arg(long)]
    pub file_per_table: bool,
    /// binlog 时间戳解释时区，如 +08:00 或 SYSTEM
    #[arg(long)]
    pub time_zone: Option<String>,
    /// 并行线程数
    #[arg(long, default_value_t = thread::available_parallelism().map(|n| n.get()).unwrap_or(8))]
    pub threads: usize,
}

/// SQL 文本形态旗标（to-sql 与 flashback 共用——回滚脚本同为 SQL 文本）；
/// stats 子命令**不** flatten 本组（无 SQL 产物）。
#[derive(Args, Debug, Clone)]
pub struct SqlTextArgs {
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
}

#[derive(Args, Debug, Clone)]
pub struct ToSqlArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    #[command(flatten)]
    pub sql: SqlTextArgs,
    /// 输出到标准输出
    #[arg(long)]
    pub to_stdout: bool,
    /// 逐事件错误策略（仅 skip-bad-event = P1 robust-continue 行为不变；
    /// stop 不适用正向模式，validate 即拒——flashback/stats 专属）
    #[arg(long, value_enum, default_value_t = OnError::SkipBadEvent)]
    pub on_error: OnError,
}

#[derive(Args, Debug, Clone)]
pub struct FlashbackArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    #[command(flatten)]
    pub sql: SqlTextArgs,
    /// 回滚脚本逐事务注入 commit;begin;（默认开，上游 -keep-trx 口径；与 --no-keep-trx 的互斥在 validate_flashback 报错）
    #[arg(long)]
    pub keep_trx: bool,
    /// 关闭 keep-trx（回滚脚本保持原事务边界）
    #[arg(long)]
    pub no_keep_trx: bool,
    /// 逐事件错误策略（flashback 默认 stop：回滚脚本宁缺毋漏）
    #[arg(long, value_enum, default_value_t = OnError::Stop)]
    pub on_error: OnError,
}

#[derive(Args, Debug, Clone)]
pub struct StatsArgs {
    #[command(flatten)]
    pub common: CommonArgs,
    /// 统计窗口落盘间隔秒（有效范围 1..=600，校验在 validate_stats）
    #[arg(long, default_value_t = 30)]
    pub print_interval: u32,
    /// 大事务行数阈值（有效范围 1..=30000，校验在 validate_stats）
    #[arg(long, default_value_t = 10)]
    pub big_trx_rows: u32,
    /// 长事务秒阈值（有效范围 0..=3600，校验在 validate_stats）
    #[arg(long, default_value_t = 1)]
    pub long_trx_seconds: u32,
    /// 两份报表同步输出 JSONL 版（binlog_status.jsonl / biglong_trx.jsonl）
    #[arg(long)]
    pub stats_json: bool,
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
    /// P2 T3：工作模式（T5 起由 `validate_*` 三臂按子命令填写）。
    pub work_type: WorkType,
    /// 回滚脚本逐事务注入 `commit;\nbegin;\n`（上游 rollback_process.go 口径，
    /// P2 T3；to-sql 默认 true 无消费）。
    pub keep_trx: bool,
    /// 逐事件错误策略（P2 T3；to-sql 默认 `SkipBadEvent` = P1 行为，
    /// flashback 默认 `Stop`，stats 恒默认 `SkipBadEvent`——T5 定旗标面）。
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
    /// 解析命令行 → 按子命令分派校验 → 产出 `Config`。**进程级失败出口唯一**：
    /// 校验错误打印 `error: …` 并 `exit(2)`（T14 Step-0 重构：`validate_*` 本身
    /// 返回 `Result`，退出决策留在这里，测试不再被 `die` 连坐）。
    pub fn from_args() -> Config {
        let cli = Cli::parse();
        let cfg = match cli.cmd {
            Command::ToSql(args) => Config::validate_to_sql(args),
            Command::Flashback(args) => Config::validate_flashback(args),
            Command::Stats(args) => Config::validate_stats(args),
        };
        cfg.unwrap_or_else(|e| {
            eprintln!("error: {e}");
            exit(2)
        })
    }

    /// 校验并归一化 `to-sql` 参数 → `Config`；失败返回人类可读错误串
    /// （由调用方决定展示/退出——`from_args` 走 `exit(2)`，测试直接 `unwrap_err`）。
    /// P2 T5：由 `validate` 直重命名而来，全部既有调用点同步改名。
    pub fn validate_to_sql(args: ToSqlArgs) -> Result<Config, String> {
        // to-sql 流水线恒 best-effort（spec §3.5：坏事件计数跳过，P1 字节面
        // 由 e2e 守卫），Stop 无消费点——受理即静默无效，validate 期拒绝
        // （P2 T5 review 裁定；急停语义仅 flashback/stats 提供）。
        if args.on_error == OnError::Stop {
            return Err(
                "--on-error stop is flashback/stats-only; to-sql is best-effort by design".into(),
            );
        }
        let mut cfg = build_common(&args.common)?;
        apply_sql(&mut cfg, &args.sql);
        cfg.to_stdout = args.to_stdout;
        // to-sql 恒 ToSql；on_error 来自旗标（默认 SkipBadEvent = P1 行为），
        // keep_trx 为无消费中性默认（P2 T3 注记）。
        cfg.work_type = WorkType::ToSql;
        cfg.keep_trx = true;
        cfg.on_error = args.on_error;
        Ok(cfg)
    }

    /// 校验并归一化 `flashback` 参数 → `Config`。默认 `on_error=stop`（回滚
    /// 脚本宁缺毋漏）、`keep_trx=true`（上游 -keep-trx 口径）；
    /// `--keep-trx`/`--no-keep-trx` 同给为语义互斥 → Err（clap 面可 parse，
    /// 冲突在此判定——简报钉死）。`--to-stdout` 不存在于本子命令（parse 即拒）。
    pub fn validate_flashback(args: FlashbackArgs) -> Result<Config, String> {
        if args.keep_trx && args.no_keep_trx {
            return Err("--keep-trx conflicts with --no-keep-trx (pick one)".into());
        }
        let mut cfg = build_common(&args.common)?;
        apply_sql(&mut cfg, &args.sql);
        cfg.to_stdout = false;
        cfg.work_type = WorkType::Flashback;
        cfg.keep_trx = !args.no_keep_trx;
        cfg.on_error = args.on_error;
        Ok(cfg)
    }

    /// 校验并归一化 `stats` 参数 → `Config`。阈值范围校验在此（clap 只做
    /// u32 类型面）：`print_interval 1..=600`、`big_trx_rows 1..=30000`、
    /// `long_trx_seconds 0..=3600`（上游 stats_process 口径）。on_error 恒
    /// `SkipBadEvent`（分析工具语义，无旗标）；无 SQL 产物，SqlTextArgs 组
    /// 不存在（parse 即拒），Config 侧落中性默认。
    pub fn validate_stats(args: StatsArgs) -> Result<Config, String> {
        if !(1..=600).contains(&args.print_interval) {
            return Err(format!(
                "--print-interval must be in 1..=600, got {}",
                args.print_interval
            ));
        }
        if !(1..=30000).contains(&args.big_trx_rows) {
            return Err(format!(
                "--big-trx-rows must be in 1..=30000, got {}",
                args.big_trx_rows
            ));
        }
        if args.long_trx_seconds > 3600 {
            return Err(format!(
                "--long-trx-seconds must be in 0..=3600, got {}",
                args.long_trx_seconds
            ));
        }
        let mut cfg = build_common(&args.common)?;
        cfg.to_stdout = false;
        cfg.work_type = WorkType::Stats;
        cfg.keep_trx = true;
        cfg.on_error = OnError::SkipBadEvent;
        cfg.print_interval = args.print_interval;
        cfg.big_trx_rows = args.big_trx_rows;
        cfg.long_trx_seconds = args.long_trx_seconds;
        cfg.stats_json = args.stats_json;
        Ok(cfg)
    }
}

/// 把共享的 SQL 文本旗标组写入 `Config`（to-sql/flashback 共用；stats 不调）。
fn apply_sql(cfg: &mut Config, s: &SqlTextArgs) {
    cfg.add_extra_info = s.add_extra_info;
    cfg.no_db_prefix = s.no_db_prefix;
    cfg.full_columns = s.full_columns;
    cfg.unique_key_first = s.unique_key_first;
    cfg.ignore_primary_key_for_insert = s.ignore_primary_key_for_insert;
    cfg.strict_schema = s.strict_schema;
    cfg.insert_batch = s.insert_batch;
}

/// 三子命令共享的校验内核（现 `validate` 的 threads/schema 源/时区/时间对/
/// 位点逻辑原样搬入，P2 T5）：产出已填 `CommonArgs` 字段的 `Config`，
/// SQL 文本组与 work_type/on_error/keep_trx/stats 阈值由调用方覆写。
fn build_common(args: &CommonArgs) -> Result<Config, String> {
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
        binlog_dir: args.binlog_dir.clone(),
        start_file: args.start_file.clone(),
        start_pos: args.start_pos,
        stop_file: args.stop_file.clone(),
        stop_pos: args.stop_pos,
        start_datetime,
        stop_datetime,
        db: args.db.clone(),
        table: args.table.clone(),
        ignore_db: args.ignore_db.clone(),
        ignore_table: args.ignore_table.clone(),
        dml: args.dml.clone(),
        uri: args.uri.clone(),
        schema_file: args.schema_file.clone(),
        schema_dump: args.schema_dump.clone(),
        output_dir: args.output_dir.clone(),
        // 输出目标/文本形态中性默认：validate_* 各自覆写（stats 恒 false）
        to_stdout: false,
        file_per_table: args.file_per_table,
        add_extra_info: false,
        no_db_prefix: false,
        full_columns: false,
        unique_key_first: false,
        ignore_primary_key_for_insert: false,
        strict_schema: false,
        insert_batch: None,
        time_zone,
        threads: args.threads,
        // 默认按 to-sql 口径（P2 T3 注记：robust-continue 不变）；
        // flashback/stats 在各自 validate_* 覆写。
        work_type: WorkType::ToSql,
        keep_trx: true,
        on_error: OnError::SkipBadEvent,
        // stats 阈值取上游缺省（30/10/1），JSONL 关；validate_stats 覆写
        print_interval: 30,
        big_trx_rows: 10,
        long_trx_seconds: 1,
        stats_json: false,
    })
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
        let Command::ToSql(a) = cli.cmd else {
            panic!("to-sql only")
        };
        a
    }

    fn fargs(extra: &[&str]) -> FlashbackArgs {
        let mut v = vec![
            "my2sql-rs",
            "flashback",
            "--binlog-dir",
            "/d",
            "--start-file",
            "f.000001",
            "--schema-file",
            "/s",
        ];
        v.extend_from_slice(extra);
        let cli = Cli::try_parse_from(v).unwrap();
        let Command::Flashback(a) = cli.cmd else {
            panic!("flashback only")
        };
        a
    }

    fn sargs(extra: &[&str]) -> StatsArgs {
        let mut v = vec![
            "my2sql-rs",
            "stats",
            "--binlog-dir",
            "/d",
            "--start-file",
            "f.000001",
            "--schema-file",
            "/s",
        ];
        v.extend_from_slice(extra);
        let cli = Cli::try_parse_from(v).unwrap();
        let Command::Stats(a) = cli.cmd else {
            panic!("stats only")
        };
        a
    }

    #[test]
    fn flashback_defaults_and_flags() {
        let c = Config::validate_flashback(fargs(&[])).unwrap();
        assert_eq!(
            (c.work_type, c.on_error, c.keep_trx),
            (WorkType::Flashback, OnError::Stop, true)
        );
        let c = Config::validate_flashback(fargs(&["--no-keep-trx"])).unwrap();
        assert!(!c.keep_trx);
        let c = Config::validate_flashback(fargs(&["--on-error", "skip-bad-event"])).unwrap();
        assert_eq!(c.on_error, OnError::SkipBadEvent);
        // --to-stdout 不存在于 flashback：try_parse 必失败（逆序回写需要盘上文件）
        assert!(
            Cli::try_parse_from([
                "x",
                "flashback",
                "--binlog-dir",
                "/d",
                "--start-file",
                "f",
                "--to-stdout"
            ])
            .is_err()
        );
        // 双 bool 互斥在 validate_flashback 报错（clap 面可同 parse，语义互斥）
        assert!(Config::validate_flashback(fargs(&["--keep-trx", "--no-keep-trx"])).is_err());
    }

    #[test]
    fn stats_flags_and_ranges() {
        let c = Config::validate_stats(sargs(&[])).unwrap();
        assert_eq!(
            (
                c.work_type,
                c.on_error,
                c.print_interval,
                c.big_trx_rows,
                c.long_trx_seconds
            ),
            (WorkType::Stats, OnError::SkipBadEvent, 30, 10, 1)
        );
        assert!(Config::validate_stats(sargs(&["--print-interval", "601"])).is_err());
        assert!(Config::validate_stats(sargs(&["--print-interval", "0"])).is_err());
        assert!(Config::validate_stats(sargs(&["--big-trx-rows", "30001"])).is_err());
        assert!(Config::validate_stats(sargs(&["--big-trx-rows", "0"])).is_err());
        assert!(Config::validate_stats(sargs(&["--long-trx-seconds", "3601"])).is_err());
        assert_eq!(
            Config::validate_stats(sargs(&["--long-trx-seconds", "0"]))
                .unwrap()
                .long_trx_seconds,
            0
        );
        // SQL 文本旗标不存在于 stats：--full-columns try_parse 必失败
        assert!(
            Cli::try_parse_from([
                "x",
                "stats",
                "--binlog-dir",
                "/d",
                "--start-file",
                "f",
                "--full-columns"
            ])
            .is_err()
        );
        assert!(
            Config::validate_stats(sargs(&["--stats-json"]))
                .unwrap()
                .stats_json
        );
    }

    #[test]
    fn to_sql_gains_default_on_error_skip() {
        let c = Config::validate_to_sql(args(&["--uri", "mysql://x@y"])).unwrap();
        assert_eq!(
            (c.work_type, c.on_error),
            (WorkType::ToSql, OnError::SkipBadEvent)
        );
        assert!(c.keep_trx); // 无消费的中性默认
        // Review 裁定（P2 T5 fix）：to-sql 流水线恒 best-effort（spec §3.5），
        // Stop 在此无消费——受理即静默无效，validate 期直接拒绝。
        let e = Config::validate_to_sql(args(&["--uri", "mysql://x@y", "--on-error", "stop"]))
            .unwrap_err();
        assert!(e.contains("stop"), "{e}");
        assert!(e.contains("flashback"), "{e}");
    }

    #[test]
    fn validate_returns_err_instead_of_killing_process() {
        // T14 Step-0 账载（HANDOVER T12「地雷」）：缺 schema 源必须是 Err 值，
        // 旧 die()→exit(2) 形态下本测试会杀掉整个测试进程。
        let e = Config::validate_to_sql(args(&[])).unwrap_err();
        assert!(e.contains("schema source"), "{e}");
        assert!(
            Config::validate_to_sql(args(&["--uri", "mysql://x@y"])).is_ok(),
            "带 --uri 应通过"
        );
        // 其余校验分支同样走 Result
        let e =
            Config::validate_to_sql(args(&["--uri", "mysql://x@y", "--threads", "0"])).unwrap_err();
        assert!(e.contains("threads"), "{e}");
        let e =
            Config::validate_to_sql(args(&["--uri", "mysql://x@y", "--time-zone", "Kathmandu"]))
                .unwrap_err();
        assert!(e.contains("time-zone"), "{e}");
        let e = Config::validate_to_sql(args(&["--uri", "mysql://x@y", "--stop-pos", "3"]))
            .unwrap_err();
        assert!(e.contains("stop_pos"), "{e}");
    }
}
