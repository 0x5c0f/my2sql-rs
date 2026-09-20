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
use my2sql_rs::pipeline::run_to_sql;

// ---------- 合成 binlog 构造器 ----------

struct Synth {
    bytes: Vec<u8>,
}

impl Synth {
    fn new() -> Self {
        // magic + 手写 FDE（v4、server 8.0.46、alg=0 → 无 checksum）
        let mut s = Synth {
            bytes: b"\xfebin".to_vec(),
        };
        let mut fde = Vec::new();
        fde.extend_from_slice(&4u16.to_le_bytes()); // binlog version
        let mut sv = [0u8; 50];
        sv[.."8.0.46".len()].copy_from_slice(b"8.0.46");
        fde.extend_from_slice(&sv);
        fde.extend_from_slice(&1600000000u32.to_le_bytes()); // create ts
        fde.push(19); // common header length
        fde.extend_from_slice(&[27u8; 39]); // event type header lengths（占位）
        fde.push(0); // checksum alg = NONE
        s.push(15, 1000, &fde);
        s
    }

    /// 追加一个事件（无 CRC），返回 (start_pos, end_pos)。
    fn push(&mut self, evtype: u8, ts: u32, body: &[u8]) -> (u32, u32) {
        let size = 19 + body.len() as u32;
        let start = self.bytes.len() as u32;
        let end = start + size;
        let mut b = Vec::new();
        b.extend_from_slice(&ts.to_le_bytes()); // timestamp
        b.push(evtype); // type code
        b.extend_from_slice(&9u32.to_le_bytes()); // server_id
        b.extend_from_slice(&size.to_le_bytes()); // event_size
        b.extend_from_slice(&end.to_le_bytes()); // log_pos
        b.extend_from_slice(&0x01u16.to_le_bytes()); // flags: BINLOG_IN_USE
        b.extend_from_slice(body);
        debug_assert_eq!(b.len(), size as usize);
        self.bytes.extend_from_slice(&b);
        (start, end)
    }

    /// QUERY 事件（BEGIN / 其它），body 布局对照 file_reader::query_text。
    fn query(&mut self, db: &str, sql: &str, ts: u32) -> (u32, u32) {
        let mut b = Vec::new();
        b.extend_from_slice(&0u32.to_le_bytes()); // proxy_db_id
        b.extend_from_slice(&0u32.to_le_bytes()); // exec_time
        b.push(db.len() as u8); // schema_len
        b.extend_from_slice(&0u16.to_le_bytes()); // error code
        b.extend_from_slice(&0u16.to_le_bytes()); // status_vars_len
        b.extend_from_slice(db.as_bytes());
        b.push(0); // schema NUL
        b.extend_from_slice(sql.as_bytes());
        self.push(2, ts, &b)
    }

    fn xid(&mut self, ts: u32) -> (u32, u32) {
        self.push(16, ts, &42u64.to_le_bytes())
    }

    /// TABLE_MAP：`n` 个 INT（type 3）列，metadata 段 0 字节。
    fn table_map(&mut self, tid: u64, db: &str, tb: &str, n: usize, ts: u32) -> (u32, u32) {
        let mut b = Vec::new();
        b.extend_from_slice(&tid.to_le_bytes()[..6]);
        b.extend_from_slice(&0u16.to_le_bytes()); // flags
        b.push(db.len() as u8);
        b.extend_from_slice(db.as_bytes());
        b.push(0);
        b.push(tb.len() as u8);
        b.extend_from_slice(tb.as_bytes());
        b.push(0);
        b.push(n as u8); // n_cols (LNE < 251)
        b.extend(std::iter::repeat_n(3u8, n)); // 每列 type = LONG / INT
        b.push(0); // metadata total length = 0（全 INT）
        b.extend_from_slice(&[0u8; 1]); // null_bits（bit_width(<=8)=1B），值不影响本用例
        self.push(19, ts, &b)
    }

    /// rows 事件体：全列 present、非 NULL，逐行 INT 值。`kind` 决定 type code。
    fn rows(
        &mut self,
        tid: u64,
        kind_rows: u8,
        n: usize,
        rows: &[Vec<i32>],
        ts: u32,
    ) -> (u32, u32) {
        let mut b = Vec::new();
        b.extend_from_slice(&tid.to_le_bytes()[..6]);
        b.extend_from_slice(&0u16.to_le_bytes()); // flags
        b.extend_from_slice(&2u16.to_le_bytes()); // extra_info_len = 2（自含，V2）
        b.push(n as u8); // n_cols
        let bm = if n >= 8 {
            0xFFu8
        } else {
            (1u16 << n) as u8 - 1
        };
        b.push(bm); // cols_present bitmap1（全列）
        for row in rows {
            assert_eq!(row.len(), n);
            b.push(0u8); // null_bits：present<=8 → 1B，全非 NULL
            for &v in row {
                b.extend_from_slice(&v.to_le_bytes());
            }
        }
        self.push(kind_rows, ts, &b)
    }

    fn write(&mut self, tid: u64, n: usize, rows: &[Vec<i32>], ts: u32) -> (u32, u32) {
        self.rows(tid, 30, n, rows, ts) // WRITE_ROWS_V2
    }
    fn delete(&mut self, tid: u64, n: usize, rows: &[Vec<i32>], ts: u32) -> (u32, u32) {
        self.rows(tid, 32, n, rows, ts) // DELETE_ROWS_V2
    }
}

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

/// 由 CLI 参数串构造 Config（复用真实解析/校验路径）。
fn config_from(args: &[&str]) -> Config {
    let cli = Cli::try_parse_from(args).expect("cli parse");
    match cli.cmd {
        Command::ToSql(a) => Config::validate(a).expect("config validate"),
    }
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
