//! P2 T3：flashback 流水线端到端（库级）。fixture = `tests/common/synth.rs`
//! 的 `d`.`t`（int pk + varchar）合成 binlog；断言逐字节钉死 reverse 产物
//! （块序 = 事件序逆序、trx 变化处 `commit;\nbegin;\n` 注入 = T2 已测上游
//! 口径，此处只接线不重实现）。
//!
//! 手推块序（简报规则「执行者按此规则手推并钉死」）：tmp 块 = 事件序
//! [W(trx1)→DELETE, U(trx1)→逆向UPDATE, D(trx2)→INSERT]；逆序回写 =
//! D 块首出（head 注入）→ U → W（同 trx 不注入）→ 尾 `commit;\n`。

use std::path::{Path, PathBuf};
use std::process;

use clap::Parser;
use my2sql_rs::config::{Cli, Command, Config, OnError, WorkType};
use my2sql_rs::pipeline::{run_flashback, run_to_sql};

#[path = "common/synth.rs"]
mod synth;
use synth::Synth;

/// 复用真实解析/校验路径构造 to-sql Config，再覆写 flashback 三字段。
/// `dir` = e2e 同款 fixture 根（内含 `binlog/` 与 `schema.json`）。
fn cfg_for(dir: &Path, out: &Path, keep: bool, oe: OnError) -> Config {
    let cli = Cli::try_parse_from([
        "my2sql-rs",
        "to-sql",
        "--binlog-dir",
        dir.join("binlog").to_str().unwrap(),
        "--start-file",
        "mysql-bin.000001",
        "--schema-file",
        dir.join("schema.json").to_str().unwrap(),
        "--output-dir",
        out.to_str().unwrap(),
    ])
    .expect("cli parse");
    // T5 三子命令后 match 不再穷尽——let-else + panic（config_from 同款
    // 形态）：本 helper 恒走 to-sql 解析路径，再在库层覆写 flashback 字段。
    let mut c = match cli.cmd {
        Command::ToSql(a) => Config::validate_to_sql(a).expect("config validate"),
        _ => panic!("cfg_for expects to-sql"),
    };
    c.work_type = WorkType::Flashback;
    c.keep_trx = keep;
    c.on_error = oe;
    c
}

fn tmp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut p = std::env::temp_dir();
    p.push(format!("my2sql-p2t3-{tag}-{}.{nanos}", process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// `d`.`t` 的 schema JSON（int pk + varchar）。
fn schema_is() -> String {
    r#"{"version":1,"tables":[{"db":"d","table":"t","cols":[
        {"name":"id","type_name":"int","unsigned":false},
        {"name":"b","type_name":"varchar","unsigned":false}],
        "pk":["id"],"uks":[]}]}"#
        .to_string()
}

struct Fix {
    root: PathBuf,
}

impl Fix {
    /// 落盘合成 binlog 与 schema；`events` 回调向 Synth 灌事件。
    fn new<F: FnMut(&mut Synth)>(tag: &str, mut events: F) -> Fix {
        let root = tmp_dir(tag);
        let binlog_dir = root.join("binlog");
        std::fs::create_dir_all(&binlog_dir).unwrap();
        let mut s = Synth::new();
        events(&mut s);
        std::fs::write(binlog_dir.join("mysql-bin.000001"), &s.bytes).unwrap();
        std::fs::write(root.join("schema.json"), schema_is()).unwrap();
        Fix { root }
    }
    fn dir(&self) -> &Path {
        &self.root
    }
}

/// out 目录内是否残留任一模式的产物（final/tmp 都必须被清场）。
fn leftovers(out: &Path) -> Vec<String> {
    std::fs::read_dir(out)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("flashback") || n.starts_with(".flashback.tmp"))
                .collect()
        })
        .unwrap_or_default()
}

// ---------- 用例 1：多事务逐字节产物 ----------

#[test]
fn flashback_e2e_multi_trx_bytes() {
    let f = Fix::new("multi", |s| {
        // trx1: BEGIN, tm, W(1,'a'), U(1→2,'a'不变), XID
        // trx2: BEGIN, tm, D(3,'b'), XID
        s.query("d", "BEGIN", 1700000000);
        s.table_map_is(85, "d", "t", 10, 1700000000);
        s.write_is(85, &[(1, "a")], 1700000000);
        s.table_map_is(85, "d", "t", 10, 1700000000);
        s.update_is(85, &[((1, "a"), (2, "a"))], 1700000000);
        s.xid(1700000000);
        s.query("d", "BEGIN", 1700000100);
        s.table_map_is(85, "d", "t", 10, 1700000100);
        s.delete_is(85, &[(3, "b")], 1700000100);
        s.xid(1700000100);
    });
    let out = f.dir().join("out");
    let mut cfg = cfg_for(f.dir(), &out, true, OnError::SkipBadEvent);
    cfg.threads = 4;
    let sum = run_flashback(&cfg).expect("run_flashback ok");
    assert_eq!(
        (sum.events, sum.statements, sum.errors, sum.files),
        (3, 3, 0, 1)
    );

    let body = std::fs::read_to_string(out.join("flashback.1.sql")).unwrap();
    assert_eq!(
        body,
        "SET NAMES utf8mb4;\n\
         commit;\nbegin;\nINSERT INTO `d`.`t` (`id`,`b`) VALUES (3,'b');\n\
         commit;\nbegin;\nUPDATE `d`.`t` SET `id`=1 WHERE `id`=2;\n\
         DELETE FROM `d`.`t` WHERE `id`=1;\n\
         commit;\n",
        "块序=事件序逆序（D|U|W），trx 变化处注入，头注入=T2 上游口径"
    );
    assert_eq!(
        std::fs::read_dir(&out).unwrap().count(),
        1,
        "仅 1 个 final，无产物残留"
    );
    assert!(
        !out.join(".flashback.tmp.1.sql").exists(),
        "隐藏 tmp 必须消失（run_files 已删）"
    );

    // threads=1 直通与并行逐字节等价（P1 契约在 flashback 形态同样成立）
}

// P6 T1: Report format validation
#[test]
fn test_report_jsonl_format() {
    use my2sql_rs::flashback::report::SkipEvent;

    let event = SkipEvent {
        timestamp: "2026-09-22T14:30:15Z".to_string(),
        binlog: "mysql-bin.000150".to_string(),
        position: 12345,
        type_: "Query".to_string(),
        sql: Some("ALTER TABLE t_users ADD COLUMN new_field VARCHAR(100)".to_string()),
    };

    let json = serde_json::to_string(&event).expect("should serialize");

    assert!(json.contains("\"timestamp\":\"2026-09-22T14:30:15Z\""));
    assert!(json.contains("\"binlog\":\"mysql-bin.000150\""));
    assert!(json.contains("\"position\":12345"));
    assert!(json.contains("\"type\":\"Query\""));
}

// ---------- 用例 2：on-error stop → Err + 全清场（并行 + 直通两形态） ----------

/// 坏事件源：rows 体 table_id=99 与 tm 85 不符 → decode_rows InvalidData。
fn push_bad_trx(s: &mut Synth) {
    s.query("d", "BEGIN", 1700000100);
    s.table_map_is(85, "d", "t", 10, 1700000100);
    s.write_is(99, &[(9, "bad")], 1700000100); // table_id 不符 = 坏事件
    s.xid(1700000100);
}

fn good_trx(s: &mut Synth) {
    s.query("d", "BEGIN", 1700000000);
    s.table_map_is(85, "d", "t", 10, 1700000000);
    s.write_is(85, &[(1, "a")], 1700000000);
    s.xid(1700000000);
}

#[test]
fn flashback_on_error_stop_aborts_cleanly() {
    for threads in [4usize, 1] {
        let f = Fix::new("stop", |s| {
            good_trx(s);
            push_bad_trx(s);
        });
        let out = f.dir().join("out");
        let mut cfg = cfg_for(f.dir(), &out, true, OnError::Stop);
        cfg.threads = threads;
        let r = run_flashback(&cfg);
        assert!(
            r.is_err(),
            "threads={threads}：stop 模式坏事件必须返回 Err（非零码由 main 负责）"
        );
        assert!(
            !out.join("flashback.1.sql").exists() && leftovers(&out).is_empty(),
            "threads={threads}：半成品不落盘（spec §3.2）——无 final 无 tmp，got {:?}",
            leftovers(&out)
        );
        std::fs::remove_dir_all(f.dir()).ok();
    }
}

// ---------- 用例 2b：prepare 侧错误（表缺失于 schema-file）同受 stop 约束 ----------

#[test]
fn flashback_on_error_stop_aborts_missing_table() {
    for threads in [4usize, 1] {
        let f = Fix::new("stop-prep", |s| {
            good_trx(s);
            // `d`.`ghost` 不在 schema.json → dispatcher prepare 侧 MetaError
            s.query("d", "BEGIN", 1700000100);
            s.table_map_is(90, "d", "ghost", 10, 1700000100);
            s.write_is(90, &[(9, "x")], 1700000100);
            s.xid(1700000100);
        });
        let out = f.dir().join("out");
        let mut cfg = cfg_for(f.dir(), &out, true, OnError::Stop);
        cfg.threads = threads;
        let r = run_flashback(&cfg);
        assert!(
            r.is_err(),
            "threads={threads}：stop 模式下 prepare 侧（schema 获取失败）必须返回 Err，\
             got Ok({:?})",
            r.ok()
        );
        assert!(
            leftovers(&out).is_empty(),
            "threads={threads}：半成品不落盘（spec §3.2）——无 final 无 tmp，got {:?}",
            leftovers(&out)
        );
        std::fs::remove_dir_all(f.dir()).ok();
    }
}

// ---------- 用例 3：skip-bad-event → Ok + 头部 WARNING 行 ----------

#[test]
fn flashback_skip_marks_header() {
    let f = Fix::new("skip", |s| {
        good_trx(s); // trx1 → DELETE id=1 块
        push_bad_trx(s); // trx2 → 坏事件：计错、无块
        s.query("d", "BEGIN", 1700000200); // trx3 → INSERT id=2 块
        s.table_map_is(85, "d", "t", 10, 1700000200);
        s.delete_is(85, &[(2, "z")], 1700000200);
        s.xid(1700000200);
    });
    let out = f.dir().join("out");
    let mut cfg = cfg_for(f.dir(), &out, true, OnError::SkipBadEvent);
    cfg.threads = 1;
    let sum = run_flashback(&cfg).expect("skip 模式不中断整跑");
    assert_eq!(sum.errors, 1, "坏事件计入摘要");
    assert_eq!(sum.files, 1);

    let body = std::fs::read_to_string(out.join("flashback.1.sql")).unwrap();
    assert_eq!(
        body.lines().nth(1).unwrap(),
        "-- WARNING: skipped 1 events, positions in stderr",
        "第 2 行 = WARNING 行（T2 钉死：FILE_HEADER 之后、块内容之前）"
    );
    assert_eq!(
        body,
        "SET NAMES utf8mb4;\n\
         -- WARNING: skipped 1 events, positions in stderr\n\
         commit;\nbegin;\nINSERT INTO `d`.`t` (`id`,`b`) VALUES (2,'z');\n\
         commit;\nbegin;\nDELETE FROM `d`.`t` WHERE `id`=1;\n\
         commit;\n",
        "坏事件所在块不出现，前后正常块照常逆序产出"
    );
    assert!(leftovers(&out).iter().all(|n| n == "flashback.1.sql"));

    std::fs::remove_dir_all(f.dir()).ok();
}

// ---------- 用例 4：DDL 排除 + 摘要不污染 ----------

#[test]
fn flashback_ddl_excluded_with_summary() {
    let f = Fix::new("ddl", |s| {
        good_trx(s); // trx1 → DELETE id=1 块
        s.query("d", "CREATE TABLE `z` (`id` int)", 1700000100); // DDL：排除
        s.query("d", "BEGIN", 1700000200); // DDL 占 trx2 → 此 BEGIN = trx3
        s.table_map_is(85, "d", "t", 10, 1700000200);
        s.delete_is(85, &[(1, "a")], 1700000200); // → INSERT (1,'a') 块
        s.xid(1700000200);
    });
    let out = f.dir().join("out");
    let cfg = cfg_for(f.dir(), &out, true, OnError::SkipBadEvent);
    let sum = run_flashback(&cfg).expect("DDL 夹流不改变成功语义（stderr 告警属 tracing）");
    assert_eq!(
        (sum.events, sum.statements, sum.errors),
        (2, 2, 0),
        "RunSummary 不掺 DDL：非行事件不进 events/errors"
    );

    let body = std::fs::read_to_string(out.join("flashback.1.sql")).unwrap();
    assert!(!body.contains("CREATE TABLE"), "DDL 不得进回滚脚本");
    assert_eq!(
        body,
        "SET NAMES utf8mb4;\n\
         commit;\nbegin;\nINSERT INTO `d`.`t` (`id`,`b`) VALUES (1,'a');\n\
         commit;\nbegin;\nDELETE FROM `d`.`t` WHERE `id`=1;\n\
         commit;\n"
    );

    std::fs::remove_dir_all(f.dir()).ok();
}

// ---------- 旁路：to-sql 行为字节不变（abort 哨兵 stop=false 常量守卫） ----------

#[test]
fn to_sql_path_unchanged_by_wiring() {
    let f = Fix::new("tosql-guard", |s| {
        good_trx(s);
        s.query("d", "BEGIN", 1700000100);
        s.table_map_is(85, "d", "t", 10, 1700000100);
        s.delete_is(85, &[(1, "a")], 1700000100);
        s.xid(1700000100);
    });
    let out = f.dir().join("out");
    let mut cfg = cfg_for(f.dir(), &out, true, OnError::SkipBadEvent);
    cfg.work_type = WorkType::ToSql; // 库层覆写不触碰 run_to_sql 语义
    let sum = run_to_sql(&cfg).expect("to-sql 照常");
    assert_eq!(
        (sum.events, sum.statements, sum.errors, sum.files),
        (2, 2, 0, 1)
    );
    assert_eq!(
        std::fs::read_to_string(out.join("to_sql.1.sql")).unwrap(),
        "SET NAMES utf8mb4;\n\
         INSERT INTO `d`.`t` (`id`,`b`) VALUES (1,'a');\n\
         DELETE FROM `d`.`t` WHERE `id`=1;\n",
        "to-sql 正序正向语句逐字节不变"
    );
    std::fs::remove_dir_all(f.dir()).ok();
}

// ---------- 用例 7（P2 T7 实跑暴露的 T3/T5 挂空缺口）：flashback 消费 --schema-dump ----------

/// `run_flashback` 必须与 `run_to_sql` 同款在成功后落 `--schema-dump`
/// （T7 harness 步骤 7 离线回放依赖该产物；此前旗标被 parse 受理但无
/// 消费点——inert affordance，与 c522c33 对 to-sql inert 旗标的裁定同类，
/// 修复取「补消费」而非「拒旗标」：回滚与正向同为 SQL 文本产物，参数面同构）。
#[test]
fn flashback_honors_schema_dump() {
    let f = Fix::new("sdump", good_trx);
    let out = f.dir().join("out");
    let dump = f.dir().join("dump.json");
    let mut cfg = cfg_for(f.dir(), &out, true, OnError::SkipBadEvent);
    cfg.threads = 1;
    cfg.schema_dump = Some(dump.clone());
    run_flashback(&cfg).expect("run_flashback ok");
    let text = std::fs::read_to_string(&dump).expect("flashback 必须落 --schema-dump 文件");
    assert!(
        text.contains("\"d\"") && text.contains("version"),
        "dump 形如 schema 文件: {text}"
    );
    std::fs::remove_dir_all(f.dir()).ok();
}
