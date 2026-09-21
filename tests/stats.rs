//! P2 T4 stats 端到端（库级）：Step 2 场景的**真实 Synth 字节布局**双文件
//! binlog → `run_stats` → 两报表逐字节 = `src/stats/mod.rs` 单元 golden
//! 同一字符串（跨层一致钉死）；`--stats-json` 双件产出；threads=1/4 输出
//! 等字节；schema 缺表（d.t9 ×2 rows 事件）→ skipped=2 进尾注与 Display。
//!
//! 位点全部由 Synth 真实事件尺寸推得（探针实测：FDE 4..120、tm_int=38、
//! write_int=31+5r、tm_is=41、write_is=19+Σ(6+len(b))、update_is=26+Σ(before+after)、
//! query=33+db+sql、xid=27），fixture 内逐一 assert 闭环自证。

use std::path::{Path, PathBuf};
use std::process;

use clap::Parser;
use my2sql_rs::config::{Cli, Command, Config};
use my2sql_rs::pipeline::run_stats;

#[path = "common/synth.rs"]
mod synth;
use synth::Synth;

const T0: u32 = 1700000000;

/// 与 `src/stats/mod.rs` tests 模块逐字节一致的 golden（cfg(test) 私有，
/// 集成测试不可达 → 按简报 Step 5 拷入同一字符串；两侧任一改动必须同步
/// 另一侧，Step 2/5 的「跨层一致钉死」由此成立）。
const STATUS_GOLDEN: &str = concat!(
    "binlog            starttime           stoptime            startpos   stoppos    inserts  updates  deletes  database        table               \n",
    "mysql-bin.000001  2023-11-14_22:13:20 2023-11-14_22:13:22 120        414        5        0        1        d               t2                  \n",
    "mysql-bin.000001  2023-11-14_22:13:20 2023-11-14_22:13:20 253        340        0        1        0        d               t1                  \n",
    "mysql-bin.000002  2023-11-14_22:13:25 2023-11-14_22:13:25 159        254        3        0        0        d               t1                  \n",
    "mysql-bin.000002  2023-11-14_22:13:29 2023-11-14_22:13:29 335        409        1        0        0        d               t3                  \n",
    "mysql-bin.000002  2023-11-14_22:13:29 2023-11-14_22:13:29 409        483        1        0        0        d               t2                  \n",
    "# skipped events: 2\n",
);
const BIGLONG_GOLDEN: &str = concat!(
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

// ---------- fixture ----------

/// 落盘双文件 binlog + schema.json（d.t9 故意缺失 → 2 次 schema 错误 =
/// skipped=2）。返回 fixture 根目录。
fn fix(tag: &str) -> PathBuf {
    let root = tmp_dir(tag);
    let binlog = root.join("binlog");
    std::fs::create_dir_all(&binlog).unwrap();
    std::fs::write(binlog.join("mysql-bin.000001"), file1()).unwrap();
    std::fs::write(binlog.join("mysql-bin.000002"), file2()).unwrap();
    std::fs::write(
        root.join("schema.json"),
        r#"{"version":1,"tables":[
            {"db":"d","table":"t1","cols":[
                {"name":"id","type_name":"int","unsigned":false},
                {"name":"b","type_name":"varchar","unsigned":false}],
                "pk":["id"],"uks":[]},
            {"db":"d","table":"t2","cols":[
                {"name":"id","type_name":"int","unsigned":false}],
                "pk":["id"],"uks":[]},
            {"db":"d","table":"t3","cols":[
                {"name":"id","type_name":"int","unsigned":false}],
                "pk":["id"],"uks":[]}]}"#,
    )
    .unwrap();
    root
}

/// 文件 1 = 单元 golden 的 E1..E5（rows-before-BEGIN、update 对、delete、
/// XID commit）。位点逐一 assert，与 golden 数字闭环。
fn file1() -> Vec<u8> {
    let mut s = Synth::new();
    assert_eq!((4, 120), (4, s.bytes.len() as u32)); // FDE
    s.table_map(81, "d", "t2", 1, T0); // 120..158
    assert_eq!(158, s.bytes.len() as u32);
    let (sp, ep) = s.write(81, 1, &[vec![1], vec![2], vec![3], vec![4], vec![5]], T0);
    assert_eq!((sp, ep), (158, 214)); // fact f1 = (120, 214)
    assert_eq!(s.query("d", "BEGIN", T0), (214, 253)); // Begin pos = 214
    let (sp, _) = s.table_map_is(82, "d", "t1", 10, T0); // 253..294
    let (_, ep) = s.update_is(82, &[((1, "a"), (2, "b"))], T0); // 294..340
    assert_eq!((sp, ep), (253, 340)); // fact f3 = (253, 340)
    let (tsp, _) = s.table_map(81, "d", "t2", 1, T0 + 2); // 340..378
    let (sp, ep) = s.delete(81, 1, &[vec![9]], T0 + 2); // 378..414
    assert_eq!((tsp, ep), (340, 414)); // fact f4 = (tm 起点, rows 终点) = (340, 414)
    assert_eq!(sp, 378, "rows 事件自身起始（非 fact 口径）");
    assert_eq!(s.xid(T0 + 3), (414, 441)); // Commit pos = 441
    s.bytes
}

/// 文件 2 = E6..E12 + 尾部 d.t9 缺表双 rows 块（×2 计数跳过）。
fn file2() -> Vec<u8> {
    let mut s = Synth::new();
    assert_eq!(s.bytes.len(), 120); // FDE 与文件 1 同尺寸
    assert_eq!(s.query("d", "BEGIN", T0 + 4), (120, 159)); // E6 Begin = 120
    let (sp, _) = s.table_map_is(82, "d", "t1", 10, T0 + 5); // 159..200
    let (_, ep) = s.write_is(
        82,
        &[(1, "a"), (2, "bb"), (3, "cc")], // 7+8+8B 行 = 事件 54B → 200..254
        T0 + 5,
    );
    assert_eq!((sp, ep), (159, 254)); // fact f7 = (159, 254)
    assert_eq!(s.query("d", "ROLLBACK", T0 + 5), (254, 296)); // Rollback pos = 296
    assert_eq!(s.query("d", "BEGIN", T0 + 6), (296, 335)); // E9 Begin pos = 296
    let (sp, _) = s.table_map(83, "d", "t3", 1, T0 + 9); // 335..373
    let (_, ep) = s.write(83, 1, &[vec![4]], T0 + 9); // 373..409
    assert_eq!((sp, ep), (335, 409)); // fact f10 = (335, 409)
    let (sp, _) = s.table_map(81, "d", "t2", 1, T0 + 9); // 409..447
    let (_, ep) = s.write(81, 1, &[vec![5]], T0 + 9); // 447..483
    assert_eq!((sp, ep), (409, 483)); // fact f11 = (409, 483)
    assert_eq!(s.xid(T0 + 9), (483, 510)); // E12 Commit pos = 510（不命中负例）
    // d.t9 ∉ schema：2 个 rows 块 = 2 次计数跳过（skipped=2）
    s.table_map(89, "d", "t9", 1, T0 + 9);
    s.write(89, 1, &[vec![1]], T0 + 9);
    s.table_map(89, "d", "t9", 1, T0 + 9);
    s.write(89, 1, &[vec![2]], T0 + 9);
    s.bytes
}

/// 真实解析/校验路径构造 stats 形态 Config（T5 前库层直改字段）。
fn cfg_stats(root: &Path, out: &Path, json: bool, threads: usize) -> Config {
    let cli = Cli::try_parse_from([
        "my2sql-rs",
        "to-sql",
        "--binlog-dir",
        root.join("binlog").to_str().unwrap(),
        "--start-file",
        "mysql-bin.000001",
        "--schema-file",
        root.join("schema.json").to_str().unwrap(),
        "--output-dir",
        out.to_str().unwrap(),
        // 跨文件泵的唯一开关（T12 裁定 7：仅设 stop 条件才续读下一文件）；
        // 未给 stop_pos → 位点分量 u32::MAX = 纯文件名截断，全事件照常流过。
        "--stop-file",
        "mysql-bin.000002",
    ])
    .expect("cli parse");
    let mut c = match cli.cmd {
        Command::ToSql(a) => Config::validate(a).expect("config validate"),
    };
    c.print_interval = 5;
    c.big_trx_rows = 3;
    c.long_trx_seconds = 1;
    c.stats_json = json;
    c.threads = threads;
    c
}

fn tmp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut p = std::env::temp_dir();
    p.push(format!("my2sql-p2t4-{tag}-{}.{nanos}", process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

// ---------- 用例 1：threads=4 报表逐字节 + 摘要/Display + skipped 尾注 ----------

#[test]
fn stats_e2e_reports_match_unit_golden() {
    let root = fix("e2e4");
    let out = root.join("out");
    let run = run_stats(&cfg_stats(&root, &out, false, 4)).expect("run_stats ok");
    assert_eq!(
        (
            run.summary.events,
            run.summary.statements,
            run.summary.errors
        ),
        (12, 12, 2),
        "events = 6 rows + 6 标记（缺表不派发不计）；statements = Σ行计数；errors = skipped"
    );
    assert_eq!(
        run.to_string(),
        "stats done: events=12, statements rows=12, windows flushed=3, big/long trx=2, skipped=2",
        "StatsRun Display 钉死（简报 Step 5 摘要面）"
    );
    assert_eq!(
        std::fs::read_to_string(out.join("binlog_status.txt")).unwrap(),
        STATUS_GOLDEN,
        "e2e binlog_status.txt 必须与单元 golden 同一字符串"
    );
    assert_eq!(
        std::fs::read_to_string(out.join("biglong_trx.txt")).unwrap(),
        BIGLONG_GOLDEN,
        "e2e biglong_trx.txt 必须与单元 golden 同一字符串"
    );
    assert_eq!(
        std::fs::read_dir(&out).unwrap().count(),
        2,
        "stats_json=false → 仅两 txt，无 jsonl 泄漏"
    );
    assert_eq!((run.windows, run.biglong), (3, 2));

    std::fs::remove_dir_all(&root).ok();
}

// ---------- 用例 2：--stats-json 双件产出逐字节 ----------

#[test]
fn stats_e2e_jsonl_pair() {
    let root = fix("e2ej");
    let out = root.join("out");
    run_stats(&cfg_stats(&root, &out, true, 4)).expect("run_stats ok");
    assert_eq!(
        std::fs::read_to_string(out.join("binlog_status.jsonl")).unwrap(),
        STATUS_JSONL_GOLDEN
    );
    assert_eq!(
        std::fs::read_to_string(out.join("biglong_trx.jsonl")).unwrap(),
        BIGLONG_JSONL_GOLDEN
    );
    assert_eq!(
        std::fs::read_dir(&out).unwrap().count(),
        4,
        "两 txt + 两 jsonl"
    );

    std::fs::remove_dir_all(&root).ok();
}

// ---------- 用例 3：threads=1 直通与并行逐字节等 ----------

#[test]
fn stats_e2e_threads1_equals_threads4() {
    let root = fix("e2e1");
    let out4 = root.join("out4");
    let out1 = root.join("out1");
    run_stats(&cfg_stats(&root, &out4, true, 4)).expect("threads=4 ok");
    run_stats(&cfg_stats(&root, &out1, true, 1)).expect("threads=1 ok");
    for f in [
        "binlog_status.txt",
        "biglong_trx.txt",
        "binlog_status.jsonl",
        "biglong_trx.jsonl",
    ] {
        assert_eq!(
            std::fs::read(out1.join(f)).unwrap(),
            std::fs::read(out4.join(f)).unwrap(),
            "threads=1 直通输出必须与并行路径逐字节一致：{f}"
        );
    }

    std::fs::remove_dir_all(&root).ok();
}

// ---------- 用例 4：缺 output-dir = 硬失败（报表即产物） ----------

#[test]
fn stats_e2e_requires_output_dir() {
    let root = fix("e2enoud");
    let mut cfg = cfg_stats(&root, &root.join("out"), false, 2);
    cfg.output_dir = None;
    let e = run_stats(&cfg).expect_err("无 output-dir 必须 Err");
    assert!(e.to_string().contains("--output-dir"), "{e}");

    std::fs::remove_dir_all(&root).ok();
}

// ---------- 用例 5：显式 --on-error stop 复用 T3 升 Err 链（默认 skip 在 T5） ----------

#[test]
fn stats_e2e_on_error_stop_escalates() {
    let root = fix("e2estop");
    let out = root.join("out");
    let mut cfg = cfg_stats(&root, &out, false, 2);
    cfg.on_error = my2sql_rs::config::OnError::Stop;
    let e = run_stats(&cfg).expect_err("stop 形态缺表事件 → 整跑 Err（prepare_fail 升格）");
    assert!(e.to_string().contains("`d.t9`"), "{e}");
    // 报表文件已由 Aggregator::new 建出（Err 路径不 finish = 无尾注半成品，
    // 重跑 O_TRUNC 覆盖——run_stats 注释裁定的字节面）。
    let status = std::fs::read_to_string(out.join("binlog_status.txt")).unwrap();
    assert!(!status.contains("# skipped events"), "{status}");

    std::fs::remove_dir_all(&root).ok();
}
