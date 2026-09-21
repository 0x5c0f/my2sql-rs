//! Task 14 端到端集成测试：合成 binlog → `run_to_sql` → 断言 `.sql` 文件
//! 逐字节内容（SET NAMES 头 / extra-info 注释 / DML 文本 / 保序 / 分表命名 /
//! threads=1 与 threads=4 输出字节等价）。
//!
//! 设计（简报 Step 1）：≥3 个 DML 事件、跨 ≥2 张表、含 1 个多行事务。
//! 合成 binlog 无 CRC（5.6 语义，镜像 `src/binlog/file_reader.rs` 测试 Synth），
//! 事件体布局严格对齐 `decode_rows`/`parse_table_map`（全 INT 列，逐列 4B）。

use std::path::{Path, PathBuf};
use std::process;

use clap::Parser;
use my2sql_rs::config::{Cli, Command, Config};
use my2sql_rs::pipeline::{run_flashback, run_stats, run_to_sql};

// ---------- 合成 binlog 构造器（P2 T3 平移至 tests/common/synth.rs，共享给
// tests/flashback.rs；本文件行为零变） ----------

#[path = "common/synth.rs"]
mod synth;
use synth::Synth;

// ---------- 临时目录 ----------

fn tmp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut p = std::env::temp_dir();
    p.push(format!("my2sql-t14-{tag}-{}.{nanos}", process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// 合成 fixture 落盘后的路径 + 各事件字节偏移（extra-info 断言用）。
struct Fix {
    binlog_dir: PathBuf,
    schema_file: PathBuf,
    a1_tm: u32,
    a1_end: u32,
    b1_tm: u32,
    b1_end: u32,
    a2_tm: u32,
    a2_end: u32,
    b2_tm: u32,
    b2_end: u32,
}

/// 写出离线 schema JSON（version 1）+ 合成 binlog 文件。
fn build_fixture(dir: &Path) -> Fix {
    let binlog_dir = dir.join("binlog");
    std::fs::create_dir_all(&binlog_dir).unwrap();

    let mut s = Synth::new();
    // trx1: BEGIN, tm a, write a(1), tm b, write b(7,70), XID
    // trx2: BEGIN, tm a, write a[(2),(3)] 多行, tm b, delete b(k=7), XID
    s.query("t10", "BEGIN", 1700000000);
    let (a1_tm, _) = s.table_map(85, "t10", "a", 1, 1700000000);
    let (_, a1_end) = s.write(85, 1, &[vec![1]], 1700000000);
    let (b1_tm, _) = s.table_map(86, "t10", "b", 2, 1700000001);
    let (_, b1_end) = s.write(86, 2, &[vec![7, 70]], 1700000001);
    s.xid(1700000001);
    s.query("t10", "BEGIN", 1700000100);
    let (a2_tm, _) = s.table_map(85, "t10", "a", 1, 1700000100);
    let (_, a2_end) = s.write(85, 1, &[vec![2], vec![3]], 1700000100);
    let (b2_tm, _) = s.table_map(86, "t10", "b", 2, 1700000101);
    let (_, b2_end) = s.delete(86, 2, &[vec![7, 70]], 1700000101);
    s.xid(1700000101);

    std::fs::write(binlog_dir.join("mysql-bin.000001"), &s.bytes).unwrap();

    let schema_file = dir.join("schema.json");
    std::fs::write(
        &schema_file,
        r#"{
  "version": 1,
  "tables": [
    {"db":"t10","table":"a","cols":[{"name":"id","type_name":"int","unsigned":false}],"pk":["id"],"uks":[]},
    {"db":"t10","table":"b","cols":[
       {"name":"k","type_name":"int","unsigned":false},
       {"name":"v","type_name":"int","unsigned":false}],"pk":["k"],"uks":[]}
  ]
}"#,
    )
    .unwrap();

    Fix {
        binlog_dir,
        schema_file,
        a1_tm,
        a1_end,
        b1_tm,
        b1_end,
        a2_tm,
        a2_end,
        b2_tm,
        b2_end,
    }
}

/// 由 CLI 参数串构造 Config（复用真实解析/校验路径；T5 三子命令后本
/// helper 仅收 to-sql——e2e 各用例首参恒 "to-sql"）。
fn config_from(args: &[&str]) -> Config {
    let cli = Cli::try_parse_from(args).expect("cli parse");
    let Command::ToSql(a) = cli.cmd else {
        panic!("config_from expects to-sql")
    };
    Config::validate_to_sql(a).expect("config validate")
}

fn read_file(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

// ---------- 主 e2e：单文件 + extra-info + 全序 ----------

#[test]
fn e2e_pipeline_produces_expected_sql_bytes() {
    let dir = tmp_dir("main");
    let f = build_fixture(&dir);
    let out = dir.join("out");

    let cfg = config_from(&[
        "my2sql-rs",
        "to-sql",
        "--binlog-dir",
        f.binlog_dir.to_str().unwrap(),
        "--start-file",
        "mysql-bin.000001",
        "--schema-file",
        f.schema_file.to_str().unwrap(),
        "--output-dir",
        out.to_str().unwrap(),
        "--add-extra-info",
        "--threads",
        "4",
    ]);
    let summary = run_to_sql(&cfg).expect("run_to_sql ok");
    assert_eq!(summary.events, 4, "4 个 rows 事件被派发");
    assert_eq!(summary.statements, 5, "1+1+2+1 条语句");
    assert_eq!(summary.errors, 0);
    assert_eq!(summary.files, 1);

    let body = read_file(&out.join("to_sql.1.sql"));
    let expected = format!(
        "SET NAMES utf8mb4;\n\
         # datetime=2023-11-14_22:13:20 database=t10 table=a binlog=mysql-bin.000001 startpos={} stoppos={}\n\
         INSERT INTO `t10`.`a` (`id`) VALUES (1);\n\
         # datetime=2023-11-14_22:13:21 database=t10 table=b binlog=mysql-bin.000001 startpos={} stoppos={}\n\
         INSERT INTO `t10`.`b` (`k`,`v`) VALUES (7,70);\n\
         # datetime=2023-11-14_22:15:00 database=t10 table=a binlog=mysql-bin.000001 startpos={} stoppos={}\n\
         INSERT INTO `t10`.`a` (`id`) VALUES (2);\n\
         INSERT INTO `t10`.`a` (`id`) VALUES (3);\n\
         # datetime=2023-11-14_22:15:01 database=t10 table=b binlog=mysql-bin.000001 startpos={} stoppos={}\n\
         DELETE FROM `t10`.`b` WHERE `k`=7;\n",
        f.a1_tm, f.a1_end, f.b1_tm, f.b1_end, f.a2_tm, f.a2_end, f.b2_tm, f.b2_end,
    );
    assert_eq!(
        body, expected,
        "逐字节对照（含 SET NAMES 头 + extra-info + 保序）"
    );

    std::fs::remove_dir_all(&dir).ok();
}

// ---------- threads=1 直通与 threads=4 字节等价 ----------

#[test]
fn e2e_single_thread_matches_parallel_byte_for_byte() {
    let dir = tmp_dir("parity");
    let f = build_fixture(&dir);
    let base = [
        "my2sql-rs",
        "to-sql",
        "--binlog-dir",
        f.binlog_dir.to_str().unwrap(),
        "--start-file",
        "mysql-bin.000001",
        "--schema-file",
        f.schema_file.to_str().unwrap(),
        "--add-extra-info",
    ];

    let out1 = dir.join("out1");
    let mut a1 = base.to_vec();
    a1.extend(["--output-dir", out1.to_str().unwrap(), "--threads", "1"]);
    run_to_sql(&config_from(&a1)).expect("threads=1");

    let out4 = dir.join("out4");
    let mut a4 = base.to_vec();
    a4.extend(["--output-dir", out4.to_str().unwrap(), "--threads", "4"]);
    run_to_sql(&config_from(&a4)).expect("threads=4");

    assert_eq!(
        read_file(&out1.join("to_sql.1.sql")),
        read_file(&out4.join("to_sql.1.sql")),
        "threads=1 直通输出必须与并行路径逐字节一致"
    );

    std::fs::remove_dir_all(&dir).ok();
}

// ---------- file_per_table 分表命名 ----------

#[test]
fn e2e_file_per_table_splits_by_table() {
    let dir = tmp_dir("per_table");
    let f = build_fixture(&dir);
    let out = dir.join("out");
    let cfg = config_from(&[
        "my2sql-rs",
        "to-sql",
        "--binlog-dir",
        f.binlog_dir.to_str().unwrap(),
        "--start-file",
        "mysql-bin.000001",
        "--schema-file",
        f.schema_file.to_str().unwrap(),
        "--output-dir",
        out.to_str().unwrap(),
        "--file-per-table",
        "--threads",
        "3",
    ]);
    let summary = run_to_sql(&cfg).expect("run_to_sql ok");
    assert_eq!(summary.files, 2, "两张表 → 两个文件");
    assert_eq!(summary.errors, 0);

    let a = read_file(&out.join("to_sql.t10.a.1.sql"));
    let b = read_file(&out.join("to_sql.t10.b.1.sql"));
    assert_eq!(
        a,
        "SET NAMES utf8mb4;\n\
         INSERT INTO `t10`.`a` (`id`) VALUES (1);\n\
         INSERT INTO `t10`.`a` (`id`) VALUES (2);\n\
         INSERT INTO `t10`.`a` (`id`) VALUES (3);\n",
        "无 extra-info：仅头 + 语句，表 a 全序"
    );
    assert_eq!(
        b,
        "SET NAMES utf8mb4;\n\
         INSERT INTO `t10`.`b` (`k`,`v`) VALUES (7,70);\n\
         DELETE FROM `t10`.`b` WHERE `k`=7;\n"
    );

    std::fs::remove_dir_all(&dir).ok();
}

// ---------- 终审 #1 回归：threads=1 直通泵遇敌意 DECIMAL 事件 → 计错不终止 ----------

#[test]
fn e2e_hostile_decimal_event_counted_not_abort() {
    // 终审 #1：DECIMAL(19,9) 满组溢出字节 `81 00 00 00 01 FF FF FF FF` 经
    // threads=1 直通泵（pump_direct，无 worker catch_unwind 兜底）→ 修复前
    // `9 − 10` 减法溢出直接 abort 整测程；修复后按逐事件错误策略计错跳过、
    // run_to_sql 仍 Ok，其后的正常事件照常产出。
    let dir = tmp_dir("decbomb");
    let binlog_dir = dir.join("binlog");
    std::fs::create_dir_all(&binlog_dir).unwrap();

    let mut s = Synth::new();
    s.query("t10", "BEGIN", 1700000000);
    s.table_map_decimal(90, "t10", "d", 19, 9, 1700000000);
    s.write_raw(
        90,
        &[vec![0x81, 0x00, 0x00, 0x00, 0x01, 0xFF, 0xFF, 0xFF, 0xFF]],
        1700000000,
    );
    s.xid(1700000000);
    s.query("t10", "BEGIN", 1700000001);
    s.table_map(91, "t10", "a", 1, 1700000001);
    s.write(91, 1, &[vec![7]], 1700000001);
    s.xid(1700000001);
    std::fs::write(binlog_dir.join("mysql-bin.000001"), &s.bytes).unwrap();

    let schema_file = dir.join("schema.json");
    std::fs::write(
        &schema_file,
        r#"{"version":1,"tables":[
  {"db":"t10","table":"d","cols":[{"name":"amt","type_name":"decimal","unsigned":false}],"pk":[],"uks":[]},
  {"db":"t10","table":"a","cols":[{"name":"id","type_name":"int","unsigned":false}],"pk":["id"],"uks":[]}
]}"#,
    )
    .unwrap();

    let out = dir.join("out");
    let cfg = config_from(&[
        "my2sql-rs",
        "to-sql",
        "--binlog-dir",
        binlog_dir.to_str().unwrap(),
        "--start-file",
        "mysql-bin.000001",
        "--schema-file",
        schema_file.to_str().unwrap(),
        "--output-dir",
        out.to_str().unwrap(),
        "--threads",
        "1",
    ]);
    let summary = run_to_sql(&cfg).expect("threads=1 直通：敌意事件计错，不 abort 整跑");
    assert_eq!(summary.events, 2, "两个 rows 事件均被派发");
    assert_eq!(
        summary.errors, 1,
        "敌意 DECIMAL 事件计 1 错（逐事件错误策略）"
    );
    assert_eq!(summary.statements, 1, "其后正常事件照常产出");

    let body = read_file(&out.join("to_sql.1.sql"));
    assert_eq!(
        body,
        "SET NAMES utf8mb4;\nINSERT INTO `t10`.`a` (`id`) VALUES (7);\n"
    );

    std::fs::remove_dir_all(&dir).ok();
}
// ---------- robust-continue：schema 缺失表 → 计数跳过、不中断 ----------

#[test]
fn e2e_schema_miss_counts_errors_and_continues() {
    let dir = tmp_dir("miss");
    let f = build_fixture(&dir);
    // 仅表 a 的 schema：b 的两事件应各计一次错误，a 正常出 SQL。
    let schema_file = dir.join("schema_partial.json");
    std::fs::write(
        &schema_file,
        r#"{"version":1,"tables":[{"db":"t10","table":"a","cols":[{"name":"id","type_name":"int","unsigned":false}],"pk":["id"],"uks":[]}]}"#,
    )
    .unwrap();
    let out = dir.join("out");
    let cfg = config_from(&[
        "my2sql-rs",
        "to-sql",
        "--binlog-dir",
        f.binlog_dir.to_str().unwrap(),
        "--start-file",
        "mysql-bin.000001",
        "--schema-file",
        schema_file.to_str().unwrap(),
        "--output-dir",
        out.to_str().unwrap(),
        "--threads",
        "2",
    ]);
    let summary = run_to_sql(&cfg).expect("robust-continue：不返回 Err");
    assert_eq!(summary.errors, 2, "b 的两事件各计一次 schema 错误");
    assert_eq!(summary.statements, 3, "仅 a 的三条 INSERT 落盘");
    assert_eq!(summary.files, 1);
    let a = read_file(&out.join("to_sql.1.sql"));
    assert_eq!(
        a,
        "SET NAMES utf8mb4;\n\
         INSERT INTO `t10`.`a` (`id`) VALUES (1);\n\
         INSERT INTO `t10`.`a` (`id`) VALUES (2);\n\
         INSERT INTO `t10`.`a` (`id`) VALUES (3);\n"
    );

    std::fs::remove_dir_all(&dir).ok();
}

// =====================================================================
// ---------- P2 T6：真件（tests/fixtures/capture_8.0_rows）----------
// =====================================================================
//
// fixture = docker mysql:8.0.46 真实捕获（CRC32、rows_query ON，见同目录
// README.txt）。schema.json 手抄 README 的 CREATE 逐列（version-1 格式）：
// - binlog.000002 `t10`.`u` FULL 镜像（1 INSERT + 1 UPDATE，无 PK →
//   WHERE 全列 = P1 语义）：flashback Ok 主体 + 正逆 multiset 对账；
// - binlog.000003 同表 MINIMAL 镜像：UPDATE#1 after 仅 f=123 其余 Missing →
//   flashback 硬规则 b（spec §3.2b：Missing 参与逆 WHERE/VALUES → Err 提示
//   binlog_row_image=FULL）+ T5 默认 Stop → 整跑 Err。**这就是期望断言**
//   （真件版硬规则证明），非失败；
// - binlog.000004 `t10`.`j` JSON + PARTIAL_UPDATE_ROWS_V2(39)：T12 路由在
//   **源层**（FileReader::next 直接 Err(PartialNotSupported)）拦截——实测
//   stop/skip 两策略、三子命令全部整跑 Err（源级错误不是逐事件错误，
//   skip-bad-event 无从跳过）。本用例钉死该实测现实（简报预期 skip 可绕，
//   现实不可——以现实为准，登记报告）。

/// 由真实 CLI 参数串走 `validate_flashback` 构造 Config（T5 旗标面全真）。
fn flashback_config(args: &[&str]) -> Config {
    let cli = Cli::try_parse_from(args).expect("cli parse");
    let Command::Flashback(a) = cli.cmd else {
        panic!("flashback_config expects flashback")
    };
    Config::validate_flashback(a).expect("flashback validate")
}

fn stats_config(args: &[&str]) -> Config {
    let cli = Cli::try_parse_from(args).expect("cli parse");
    let Command::Stats(a) = cli.cmd else {
        panic!("stats_config expects stats")
    };
    Config::validate_stats(a).expect("stats validate")
}

fn capture_args<'a>(
    mode: &'a str,
    start: &'a str,
    out: &'a Path,
    extra: &'a [&'a str],
) -> Vec<&'a str> {
    let mut v = vec![
        "my2sql-rs",
        mode,
        "--binlog-dir",
        "tests/fixtures/capture_8.0_rows",
        "--start-file",
        start,
        "--schema-file",
        "tests/fixtures/capture_8.0_rows/schema.json",
        "--output-dir",
        out.to_str().unwrap(),
    ];
    v.extend_from_slice(extra);
    v
}

/// 产物目录内 flashback 相关残留（final + 隐藏 tmp）。
fn flash_leftovers(out: &Path) -> Vec<String> {
    std::fs::read_dir(out)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("flashback") || n.starts_with(".flashback.tmp"))
                .collect()
        })
        .unwrap_or_default()
}

// ---------- 语句解析 + 镜像映射（正逆 multiset 对账的最小实现） ----------

/// 归一化语句：(列名, 字面量原文) 有序表。NULL 统一渲染为字面量 `NULL`
/// （WHERE 的 `col IS NULL` 与 SET/VALUES 的 `=NULL` 同型，正逆可通比）。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Stmt {
    Ins(Vec<(String, String)>),
    Del(Vec<(String, String)>),
    Upd {
        set: Vec<(String, String)>,
        whr: Vec<(String, String)>,
    },
}

fn unquote(col: &str) -> String {
    col.trim().trim_matches('`').to_string()
}

/// `a=1 AND b='x' AND c IS NULL` → 有序对（IS NULL → 值 "NULL"）。
fn parse_conds(s: &str) -> Vec<(String, String)> {
    s.split(" AND ")
        .map(|c| {
            if let Some(k) = c.strip_suffix(" IS NULL") {
                (unquote(k), "NULL".to_string())
            } else {
                let (k, v) = c.split_once('=').expect("cond has '='");
                (unquote(k), v.trim().to_string())
            }
        })
        .collect()
}

/// 单条 SQL 行 → `Stmt`。前提（真件成立、fixture 钉死）：字符串字面量不含
/// `,`、`(`、`)`、` AND `、` WHERE `——本 fixture 数据 `ab`/`xyz` 满足。
fn parse_stmt(line: &str) -> Stmt {
    assert!(line.ends_with(';'), "statement must end with ';': {line}");
    let s = &line[..line.len() - 1];
    if let Some(rest) = s.strip_prefix("INSERT INTO ") {
        let li = rest.find('(').unwrap();
        let ri = rest.find(')').unwrap();
        let vi = rest[ri..].rfind('(').unwrap() + ri;
        let ve = rest.rfind(')').unwrap();
        let cols: Vec<String> = rest[li + 1..ri].split(',').map(unquote).collect();
        let vals: Vec<String> = rest[vi + 1..ve]
            .split(',')
            .map(|v| v.trim().to_string())
            .collect();
        Stmt::Ins(cols.into_iter().zip(vals).collect())
    } else if let Some(rest) = s.strip_prefix("DELETE FROM ") {
        Stmt::Del(parse_conds(rest.split_once(" WHERE ").unwrap().1))
    } else {
        let body = s.strip_prefix("UPDATE ").unwrap();
        let si = body.find(" SET ").unwrap();
        let wi = body.find(" WHERE ").unwrap();
        Stmt::Upd {
            set: body[si + 5..wi]
                .split(',')
                .map(|a| {
                    let (k, v) = a.split_once('=').unwrap();
                    (unquote(k), v.trim().to_string())
                })
                .collect(),
            whr: parse_conds(&body[wi + 7..]),
        }
    }
}

fn body_stmts(path: &Path) -> Vec<Stmt> {
    read_file(path)
        .lines()
        .filter(|l| {
            l.starts_with("INSERT ") || l.starts_with("UPDATE ") || l.starts_with("DELETE ")
        })
        .map(parse_stmt)
        .collect()
}

fn get<'a>(pairs: &'a [(String, String)], k: &str) -> Option<&'a str> {
    pairs.iter().find(|(c, _)| c == k).map(|(_, v)| v.as_str())
}

/// 正序语句 → 逆序镜像语句。UPDATE 的镜像由「SET ⊆ WHERE 键、WHERE 全列」
/// 前提重建（本 fixture `u` 无 PK → WHERE 恒全列，P1 语义）：
/// 逆向 SET[c] = 正向 WHERE[c]（before 值）；逆向 WHERE[c] = 正向 SET 有则
/// after 值、无则未变化 = 正向 WHERE 值。
fn mirror(s: &Stmt) -> Stmt {
    match s {
        Stmt::Ins(p) => Stmt::Del(p.clone()),
        Stmt::Del(p) => Stmt::Ins(p.clone()),
        Stmt::Upd { set, whr } => Stmt::Upd {
            set: set
                .iter()
                .map(|(c, _)| {
                    (
                        c.clone(),
                        get(whr, c).expect("SET col in WHERE").to_string(),
                    )
                })
                .collect(),
            whr: whr
                .iter()
                .map(|(c, v)| (c.clone(), get(set, c).unwrap_or(v).to_string()))
                .collect(),
        },
    }
}

// ---------- Step 1a：FULL 镜像真件 flashback Ok + 产物结构 ----------
// ---------- Step 1b：正逆 multiset 对账（语句级镜像一一配对） ----------

#[test]
fn real_capture_flashback_full_image_and_forward_reconcile() {
    let out = tmp_dir("t6-flash");
    let out1 = tmp_dir("t6-tosql");
    let sum = run_flashback(&flashback_config(&capture_args(
        "flashback",
        "binlog.000002",
        &out,
        &["--threads", "2"],
    )))
    .expect("FULL 镜像真件 flashback Ok（默认 stop）");
    assert_eq!(
        (sum.events, sum.statements, sum.errors, sum.files),
        (2, 2, 0, 1)
    );

    let final_path = out.join("flashback.2.sql"); // 编号 = binlog 尾号
    let body = read_file(&final_path);
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines[0], "SET NAMES utf8mb4;", "首行 = FILE_HEADER");
    assert_eq!(
        lines.last().unwrap(),
        &"commit;",
        "尾行 = commit;（keep-trx 默认开）"
    );
    let begins = lines.iter().filter(|l| **l == "begin;").count();
    let commits = lines.iter().filter(|l| **l == "commit;").count();
    assert_eq!(
        begins, 2,
        "begin; 计数 = 事务段数（2 autocommit 事件 = 2 段）"
    );
    assert_eq!(commits, begins + 1, "每段前置 commit; + 尾 commit;");
    assert_eq!(
        flash_leftovers(&out),
        vec!["flashback.2.sql".to_string()],
        "仅 1 个 final，无 .flashback.tmp* 残留"
    );

    // 正序 to-sql（skip 默认）→ 逐语句镜像 → 与逆序产物做有序全等
    // （= multiset 相等 + 逆序排列；真件事务序重建的唯一诚实口径）。
    run_to_sql(&config_from(&capture_args(
        "to-sql",
        "binlog.000002",
        &out1,
        &["--threads", "2"],
    )))
    .expect("to-sql 真件 Ok");
    let forward = body_stmts(&out1.join("to_sql.2.sql"));
    let back = body_stmts(&final_path);
    assert_eq!(forward.len(), 2);
    let expected: Vec<Stmt> = forward.iter().rev().map(mirror).collect();
    assert_eq!(
        back, expected,
        "逆序产物 = 正序产物逐条镜像（ins↔del、upd SET/WHERE 互换重建）且序反"
    );

    std::fs::remove_dir_all(&out).ok();
    std::fs::remove_dir_all(&out1).ok();
}

// ---------- Step 1c：MINIMAL 镜像真件 → 硬规则 b 的 Stop 整跑 Err ----------

#[test]
fn real_capture_flashback_minimal_image_hard_errors() {
    // threads=1 直通：错误消息原样上抛（并行路径是哨兵摘要串），钉死 §3.2b 文案
    let out = tmp_dir("t6-min-stop");
    let e = run_flashback(&flashback_config(&capture_args(
        "flashback",
        "binlog.000003",
        &out,
        &["--on-error", "stop", "--threads", "1"],
    )))
    .expect_err("MINIMAL 镜像 UPDATE 必触发硬规则 b → Stop 整跑 Err（真件证明）");
    let msg = e.to_string();
    assert!(msg.contains("binlog_row_image=FULL"), "{msg}");
    assert!(msg.contains("MINIMAL row image"), "{msg}");
    assert!(
        flash_leftovers(&out).is_empty(),
        "半成品不落盘：{:?}",
        flash_leftovers(&out)
    );
    // 并行形态同 Err 同清场（消息为哨兵摘要，不比串）
    let out2 = tmp_dir("t6-min-stop2");
    assert!(
        run_flashback(&flashback_config(&capture_args(
            "flashback",
            "binlog.000003",
            &out2,
            &["--on-error", "stop", "--threads", "2"],
        )))
        .is_err()
    );
    assert!(flash_leftovers(&out2).is_empty());

    // SkipBadEvent：坏 MINIMAL 事件计数跳过，FULL 事件（UPDATE#2）照常逆产，
    // 头部 WARNING 行（T3 契约）；正向 to-sql 同件同样 skip（Missing 是
    // 事件级编码错误，非源级——与 000004 的源级硬错误形成对照）。
    let out3 = tmp_dir("t6-min-skip");
    let sum = run_flashback(&flashback_config(&capture_args(
        "flashback",
        "binlog.000003",
        &out3,
        &["--on-error", "skip-bad-event", "--threads", "1"],
    )))
    .expect("skip 模式不中断整跑");
    assert_eq!(
        (sum.events, sum.statements, sum.errors, sum.files),
        (2, 1, 1, 1)
    );
    assert_eq!(
        read_file(&out3.join("flashback.3.sql")),
        "SET NAMES utf8mb4;\n\
         -- WARNING: skipped 1 events, positions in stderr\n\
         commit;\nbegin;\n\
         UPDATE `t10`.`u` SET `c`=NULL WHERE `a`=2 AND `b`='xyz' AND `c`=55 AND `d`=11 \
         AND `e` IS NULL AND `f`=123 AND `g`=99 AND `h` IS NULL AND `i2`=-5;\n\
         commit;\n",
        "逐字节：FULL 镜像 UPDATE 逆产（SET=before 变化列、WHERE=after 全列）"
    );
    let out4 = tmp_dir("t6-min-tosql");
    let tsum = run_to_sql(&config_from(&capture_args(
        "to-sql",
        "binlog.000003",
        &out4,
        &["--threads", "1"],
    )))
    .expect("to-sql 恒 robust-continue：事件级 Missing 计错不 Err");
    assert_eq!((tsum.statements, tsum.errors), (1, 1));

    std::fs::remove_dir_all(&out).ok();
    std::fs::remove_dir_all(&out2).ok();
    std::fs::remove_dir_all(&out3).ok();
    std::fs::remove_dir_all(&out4).ok();
}

// ---------- 000004 partial(39)：源级硬错误，三形态两策略全 Err（实测钉死） ----------

#[test]
fn real_capture_partial_event_is_source_level_error() {
    // T12 路由：event 39 在 FileReader::next 即 Err(PartialNotSupported)——
    // **源级**错误不经逐事件错误通道，`--on-error skip-bad-event` 也无法跳过
    // （简报预期 skip 可绕，实测为否；以实测为准钉死，报告登记）。
    let out = tmp_dir("t6-partial");
    for mode_args in [
        capture_args("flashback", "binlog.000004", &out, &["--threads", "1"]),
        capture_args(
            "flashback",
            "binlog.000004",
            &out,
            &["--on-error", "skip-bad-event", "--threads", "1"],
        ),
        capture_args("to-sql", "binlog.000004", &out, &["--threads", "1"]),
    ] {
        let sub = mode_args[1];
        let r = if sub == "to-sql" {
            run_to_sql(&config_from(&mode_args)).map(|_| ())
        } else {
            run_flashback(&flashback_config(&mode_args)).map(|_| ())
        };
        let e = r.expect_err(&format!(
            "{sub} on 000004 must Err (source-level partial gate)"
        ));
        assert!(e.to_string().contains("partial rows"), "{sub}: {e}");
        assert!(flash_leftovers(&out).is_empty(), "flashback 半成品清场");
    }
    // stats 同件同 Err（泵共用 reader）；报表文件建文件即写头行，Err 路径
    // 不 finish → 仅剩头行（实测现实：无尾注 `# skipped` 的半成品头文件保留，
    // 重跑 O_TRUNC 覆盖——登记报告，非本任务修复项）。
    let e = run_stats(&stats_config(&capture_args(
        "stats",
        "binlog.000004",
        &out,
        &["--threads", "1"],
    )))
    .expect_err("stats on 000004 must Err");
    assert!(e.to_string().contains("partial rows"), "{e}");
    let status = read_file(&out.join("binlog_status.txt"));
    assert_eq!(
        status.lines().count(),
        1,
        "stats 错误路径 = 报表仅头行（finish 未达，无 skipped 尾注）:\n{status}"
    );
    assert!(status.starts_with("binlog ") && status.contains("inserts"));
    std::fs::remove_dir_all(&out).ok();
}

// ---------- Step 2：真件 stats（000002+000003 全事件流） ----------

#[test]
fn real_capture_stats_totals_match_fixture_rows() {
    let out = tmp_dir("t6-stats");
    let run = run_stats(&stats_config(&capture_args(
        "stats",
        "binlog.000002",
        &out,
        &["--stop-file", "binlog.000003", "--threads", "2"],
    )))
    .expect("stats 真件 Ok（skip 默认，无 PartialNotSupported 事件）");
    assert_eq!(
        run.summary.errors, 0,
        "stats 解码零跳过（MINIMAL 镜像可计数）"
    );
    assert_eq!(run.biglong, 0, "每事务 ≤2 行 < big_trx_rows=10 → 零命中");
    assert_eq!(run.windows, 2, "binlog 切换即落盘 → 两窗口");

    let status = read_file(&out.join("binlog_status.txt"));
    let mut ins = [0u64; 3]; // 数据行 [inserts, updates, deletes] 总和
    let mut per_file: Vec<(String, u64, u64)> = Vec::new();
    for line in status.lines() {
        if line.starts_with("binlog ") || line.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        assert_eq!((f[8], f[9]), ("t10", "u"), "窗口行只应有表 u: {line}");
        let (i, u, d): (u64, u64, u64) = (
            f[5].parse().unwrap(),
            f[6].parse().unwrap(),
            f[7].parse().unwrap(),
        );
        ins[0] += i;
        ins[1] += u;
        ins[2] += d;
        per_file.push((f[0].to_string(), u, i));
    }
    // 独立计数（README.txt 枚举）：000002 = 1 insert + 1 update 对；
    // 000003 = 2 update 对（含 1 个 MINIMAL 镜像——stats 通道 decode 成功
    // 即可计数，Missing 不影响行对折算 len/2，实测确认计数如常）。
    assert_eq!(
        ins,
        [1, 3, 0],
        "binlog_status 行总和 == fixture 行事件独立计数"
    );
    assert_eq!(
        per_file,
        vec![
            ("binlog.000002".to_string(), 1, 1),
            ("binlog.000003".to_string(), 2, 0)
        ],
        "MINIMAL 镜像 UPDATE 照常计入 000003 的 updates=2（stats 不挑食镜像）"
    );
    assert_eq!(
        run.summary.statements, 4,
        "摘要行计数 = 1+3 行事件（与报表列总和互证）"
    );

    let biglong = read_file(&out.join("biglong_trx.txt"));
    assert!(
        biglong.starts_with("binlog ") && biglong.lines().count() == 2,
        "biglong 报表存在且仅头 + skipped 尾注（零命中）:\n{biglong}"
    );
    std::fs::remove_dir_all(&out).ok();
}
