//! stats 子系统（P2 T4）：轻量事实流 + 上游口径聚合器。
//!
//! 数据源是流水线的 `worker::Out::Fact` / `Out::Status` 保序流（dispatcher
//! 单线程消费，聚合非瓶颈），聚合算法逐行照抄上游
//! `reference/my2sql-go/base/stats_process.go:150-268`（只读权威）：
//!
//! - `binlog_status.txt`：窗口×表 行计数报表。头行在建文件时写入
//!   （context.go:530 口径，`GetStatsPrintHeaderLine` = 同宽 `%s` 版）；
//!   内容行 `%-17s %-19s %-19s %-10d %-10d %-8d %-8d %-8d %-15s %-20s\n`，
//!   datetime 为下划线形（= P1 `datetime_str`，constvar DATETIME_FORMAT_NOSPACE）。
//! - `biglong_trx.txt`：大/长事务命中行（`RowCnt >= big || Duration >= long`，
//!   `>=` 边界照抄 stats_process.go:201），内容行
//!   `%-17s %-19s %-19s %-10d %-10d %-8d %-10d %s\n`；明细
//!   `[db.tb(inserts=N, updates=N, deletes=N) …]` 单空格连接。
//! - 窗口落盘时机：`last_print_time==0 → ts+interval`；`ts >= last_print_time`
//!   全量落盘并重置 `ts+interval`；binlog 切换落盘清空；`finish` 落残余
//!   （上游 StatChan 关闭后的收尾循环，stats_process.go:262-265）。
//!
//! 对上游的有意超越（计划「超越项 3」登记）：Go map 随机序 → 本实现
//! 窗口行 = **首次出现序**、`[...]` 明细 = **db.tb 升序**（确定性输出）。

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use chrono::FixedOffset;

use crate::config::Config;
use crate::output::datetime_str;

/// 行事件种类（stats 口径 = 上游 QueryType 的 insert/update/delete 三支；
/// update 行数按**行对**计，stats_process.go:111 `rowCnt = len/2`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactKind {
    Insert,
    Update,
    Delete,
}

/// worker 上送的轻量行事实（P2 T4 简报钉死形态；`rows` = 已按对折算的行数）。
#[derive(Debug, Clone)]
pub struct StatFact {
    pub binlog: String,
    pub start_pos: u32,
    pub end_pos: u32,
    pub timestamp: u32,
    pub db: String,
    pub table: String,
    pub kind: FactKind,
    pub rows: u64,
    pub trx_id: u64,
}

/// dispatcher/worker 合流后的保序事件流（Aggregator 的唯一输入面，
/// 简报钉死形态）：行事实 + 三个事务标记（`pos`：Begin = 标记事件起始位，
/// Commit/Rollback = 结束位——上游 biglong 分别消费 StartPos/StopPos 两列，
/// 本接口以单 `pos` 按角色承载，字节口径与上游一致）。
#[derive(Debug)]
pub enum StreamEvent<'a> {
    Row(&'a StatFact),
    Begin {
        binlog: &'a str,
        pos: u32,
        ts: u32,
    },
    Commit {
        binlog: &'a str,
        pos: u32,
        ts: u32,
    },
    Rollback {
        binlog: &'a str,
        pos: u32,
        ts: u32,
    },
    /// 纯 tick 事件（P2 T9 B.4(b) 对齐）：上游对**所有**喂入 StatChan 的
    /// 事件逐件判定 interval tick 与 binlog 切换（file.go:274-281 +
    /// stats_process.go:247-257），其中非 begin/commit/rollback 的 QUERY
    /// （DDL/`use`/空文本 GTID 载体）既不入窗也不碰 biglong，但**会冲刷
    /// 窗口并重设锚点**。本变体即该语义的载体：只走 feed 的头部（切换
    /// 检查 + 锚点初始化）与尾部 tick 判定，match 主体为空操作。
    Tick {
        binlog: &'a str,
        ts: u32,
    },
}

/// `finish` 摘要：`windows` = 非空窗口落盘次数，`biglong` = 命中行数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StatsSummary {
    pub windows: u64,
    pub biglong: u64,
}

/// 上游 `GetStatsPrintHeaderLine`（context.go:530 建文件时写入）：
/// 同款宽度 `%s` 版（Go 源串 `"%-17s %-19s %-19s %-10s %-10s %-8s %-8s %-8s %-15s %-20s\n"`）。
const STATS_HEADER: &str = concat!(
    "binlog            starttime           stoptime            ",
    "startpos   stoppos    inserts  updates  deletes  database        table               \n",
);
/// 上游 `GetBigLongTrxPrintHeaderLine`（context.go:540；Go 源串
/// `"%-17s %-19s %-19s %-10s %-10s %-8s %-10s %s\n"`，尾列不补宽）。
const BIGLONG_HEADER: &str = concat!(
    "binlog            starttime           stoptime            ",
    "startpos   stoppos    rows     duration   tables\n",
);

/// 窗口累积行（上游 `BinEventStatsPrint`，同字段同语义）。
#[derive(Debug, Clone)]
struct WinRow {
    binlog: String,
    start_time: u32,
    stop_time: u32,
    start_pos: u32,
    stop_pos: u32,
    database: String,
    table: String,
    inserts: u64,
    updates: u64,
    deletes: u64,
}

/// JSONL 行（`--stats-json`；字段序 = serde 声明序 = 报表列序，钉死于 golden）。
#[derive(serde::Serialize)]
struct StatusJson<'a> {
    binlog: &'a str,
    starttime: String,
    stoptime: String,
    startpos: u32,
    stoppos: u32,
    inserts: u64,
    updates: u64,
    deletes: u64,
    database: &'a str,
    table: &'a str,
}

#[derive(serde::Serialize)]
struct TrxTableJson<'a> {
    table: &'a str,
    inserts: u64,
    updates: u64,
    deletes: u64,
}

#[derive(serde::Serialize)]
struct BigLongJson<'a> {
    binlog: &'a str,
    starttime: String,
    stoptime: String,
    startpos: u32,
    stoppos: u32,
    rows: u64,
    duration: u32,
    tables: Vec<TrxTableJson<'a>>,
}

/// 单线程聚合器（上游 `ProcessBinEventStats` stats_process.go:150-268 的
/// 逐行移植，消费侧置于 reorder 弹出流上保「事件序」前提成立）。
/// 报表文件在建文件时写头行（O_TRUNC 语义 = `File::create` 天然）。
pub struct Aggregator {
    interval: u32,
    big: u64,
    long: u32,
    tz: FixedOffset,
    stat: BufWriter<std::fs::File>,
    biglong: BufWriter<std::fs::File>,
    stat_json: Option<BufWriter<std::fs::File>>,
    biglong_json: Option<BufWriter<std::fs::File>>,
    /// 上游 lastPrintTime：0 = 未初始化（首事件置 ts+interval）。
    last_print_time: u32,
    last_binlog: String,
    /// 窗口 map（上游 statsPrintArr）：Vec 保**首次出现序** + HashMap 索引
    /// （超越项 3：Go map 随机序的确定性替代）。
    window: Vec<WinRow>,
    widx: HashMap<String, usize>,
    /// biglong 累加器（上游 oneBigLong）：Begin 重置；commit/rollback 且
    /// `start_time > 0`（事务内见过首个 row 事件）才判定；命中后**不**清零
    /// （下一 BEGIN 前持续累加——上游原样，含病态流语义）。
    bl_binlog: String,
    bl_start_pos: u32,
    bl_start_time: u32,
    bl_rows: u64,
    /// db.tb → [insert, update, delete]（BTreeMap = 打印升序，超越项 3）。
    bl_stmts: std::collections::BTreeMap<String, [u64; 3]>,
    windows: u64,
    biglong_hits: u64,
}

impl Aggregator {
    /// 建两份报表（+ 可选两份 JSONL）并写头行；`output_dir` 须已存在
    /// （装配层 run_stats 负责建目录）。
    pub fn new(cfg: &Config, output_dir: &Path) -> Result<Self, std::io::Error> {
        let mk = |name: &str| -> Result<BufWriter<File>, std::io::Error> {
            Ok(BufWriter::new(File::create(output_dir.join(name))?))
        };
        let mut stat = mk("binlog_status.txt")?;
        stat.write_all(STATS_HEADER.as_bytes())?;
        let mut biglong = mk("biglong_trx.txt")?;
        biglong.write_all(BIGLONG_HEADER.as_bytes())?;
        let (stat_json, biglong_json) = if cfg.stats_json {
            (
                Some(mk("binlog_status.jsonl")?),
                Some(mk("biglong_trx.jsonl")?),
            )
        } else {
            (None, None)
        };
        Ok(Self {
            interval: cfg.print_interval,
            big: cfg.big_trx_rows as u64,
            long: cfg.long_trx_seconds,
            tz: cfg.time_zone,
            stat,
            biglong,
            stat_json,
            biglong_json,
            last_print_time: 0,
            last_binlog: String::new(),
            window: Vec::new(),
            widx: HashMap::new(),
            bl_binlog: String::new(),
            bl_start_pos: 0,
            bl_start_time: 0,
            bl_rows: 0,
            bl_stmts: Default::default(),
            windows: 0,
            biglong_hits: 0,
        })
    }

    /// 喂一个保序事件（上游主循环单圈，顺序逐行对应 stats_process.go）。
    pub fn feed(&mut self, ev: &StreamEvent) -> std::io::Result<()> {
        let (binlog, ts) = match ev {
            StreamEvent::Row(f) => (f.binlog.as_str(), f.timestamp),
            StreamEvent::Begin { binlog, ts, .. }
            | StreamEvent::Commit { binlog, ts, .. }
            | StreamEvent::Rollback { binlog, ts, .. }
            | StreamEvent::Tick { binlog, ts } => (*binlog, *ts),
        };
        // binlog 切换：落盘清空 + 重置窗口钟（stats_process.go:169-178）。
        if self.last_binlog != binlog {
            self.flush_window()?;
            self.last_print_time = 0;
        }
        if self.last_print_time == 0 {
            self.last_print_time = ts.saturating_add(self.interval);
        }
        match *ev {
            StreamEvent::Begin { pos, .. } => {
                self.bl_binlog = binlog.to_string();
                self.bl_start_pos = pos;
                self.bl_start_time = 0;
                self.bl_rows = 0;
                self.bl_stmts.clear();
            }
            StreamEvent::Commit { pos, ts, .. } | StreamEvent::Rollback { pos, ts, .. } => {
                if self.bl_start_time > 0 {
                    // 上游 :200-201——duration = commit_ts - 事务内首个 row_ts；
                    // `>=` 边界照抄（rows>=big **或** dur>=long 即命中）。
                    let dur = ts.saturating_sub(self.bl_start_time);
                    if self.bl_rows >= self.big || dur >= self.long {
                        self.write_biglong(ts, pos, dur)?;
                    }
                }
            }
            StreamEvent::Row(f) => self.accumulate_row(f)?,
            // 纯 tick：不参与窗口/biglong 状态机（上游 query 分支对
            // 非三关键字文本即 no-op，仅尾部 tick 判定生效）。
            StreamEvent::Tick { .. } => {}
        }
        // 窗口 tick：当前事件**入窗后**落盘（上游 map 更新在判前）。
        if ts >= self.last_print_time {
            self.flush_window()?;
            self.last_print_time = ts.saturating_add(self.interval);
        }
        self.last_binlog = binlog.to_string();
        Ok(())
    }

    /// 收尾：残余窗口落盘 + 两 txt 报表尾注 `# skipped events: {N}`（简报裁定：
    /// N = RunSummary.errors，finish 收参；尾注仅写两 txt，jsonl 只冲刷）。
    pub fn finish(&mut self, skipped: u64) -> std::io::Result<StatsSummary> {
        self.flush_window()?;
        let tail = format!("# skipped events: {skipped}\n");
        self.stat.write_all(tail.as_bytes())?;
        self.biglong.write_all(tail.as_bytes())?;
        for w in [&mut self.stat, &mut self.biglong] {
            w.flush()?;
        }
        for w in [&mut self.stat_json, &mut self.biglong_json]
            .into_iter()
            .flatten()
        {
            w.flush()?;
        }
        Ok(StatsSummary {
            windows: self.windows,
            biglong: self.biglong_hits,
        })
    }

    /// 行事实 → biglong 累加器 + 窗口 map（上游 :207-245 双更新）。
    fn accumulate_row(&mut self, f: &StatFact) -> std::io::Result<()> {
        if self.bl_binlog.is_empty() {
            self.bl_binlog = f.binlog.clone();
        }
        if self.bl_start_pos == 0 {
            self.bl_start_pos = f.start_pos;
        }
        self.bl_rows += f.rows;
        let key = format!("{}.{}", f.db, f.table);
        let st = self.bl_stmts.entry(key.clone()).or_insert([0, 0, 0]);
        match f.kind {
            FactKind::Insert => st[0] += f.rows,
            FactKind::Update => st[1] += f.rows,
            FactKind::Delete => st[2] += f.rows,
        }
        if self.bl_start_time == 0 {
            self.bl_start_time = f.timestamp;
        }
        // 窗口行：首现建档（Start* = 本事件），复访仅推 Stop*/计数。
        let idx = match self.widx.get(&key) {
            Some(i) => *i,
            None => {
                self.window.push(WinRow {
                    binlog: f.binlog.clone(),
                    start_time: f.timestamp,
                    stop_time: f.timestamp,
                    start_pos: f.start_pos,
                    stop_pos: f.end_pos,
                    database: f.db.clone(),
                    table: f.table.clone(),
                    inserts: 0,
                    updates: 0,
                    deletes: 0,
                });
                let i = self.window.len() - 1;
                self.widx.insert(key.clone(), i);
                i
            }
        };
        let w = &mut self.window[idx];
        match f.kind {
            FactKind::Insert => w.inserts += f.rows,
            FactKind::Update => w.updates += f.rows,
            FactKind::Delete => w.deletes += f.rows,
        }
        w.stop_time = f.timestamp;
        w.stop_pos = f.end_pos;
        Ok(())
    }

    /// 窗口落盘（首现序）+ 清空；非空才计 `windows`（首轮空 tick 不计数，
    /// 钉死于 StatsRun Display 语义，登记报告）。
    fn flush_window(&mut self) -> std::io::Result<()> {
        if self.window.is_empty() {
            return Ok(());
        }
        self.windows += 1;
        for w in self.window.drain(..) {
            let line = format!(
                // Go: "%-17s %-19s %-19s %-10d %-10d %-8d %-8d %-8d %-15s %-20s\n"（stats_process.go:272）
                "{:<17} {:<19} {:<19} {:<10} {:<10} {:<8} {:<8} {:<8} {:<15} {:<20}\n",
                w.binlog,
                datetime_str(w.start_time, self.tz),
                datetime_str(w.stop_time, self.tz),
                w.start_pos,
                w.stop_pos,
                w.inserts,
                w.updates,
                w.deletes,
                w.database,
                w.table
            );
            self.stat.write_all(line.as_bytes())?;
            if let Some(j) = &mut self.stat_json {
                let s = serde_json::to_string(&StatusJson {
                    binlog: &w.binlog,
                    starttime: datetime_str(w.start_time, self.tz),
                    stoptime: datetime_str(w.stop_time, self.tz),
                    startpos: w.start_pos,
                    stoppos: w.stop_pos,
                    inserts: w.inserts,
                    updates: w.updates,
                    deletes: w.deletes,
                    database: &w.database,
                    table: &w.table,
                })
                .map_err(|e| std::io::Error::other(format!("status jsonl: {e}")))?;
                writeln!(j, "{s}")?;
            }
        }
        self.widx.clear();
        Ok(())
    }

    /// biglong 命中行（上游 :202 + GetBigLongTrxContentLine :278-285；
    /// 明细 `[...]` 单空格连接、db.tb 升序 = 超越项 3）。
    fn write_biglong(&mut self, stop_ts: u32, stop_pos: u32, dur: u32) -> std::io::Result<()> {
        self.biglong_hits += 1;
        let stmts: Vec<String> = self
            .bl_stmts
            .iter()
            .map(|(k, v)| {
                format!(
                    "{}(inserts={}, updates={}, deletes={})",
                    k, v[0], v[1], v[2]
                )
            })
            .collect();
        let line = format!(
            // Go: "%-17s %-19s %-19s %-10d %-10d %-8d %-10d %s\n"（stats_process.go:280）
            "{:<17} {:<19} {:<19} {:<10} {:<10} {:<8} {:<10} [{}]\n",
            self.bl_binlog,
            datetime_str(self.bl_start_time, self.tz),
            datetime_str(stop_ts, self.tz),
            self.bl_start_pos,
            stop_pos,
            self.bl_rows,
            dur,
            stmts.join(" ")
        );
        self.biglong.write_all(line.as_bytes())?;
        if let Some(j) = &mut self.biglong_json {
            let s = serde_json::to_string(&BigLongJson {
                binlog: &self.bl_binlog,
                starttime: datetime_str(self.bl_start_time, self.tz),
                stoptime: datetime_str(stop_ts, self.tz),
                startpos: self.bl_start_pos,
                stoppos: stop_pos,
                rows: self.bl_rows,
                duration: dur,
                tables: self
                    .bl_stmts
                    .iter()
                    .map(|(k, v)| TrxTableJson {
                        table: k,
                        inserts: v[0],
                        updates: v[1],
                        deletes: v[2],
                    })
                    .collect(),
            })
            .map_err(|e| std::io::Error::other(format!("biglong jsonl: {e}")))?;
            writeln!(j, "{s}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Cli, Command, Config};
    use clap::Parser;

    /// 场景基准 unix 秒（UTC 渲染 = `2023-11-14_22:13:20`，config 默认时区 +00:00）。
    const T0: u32 = 1700000000;
    const B1: &str = "mysql-bin.000001";
    const B2: &str = "mysql-bin.000002";

    /// 手推 golden（逐字节对照 stats_process.go:272/280 宽度模板；Go 源串
    /// `"%-17s %-19s %-19s %-10d %-10d %-8d %-8d %-8d %-15s %-20s\n"` 与
    /// `"%-17s %-19s %-19s %-10d %-10d %-8d %-10d %s\n"`；头行 = context.go:530/540
    /// 建文件时同款 `%s` 版）。位点/时间与 `tests/stats.rs` 的 Synth 真实布局
    /// 一致（跨层同一字符串钉死，Step 5）；行数宽度=十进制左对齐（`%-Nd`）。
    pub(crate) const STATUS_GOLDEN: &str = concat!(
        "binlog            starttime           stoptime            startpos   stoppos    inserts  updates  deletes  database        table               \n",
        "mysql-bin.000001  2023-11-14_22:13:20 2023-11-14_22:13:22 120        414        5        0        1        d               t2                  \n",
        "mysql-bin.000001  2023-11-14_22:13:20 2023-11-14_22:13:20 253        340        0        1        0        d               t1                  \n",
        "mysql-bin.000002  2023-11-14_22:13:25 2023-11-14_22:13:25 159        254        3        0        0        d               t1                  \n",
        "mysql-bin.000002  2023-11-14_22:13:29 2023-11-14_22:13:29 335        409        1        0        0        d               t3                  \n",
        "mysql-bin.000002  2023-11-14_22:13:29 2023-11-14_22:13:29 409        483        1        0        0        d               t2                  \n",
        "# skipped events: 2\n",
    );
    pub(crate) const BIGLONG_GOLDEN: &str = concat!(
        "binlog            starttime           stoptime            startpos   stoppos    rows     duration   tables\n",
        "mysql-bin.000001  2023-11-14_22:13:20 2023-11-14_22:13:23 214        441        2        3          [d.t1(inserts=0, updates=1, deletes=0) d.t2(inserts=0, updates=0, deletes=1)]\n",
        "mysql-bin.000002  2023-11-14_22:13:25 2023-11-14_22:13:25 120        296        3        0          [d.t1(inserts=3, updates=0, deletes=0)]\n",
        "# skipped events: 2\n",
    );
    const STATUS_JSONL_GOLDEN: &str = concat!(
        "{\"binlog\":\"mysql-bin.000001\",\"starttime\":\"2023-11-14_22:13:20\",\"stoptime\":\"2023-11-14_22:13:22\",\"startpos\":120,\"stoppos\":414,\"inserts\":5,\"updates\":0,\"deletes\":1,\"database\":\"d\",\"table\":\"t2\"}\n",
        "{\"binlog\":\"mysql-bin.000001\",\"starttime\":\"2023-11-14_22:13:20\",\"stoptime\":\"2023-11-14_22:13:20\",\"startpos\":253,\"stoppos\":340,\"inserts\":0,\"updates\":1,\"deletes\":0,\"database\":\"d\",\"table\":\"t1\"}\n",
        "{\"binlog\":\"mysql-bin.000002\",\"starttime\":\"2023-11-14_22:13:25\",\"stoptime\":\"2023-11-14_22:13:25\",\"startpos\":159,\"stoppos\":254,\"inserts\":3,\"updates\":0,\"deletes\":0,\"database\":\"d\",\"table\":\"t1\"}\n",
        "{\"binlog\":\"mysql-bin.000002\",\"starttime\":\"2023-11-14_22:13:29\",\"stoptime\":\"2023-11-14_22:13:29\",\"startpos\":335,\"stoppos\":409,\"inserts\":1,\"updates\":0,\"deletes\":0,\"database\":\"d\",\"table\":\"t3\"}\n",
        "{\"binlog\":\"mysql-bin.000002\",\"starttime\":\"2023-11-14_22:13:29\",\"stoptime\":\"2023-11-14_22:13:29\",\"startpos\":409,\"stoppos\":483,\"inserts\":1,\"updates\":0,\"deletes\":0,\"database\":\"d\",\"table\":\"t2\"}\n",
    );
    const BIGLONG_JSONL_GOLDEN: &str = concat!(
        "{\"binlog\":\"mysql-bin.000001\",\"starttime\":\"2023-11-14_22:13:20\",\"stoptime\":\"2023-11-14_22:13:23\",\"startpos\":214,\"stoppos\":441,\"rows\":2,\"duration\":3,\"tables\":[{\"table\":\"d.t1\",\"inserts\":0,\"updates\":1,\"deletes\":0},{\"table\":\"d.t2\",\"inserts\":0,\"updates\":0,\"deletes\":1}]}\n",
        "{\"binlog\":\"mysql-bin.000002\",\"starttime\":\"2023-11-14_22:13:25\",\"stoptime\":\"2023-11-14_22:13:25\",\"startpos\":120,\"stoppos\":296,\"rows\":3,\"duration\":0,\"tables\":[{\"table\":\"d.t1\",\"inserts\":3,\"updates\":0,\"deletes\":0}]}\n",
    );

    fn cfg(json: bool) -> Config {
        let cli = Cli::try_parse_from([
            "x",
            "to-sql",
            "--binlog-dir",
            "/d",
            "--start-file",
            "f.000001",
            "--schema-file",
            "/s.json",
        ])
        .unwrap();
        let mut c = match cli.cmd {
            Command::ToSql(a) => Config::validate_to_sql(a).unwrap(),
            _ => panic!("cfg expects to-sql"),
        };
        c.print_interval = 5;
        c.big_trx_rows = 3;
        c.long_trx_seconds = 1;
        c.stats_json = json;
        c
    }

    fn fact(
        binlog: &str,
        db: &str,
        table: &str,
        kind: FactKind,
        rows: u64,
        ts: u32,
        pos: (u32, u32),
    ) -> StatFact {
        StatFact {
            binlog: binlog.into(),
            start_pos: pos.0,
            end_pos: pos.1,
            timestamp: ts,
            db: db.into(),
            table: table.into(),
            kind,
            rows,
            trx_id: 0,
        }
    }

    fn out_dir(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "my2sql-p2t4-{tag}-{}.{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// 简报 Step 2 的 12 事件场景（上游 ProcessBinEventStats 控制流全支路）：
    /// E1 rows-before-BEGIN（5 insert 入零时累加器）→ E2 BEGIN 重置（**不判**）；
    /// E3 update 双行=1 对；E4 delete；E5 COMMIT → **long 命中**（dur 3>=1、
    /// rows 2<3）= L1；E6 BEGIN(B2) → **binlog 切换落盘**（窗口#1 首现序
    /// d.t2, d.t1）+ last_print 重置 T0+9；E7 insert×3；E8 **ROLLBACK 收尾**
    /// → **big 命中**（rows 3>=3、dur 0）= L2；E9 BEGIN；E10 insert ts=T0+9
    /// ≥ last_print → **窗口#2 落盘**（d.t1, d.t3），重置 T0+14；E11 insert
    /// d.t2（ts T0+9 < T0+14 不落盘）；E12 COMMIT（rows 2<3、dur 0<1 →
    /// **不命中**负例）；finish → 残余窗口#3（d.t2）+ skip 尾注（skipped=2）。
    fn feed_all(agg: &mut Aggregator) -> std::io::Result<StatsSummary> {
        let facts = [
            fact(B1, "d", "t2", FactKind::Insert, 5, T0, (120, 214)),
            fact(B1, "d", "t1", FactKind::Update, 1, T0, (253, 340)),
            fact(B1, "d", "t2", FactKind::Delete, 1, T0 + 2, (340, 414)),
            fact(B2, "d", "t1", FactKind::Insert, 3, T0 + 5, (159, 254)),
            fact(B2, "d", "t3", FactKind::Insert, 1, T0 + 9, (335, 409)),
            fact(B2, "d", "t2", FactKind::Insert, 1, T0 + 9, (409, 483)),
        ];
        let [f1, f3, f4, f7, f10, f11] = &facts;
        agg.feed(&StreamEvent::Row(f1))?;
        agg.feed(&StreamEvent::Begin {
            binlog: B1,
            pos: 214,
            ts: T0,
        })?;
        agg.feed(&StreamEvent::Row(f3))?;
        agg.feed(&StreamEvent::Row(f4))?;
        agg.feed(&StreamEvent::Commit {
            binlog: B1,
            pos: 441,
            ts: T0 + 3,
        })?;
        agg.feed(&StreamEvent::Begin {
            binlog: B2,
            pos: 120,
            ts: T0 + 4,
        })?;
        agg.feed(&StreamEvent::Row(f7))?;
        agg.feed(&StreamEvent::Rollback {
            binlog: B2,
            pos: 296,
            ts: T0 + 5,
        })?;
        agg.feed(&StreamEvent::Begin {
            binlog: B2,
            pos: 296,
            ts: T0 + 6,
        })?;
        agg.feed(&StreamEvent::Row(f10))?;
        agg.feed(&StreamEvent::Row(f11))?;
        agg.feed(&StreamEvent::Commit {
            binlog: B2,
            pos: 510,
            ts: T0 + 9,
        })?;
        agg.finish(2)
    }

    /// 逐字节 golden：两 txt + 两 jsonl 全文件断言（stats_json=true）。
    #[test]
    fn aggregator_golden_byte_exact() {
        let dir = out_dir("gold");
        {
            let mut agg = Aggregator::new(&cfg(true), &dir).unwrap();
            let sum = feed_all(&mut agg).unwrap();
            assert_eq!(
                (sum.windows, sum.biglong),
                (3, 2),
                "窗口落盘 3 次（切换/tick/finish 残余），biglong 命中 2"
            );
        }
        assert_eq!(
            std::fs::read_to_string(dir.join("binlog_status.txt")).unwrap(),
            STATUS_GOLDEN
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("biglong_trx.txt")).unwrap(),
            BIGLONG_GOLDEN
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("binlog_status.jsonl")).unwrap(),
            STATUS_JSONL_GOLDEN
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("biglong_trx.jsonl")).unwrap(),
            BIGLONG_JSONL_GOLDEN
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// stats_json=false：不产 .jsonl，txt 面逐字节不变。
    #[test]
    fn aggregator_without_jsonl() {
        let dir = out_dir("nojson");
        {
            let mut agg = Aggregator::new(&cfg(false), &dir).unwrap();
            feed_all(&mut agg).unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(dir.join("binlog_status.txt")).unwrap(),
            STATUS_GOLDEN
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("biglong_trx.txt")).unwrap(),
            BIGLONG_GOLDEN
        );
        assert!(!dir.join("binlog_status.jsonl").exists());
        assert!(!dir.join("biglong_trx.jsonl").exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
