//! 事件过滤器：db/table 白黑名单、DML 类型、位点/时间窗口（Task 12）。
//!
//! ## 上游口径对照（my2sql-go，权威引用）
//!
//! - **位点窗口按「事件结束位置」比较**（非起始）：base/com.go
//!   `CheckBinHeaderCondition`/`CheckBinEvent` 均取 `myPos = Position{Name:
//!   currentBinlog, Pos: header.LogPos}`（事件尾），随后
//!   `myPos.Compare(StartFilePos) == -1 → continue`（尾部还没到 start 就跳过）、
//!   `myPos.Compare(StopFilePos) >= 0 → break`（尾部到达/超过 stop 即停，
//!   **等号也停**：恰好以 stop_pos 结束的事件被排除）。文件名次序 = **基础名
//!   字典序 + 末尾 `.` 后数字后缀按整数比较**（vendored go-mysql
//!   `mysql.Position.Compare`/`CompareBinlogFileName`，position.go:39-80——
//!   纯字典序在 999999→1000000 进位处失序，T12 审阅勘误、本层 T14 Step-0
//!   复刻，含空名特例）。本层照此实现（T15 差分口径）。
//! - **时间窗口**：com.go:63-74 —— `ts < start_dt → continue`；
//!   `ts >= stop_dt → break`（stop 等号排除）。unix 秒直接比较。
//! - **db/table 白黑名单只作用于 rows 事件**（com.go:119-140，QUERY/XID 等直接
//!   放行——BEGIN/COMMIT 信号必须无过滤地进入事务机）；DML 类型过滤同样仅
//!   针对 rows 事件（com.go:79-101）。
//! - **表名单语义差异（有意超集，记录在案）**：上游 `-tables` 明确
//!   "DONOT prefix with schema"（context.go:196，纯表名匹配、不含库名）；
//!   本工具 CLI（T1 重设计）`--table` 声明 `db.table`。折中：条目含 `.` →
//!   按 `db.table` 精确匹配；不含 `.` → 按表名匹配（= 上游行为）。
//!   差分测试给裁判喂不含 `.` 的条目即可保持上游等价。

use std::cmp::Ordering;

use crate::binlog::rows::RowsKind;
use crate::binlog::table_map::TableMapEvent;
use crate::config::{Config, Dml};
use crate::pipeline::source::{RawEvent, RawKind};

/// 位点比较：先文件名（[`binlog_name_cmp`]），同名再比偏移
/// （vendored mysql.Position.Compare，position.go:17-29 同构）。
fn pos_cmp(name: &str, pos: u32, bound: (&str, u32)) -> Ordering {
    binlog_name_cmp(name, bound.0).then_with(|| pos.cmp(&bound.1))
}

/// 复刻 vendored go-mysql `mysql.CompareBinlogFileName`（position.go:39-80）：
/// 基础名（最后一个 `.` 之前）字典序 + 尾部十进制后缀**按整数**比较——
/// 字典序在 `999999 → 1000000` 进位处给出反序（`"…9…"` > `"…1…"`），
/// 整数序修正之。空名特例照抄（双空=等、空<非空、非空>空）。
/// 与 go-mysql 的一处有意偏差：非数字后缀上游 `panic`（position.go:66-68），
/// 本层按无 `.` 分支同款回退 `(整名, 0)`（D5 立场：敌意输入报错/降级，不 panic）。
fn binlog_name_cmp(a: &str, b: &str) -> Ordering {
    if a.is_empty() || b.is_empty() {
        return match (a.is_empty(), b.is_empty()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => Ordering::Equal,
        };
    }
    let (a_base, a_seq) = split_binlog_name(a);
    let (b_base, b_seq) = split_binlog_name(b);
    a_base.cmp(b_base).then_with(|| a_seq.cmp(&b_seq))
}

/// `name[.seq]` 拆分：后缀必须**非空且全为 ASCII 数字**才按整数计
/// （Go `strconv.Atoi` 能吃 `+5`/`-5`/`1_2` 之类，binlog 文件名不会出现，
/// 本层以「全数字」口径避免 Rust `parse` 的下划线分隔宽容造成误判）；
/// 否则整名作基础名、序号 0（= go-mysql 无 `.` 兼容分支）。
fn split_binlog_name(n: &str) -> (&str, u64) {
    if let Some(i) = n.rfind('.')
        && let Some(suffix) = n.get(i + 1..)
        && !suffix.is_empty()
        && suffix.bytes().all(|c| c.is_ascii_digit())
    {
        // 全 ASCII 数字且来自 u32 时代位点上下文：长度封顶防御性取 0 兜底。
        if let Ok(seq) = suffix.parse::<u64>() {
            return (&n[..i], seq);
        }
    }
    (n, 0)
}

/// 事件过滤器集合（空列表 = 不过滤；`None` 窗口 = 未设）。
#[derive(Debug, Clone, Default)]
pub struct Filters {
    pub db: Vec<String>,
    pub table: Vec<String>,
    pub ignore_db: Vec<String>,
    pub ignore_table: Vec<String>,
    /// 空 = 全部 DML。
    pub dml: Vec<Dml>,
    /// (start_file, start_pos)：事件尾位点 < start → 未进入窗口。
    pub start: Option<(String, u32)>,
    /// (stop_file, stop_pos)：事件尾位点 >= stop → 停止（等号排除，上游口径）。
    pub stop: Option<(String, u32)>,
    pub start_ts: Option<u32>,
    pub stop_ts: Option<u32>,
}

impl Filters {
    /// 全放行（源单测/无过滤场景）。
    pub fn none() -> Self {
        Self::default()
    }

    /// 从 CLI `Config` 构造（字段即 config.rs 实际名，T1 重设计后口径）。
    ///
    /// stop 语义裁定：上游 `StopFilePos` 仅在 `-stop-file` 给出时生效
    /// （context.go:325-334，只给 stop-pos 不配 stop-file = 不设 stop）；
    /// 本工具 Config 校验已按「stop_file 缺省或与 start_file 同名要求
    /// stop_pos > start_pos」设计（config.rs:222-234），故采更宽松的
    /// 「任一给出即生效、文件名缺省回落 start_file」，单文件语义下与上游
    /// 等价且更符合本 CLI 文案。stop_file 给出而未给 stop_pos → 位点分量
    /// 取 u32::MAX（纯文件名截断，等价上游 pos 缺省 4 + 名次序主导）。
    pub fn from_config(cfg: &Config) -> Self {
        let to_ts = |d: Option<chrono::DateTime<chrono::FixedOffset>>| {
            d.map(|d| d.timestamp().max(0) as u32)
        };
        let stop = match (&cfg.stop_file, cfg.stop_pos) {
            (None, None) => None,
            (f, p) => Some((
                f.clone().unwrap_or_else(|| cfg.start_file.clone()),
                p.unwrap_or(u32::MAX),
            )),
        };
        Filters {
            db: cfg.db.clone(),
            table: cfg.table.clone(),
            ignore_db: cfg.ignore_db.clone(),
            ignore_table: cfg.ignore_table.clone(),
            dml: cfg.dml.clone(),
            start: Some((cfg.start_file.clone(), cfg.start_pos)),
            stop,
            start_ts: to_ts(cfg.start_datetime),
            stop_ts: to_ts(cfg.stop_datetime),
        }
    }

    /// 尾位点/时间是否已越过 stop 界（含等号 → true，上游 break 语义）。
    pub fn pos_stopped(&self, binlog: &str, end_pos: u32, ts: u32) -> bool {
        if let Some((n, p)) = self.stop.as_ref()
            && pos_cmp(binlog, end_pos, (n, *p)) != Ordering::Less
        {
            return true;
        }
        self.stop_ts.is_some_and(|t| ts >= t)
    }

    /// 尾位点/时间是否尚未到达 start 界（上游 continue 语义）。
    pub fn pos_pending(&self, binlog: &str, end_pos: u32, ts: u32) -> bool {
        if let Some((n, p)) = self.start.as_ref()
            && pos_cmp(binlog, end_pos, (n, *p)) == Ordering::Less
        {
            return true;
        }
        self.start_ts.is_some_and(|t| ts < t)
    }

    /// db/table 名单是否放行该表（仅 rows 事件调用，上游 com.go 口径）。
    /// 条目含 `.` → `db.table` 精确匹配；否则纯表名（上游语义）。
    pub fn table_ok(&self, db: &str, tb: &str) -> bool {
        if !self.db.is_empty() && !self.db.iter().any(|d| d == db) {
            return false;
        }
        if !self.table.is_empty() && !self.table.iter().any(|t| entry_match(t, db, tb)) {
            return false;
        }
        if self.ignore_db.iter().any(|d| d == db) {
            return false;
        }
        if self.ignore_table.iter().any(|t| entry_match(t, db, tb)) {
            return false;
        }
        true
    }

    /// 该 rows 种类是否启用（空 dml 列表 = 全部；原 `Config::dml_enabled`，T14 收口于此）。
    pub fn dml_ok(&self, kind: RowsKind) -> bool {
        self.dml.is_empty() || self.dml.contains(&Dml::from(kind))
    }

    /// 完整谓词（简报绑定 `accept(&RawEvent, &TableMapEvent?)`）：
    /// 窗口适用于一切事件；名单/DML 仅作用于 rows 事件（非 rows 放行，
    /// 保证事务机拿到无损的 BEGIN/XID 序列）。
    pub fn accept(&self, ev: &RawEvent, tm: Option<&TableMapEvent>) -> bool {
        if self.pos_pending(&ev.binlog, ev.end_pos, ev.timestamp)
            || self.pos_stopped(&ev.binlog, ev.end_pos, ev.timestamp)
        {
            return false;
        }
        match &ev.kind {
            RawKind::Rows(kind, _) => {
                if !self.dml_ok(*kind) {
                    return false;
                }
                // rows 必带 tm（源不变式）；缺失按 fail-closed 拒绝并交由
                // 上层告警（正常路径不可达：FileReader 对无 tm rows 硬错误）。
                tm.or(ev.tm.as_deref())
                    .is_some_and(|t| self.table_ok(&t.schema, &t.table))
            }
            _ => true,
        }
    }
}

/// `db.table` / 纯表名双形态条目匹配（见模块注释）。
fn entry_match(entry: &str, db: &str, tb: &str) -> bool {
    match entry.split_once('.') {
        Some((d, t)) => d == db && t == tb,
        None => entry == tb,
    }
}

impl From<RowsKind> for Dml {
    fn from(k: RowsKind) -> Self {
        match k {
            RowsKind::Write => Dml::Insert,
            RowsKind::Update => Dml::Update,
            RowsKind::Delete => Dml::Delete,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Cli, Command, Config, Dml};
    use clap::Parser;

    fn tm_of(db: &str, tb: &str) -> TableMapEvent {
        TableMapEvent {
            table_id: 1,
            schema: db.into(),
            table: tb.into(),
            n_cols: 1,
            column_type: vec![3],
            column_meta: vec![0],
            null_bits: vec![0],
            charset: vec![],
        }
    }

    fn rows_ev(kind: RowsKind, tm: &TableMapEvent, end: u32, ts: u32) -> RawEvent {
        RawEvent {
            binlog: "mysql-bin.000001".into(),
            start_pos: end - 50,
            end_pos: end,
            timestamp: ts,
            kind: RawKind::Rows(kind, true),
            body: vec![],
            tm: Some(Arc::new(tm.clone())),
        }
    }

    fn query_ev(sql: &str, end: u32) -> RawEvent {
        RawEvent {
            binlog: "mysql-bin.000001".into(),
            start_pos: end - 10,
            end_pos: end,
            timestamp: 0,
            kind: RawKind::Query(sql.into()),
            body: vec![],
            tm: None,
        }
    }

    use std::sync::Arc;

    // ---------- db/table 白黑名单 ----------

    #[test]
    fn db_whitelist_blacklist_only_gates_rows() {
        let f = Filters {
            db: vec!["t10".into()],
            ..Filters::none()
        };
        let tm = tm_of("t10", "u");
        assert!(f.accept(&rows_ev(RowsKind::Write, &tm, 100, 0), Some(&tm)));
        let other = tm_of("mysql", "user");
        assert!(!f.accept(&rows_ev(RowsKind::Write, &other, 100, 0), Some(&other)));
        // 非 rows 事件不受名单影响（BEGIN 必须透传给事务机）
        assert!(f.accept(&query_ev("BEGIN", 100), None));
        // 忽略 tm 参数覆盖？None 时回退 ev.tm
        assert!(f.accept(&rows_ev(RowsKind::Write, &tm, 100, 0), None));
    }

    #[test]
    fn blacklists_win_and_ignore_db() {
        let f = Filters {
            ignore_db: vec!["mysql".into()],
            ignore_table: vec!["t10.secret".into()],
            ..Filters::none()
        };
        assert!(!f.table_ok("mysql", "user"));
        assert!(!f.table_ok("t10", "secret"));
        assert!(f.table_ok("t10", "u"));
    }

    #[test]
    fn table_entries_support_plain_and_qualified_forms() {
        // 含 '.' → db.table 精确；不含 '.' → 纯表名（上游 -tables 语义）
        let f = Filters {
            table: vec!["t10.u".into(), "v".into()],
            ..Filters::none()
        };
        assert!(f.table_ok("t10", "u"));
        assert!(!f.table_ok("other", "u")); // 限定条目不匹配跨库同名表
        assert!(f.table_ok("any", "v"));
        assert!(!f.table_ok("t10", "w"));
    }

    // ---------- DML 类型 ----------

    #[test]
    fn dml_filter_maps_rowskind_and_empty_means_all() {
        let f = Filters {
            dml: vec![Dml::Insert],
            ..Filters::none()
        };
        let tm = tm_of("t10", "u");
        assert!(f.dml_ok(RowsKind::Write));
        assert!(!f.dml_ok(RowsKind::Update));
        assert!(!f.dml_ok(RowsKind::Delete));
        assert!(!f.accept(&rows_ev(RowsKind::Update, &tm, 100, 0), Some(&tm)));
        let all = Filters::none();
        assert!(all.dml_ok(RowsKind::Delete));
    }

    // ---------- 位点窗口（end_pos 口径 + 等号边界，上游引用见模块注释） ----------

    #[test]
    fn pos_window_uses_end_pos_with_upstream_boundaries() {
        // start=(F,100)：尾位点 99 → pending；100 → 进入（>= start 保留）
        let f = Filters {
            start: Some(("F".into(), 100)),
            ..Filters::none()
        };
        assert!(f.pos_pending("F", 99, 0));
        assert!(!f.pos_pending("F", 100, 0));
        // stop=(F,200)：尾位点 199 → 未停；200 → 停（>= stop 排除，等号也排除）
        let f = Filters {
            stop: Some(("F".into(), 200)),
            ..Filters::none()
        };
        assert!(!f.pos_stopped("F", 199, 0));
        assert!(f.pos_stopped("F", 200, 0));
        // 文件名次序主导：更早文件的尾位点再大也 pending；更晚文件直接 stopped
        let f = Filters {
            start: Some(("mysql-bin.000002".into(), 500)),
            stop: Some(("mysql-bin.000002".into(), 900)),
            ..Filters::none()
        };
        assert!(f.pos_pending("mysql-bin.000001", u32::MAX, 0));
        assert!(f.pos_stopped("mysql-bin.000003", 4, 0));
        // accept 粒度：窗口截断
        let tm = tm_of("t10", "u");
        let f = Filters {
            stop: Some(("mysql-bin.000001".into(), 100)),
            ..Filters::none()
        };
        assert!(!f.accept(&rows_ev(RowsKind::Write, &tm, 100, 0), Some(&tm)));
        assert!(f.accept(&rows_ev(RowsKind::Write, &tm, 99, 0), Some(&tm)));
    }

    // ---------- 时间窗口 ----------

    #[test]
    fn time_window_boundaries() {
        // start_ts=1000：ts 999 pending；1000 进入。stop_ts=2000：1999 未停；2000 停。
        let f = Filters {
            start_ts: Some(1000),
            ..Filters::none()
        };
        assert!(f.pos_pending("F", u32::MAX, 999));
        assert!(!f.pos_pending("F", u32::MAX, 1000));
        let f = Filters {
            stop_ts: Some(2000),
            ..Filters::none()
        };
        assert!(!f.pos_stopped("F", u32::MAX, 1999));
        assert!(f.pos_stopped("F", u32::MAX, 2000));
    }

    // ---------- 文件名整数后缀序（T14 Step-0 账载：position.go:39-80 复刻） ----------

    #[test]
    fn binlog_name_cmp_uses_numeric_suffix_not_lexicographic() {
        use crate::binlog::file_reader::FileReader;
        type F = FileReader<std::fs::File>;
        let lo = "mysql-bin.999999";
        // next_binlog_name 的 %06d 进位产物（7 位）：字典序判它「更小」，整数序必须判「更大」
        let hi = F::next_binlog_name(lo).unwrap();
        assert_eq!(hi, "mysql-bin.1000000");
        assert_eq!(pos_cmp(&hi, 4, (lo, 4)), Ordering::Greater);
        assert_eq!(pos_cmp(lo, u32::MAX, (hi.as_str(), 4)), Ordering::Less);
        // 前导零同一整数（000010 == 9+1 的下一档）
        assert_eq!(pos_cmp("x.000010", 4, ("x.000009", 4)), Ordering::Greater);
        // ""-name 特例（position.go:41-47：双空等、空为最小）
        assert_eq!(pos_cmp("", 0, ("", 0)), Ordering::Equal);
        assert_eq!(
            pos_cmp("", u32::MAX, ("mysql-bin.000001", 4)),
            Ordering::Less
        );
        assert_eq!(
            pos_cmp("mysql-bin.000001", 4, ("", u32::MAX)),
            Ordering::Greater
        );
        // 非数字后缀：上游 panic，本层回退 (整名,0)（无 '.' 分支同款，注释已录）
        assert_eq!(pos_cmp("abcd", 5, ("abcd", 4)), Ordering::Greater);
        assert_eq!(pos_cmp("a.b", 4, ("a", u32::MAX)), Ordering::Greater); // base "a.b"(seq0) > "a"
        // 基础名不同 → 后缀不越权
        assert_eq!(
            pos_cmp("mysql-bin.000001", 4, ("mysqld.999999", 4)),
            Ordering::Less
        );
        // 接受面（accept 粒度）：跨 999999→1000000 进位窗口不误判 pending
        let f = Filters {
            start: Some((lo.into(), 100)),
            ..Filters::none()
        };
        assert!(!f.pos_pending(&hi, 200, 0), "字典序误判会在此 RED");
        assert!(!f.pos_pending(lo, 200, 0));
        let g = Filters {
            stop: Some((hi, 100)),
            ..Filters::none()
        };
        assert!(!g.pos_stopped(lo, 200, 0));
    }

    // ---------- Config 映射 ----------

    #[test]
    fn from_config_maps_real_cli_fields() {
        let cli = Cli::try_parse_from([
            "my2sql-rs",
            "to-sql",
            "--binlog-dir",
            "/var/lib/mysql",
            "--start-file",
            "mysql-bin.000007",
            "--start-pos",
            "300",
            "--stop-file",
            "mysql-bin.000009",
            "--stop-pos",
            "800",
            "--db",
            "t10",
            "--table",
            "t10.u",
            "--ignore-db",
            "mysql",
            "--dml",
            "insert,update",
            "--uri",
            "mysql://x@y",
        ])
        .unwrap();
        let Command::ToSql(args) = cli.cmd else {
            panic!("expects to-sql")
        };
        let cfg = Config::validate_to_sql(args).unwrap();
        let f = Filters::from_config(&cfg);
        assert_eq!(
            f.start.as_ref().map(|(n, p)| (n.as_str(), *p)),
            Some(("mysql-bin.000007", 300))
        );
        assert_eq!(
            f.stop.as_ref().map(|(n, p)| (n.as_str(), *p)),
            Some(("mysql-bin.000009", 800))
        );
        assert_eq!(f.db, vec!["t10".to_string()]);
        assert_eq!(f.table, vec!["t10.u".to_string()]);
        assert_eq!(f.ignore_db, vec!["mysql".to_string()]);
        assert_eq!(f.dml, vec![Dml::Insert, Dml::Update]);
        assert_eq!(f.start_ts, None);
        assert_eq!(f.stop_ts, None);
    }

    #[test]
    fn from_config_stop_pos_without_stop_file_falls_back_to_start_file() {
        // 上游：stop-file 缺省时 StopFilePos 不生效；本工具 CLI 语义：
        // stop_pos 单独给出 = 在 start_file 内截断（Config 校验已按同文件口径约束）。
        let cli = Cli::try_parse_from([
            "my2sql-rs",
            "to-sql",
            "--binlog-dir",
            "/d",
            "--start-file",
            "mysql-bin.000002",
            "--stop-pos",
            "5000",
            "--uri",
            "mysql://x@y",
        ])
        .unwrap();
        let Command::ToSql(args) = cli.cmd else {
            panic!("expects to-sql")
        };
        let cfg = Config::validate_to_sql(args).unwrap();
        let f = Filters::from_config(&cfg);
        assert_eq!(f.stop, Some(("mysql-bin.000002".to_string(), 5000)));
    }

    #[test]
    fn from_config_datetime_converts_to_unix_seconds() {
        let cli = Cli::try_parse_from([
            "my2sql-rs",
            "to-sql",
            "--binlog-dir",
            "/d",
            "--start-file",
            "mysql-bin.000002",
            "--stop-datetime",
            "2026-01-01 00:00:00",
            "--time-zone",
            "+00:00",
            "--uri",
            "mysql://x@y",
        ])
        .unwrap();
        let Command::ToSql(args) = cli.cmd else {
            panic!("expects to-sql")
        };
        let cfg = Config::validate_to_sql(args).unwrap();
        let f = Filters::from_config(&cfg);
        assert_eq!(f.stop_ts, Some(1767225600)); // 2026-01-01T00:00:00Z
    }
}
