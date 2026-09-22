//! P3 T2 对测（byte-parity 双通道闸）：`tests/common/synth.rs::frame_bytes`
//! 导出的文件同构全帧经 `ReplSource` ≡ 同一字节流过 `FileReader`。
//!
//! 与 src/repl/source.rs 内置单测互为两测（冻结区下的有意重复由双侧
//! 同红钉死）：本文件走 **tests 公共夹具** 通道（P2 起的 e2e 事件族，
//! 含 rows 解码实义体），源内单测走 build_frame 手搓通道（合成帧/心跳/
//! 断链形态）。CRC 两态（无 checksum 5.6 流 / CRC32 8.0 流含 FDE 特例
//! 掩位）各拉一遍：binlog 名（含 rotate 更名时序）/start_pos/end_pos/
//! timestamp/kind/body 逐字段一致。

#![cfg_attr(test, allow(unused, non_snake_case))] // live 件位点变量沿用 binlog 记号（pA/pB/pe…）

#[path = "common/synth.rs"]
mod synth;

use std::collections::VecDeque;
use std::io::Cursor;

use my2sql_rs::binlog::file_reader::FileReader;
use my2sql_rs::pipeline::filter::Filters;
use my2sql_rs::pipeline::source::{EventSource, RawEvent};
use my2sql_rs::repl::ReplSource;
use my2sql_rs::repl::transport::{Frame, FrameStream, ReplError};

/// 帧队列假流（集成测试面本地注入件——lib 内 `test_support` 为
/// `cfg(test)` 不可见）：耗尽即 `Ok(None)`（消费方干净停止语义）。
struct VecDequeStream {
    queue: VecDeque<Frame>,
}

impl VecDequeStream {
    fn new(frames: Vec<Vec<u8>>) -> Self {
        Self {
            queue: frames
                .into_iter()
                .map(|bytes| Frame {
                    bytes,
                    binlog_hint: None,
                })
                .collect(),
        }
    }
}

impl FrameStream for VecDequeStream {
    fn next_frame(&mut self) -> Result<Option<Frame>, ReplError> {
        Ok(self.queue.pop_front())
    }
}

/// 事件序列（e2e 家族：FDE + query/tm/rows/xid + 真文件 rotate + 第二组）
/// ——rotate 之后续事件，钉更名时序。
fn scripted() -> Vec<u8> {
    let mut s = synth::Synth::new();
    s.query("t10", "BEGIN", 1001);
    s.table_map(7, "t10", "u", 1, 1001);
    s.write(7, 1, &[vec![42]], 1002);
    s.xid(1003);
    let mut rb = 4u64.to_le_bytes().to_vec(); // 下一文件起始位
    rb.extend_from_slice(b"mysql-bin.000002");
    s.push(4, 1004, &rb); // ROTATE（真实文件形态：log_pos 正常）
    s.query("t10", "BEGIN", 1005);
    s.table_map(8, "t10", "v", 1, 1005);
    s.write(8, 1, &[vec![43]], 1006);
    s.xid(1007);
    s.bytes
}

fn collect_file(bytes: Vec<u8>) -> Vec<RawEvent> {
    let mut r = FileReader::new(
        "mysql-bin.000001".into(),
        Cursor::new(bytes),
        Filters::none(),
    )
    .unwrap();
    let mut v = Vec::new();
    while let Some(e) = r.next().unwrap() {
        v.push(e);
    }
    v
}

fn collect_repl(frames: Vec<Vec<u8>>) -> Vec<RawEvent> {
    let mut s = ReplSource::new(
        Box::new(VecDequeStream::new(frames)),
        "mysql-bin.000001".into(),
        Filters::none(),
        None,
    );
    let mut v = Vec::new();
    while let Some(e) = s.next().unwrap() {
        v.push(e);
    }
    v
}

#[test]
fn synth_frame_export_is_byte_equal_through_repl_source() {
    for with_crc in [false, true] {
        let raw = scripted();
        let src = synth::Synth { bytes: raw };
        let frames = src.frame_bytes(with_crc);
        // 帧自洽烟雾：帧数 = 事件数（FDE 起 10 帧）
        assert_eq!(frames.len(), 10, "帧数（with_crc={with_crc}）");
        // repl 通道文件视图 = magic + 帧拼接（FileReader 吃同一字节）
        let mut file = b"\xfebin".to_vec();
        for f in &frames {
            file.extend_from_slice(f);
        }
        let a = collect_file(file.clone());
        let b = collect_repl(frames);
        assert_eq!(a.len(), b.len(), "事件数（with_crc={with_crc}）");
        assert_eq!(a.len(), 7, "FDE/TABLE_MAP 双通道均由源消化");
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(x.binlog, y.binlog, "binlog（with_crc={with_crc}）");
            assert_eq!(x.start_pos, y.start_pos, "start_pos（with_crc={with_crc}）");
            assert_eq!(x.end_pos, y.end_pos, "end_pos（with_crc={with_crc}）");
            assert_eq!(x.timestamp, y.timestamp, "timestamp（with_crc={with_crc}）");
            assert_eq!(
                format!("{:?}", x.kind),
                format!("{:?}", y.kind),
                "kind（with_crc={with_crc}）"
            );
            assert_eq!(x.body, y.body, "body 字节（with_crc={with_crc}）");
            assert_eq!(
                x.tm.is_some(),
                y.tm.is_some(),
                "tm 有无（with_crc={with_crc}）"
            );
            assert_eq!(
                x.tm.as_ref().map(|t| t.table_id),
                y.tm.as_ref().map(|t| t.table_id),
                "tm table_id（with_crc={with_crc}）"
            );
        }
        // 绝对口径抽查（防「双侧同错」对称盲区）：rows start = table_map
        // 起始、rotate 先旧名后切名、更名后事件记新名。
        assert_eq!(
            format!("{:?}", b[0].kind),
            "Query(\"BEGIN\")".to_string(),
            "b0=BEGIN"
        );
        assert_eq!(b[1].start_pos, b[0].end_pos, "rows start = table_map 起始");
        assert!(b[1].tm.is_some() && b[5].tm.is_some(), "rows 携带 tm");
        assert_eq!(b[1].tm.as_ref().map(|t| t.table_id), Some(7));
        assert_eq!(b[5].tm.as_ref().map(|t| t.table_id), Some(8));
        let rotate = &b[3];
        assert!(
            matches!(&rotate.kind, my2sql_rs::pipeline::source::RawKind::Rotate(n) if n == "mysql-bin.000002"),
            "rotate kind：{:?}",
            rotate.kind
        );
        assert_eq!(rotate.binlog, "mysql-bin.000001", "rotate 记旧名");
        assert_eq!(b[6].binlog, "mysql-bin.000002", "rotate 后事件记新名");
        // 帧间连续性（导出器位点重排正确性，with_crc 态 log_pos 已 +4 重排）
        for w in b.windows(2) {
            if w[0].binlog == w[1].binlog {
                assert!(
                    w[0].end_pos <= w[1].end_pos,
                    "位点单调（with_crc={with_crc}）"
                );
            }
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// P3 T5：live docker 组（简报 Step 1）——`#[ignore]` + `MY2SQL_TEST_URI` 门，
// 与 make live 同型本地跑。跑法（8.0 容器）：
//   NAME=p3t5-$$; docker run -d --name $NAME -e MYSQL_ALLOW_EMPTY_PASSWORD=1 \
//     -e TZ=UTC -p 127.0.0.1::3306 mysql:8.0 --log-bin=mysql-bin \
//     --binlog-format=row --server-id=1
//   PORT=$(docker port $NAME 3306/tcp | sed 's/.*://')
//   MY2SQL_TEST_URI="mysql://root@127.0.0.1:$PORT" \
//     cargo test --test repl -- --ignored --nocapture
//   docker rm -f $NAME
// ────────────────────────────────────────────────────────────────────────────

use std::sync::mpsc;
use std::time::{Duration, Instant};

use clap::Parser;
use my2sql_rs::config::{Cli, Command, Config};
use my2sql_rs::pipeline::run_repl;
use mysql::prelude::Queryable;

fn live_uri() -> String {
    std::env::var("MY2SQL_TEST_URI").expect("MY2SQL_TEST_URI required for ignored live tests")
}

/// live 组公共 Config 构造（完整 argv 直解，validate_repl 全过）。
fn live_cfg(extra: &[&str]) -> Config {
    let mut argv = vec![
        "my2sql-rs".to_string(),
        "repl".to_string(),
        "--binlog-dir".to_string(),
        "/nonused".to_string(),
        "--uri".to_string(),
        live_uri(),
    ];
    argv.extend(extra.iter().map(|s| s.to_string()));
    let cli = Cli::try_parse_from(&argv).expect("live cli parse");
    let Command::Repl(a) = cli.cmd else {
        panic!("repl expected")
    };
    Config::validate_repl(a).expect("live repl validate")
}

/// now 哨兵 + 流式新写入：起流前预灌段 A **绝不得**出现（now 定位），
/// 起流后新灌段 B 必须逐事务实时落盘（完成前即可读到 B 首行 = 流式
/// 刷盘证据），`--stop-datetime` 到点优雅收尾（Ok、errors=0、终档在场）。
#[test]
#[ignore = "requires live mysql container + MY2SQL_TEST_URI"]
fn repl_locates_now_and_streams_new_writes() {
    let uri = live_uri();
    let mut conn = mysql::Conn::new(uri.as_str()).expect("live connect");
    conn.query_drop("DROP DATABASE IF EXISTS p3t5live").unwrap();
    conn.query_drop("CREATE DATABASE p3t5live").unwrap();
    conn.query_drop("CREATE TABLE p3t5live.t (id INT PRIMARY KEY AUTO_INCREMENT, tag VARCHAR(32))")
        .unwrap();
    for i in 0..3 {
        conn.query_drop(format!("INSERT INTO p3t5live.t (tag) VALUES ('PRE_A{i}')"))
            .unwrap();
    }

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let out = std::env::temp_dir().join(format!("my2sql-p3t5-now-{nanos}"));
    // 裸默认位点（--start-file "" + clap start_pos=4）= now 哨兵实弹版
    let stop_unix = chrono::Utc::now().timestamp() + 12;
    let stop = chrono::DateTime::from_timestamp(stop_unix, 0)
        .unwrap()
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let cfg = live_cfg(&[
        "--start-file",
        "",
        "--stop-datetime",
        &stop,
        "--output-dir",
        out.to_str().unwrap(),
        "--server-id",
        "4251",
        "--db",
        "p3t5live",
        "--threads",
        "1",
        "--heartbeat-secs",
        "5",
    ]);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(run_repl(&cfg));
    });
    // 让 repl 侧完成 SHOW MASTER STATUS 定位 + dump 注册（预灌段 A 全部
    // 落在定位点之前）。
    std::thread::sleep(Duration::from_millis(2500));

    // 起流后新流量段 B；轮询输出文件出现 B0 = 流中（未 finish）即可见。
    let out2 = out.clone();
    let seed = std::thread::spawn(move || {
        let mut conn = mysql::Conn::new(uri.as_str()).expect("seeder connect");
        let deadline = Instant::now() + Duration::from_secs(75);
        for i in 0u32.. {
            conn.query_drop(format!(
                "INSERT INTO p3t5live.t (tag) VALUES ('B{i}') /*{}*/",
                chrono::Utc::now().timestamp()
            ))
            .expect("seed insert");
            std::thread::sleep(Duration::from_millis(900));
            let seen = std::fs::read_dir(&out2)
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .filter(|e| e.path().extension().is_some_and(|x| x == "sql"))
                        .any(|e| {
                            std::fs::read_to_string(e.path()).is_ok_and(|s| {
                                s.contains("'B0") || s.contains("(1,'B0") || s.contains(",'B0")
                            })
                        })
                })
                .unwrap_or(false);
            // 严格按 unix 秒整数比较（字符串比较会因事件 ts 向下取整在
            // stop 边界秒误判「未到」）。
            let past = chrono::Utc::now().timestamp() > stop_unix;
            if (seen && past) || Instant::now() > deadline {
                // stop-datetime 是事件驱动语义（上游同构）：仅当某事件
                // ts ≥ stop 到达才触发收尾。再等 1.2s（保证脉冲事件 ts
                // 严格越过 stop）灌一枚 STOP_PULSE，钉死优雅收尾。
                std::thread::sleep(Duration::from_millis(1200));
                conn.query_drop("INSERT INTO p3t5live.t (tag) VALUES ('STOP_PULSE')")
                    .expect("seed pulse");
                break;
            }
        }
    });

    let sum = rx
        .recv_timeout(Duration::from_secs(90))
        .expect("repl run must finish by stop-datetime")
        .expect("run_repl → Ok on stop hit (graceful finalize)");
    let _ = seed.join();

    assert_eq!(sum.errors, 0, "errors 必须为 0");
    assert!(sum.events > 0, "必须消费到新写入（events={}）", sum.events);
    assert!(sum.files >= 1, "有 .sql 产物（files={}）", sum.files);
    // 产物：非空 + SET NAMES 头 + 含 B0 + **绝不含预灌段 A**（now 定位铁证）
    let sqls: Vec<std::path::PathBuf> = std::fs::read_dir(&out)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "sql"))
        .collect();
    assert_eq!(sqls.len(), sum.files, "files 计数与实物一致");
    let mut text = String::new();
    for p in &sqls {
        let t = std::fs::read_to_string(p).unwrap();
        assert!(
            t.starts_with("SET NAMES utf8mb4;\n"),
            "产物须以 SET NAMES 头起始: {}",
            p.display()
        );
        text.push_str(&t);
    }
    assert!(text.contains("'B0"), "新流量必须入产: {text}");
    assert!(!text.contains("PRE_A"), "now 位点不得回放预灌旧流量");
    // 终档在场（收尾链：drain→flush→checkpoint→finish）
    assert!(
        out.join("resume.json").is_file(),
        "干净收尾必须落终档 checkpoint"
    );
    conn.query_drop("DROP DATABASE p3t5live").ok();
    std::fs::remove_dir_all(&out).ok();
}

/// 起始位点不存在/被 purge → 立即硬错终止（不重连），文案逐字含简报钉。
#[test]
#[ignore = "requires live mysql container + MY2SQL_TEST_URI"]
fn repl_refuses_purged_start() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let out = std::env::temp_dir().join(format!("my2sql-p3t5-purged-{nanos}"));
    let cfg = live_cfg(&[
        "--start-file",
        "mysql-bin.999999",
        "--start-pos",
        "999999999",
        "--output-dir",
        out.to_str().unwrap(),
        "--server-id",
        "4252",
        "--heartbeat-secs",
        "0",
    ]);
    let t0 = Instant::now();
    let e = run_repl(&cfg).expect_err("purged/不存在起点必须硬错");
    let s = e.to_string();
    assert!(
        s.contains("replication position ... does not exist on master (binlog purged): choose a newer start"),
        "须含逐字 purge 终止文案，got: {s}"
    );
    assert!(
        t0.elapsed() < Duration::from_secs(30),
        "终止不该进退避循环: {:?}",
        t0.elapsed()
    );
    std::fs::remove_dir_all(&out).ok();
}

// ────────────────────────────────────────────────────────────────────────────
// P3 T6a：等价性总闸（spec §7-1）——repl 无 Go 裁判（§8），「repl 流 ≡
// file 模式同段重跑，逐字节」**就是**正确性权威本身。
//
// 编排（全量复用 tools/repl-e2e-lib.sh，T7 矩阵同一对函数）：
//   1. seed_schema（t_doc 含 JSON+BLOB、t_ord）——DDL 全部落在窗口之前；
//   2. 原子 SHOW MASTER STATUS 取 (f0,p0) → 灌混合 DML 段 A（3 轮：双表
//      insert/update/delete + 10 行显式事务 + 回滚事务 + JSON/BLOB/中文值）
//      → 再取 (f1,p1) → 灌段 B（stop 之后的流量，两侧必须均不可见）；
//   3. repl 子进程：显式 file+pos 起止（不用 stop-datetime——安静主库的
//      事件驱动收尾在本件无须冒险，钉死显式位点窗口）；
//   4. docker cp 取 [f0..f1] 区段 binlog → 同窗同旗标跑 to-sql 子进程；
//   5. 比较器 = 双侧全部 .sql **原始字节逐一相等** + 文件名集合相等 +
//      系统 `diff -r -x resume.json` rc=0 双保险。选项对平由「同一 window
//      参数组拼两侧 argv」结构性保证（--dml 双侧同为缺省全量、
//      --add-extra-info 双侧同开、--db 同名单；--server-id/--heartbeat-secs
//      为 repl 独有且只触传输层，不入产物字节）。
//      若两侧有差 = 真缺陷——不削弱比较器、不加排除项，diff 原文上报。
//
// 跑法：`make repl-test`（lib 起唯一名容器 + EXIT trap 清场，导出
// MY2SQL_TEST_URI / MY2SQL_TEST_CTR）。产物留档：MY2SQL_T6_KEEP=<dir>
// 把整个工作目录（repl/ file/ bins/）拷入该路径。
// ────────────────────────────────────────────────────────────────────────────

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command as ProcCommand, Stdio};

/// source 正交件并调一个函数（灌流器/取段与 T7 矩阵同源同码）。
fn libf(call: &str) -> String {
    let lib = concat!(env!("CARGO_MANIFEST_DIR"), "/tools/repl-e2e-lib.sh");
    let out = ProcCommand::new("bash")
        .arg("-c")
        .arg(format!("set -euo pipefail; source '{lib}'; {call}"))
        .output()
        .unwrap_or_else(|e| panic!("spawn bash for `{call}`: {e}"));
    assert!(
        out.status.success(),
        "lib `{call}` rc={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("lib stdout utf8")
}

/// 单次调用不设防版（rc≠0 回传分类，供并发灌流重试；常规串行道仍走 libf）。
fn libf_try(call: &str) -> Result<String, (Option<i32>, String)> {
    let lib = concat!(env!("CARGO_MANIFEST_DIR"), "/tools/repl-e2e-lib.sh");
    let out = ProcCommand::new("bash")
        .arg("-c")
        .arg(format!("set -euo pipefail; source '{lib}'; {call}"))
        .output()
        .unwrap_or_else(|e| panic!("spawn bash for `{call}`: {e}"));
    if out.status.success() {
        return Ok(String::from_utf8(out.stdout).expect("lib stdout utf8"));
    }
    Err((
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

/// 并发灌流锁冲突重试（T6b mtw 实踩：双流同库同表 UPDATE…LIKE 相交间隙锁
/// 互杀，ERROR 1213 是 InnoDB 正常并发行为——双流交错正是本件目的；另见
/// docker exec 在并发 exec 风暴下的 rc=128/空 stderr 瞬断——mysql 客户端
/// 自身报错恒 rc=1 带 ERROR 行，128 空文本无过可指）。这是 harness 力学
/// 而非断言放水：灌流脚本值 = f(tag,r) 纯函数、schema 无 UNIQUE 约束，
/// 整脚本重跑幂等收敛；其余失败（连接/认证/磁盘）当场照红。
fn libf_lock_retry(label: &str, call: &str, tries: u32) {
    let mut last = String::new();
    for k in 1..=tries {
        match libf_try(call) {
            Ok(_) => {
                if k > 1 {
                    println!("[harness:{label}] `{call}` ok @attempt {k}（前误: {last}）");
                }
                return;
            }
            Err((rc, err)) => {
                let transient = err.contains("Deadlock found")
                    || err.contains("Lock wait timeout")
                    || (rc == Some(128) && err.trim().is_empty());
                assert!(
                    transient,
                    "[harness:{label}] lib `{call}` rc={rc:?} stderr={err}"
                );
                last = format!("rc={rc:?} {}", err.trim());
                std::thread::sleep(Duration::from_millis(150));
            }
        }
    }
    panic!("[harness:{label}] lib `{call}` {tries} 次仍败: {last}");
}

/// 原子位点：一条 SHOW MASTER STATUS 同回 File+Position（8.4 1064 →
/// SHOW BINARY LOG STATUS，与 src/metadata/store.rs 同臂）。
fn master_pos(conn: &mut mysql::Conn) -> (String, u32) {
    let mut rows: Vec<mysql::Row> = match conn.query("SHOW MASTER STATUS") {
        Ok(r) => r,
        Err(mysql::Error::MySqlError(m)) if m.code == 1064 => conn
            .query("SHOW BINARY LOG STATUS")
            .expect("8.4 SHOW BINARY LOG STATUS"),
        Err(e) => panic!("SHOW MASTER STATUS: {e}"),
    };
    let row = rows
        .pop()
        .expect("SHOW MASTER STATUS returns exactly one row (log-bin on)");
    let file: String = row.get(0).expect("File column");
    let pos: u64 = row.get(1).expect("Position column");
    (file, pos as u32)
}

/// 跑 my2sql-rs 二进制（真 CLI 面 = 产品路径），带硬超时防静默挂死。
fn run_bin(args: &[&str], label: &str, deadline: Duration) -> std::process::Output {
    let mut child = ProcCommand::new(env!("CARGO_BIN_EXE_my2sql-rs"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("[{label}] spawn: {e}"));
    let t0 = Instant::now();
    loop {
        if child
            .try_wait()
            .unwrap_or_else(|e| panic!("[{label}] try_wait: {e}"))
            .is_some()
        {
            break;
        }
        if t0.elapsed() > deadline {
            child.kill().ok();
            child.wait().ok();
            panic!("[{label}] {args:?} exceeded {deadline:?}（stop 未到点？）");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = child
        .wait_with_output()
        .unwrap_or_else(|e| panic!("[{label}] collect: {e}"));
    eprintln!(
        "[{label}] rc={:?} stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// 目录内全部 .sql（名字 → 原始字节）。resume.json 等非 .sql 天然不在集合
/// （repl 侧独有的 checkpoint 产物，file 侧无对应物——比较器只钉 SQL 面）。
fn sql_map(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut m = BTreeMap::new();
    for e in std::fs::read_dir(dir).unwrap_or_else(|er| panic!("read {}: {er}", dir.display())) {
        let p = e.expect("dir entry").path();
        if p.extension().is_some_and(|x| x == "sql") {
            let name = p
                .file_name()
                .expect("file name")
                .to_string_lossy()
                .into_owned();
            let old = m.insert(name, std::fs::read(&p).expect("read sql"));
            assert!(old.is_none(), "重复 .sql 名不应存在");
        }
    }
    m
}

/// 首个差异字节偏移 + 双上下文摘录（红例上报用，禁虚账）。
fn diff_report(name: &str, a: &[u8], b: &[u8]) -> String {
    let off = a
        .iter()
        .zip(b.iter())
        .position(|(x, y)| x != y)
        .unwrap_or_else(|| a.len().min(b.len()));
    let win = |s: &[u8]| {
        let lo = off.saturating_sub(40);
        String::from_utf8_lossy(&s[lo..s.len().min(off + 40)]).into_owned()
    };
    format!(
        "{name}: 字节漂移 @offset {off} (repl len={}, file len={})\n  repl 上下文: {:?}\n  file 上下文: {:?}",
        a.len(),
        b.len(),
        win(a),
        win(b)
    )
}

#[test]
#[ignore = "requires live mysql container (make repl-test): MY2SQL_TEST_URI + MY2SQL_TEST_CTR"]
fn repl_stream_equals_file_mode_byte_for_byte() {
    let uri = live_uri();
    let ctr = std::env::var("MY2SQL_TEST_CTR")
        .expect("MY2SQL_TEST_CTR required（binlog docker cp 的容器名；make repl-test 自动导出）");
    let db = "p3t6eq".to_string();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("my2sql-p3t6eq-{nanos}"));
    for d in ["repl", "file", "bins"] {
        std::fs::create_dir_all(root.join(d)).expect("mkdir work dirs");
    }

    // panic 也必须清场（db + 临时目录；容器由 make 的 EXIT trap 负责）——
    // 失败现场先落 KEEP 路径或原地保留并打印，绝不静默蒸发。
    let body = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Vec<String> {
        let mut notes = Vec::new();
        let repl_out = root.join("repl");
        let file_out = root.join("file");
        let bins = root.join("bins");
        let mut conn = mysql::Conn::new(uri.as_str()).expect("live connect");
        libf(&format!("p3e2e_seed_schema {ctr} {db}"));
        let (f0, p0) = master_pos(&mut conn);
        libf(&format!("p3e2e_feed_mixed {ctr} {db} 3 A"));
        let (f1, p1) = master_pos(&mut conn);
        libf(&format!("p3e2e_feed_mixed {ctr} {db} 1 B"));
        assert_eq!(f0, f1, "本件窗口须在同一 binlog（矩阵跨档件归 T6b/T7）");
        assert!(p1 > p0, "窗口必须前进：{f0}:{p0} .. {f1}:{p1}");

        // ── 双侧共用的窗口/过滤/文本旗标组（对平=结构性，不靠人肉对齐）──
        let window: Vec<String> = vec![
            "--uri".into(),
            uri.clone(),
            "--start-file".into(),
            f0.clone(),
            "--start-pos".into(),
            p0.to_string(),
            "--stop-file".into(),
            f1.clone(),
            "--stop-pos".into(),
            p1.to_string(),
            "--db".into(),
            db.clone(),
            "--add-extra-info".into(),
        ];
        let sid = (4260 + std::process::id() % 1000).to_string();
        let (ro, fo, bd) = (
            repl_out.to_str().unwrap(),
            file_out.to_str().unwrap(),
            bins.to_str().unwrap(),
        );

        let mut repl_args: Vec<&str> = vec!["repl", "--binlog-dir", "/nonused"];
        repl_args.extend(window.iter().map(String::as_str));
        repl_args.extend([
            "--output-dir",
            ro,
            "--server-id",
            &sid,
            "--heartbeat-secs",
            "10",
        ]);
        let r = run_bin(&repl_args, "repl", Duration::from_secs(150));
        assert!(r.status.success(), "repl 须 exit 0（stderr 见上）");
        let rsum = String::from_utf8_lossy(&r.stdout).into_owned();
        assert!(
            rsum.contains("repl done") && rsum.contains("errors=0"),
            "repl 摘要须含 done+零错误: {rsum}"
        );

        libf(&format!("p3e2e_capture_binlogs {ctr} {f0} {f1} {bd}"));
        let mut file_args: Vec<&str> = vec!["to-sql", "--binlog-dir", bd];
        file_args.extend(window.iter().map(String::as_str));
        file_args.extend(["--output-dir", fo]);
        let f = run_bin(&file_args, "to-sql", Duration::from_secs(150));
        assert!(f.status.success(), "file 模式须 exit 0");
        let fsum = String::from_utf8_lossy(&f.stdout).into_owned();
        assert!(
            fsum.contains("to-sql done") && fsum.contains("errors=0"),
            "file 摘要须含 done+零错误: {fsum}"
        );

        // ── 总闸：同窗同旗标，两路 .sql 逐字节 ──
        let a = sql_map(&repl_out);
        let b = sql_map(&file_out);
        assert_eq!(
            a.keys().collect::<Vec<_>>(),
            b.keys().collect::<Vec<_>>(),
            "文件名单集合须一致（命名族 to_sql.[db.table.]N.sql，N=binlog 序号）"
        );
        assert!(!a.is_empty(), "双侧不得同为空（假绿禁止）");
        let mut total = 0usize;
        for name in a.keys() {
            let (x, y) = (&a[name], &b[name]);
            total += x.len();
            println!("  {name}: repl={}B file={}B", x.len(), y.len());
            assert!(
                x.starts_with(b"SET NAMES utf8mb4;\n"),
                "{name} 缺 SET NAMES 头"
            );
            if x != y {
                notes.push(diff_report(name, x, y));
            }
        }
        println!(
            "== T6a 等价性总闸: {} 文件 / {total}B 逐字节比对 ==",
            a.len()
        );
        assert!(total > 1000, "产物过小（{total}B）疑未灌到流量");

        // 系统 diff -r 复验（公共口径镜像；唯一排除项 resume.json =
        // repl 独有 checkpoint，非 SQL 产物，不构成内容豁免）
        let d = ProcCommand::new("diff")
            .args(["-r", "-x", "resume.json", ro, fo])
            .output()
            .expect("spawn diff");
        assert!(
            d.status.success(),
            "diff -r 非干净: stdout={}",
            String::from_utf8_lossy(&d.stdout)
        );
        println!("  diff -r -x resume.json repl/ file/ → clean (rc=0)");

        // ── 内容反空闸：混合 DML 各形态必须真实进窗（防「同为空」假绿）──
        let text: String = b
            .values()
            .map(|v| String::from_utf8_lossy(v).into_owned())
            .collect();
        let markers: Vec<String> = vec![
            format!("INSERT INTO `{db}`."),
            format!("UPDATE `{db}`."),
            format!("DELETE FROM `{db}`."),
            "Adoc1".into(),          // 段 A 首轮（JSON+BLOB+中文行）
            "Atrx5".into(),          // 多行事务成员
            "0x00FF10DE2AD0".into(), // BLOB 十六进制渲染
            "中文".into(),           // utf8mb4 透传
            r#"{\"r\":1,"#.into(),   // JSON 文本形态（8.0 归一化+SQL 转义后）
        ];
        for marker in &markers {
            assert!(
                text.contains(marker.as_str()),
                "产物必须含混合 DML 指纹 {marker:?}"
            );
        }
        for forbidden in ["Bdoc1", "Btrx1", "JUNKRB", "Blast"] {
            assert!(
                !text.contains(forbidden),
                "{forbidden} 不得入产（stop 后流量/回滚事务泄漏）"
            );
        }
        assert!(
            repl_out.join("resume.json").is_file(),
            "repl 终档 checkpoint 应在场（非 SQL 产物，不入比对）"
        );
        println!("窗口 {f0}:{p0} .. {f1}:{p1}；repl 摘要 {rsum}；file 摘要 {fsum}");
        notes
    }));

    // ── 无论成败先清场 ──
    let failed = body.is_err();
    let keep = std::env::var("MY2SQL_T6_KEEP").unwrap_or_default();
    if !keep.is_empty() {
        std::fs::create_dir_all(&keep).ok();
        let st = ProcCommand::new("cp")
            .arg("-r")
            .args([root.to_str().unwrap(), keep.as_str()])
            .status();
        println!(
            "T6_KEEP: 现场已拷入 {keep}/ (spawn rc={st:?})，工作目录 {}",
            root.display()
        );
    }
    if !failed && keep.is_empty() {
        std::fs::remove_dir_all(&root).ok();
    } else if keep.is_empty() {
        eprintln!("T6a 失败现场保留于 {}", root.display());
    }
    if let Ok(mut c) = mysql::Conn::new(uri.as_str()) {
        c.query_drop(format!("DROP DATABASE IF EXISTS {db}")).ok();
    }
    let notes = match body {
        Ok(n) => n,
        Err(p) => std::panic::resume_unwind(p),
    };
    assert!(
        notes.is_empty(),
        "等价性总闸红例（真缺陷，不削弱比较器）:\n{}",
        notes.join("\n")
    );
}

// ────────────────────────────────────────────────────────────────────────────
// P3 T6b：live 灾难/矩阵件（简报 Step 2-4 + T4 挂账 threads>1 水位）。
//
// 五件（每件自带**专属容器** `p3e2e-8-0-p3t6b<slug>-<pid>`，Bt::Drop 全路径
// 清场——restart 件必须独占容器，静默件不能被邻居流量污染，kill 件的容器
// binlog 不能提前进位；Makefile 的 trap 只兜共享容器，这里是构造/析构对）：
//   1. repl_kill9_resume_zero_loss      SIGKILL 中断 → 水位=整事务界、
//      read_verify 对账、--resume-file+新 dir 接续（I1 回归：消费档字节
//      不变）、两段合并 vs 不间断基准 = 已提交事务零缺失、重复仅单事务
//      前缀且可列出（重复体由 p3e2e_feed_bigtrx 的 80+ 事件流保证非空）。
//   2. repl_survives_server_restart     灌流中 docker restart：进程不死、
//      日志(stdout∪stderr)现 `repl: reconnect #`、恢复后到 stop 条件 exit 0，产物 =
//      基准 + 重连窗整事务重复（通用「前缀+回绕后缀」求解器比对）。
//   3. repl_start_from_file_pos_matches_slice   mid-file 事件界起点 =
//      file 模式同 pos 切片逐字节（复用 T6a 比较器 + lib 取段）。
//   4. repl_start_from_datetime_bisects_then_filters  多档（FLUSH LOGS）
//      二分 + ts 过滤：产物无早于请求时刻的行，且与 file 模式同 datetime
//      切片逐字节（灌流驱动，探针不空转）。
//   5. repl_stop_pos_finalizes          stop=file+pos（钉 T6a 关注#1：
//      末提交后再一格）→ exit 0、终档在场且 =末提交 end_pos、末档完整
//      （与 file 模式同窗逐字节）。
//   6. repl_idle_60s_no_false_drop      heartbeat 20s、主库静默 60s：零
//      reconnect 行、进程存活、静默后新流量仍能抓到并干净 stop。
//   7. repl_threads_gt1_watermark_boundaries（T4 挂账）--threads 3 + 双
//      线程交错灌流：全程轮询 checkpoint——每次出件前 pos ∈ 真 XID 边界
//      集、单调不回退、且水位声称 (P) 的全部事件必已在 .sql 落盘（「只推
//      进至完整 flush 事务界」的活体版）；停点终档 = marker XID end_pos；
//      产物与 file 模式同窗逐字节（并行重排不改序）。
//
// 跑法：`make repl-test VER=8.0`（live 件共享构建、串行）。留档：
// MY2SQL_T6_KEEP=<dir>（panic 现场无条件保留于 temp dir 并打印路径）。
// ────────────────────────────────────────────────────────────────────────────

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};

use my2sql_rs::repl::checkpoint::{self, Checkpoint};

/// 专属容器句柄：构造 = lib 起容器（唯一 slug+pid 名），析构 = rm -f（panic
/// 展开同样执行——失败路径不泄漏，T6a 实踩纪律的 RAII 化）。
struct Bt {
    ctr: String,
    uri: String,
    root: std::path::PathBuf,
}

impl Bt {
    fn new(slug: &str) -> Bt {
        Bt::new_pinned(slug, None)
    }
    /// hostport=Some(p)：钉死宿主端口——`docker restart` 会**重新分配**动态
    /// 映射口（2026-09-21 实踩 1123→1124），重启件的 repl 子进程 URI 必须
    /// 跨重启稳定才谈得上「重连恢复」。
    fn new_pinned(slug: &str, hostport: Option<u16>) -> Bt {
        Bt::new_ver_pinned(slug, "8.0", hostport)
    }
    /// P4a Lane D（加性）：版本参构造——5.6/5.7 idle 心跳件用。容器名
    /// `p3e2e-{ver_with_dashes}-{sfx}`、起容器走 `p3e2e_container_start {ver}`
    /// （lib 原生支持 5.6|5.7|8.0|8.4，seed 的 5.6 LONGTEXT 降级等版本探测
    /// 全在 lib 既有分支内）。`new`/`new_pinned` 原签名 = ver="8.0" 委托。
    fn new_ver(slug: &str, ver: &str) -> Bt {
        Bt::new_ver_pinned(slug, ver, None)
    }
    fn new_ver_pinned(slug: &str, ver: &str, hostport: Option<u16>) -> Bt {
        let sfx = format!("p3t6b{slug}-{}", std::process::id());
        let ctr = format!("p3e2e-{}-{sfx}", ver.replace('.', "-"));
        let port_arg = hostport.map(|p| format!(" {p}")).unwrap_or_default();
        let call = format!("p3e2e_container_start {ver} {sfx}{port_arg}");
        // docker run 成功但后续步骤（如 wait_healthy 超时）失败时 Bt 尚未构造、
        // 析构 rm -f 不会跑——失败路径显式清容器再上抛 panic（不泄漏容器）。
        let out = libf_try(&call).unwrap_or_else(|(rc, err)| {
            let _ = ProcCommand::new("docker").args(["rm", "-f", &ctr]).output();
            panic!("lib `{call}` rc={rc:?} stderr={err}（ctr {ctr} 已尽力清理）");
        });
        let uri = out
            .trim_end()
            .lines()
            .last()
            .expect("container_start echoes URI")
            .to_string();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "my2sql-p3t6b-{slug}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("mkdir bt root");
        eprintln!("[t6b:{slug}] ctr={ctr} uri={uri} root={}", root.display());
        Bt { ctr, uri, root }
    }
    fn sub(&self, name: &str) -> std::path::PathBuf {
        let d = self.root.join(name);
        std::fs::create_dir_all(&d).expect("mkdir sub");
        d
    }
    fn seed(&self, db: &str) {
        libf(&format!("p3e2e_seed_schema {} {db}", self.ctr));
    }
    fn feed(&self, db: &str, rounds: u32, tag: &str) {
        libf(&format!(
            "p3e2e_feed_mixed {} {db} {rounds} {tag}",
            self.ctr
        ));
    }
    /// 容忍失败的灌流（restart 窗口内连接被拒是预期）。true = 整脚本成功。
    fn feed_soft(&self, db: &str, rounds: u32, tag: &str) -> bool {
        libf_soft(&format!(
            "p3e2e_feed_mixed {} {db} {rounds} {tag}",
            self.ctr
        ))
        .is_ok()
    }
    fn sql(&self, stmt: &str) {
        libf(&format!("p3e2e_sql {} -e \"{stmt}\"", self.ctr));
    }
    fn sql_soft(&self, stmt: &str) -> bool {
        libf_soft(&format!("p3e2e_sql {} -e \"{stmt}\"", self.ctr)).is_ok()
    }
    fn master_pos(&self) -> (String, u32) {
        let o = libf(&format!("p3e2e_master_pos {}", self.ctr));
        let mut it = o.split_whitespace();
        (
            it.next().expect("file").to_string(),
            it.next().expect("pos").parse().expect("pos int"),
        )
    }
    /// 全档事件表：Vec<(start_pos, event_type, end_pos, info)>（-N 制表符
    /// 分隔；8.0 SHOW BINLOG EVENTS 的 Pos 列 = 事件**起始**位点，
    /// End_log_pos = 结束位点——2026-09-21 真机探测钉死）。
    fn events(&self, file: &str) -> Vec<(u32, String, u32, String)> {
        let o = libf(&format!(
            "p3e2e_sql {} -N -e \"SHOW BINLOG EVENTS IN '{file}'\"",
            self.ctr
        ));
        let mut v = Vec::new();
        for line in o.lines() {
            let mut c = line.split('\t');
            let (log, pos, et, _sid, endp) = (c.next(), c.next(), c.next(), c.next(), c.next());
            let info = c.next().unwrap_or("").to_string();
            if log.is_none() || et.is_none() {
                continue;
            }
            let Ok(pos) = pos.expect("pos").parse::<u32>() else {
                continue;
            };
            let Ok(endp) = endp.expect("end").parse::<u32>() else {
                continue;
            };
            v.push((pos, et.expect("et").to_string(), endp, info));
        }
        assert!(!v.is_empty(), "SHOW BINLOG EVENTS {file} 不得为空表");
        v
    }
    /// 事务收束事件（Xid / Query COMMIT / Query ROLLBACK）的 end_pos 全集
    /// ——TrxStateMachine 的 Commit/Rollback 分类法在 binlog 侧的镜像。
    fn trx_ends(&self, file: &str) -> HashSet<u32> {
        self.events(file)
            .into_iter()
            .filter(|(_, et, _, info)| {
                let kw = info
                    .trim()
                    .trim_end_matches(';')
                    .trim()
                    .to_ascii_lowercase();
                et == "Xid" || (et == "Query" && (kw == "commit" || kw == "rollback"))
            })
            .map(|(_, _, endp, _)| endp)
            .collect()
    }
    fn restart(&self) {
        let o = ProcCommand::new("docker")
            .args(["restart", "--time", "10", &self.ctr])
            .output()
            .expect("docker restart spawn");
        assert!(o.status.success(), "docker restart: {o:?}");
    }
    fn wait_healthy(&self) {
        libf(&format!("p3e2e_wait_healthy {}", self.ctr));
    }
}

impl Drop for Bt {
    fn drop(&mut self) {
        libf_soft(&format!("p3e2e_container_stop {}", self.ctr));
        let panicking = std::thread::panicking();
        let keep = std::env::var("MY2SQL_T6_KEEP").unwrap_or_default();
        if panicking {
            eprintln!("T6b 失败现场保留于 {}", self.root.display());
        } else if !keep.is_empty() {
            std::fs::create_dir_all(&keep).ok();
            ProcCommand::new("cp")
                .arg("-r")
                .args([self.root.to_str().unwrap(), keep.as_str()])
                .status()
                .ok();
            std::fs::remove_dir_all(&self.root).ok();
        } else {
            std::fs::remove_dir_all(&self.root).ok();
        }
    }
}

/// source lib 调函数，失败返 Err(输出) 而非 panic（软臂：restart 窗口灌流、
/// 容器清场兜底）。
fn libf_soft(call: &str) -> Result<String, std::process::Output> {
    let lib = concat!(env!("CARGO_MANIFEST_DIR"), "/tools/repl-e2e-lib.sh");
    let out = ProcCommand::new("bash")
        .arg("-c")
        .arg(format!("set -uo pipefail; source '{lib}'; {call}"))
        .output()
        .expect("spawn bash");
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(out)
    }
}

/// 真二进制后台起进程（stdout/stderr 管道，收尾 wait_with_output 取证）。
fn spawn_bin(args: &[&str]) -> std::process::Child {
    ProcCommand::new(env!("CARGO_BIN_EXE_my2sql-rs"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {args:?}: {e}"))
}

fn kill9(c: std::process::Child) -> std::process::Output {
    let mut c = c;
    c.kill().expect("kill -9");
    c.wait_with_output().expect("reap killed child")
}

fn wait_bounded(mut c: std::process::Child, label: &str, dur: Duration) -> std::process::Output {
    let t0 = Instant::now();
    loop {
        if c.try_wait().expect("try_wait").is_some() {
            return c.wait_with_output().expect("collect");
        }
        assert!(t0.elapsed() <= dur, "[{label}] 超硬超时 {dur:?} 未退出");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `--add-extra-info` 产物 → 块序列（一块 = 一行 `# datetime=… binlog=…
/// startpos=… stoppos=…` 头 + 其下语句行）。torn（崩溃截断的末块，末行无
/// `\n`）不进块列，原始字节经 torn 口返回。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Blk {
    binlog: String,
    start: u32,
    stop: u32,
    dt: String,
    db: String,
    tbl: String,
    stmts: Vec<Vec<u8>>,
}

fn parse_blocks(bytes: &[u8]) -> (Vec<Blk>, Option<Vec<u8>>) {
    let mut out: Vec<Blk> = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let mut push_block = |blk_raw: Vec<u8>, out: &mut Vec<Blk>| {
        let mut it = blk_raw.split_inclusive(|b| *b == b'\n').peekable();
        let head = it.next().expect("block has header");
        let h = String::from_utf8_lossy(head).into_owned();
        let mut b = Blk {
            binlog: String::new(),
            start: u32::MAX,
            stop: u32::MAX,
            dt: String::new(),
            db: String::new(),
            tbl: String::new(),
            stmts: Vec::new(),
        };
        for kv in h.split_whitespace().skip(1) {
            if let Some((k, v)) = kv.split_once('=') {
                match k {
                    "datetime" => b.dt = v.to_string(),
                    "database" => b.db = v.to_string(),
                    "table" => b.tbl = v.to_string(),
                    "binlog" => b.binlog = v.to_string(),
                    "startpos" => b.start = v.parse().unwrap_or(u32::MAX),
                    "stoppos" => b.stop = v.parse().unwrap_or(u32::MAX),
                    _ => {}
                }
            }
        }
        for line in it {
            // 行尾换符保留于 stmts（比较器字节级）
            b.stmts.push(line.to_vec());
        }
        out.push(b);
    };
    let mut saw_first = false;
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        if !saw_first {
            saw_first = true;
            if line.starts_with(b"SET NAMES") {
                continue; // 文件头（Writer 契约），不是块
            }
        }
        if line.starts_with(b"# datetime=") {
            if !cur.is_empty() {
                push_block(std::mem::take(&mut cur), &mut out);
            }
            cur.extend_from_slice(line);
        } else if !cur.is_empty() {
            cur.extend_from_slice(line);
        }
    }
    let torn = if cur.is_empty() {
        None
    } else if cur.last() == Some(&b'\n') {
        // 完整末块（末行有换行终止）：其实该并进 out —— 重解析一次
        push_block(std::mem::take(&mut cur), &mut out);
        None
    } else {
        Some(std::mem::take(&mut cur))
    };
    (out, torn)
}

/// 目录内全部 .sql（按 binlog 序号升序）拼成的块序列；torn 容忍版（活体轮询
/// 用：writer 追加瞬间可能读到半行）。返回 (块列, 末块 torn 原始字节)。
fn dir_blocks_tolerant(dir: &Path) -> (Vec<Blk>, Option<Vec<u8>>) {
    let mut files: Vec<(u64, std::path::PathBuf)> = Vec::new();
    for e in std::fs::read_dir(dir).expect("read dir") {
        let p = e.expect("entry").path();
        let name = p.file_name().expect("name").to_string_lossy().into_owned();
        if let Some(rest) = name.strip_prefix("to_sql.")
            && let Some(n) = rest.strip_suffix(".sql")
        {
            files.push((n.parse().expect("sql seq"), p));
        }
    }
    files.sort();
    let mut v = Vec::new();
    let mut torn: Option<Vec<u8>> = None;
    for (_, p) in files {
        let (mut b, t) = parse_blocks(&std::fs::read(&p).expect("read sql"));
        v.append(&mut b);
        torn = torn.or(t);
    }
    (v, torn)
}

fn dir_blocks(dir: &Path) -> Vec<Blk> {
    let (v, torn) = dir_blocks_tolerant(dir);
    assert!(
        torn.is_none(),
        "对照/基准目录不得有 torn 块: {}",
        dir.display()
    );
    v
}

fn blk_has(b: &Blk, pat: &[u8]) -> bool {
    b.stmts
        .iter()
        .any(|s| s.windows(pat.len()).any(|w| w == pat))
}

fn first_blk_with(bs: &[Blk], pat: &[u8]) -> Option<usize> {
    bs.iter().position(|b| blk_has(b, pat))
}

/// 通用「不间断基准 vs 中断产物」对齐求解器：merged = SB[..j] ++ SB[i..]
/// （i ≤ j：i..j 为回绕重复段，重复长恒 = n-m，j 选回绕位）。返回 (j, i, 重复块)。
/// 等值退化 (n, n)。语义为**整块**等值：半事务字节流级重放不在解空间内，
/// 落入分歧 panic 臂 = 总闸按设计拒绝（writer 只在事务界重放，故非缺陷面）。
fn reconcile(merged: &[Blk], sb: &[Blk]) -> (usize, usize, Vec<Blk>) {
    let (n, m) = (merged.len(), sb.len());
    assert!(m > 0, "基准不得为空（假绿禁止）");
    assert!(n >= m, "产物块数 {n} < 基准 {m} = 有事件缺失（零缺失违背）");
    let mut best: Option<(usize, usize)> = None; // dup 长恒 = n-m；j 只选回绕位（取最大合法解）
    // 上界 min(n,m)：j>m 时 `sb[..j]` 越界（n>m 恰是重连 dup 常态——T6
    // review fix 1，合成件 reconcile_dup_shape_synthetic 钉死）。
    for j in (0..=n.min(m)).rev() {
        let i = m as isize - (n - j) as isize;
        if i < 0 || i > j as isize {
            continue;
        }
        let i = i as usize;
        if merged[..j] == sb[..j] && merged[j..] == sb[i..] {
            best = Some((j, i));
            break;
        }
    }
    let (j, i) = best.unwrap_or_else(|| {
        let d = (0..n.min(m))
            .find(|k| merged[*k] != sb[*k])
            .unwrap_or(n.min(m));
        panic!(
            "产物与基准不可对齐（非「整事务重复」形态）：首个分歧块 idx {d}\n  merged={:?}\n  base  ={:?}",
            merged.get(d),
            sb.get(d)
        )
    });
    (j, i, sb[i..j].to_vec())
}

fn read_cp(p: &Path) -> Option<Checkpoint> {
    let raw = std::fs::read(p).ok()?;
    serde_json::from_slice(&raw).ok()
}

fn sid(off: u32) -> String {
    (6300 + off + std::process::id() % 600).to_string()
}

/// 水位出样（test 7 monitor 线程）：(checkpoint, 该样时刻块列快照·容忍 torn)。
type Samples = Arc<Mutex<Vec<(Checkpoint, Vec<Blk>)>>>;

/// 目录内任一 .sql 原始字节含给定 token（轮询用廉价扫描，不解析块）。
fn raw_has(dir: &Path, pat: &[u8]) -> bool {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "sql"))
                .any(|p| std::fs::read(&p).is_ok_and(|b| blk_has_raw(&b, pat)))
        })
        .unwrap_or(false)
}

/// 环境门：live 套件由 `make repl-test` 导出双变量启用（Bt 件自起容器不占
/// 共享 CTR，但门启口径同 T5/T6a——防无 docker 意图的环境误触）。
fn live_gate() {
    live_uri();
    std::env::var("MY2SQL_TEST_CTR")
        .expect("MY2SQL_TEST_CTR required (live 门契约, make repl-test 导出)");
}

/// 通用「前缀+回绕后缀」合并件的对账公共段：repl 产物目录 vs 不间断基准
/// 目录 → 零缺失 + 重复段枚举（reconcile）+ 重复段事务界核验辅助数据。
fn merge_reconcile(merged_dir: &Path, base_dir: &Path) -> (Vec<Blk>, Vec<Blk>, usize, usize) {
    let sb = dir_blocks(base_dir);
    let mb = dir_blocks(merged_dir);
    let (j, i, dup) = reconcile(&mb, &sb);
    // 形状恒等式 n = j + m - i 由 reconcile 的解定义 (i = m-(n-j)) 代数兑现，
    // 作为断言是空转——dup 语义的真钉子外移至合成件
    // reconcile_dup_shape_synthetic（T6 review fix 1/2）。
    (sb, dup, i, j)
}

/// 合成 dup 件（非 live）：merged 比基准**长**（重连整事务回绕重复，n > m）
/// 时 reconcile 必须解出 (j, i, dup=sb[i..j]) 而非越界 panic——循环上界若
/// 用 n，`sb[..j]` 在首个迭代 j=n>m 即 slice index out of range（T6 review
/// 实锤的 Important）。dup 形态为**整块**重复：块内（半事务字节流）重放在
/// 整块等值语义下不可表达，会走 reconcile 的分歧 panic 臂 = 总闸正确拒绝。
fn synth_blk(tag: &str) -> Blk {
    Blk {
        binlog: "mysql-bin.000001".into(),
        start: 4,
        stop: 8,
        dt: "1970-01-01_00:00:00".into(),
        db: "synth".into(),
        tbl: "t".into(),
        stmts: vec![format!("INSERT INTO t VALUES ('{tag}');\n").into_bytes()],
    }
}

#[test]
fn reconcile_dup_shape_synthetic() {
    let sb: Vec<Blk> = (0..10u32).map(|k| synth_blk(&format!("b{k}"))).collect();
    let (m, i, j) = (sb.len(), 4usize, 7usize);
    // merged = SB[..j] ++ SB[i..]：回绕重复段 = sb[i..j]（3 块）
    let mut mb: Vec<Blk> = sb[..j].to_vec();
    mb.extend_from_slice(&sb[i..]);
    assert!(mb.len() > m, "合成件前提：dup 非空即 n > m");
    let (rj, ri, dup) = reconcile(&mb, &sb);
    assert_eq!((rj, ri), (j, i), "回绕点须唯一解出（块值互异）");
    assert_eq!(dup, sb[i..j], "dup 必须正是基准 [i..j) 段");
    // 等值退化（n == m）：(m, m) + 空 dup
    let (ej, ei, edup) = reconcile(&sb, &sb);
    assert_eq!((ej, ei), (m, m));
    assert!(edup.is_empty());
    // 非 dup 形态（内容分歧）仍须红：把回绕后缀换成别的内容
    let mut bad = mb.clone();
    bad[mb.len() - 1] = synth_blk("zzz");
    let r = std::panic::catch_unwind(|| reconcile(&bad, &sb));
    assert!(r.is_err(), "不可对齐形态必须 panic（假绿禁止）");
}

/// 抢一个瞬空端口给钉死映射用（绑定即释，race 窗口可忽略——单跑串行）。
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("port probe bind")
        .local_addr()
        .expect("local addr")
        .port()
}

/// SIGKILL 断流 → 边界/对账/新目录接续/双段合并零缺失 + 单事务重复可列。
#[test]
#[ignore = "requires live mysql 8.0 container (make repl-test 门: MY2SQL_TEST_URI+CTR)"]
fn repl_kill9_resume_zero_loss() {
    live_gate();
    let bt = Bt::new("k9");
    let db = "p3t6bk9";
    bt.seed(db);
    let (f0, p0) = bt.master_pos();
    let (d1, d2, d3) = (bt.sub("seg1"), bt.sub("seg2"), bt.sub("base"));
    let (uri, d1s, d2s, d3s) = (
        bt.uri.clone(),
        d1.to_str().unwrap().to_string(),
        d2.to_str().unwrap().to_string(),
        d3.to_str().unwrap().to_string(),
    );

    // run1：显式起点、无 stop（灌流中被 -9）。--threads 1 = 水位每提交即推
    // 的直通形态（kill 现场唯一在飞事务 = bigtrx，重复体可精确点名）。
    let p0s = p0.to_string();
    let mut child = spawn_bin(&[
        "repl",
        "--binlog-dir",
        "/nonused",
        "--uri",
        &uri,
        "--start-file",
        &f0,
        "--start-pos",
        &p0s,
        "--db",
        db,
        "--add-extra-info",
        "--output-dir",
        &d1s,
        "--server-id",
        &sid(1),
        "--heartbeat-secs",
        "10",
        "--threads",
        "1",
    ]);
    bt.feed(db, 5, "A");
    let (fA, pA) = bt.master_pos(); // = 'Alast' XID end_pos（feed 尾事务提交界）
    assert_eq!(fA, f0, "A 段须停留同一 binlog 档（跨档矩阵归 T7）");

    // 大事务：单事务 50000 行 → 200+ 个 rows 事件（各 ~8KB）；副本端从首个
    // 事件落盘到 XID 到达之间有几十毫秒窗口——kill 必落在「事务已开流、
    // 水位仍冻结在 pA」的区间内（重复体非空是**构造性保证**而非运气）。
    let ctr2 = bt.ctr.clone();
    let big = std::thread::spawn(move || {
        libf(&format!(
            "p3e2e_feed_bigtrx {ctr2} {db} 50000 W",
            db = "p3t6bk9"
        ))
    });
    let t0 = Instant::now();
    loop {
        if read_cp(&d1.join("resume.json")).is_some_and(|cp| cp.pos == pA && cp.file == f0)
            && raw_has(&d1, b"'W1',")
        {
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(90),
            "90s 内未见「水位=Alast 界 ∧ seg1 已现 bigtrx 首块」"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    let ko = kill9(child);
    eprintln!(
        "[k9] SIGKILL 落位：seg1 blocks={} cp={:?}（stderr={}B）",
        dir_blocks(&d1).len(),
        read_cp(&d1.join("resume.json")),
        ko.stderr.len()
    );
    assert!(big.join().is_ok(), "bigtrx 灌流线程须正常收尾");

    // kill 之后的窗口流量：B 段 + 一条 PAD（stop 位点 = PAD XID end → PAD
    // rows 在场、其提交被 >= 排除——两侧同构，见 T6a 关注#1）。
    bt.feed(db, 5, "B");
    // 注意：libf 拼串经 bash 双引号——语句内绝不带反引号/美元号（标识符皆
    // 安全字符集，无须引用）。
    bt.sql("INSERT INTO p3t6bk9.t_ord (sku, qty, note) VALUES ('K9PAD', 0, 'pad')");
    let (f1, p1) = bt.master_pos();
    assert_eq!(f0, f1, "本件须停留单 binlog 档（跨档矩阵归 T7）");
    assert!(p1 > pA);

    // ── (a) 水位恰在整事务界：== pA 且 pA ∈ 真 XID end_pos 集 ──
    let cp = read_cp(&d1.join("resume.json")).expect("SIGKILL 前必有至少一次水位");
    assert_eq!(cp.file, f0);
    assert_eq!(cp.pos, pA, "kill 后水位必须恰在最后一个完整提交事务界");
    assert!(
        bt.trx_ends(&f0).contains(&pA),
        "pA 必须是真事务收束 end_pos"
    );
    // ── (b) 幸存者对账：written_files ↔ 磁盘实物一一对应 ──
    checkpoint::read_verify(&d1.join("resume.json"), &d1)
        .expect("kill-9 幸存 checkpoint 必须通过 read_verify 对账");

    // ── (c) --resume-file + 新 output-dir：消费档字节不变（I1 回归）──
    let snap1: Vec<(String, Vec<u8>)> = std::fs::read_dir(&d1)
        .expect("snap dir")
        .filter_map(|e| e.ok())
        .map(|e| {
            let p = e.path();
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read(&p).unwrap_or_default(),
            )
        })
        .collect();
    let cps = d1.join("resume.json").to_str().unwrap().to_string();
    let r = run_bin(
        &[
            "repl",
            "--binlog-dir",
            "/nonused",
            "--uri",
            &uri,
            "--resume-file",
            &cps,
            // CLI 位点三态哨兵：resume 臂要求脱离默认 4（validate_repl 冲突
            // 判据 start_pos != 0），显式给空串+0 表达「无独立 start」。
            "--start-file",
            "",
            "--start-pos",
            "0",
            "--output-dir",
            &d2s,
            "--db",
            db,
            "--add-extra-info",
            "--stop-file",
            &f1,
            "--stop-pos",
            &p1.to_string(),
            "--server-id",
            &sid(2),
            "--heartbeat-secs",
            "10",
            "--threads",
            "1",
        ],
        "repl-resume",
        Duration::from_secs(150),
    );
    assert!(r.status.success(), "resume run 须 exit 0");
    let rsum = String::from_utf8_lossy(&r.stdout).into_owned();
    assert!(
        rsum.contains("repl done") && rsum.contains("errors=0"),
        "{rsum}"
    );
    let snap2: Vec<(String, Vec<u8>)> = std::fs::read_dir(&d1)
        .expect("resnap dir")
        .filter_map(|e| e.ok())
        .map(|e| {
            let p = e.path();
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read(&p).unwrap_or_default(),
            )
        })
        .collect();
    let mut names1: Vec<_> = snap1.iter().map(|(n, _)| n.clone()).collect();
    let mut names2: Vec<_> = snap2.iter().map(|(n, _)| n.clone()).collect();
    names1.sort();
    names2.sort();
    assert_eq!(names1, names2, "seg1 文件集合不得被 resume run 触碰");
    for (n, b) in &snap1 {
        let cur = snap2
            .iter()
            .find(|(m, _)| m == n)
            .expect("same name")
            .1
            .clone();
        assert_eq!(
            b,
            &cur,
            "seg1/{n} 字节必须不变（消费档+产物只读契约）",
            n = n
        );
    }
    assert!(
        d2.join("resume.json").is_file(),
        "resume run 必须在新目录落自己的 checkpoint（I1）"
    );
    checkpoint::read_verify(&d2.join("resume.json"), &d2).expect("seg2 自档对账");

    // ── 基准：同窗不间断 run ──
    let rb = run_bin(
        &[
            "repl",
            "--binlog-dir",
            "/nonused",
            "--uri",
            &uri,
            "--start-file",
            &f0,
            "--start-pos",
            &p0s,
            "--output-dir",
            &d3s,
            "--db",
            db,
            "--add-extra-info",
            "--stop-file",
            &f1,
            "--stop-pos",
            &p1.to_string(),
            "--server-id",
            &sid(3),
            "--heartbeat-secs",
            "10",
            "--threads",
            "1",
        ],
        "repl-base",
        Duration::from_secs(150),
    );
    assert!(rb.status.success(), "基准 run 须 exit 0");

    // ── (d) 两段合并 vs 基准：零缺失 + 重复=单一在飞事务前缀且可列出 ──
    let sb = dir_blocks(&d3);
    // seg1 走 tolerant 口：writer 行原子刷盘，torn 末块只可能是 kill 与
    // write 同刻相撞的残行——残行出现即须逐字节核验（不是删掉的死分支）。
    let (s1, torn1) = dir_blocks_tolerant(&d1);
    let s2 = dir_blocks(&d2);
    assert!(sb.len() > 50, "基准块数过少（{}）疑流量未进窗", sb.len());
    let iw = first_blk_with(&sb, b"'W1',").expect("基准必含 bigtrx 首块");
    assert!(
        sb[iw].start >= cp.pos,
        "重复段首块 start={} < 水位 {}（对齐前提破坏）",
        sb[iw].start,
        cp.pos
    );
    // seg1 = 基准前缀（含 kill 时已完整落盘的 W 块；torn 末块单独核）
    assert!(
        s1.len() > iw,
        "kill 时 seg1 必须已写入 ≥1 个 W 块（构造保证）"
    );
    for k in 0..s1.len() {
        assert_eq!(s1[k], sb[k], "seg1 块 {k} 与基准分歧（前缀性破坏）");
    }
    let n_torn = match &torn1 {
        Some(torn) => {
            // torn 块必须是基准下一块的字节前缀（头行含真 db/table 的忠实
            // 重建比对，dir_bytes_of_block）；该下一块仍是 W 事务内部块
            // ⇒ torn 落在 bigtrx 流内的判定由前缀关系一并兑现。
            let nb = sb
                .get(s1.len())
                .expect("torn 末块在基准必须有下一块（seg1 不得越写）");
            let expect = dir_bytes_of_block(nb);
            assert!(expect.starts_with(torn), "torn 块非基准块字节前缀");
            assert!(blk_has(nb, b"'W"), "torn 对应基准块须为 bigtrx 块");
            true
        }
        None => false,
    };
    // seg2 = 基准自水位起点的完整后缀（resume 从边界重放 = 全等）
    let i = sb.iter().position(|b| b.start >= cp.pos).expect("后缀起点");
    assert_eq!(i, iw, "resume 后缀首块必须正是重复段首块");
    assert_eq!(s2.len(), sb.len() - i, "seg2 必须覆盖水位后全部块");
    for k in 0..s2.len() {
        assert_eq!(s2[k], sb[i + k], "seg2 块 {k} 与基准后缀分歧");
    }
    // 重复段 = sb[i .. s1.len())：全部是 bigtrx（'W…'）块 = 恰一个事务
    let dup = &sb[i..s1.len()];
    assert!(
        !dup.is_empty(),
        "重复段不得为空（构造保证 kill 在事务开流后）"
    );
    for b in dup {
        for s in &b.stmts {
            assert!(
                s.starts_with(b"INSERT INTO ") && blk_has_raw(s, b"'W"),
                "重复块语句必须只含 bigtrx 的 INSERT 行: {:?}",
                String::from_utf8_lossy(s)
            );
        }
    }
    let ends = bt.trx_ends(&f0);
    let dup_last = dup.last().expect("non-empty");
    assert!(
        !ends.iter().any(|e| *e > cp.pos && *e <= dup_last.stop),
        "重复段内部不得含事务收束界（越界重复只可能是单事务前缀）"
    );
    let dup_stmts: usize = dup.iter().map(|b| b.stmts.len()).sum();
    println!(
        "[k9] 零缺失核对：基准 {} 块（{} 语句）= seg1 {} 块(+torn={}) ∪ seg2 {} 块；重复段 {} 块 {} 语句（全为 bigtrx 'W…' 单事务前缀，位点 {}..{}）",
        sb.len(),
        sb.iter().map(|b| b.stmts.len()).sum::<usize>(),
        s1.len(),
        n_torn,
        s2.len(),
        dup.len(),
        dup_stmts,
        dup.first().expect("non-empty").start,
        dup_last.stop
    );
    // 反假绿：指纹
    let allt: String = sb
        .iter()
        .flat_map(|b| {
            b.stmts
                .iter()
                .map(|s| String::from_utf8_lossy(s).into_owned())
        })
        .collect();
    for tok in ["Adoc1", "'W1'", "'W50000'", "'Btrx5'", "'Alast'", "'K9PAD'"] {
        assert!(allt.contains(tok), "基准缺指纹 {tok}");
    }
    assert!(
        allt.contains("中文") && allt.contains("0x00FF10DE2AD0"),
        "混合 DML 形态必须在窗"
    );
}

fn blk_has_raw(bytes: &[u8], pat: &[u8]) -> bool {
    bytes.windows(pat.len()).any(|w| w == pat)
}

/// 块 → Writer 原始字节忠实重建（头行格式与 src/output.rs 逐字段对齐：
/// 真 database/table，非占位串——torn 前缀核验用）。
fn dir_bytes_of_block(b: &Blk) -> Vec<u8> {
    let mut v = format!(
        "# datetime={} database={} table={} binlog={} startpos={} stoppos={}\n",
        b.dt, b.db, b.tbl, b.binlog, b.start, b.stop
    )
    .into_bytes();
    for s in &b.stmts {
        v.extend_from_slice(s);
    }
    v
}

/// 块列 → 全文（指纹点验用）。
fn blocks_text(bs: &[Blk]) -> String {
    bs.iter()
        .flat_map(|b| {
            b.stmts
                .iter()
                .map(|s| String::from_utf8_lossy(s).into_owned())
        })
        .collect()
}

/// file 模式同窗对照 → 与 repl 产物逐字节（T6a 比较器复用：文件名单集合 +
/// 原始字节逐一 + 双侧非空）。`f_from..f_to` = docker cp 取段闭区间。
fn cmp_to_file_mode(bt: &Bt, repl_dir: &Path, f_from: &str, f_to: &str, window: &[String]) {
    let bins = bt.sub("bins-cmp");
    let fout = bt.sub("file-cmp");
    libf(&format!(
        "p3e2e_capture_binlogs {} {f_from} {f_to} {}",
        bt.ctr,
        bins.display()
    ));
    let mut args: Vec<&str> = vec!["to-sql", "--binlog-dir", bins.to_str().unwrap()];
    args.extend(window.iter().map(String::as_str));
    args.extend(["--output-dir", fout.to_str().unwrap()]);
    let r = run_bin(&args, "to-sql-cmp", Duration::from_secs(180));
    assert!(r.status.success(), "file 模式对照须 exit 0");
    let s = String::from_utf8_lossy(&r.stdout).into_owned();
    assert!(
        s.contains("to-sql done") && s.contains("errors=0"),
        "file 对照摘要: {s}"
    );
    let a = sql_map(repl_dir);
    let b = sql_map(&fout);
    assert_eq!(
        a.keys().collect::<Vec<_>>(),
        b.keys().collect::<Vec<_>>(),
        "repl/file 文件名单集合不一致"
    );
    assert!(!a.is_empty(), "双侧同空 = 假绿禁止");
    let mut notes = Vec::new();
    for k in a.keys() {
        if a[k] != b[k] {
            notes.push(diff_report(k, &a[k], &b[k]));
        }
    }
    println!(
        "  [cmp] {} 文件 / {}B repl≡file 逐字节 {}",
        a.len(),
        a.values().map(|v| v.len()).sum::<usize>(),
        if notes.is_empty() { "OK" } else { "红" }
    );
    assert!(
        notes.is_empty(),
        "repl 产物 vs file 模式同窗字节分歧（真缺陷，不削弱）:\n{}",
        notes.join("\n")
    );
}

/// 断链重连：灌流中 `docker restart` —— 进程必须活着扛过去、日志现
/// `repl: reconnect #`、恢复后到 stop 条件 exit 0；产物 = 基准 + 重连窗
/// 重复（前缀+回绕后缀，形状求解器枚举；threads=1 下重复体为整事务/尾
/// 事务前缀）。
#[test]
#[ignore = "requires live mysql 8.0 container (make repl-test 门: MY2SQL_TEST_URI+CTR)"]
fn repl_survives_server_restart() {
    live_gate();
    // 钉死宿主口：docker restart 重分配动态口（实踩），子进程 URI 必须
    // 跨重启可寻址。
    let bt = Bt::new_pinned("rsr", Some(free_port()));
    let db = "p3t6brsr";
    bt.seed(db);
    let (f0, p0) = bt.master_pos();
    let run = bt.sub("run");
    let base = bt.sub("base");
    let (run_s, base_s) = (
        run.to_str().unwrap().to_string(),
        base.to_str().unwrap().to_string(),
    );
    // stop-datetime = 事件驱动收尾（重启 ~20s + 恢复补灌 ~15s，90s 余量
    // 充裕；到点后再钉一枚 PULSE，同 T5 形态）。
    let stop_unix = chrono::Utc::now().timestamp() + 90;
    let stop = chrono::DateTime::from_timestamp(stop_unix, 0)
        .unwrap()
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let p0s = p0.to_string();
    let mut child = spawn_bin(&[
        "repl",
        "--binlog-dir",
        "/nonused",
        "--uri",
        &bt.uri,
        "--start-file",
        &f0,
        "--start-pos",
        &p0s,
        "--db",
        db,
        "--add-extra-info",
        "--output-dir",
        &run_s,
        "--server-id",
        &sid(11),
        "--heartbeat-secs",
        "10",
        "--threads",
        "1",
        "--stop-datetime",
        &stop,
    ]);
    bt.feed(db, 3, "R1");
    bt.feed(db, 3, "R2");

    // ── 重启编排：docker restart 非阻塞 spawn；主线程同步轮询 repl 存活 ──
    let mut drun = ProcCommand::new("docker")
        .args(["restart", "--time", "10", &bt.ctr])
        .spawn()
        .expect("spawn docker restart");
    let t0 = Instant::now();
    loop {
        assert!(
            child.try_wait().expect("try_wait repl").is_none(),
            "repl 进程在服务器重启窗口内不得退出（断链即退 = T2-T5 缺陷）"
        );
        if drun.try_wait().expect("try_wait docker").is_some() {
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(180),
            "docker restart 硬超时"
        );
        // 宕机盲区灌流：连接被拒/中断是预期（软臂，成败均不入断言）
        bt.feed_soft(db, 1, "D");
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        drun.wait().expect("reap docker restart").success(),
        "docker restart rc 须 0"
    );
    bt.wait_healthy();

    // ── 恢复后补灌：必须全部成功（重连后的流对写入侧透明）──
    for i in 3..=6u32 {
        assert!(
            bt.feed_soft(db, 2, &format!("R{i}")),
            "重启恢复后灌流 R{i} 必须成功（连接已复原？）"
        );
    }
    let remain = stop_unix + 2 - chrono::Utc::now().timestamp();
    if remain > 0 {
        std::thread::sleep(Duration::from_secs(remain as u64));
    }
    bt.sql("INSERT INTO p3t6brsr.t_ord (sku, qty, note) VALUES ('RSRPULSE', 0, 'stop')");
    let out = wait_bounded(child, "restart-run", Duration::from_secs(120));
    // tracing_subscriber::fmt 默认 MakeWriter 落 **stdout**（main.rs 旧注释
    // 的「stderr」是历史误记——T6b r3 实测：`repl: reconnect #` 进 stdout，
    // stderr 恒 0B）。日志面断言统一扫 stdout∪stderr 合并串——流接线修
    // 正，判据语义不降。
    let esum = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "重启后须撑到 stop 条件并 exit 0（rc={:?}）\n日志:\n{esum}",
        out.status.code()
    );
    let sum = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        sum.contains("repl done") && sum.contains("errors=0"),
        "摘要: {sum}"
    );
    let rlines: Vec<&str> = esum
        .lines()
        .filter(|l| l.contains("repl: reconnect #"))
        .collect();
    assert!(
        !rlines.is_empty(),
        "stdout∪stderr 必须现 `repl: reconnect #`（真断链真重连？）\n全日志:\n{esum}"
    );
    for l in &rlines {
        println!("[restart] 重连行: {l}");
    }

    // ── 基准：同窗不间断（binlog 已静止，历史重放确定性）──
    let rb = run_bin(
        &[
            "repl",
            "--binlog-dir",
            "/nonused",
            "--uri",
            &bt.uri,
            "--start-file",
            &f0,
            "--start-pos",
            &p0s,
            "--db",
            db,
            "--add-extra-info",
            "--output-dir",
            &base_s,
            "--server-id",
            &sid(12),
            "--heartbeat-secs",
            "10",
            "--threads",
            "1",
            "--stop-datetime",
            &stop,
        ],
        "restart-base",
        Duration::from_secs(150),
    );
    assert!(rb.status.success(), "基准 run 须 exit 0");

    // ── 对账：零缺失 + 重复段枚举 + 回绕点对齐事务界 ──
    let (sb, dup, i, j) = merge_reconcile(&run, &base);
    let mb = dir_blocks(&run);
    println!(
        "[restart] 基准 {} 块；产物 {} 块 = 前缀 {} + 回绕后缀 {}；重复 {} 块（位点 {}..{}）",
        sb.len(),
        mb.len(),
        j,
        sb.len() - i,
        dup.len(),
        dup.first().map(|b| b.start).unwrap_or(0),
        dup.last().map(|b| b.stop).unwrap_or(0)
    );
    if !dup.is_empty() {
        assert!(
            j < sb.len(),
            "重复段后必须有回绕重放余量（dup 吞尽基准 = 假窗）"
        );
        if i > 0 && sb[i - 1].binlog == sb[i].binlog {
            let ends = bt.trx_ends(&sb[i].binlog);
            let (prev_stop, cur_start) = (sb[i - 1].stop, sb[i].start);
            assert!(
                ends.iter().any(|e| *e >= prev_stop && *e <= cur_start),
                "回绕重放起点未对齐事务界：{}.({prev_stop}, {cur_start}] 无收束界 = 半事务拼接（真缺陷）",
                sb[i].binlog
            );
        }
        // 回绕点跨档（旧档尾=shutdown 收口 XID、新档头=FDE 序）本身即
        // 文件级事务界，跨档位点不可比，跳过数值核对。
    }
    // 双 run 终档一致（同 stop 同流 → 收尾决定论）+ 事务界 + 对账
    let cp1 = read_cp(&run.join("resume.json")).expect("run 终档在场");
    let cp2 = read_cp(&base.join("resume.json")).expect("基准终档在场");
    assert_eq!(
        (cp1.file.as_str(), cp1.pos),
        (cp2.file.as_str(), cp2.pos),
        "中断 run 与基准终档必须一致（stop 决定论）"
    );
    assert!(
        bt.trx_ends(&cp1.file).contains(&cp1.pos),
        "终档恰在事务收束界（{}:{}）",
        cp1.file,
        cp1.pos
    );
    checkpoint::read_verify(&run.join("resume.json"), &run).expect("run read_verify");
    checkpoint::read_verify(&base.join("resume.json"), &base).expect("基准 read_verify");
    // 反假绿：指纹 + PULSE 排除（stop-datetime 之前事件驱动收尾）
    let text = blocks_text(&mb);
    for tok in ["'R1doc1中文'", "'R6trx5'", "'R6last'"] {
        assert!(text.contains(tok), "缺指纹 {tok}（流量未进窗/丢事件？）");
    }
    assert!(!text.contains("RSRPULSE"), "PULSE ts ≥ stop，不得入产");
}

/// mid-file 事件界起点：repl 从 (f0, 真事务 end_pos) 起流 ≡ file 模式同
/// pos 切片逐字节（起点两侧同码同判——等价性的「任意起点」维）。
#[test]
#[ignore = "requires live mysql 8.0 container (make repl-test 门: MY2SQL_TEST_URI+CTR)"]
fn repl_start_from_file_pos_matches_slice() {
    live_gate();
    let bt = Bt::new("slc");
    let db = "p3t6bslc";
    bt.seed(db);
    bt.feed(db, 2, "A");
    let (fs, ps) = bt.master_pos(); // 窗内中段真事务界
    assert!(
        bt.trx_ends(&fs).contains(&ps),
        "起点 {ps} 必须是事件收束界（FROM 非界位点直接 ERROR 1220，无意义切片禁入）"
    );
    assert!(ps > 4, "起点必须 mid-file（{ps}）");
    bt.feed(db, 4, "M");
    let (_fb, pB) = bt.master_pos(); // = Xid('Mlast') end_pos（末意图提交界）
    // T6a 关注#1：stop 钉在末提交**再一格**（PAD 之后取位点）
    bt.sql("INSERT INTO p3t6bslc.t_ord (sku, qty, note) VALUES ('SLCPAD', 0, 'pad')");
    let (fe, pe) = bt.master_pos();
    assert_eq!(fe, fs, "本件单档");

    let dir_r = bt.sub("repl");
    let ro = dir_r.to_str().unwrap().to_string();
    let window: Vec<String> = vec![
        "--uri".into(),
        bt.uri.clone(),
        "--start-file".into(),
        fs.clone(),
        "--start-pos".into(),
        ps.to_string(),
        "--stop-file".into(),
        fe.clone(),
        "--stop-pos".into(),
        pe.to_string(),
        "--db".into(),
        db.into(),
        "--add-extra-info".into(),
    ];
    let mut args: Vec<&str> = vec!["repl", "--binlog-dir", "/nonused"];
    args.extend(window.iter().map(String::as_str));
    let sidb21 = sid(21);
    args.extend([
        "--output-dir",
        &ro,
        "--server-id",
        &sidb21,
        "--heartbeat-secs",
        "10",
        "--threads",
        "1",
    ]);
    let r = run_bin(&args, "slice-repl", Duration::from_secs(150));
    assert!(r.status.success(), "repl 中段起流须 exit 0");
    let s = String::from_utf8_lossy(&r.stdout).into_owned();
    assert!(s.contains("repl done") && s.contains("errors=0"), "{s}");

    cmp_to_file_mode(&bt, &dir_r, &fs, &fe, &window);

    // 内容闸：起点前流量零泄漏、窗内全量在场。T6a 关注#1 的「再一格」=
    // PAD XID end_pos：PAD 的 rows 事件 end < stop → 流式出块在场，其 XID
    // 等号排除（不入 cp）——file 侧同码同判，逐字节对照兜住。
    let text = blocks_text(&dir_blocks(&dir_r));
    // 指纹 = t_doc 首插入完整值字面量（feed_mixed 形态 '{tag}doc{r}中文'
    // ——闭引号在中文之后，token 必须带全值，2026-09-22 实踩修正）。
    assert!(
        text.contains("'Mdoc1中文'") && text.contains("'Mlast'"),
        "窗内指纹缺（'Mdoc1中文'/'Mlast'）"
    );
    assert!(
        !text.contains("Adoc1") && !text.contains("Alast"),
        "起点前（A 段）泄漏"
    );
    assert!(
        text.contains("SLCPAD"),
        "PAD rows 应流式在场（stop 只等号排除其 XID）"
    );
    let cp = read_cp(&dir_r.join("resume.json")).expect("终档在场");
    assert_eq!(
        (cp.file.as_str(), cp.pos),
        (fe.as_str(), pB),
        "终档 = 末意图提交（Mlast）end_pos；PAD 未收口不入水位"
    );
    println!(
        "[slice] {fs}:{ps} 起点 repl≡file；终档 {fe}:{}（PAD XID 等号排除）",
        cp.pos
    );
}

/// --start-datetime 多档定位：二分（probe 有流量驱动，不空转）→ 首出事件
/// ts ≥ 请求时刻、无更早行；跨档续读；≡ file 模式同 datetime 切片逐字节。
#[test]
#[ignore = "requires live mysql 8.0 container (make repl-test 门: MY2SQL_TEST_URI+CTR)"]
fn repl_start_from_datetime_bisects_then_filters() {
    live_gate();
    let bt = Bt::new("dtb");
    let db = "p3t6bdtb";
    bt.seed(db);
    let (f0, _) = bt.master_pos();
    bt.feed(db, 2, "G1");
    // 事件 ts 秒级粒度：G1 与 G2 之间垫 ≥2.5s 硬间隔，dt0 取缝中——
    // 「无更早行」判定在秒粒度下也无边界歧义。
    std::thread::sleep(Duration::from_millis(2600));
    let dt0 = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let dt0u = chrono::Utc::now().format("%Y-%m-%d_%H:%M:%S").to_string();
    bt.feed(db, 2, "G2"); // 请求时刻**之后**的流量：probe 有真事件可跳
    bt.sql("FLUSH LOGS");
    let (f2, _) = bt.master_pos();
    assert_ne!(f0, f2, "FLUSH LOGS 必须已切新档（跨档前提破坏）");
    bt.feed(db, 2, "G3");
    bt.sql("INSERT INTO p3t6bdtb.t_ord (sku, qty, note) VALUES ('DTBPAD', 0, 'pad')");
    let (fe, pe) = bt.master_pos();
    assert_eq!(fe, f2, "本件恰两档（G3+PAD 在新档）");

    let dir_r = bt.sub("repl");
    let ro = dir_r.to_str().unwrap().to_string();
    // repl 窗：start-datetime 单态（M3：与 start-file 两两互斥，缺省哨兵
    // ""/0 不入冲突）。file 对照窗：--start-file 为 to-sql 必填（扫描起点
    // = 首档头），datetime 同为事件 ts 过滤——两侧共用同一 stop/过滤/文本组。
    let repl_window: Vec<String> = vec![
        "--uri".into(),
        bt.uri.clone(),
        "--start-datetime".into(),
        dt0.clone(),
        "--stop-file".into(),
        fe.clone(),
        "--stop-pos".into(),
        pe.to_string(),
        "--db".into(),
        db.to_string(),
        "--add-extra-info".into(),
    ];
    let file_window: Vec<String> = {
        let mut w = repl_window.clone();
        w.splice(
            2..2,
            [
                "--start-file".into(),
                f0.clone(),
                "--start-pos".into(),
                "4".into(),
            ],
        );
        w
    };
    let mut args: Vec<&str> = vec![
        "repl",
        "--binlog-dir",
        "/nonused",
        // CLI 必填面：datetime 臂给空串哨兵（M3 只禁**非空** start-file）
        "--start-file",
        "",
    ];
    args.extend(repl_window.iter().map(String::as_str));
    let sidb31 = sid(31);
    args.extend([
        "--output-dir",
        &ro,
        "--server-id",
        &sidb31,
        "--heartbeat-secs",
        "10",
        "--threads",
        "1",
    ]);
    let r = run_bin(&args, "dt-repl", Duration::from_secs(150));
    assert!(r.status.success(), "datetime 定位 run 须 exit 0");
    let s = String::from_utf8_lossy(&r.stdout).into_owned();
    assert!(s.contains("repl done") && s.contains("errors=0"), "{s}");

    let blocks = dir_blocks(&dir_r);
    assert!(
        !blocks.is_empty(),
        "datetime 产物不得为空（probe 空转/过滤过度？）"
    );
    // (i) 无更早行：全部块头 datetime ≥ 请求时刻（零补同形串序=时序）
    for b in &blocks {
        assert!(
            b.dt.as_str() >= dt0u.as_str(),
            "块 {}/{} datetime={} 早于请求 {}",
            b.binlog,
            b.start,
            b.dt,
            dt0
        );
    }
    // (ii) 二分落点精确：首块正是 G2 首事务（不滞留 G1、不跳过 G2 头部）
    assert!(
        blk_has(&blocks[0], "'G2doc1中文'".as_bytes()),
        "首块应为 G2 首事件（二分越过/欠冲）: {:?}",
        blocks[0]
    );
    // (iii) 跨档续读：两档都有块
    let mut files: Vec<&str> = blocks.iter().map(|b| b.binlog.as_str()).collect();
    files.dedup();
    assert!(
        files.contains(&f0.as_str()) && files.contains(&f2.as_str()),
        "产物须横跨 {f0}（G2 头部块）与 {f2}（G3）两档，got {files:?}"
    );
    // (iv) 指纹：G2/G3 全量、G1 排除；DTBPAD = stop「再一格」本体（rows
    // 流式在场、其 XID 等号排除，双侧同判）
    let text = blocks_text(&blocks);
    for tok in [
        "'G2doc1中文'",
        "'G2last'",
        "'G3doc1中文'",
        "'G3last'",
        "DTBPAD",
    ] {
        assert!(text.contains(tok), "缺指纹 {tok}");
    }
    for no in ["G1doc1", "G1last"] {
        assert!(!text.contains(no), "{no} 不得入产（早于请求时刻）");
    }

    cmp_to_file_mode(&bt, &dir_r, &f0, &fe, &file_window);
    println!(
        "[dtb] start-datetime {dt0:?} 二分+过滤：{} 块两档，≡file 逐字节",
        blocks.len()
    );
}

/// stop=file+pos 收尾三钉：exit 0、**终档在场且 = 末提交 end_pos**、writer
/// 收尾（files= 计数=盘上实物）+ 末文件完整（≡ file 模式同窗逐字节）。
#[test]
#[ignore = "requires live mysql 8.0 container (make repl-test 门: MY2SQL_TEST_URI+CTR)"]
fn repl_stop_pos_finalizes() {
    live_gate();
    let bt = Bt::new("stp");
    let db = "p3t6bstp";
    bt.seed(db);
    let (f0, p0) = bt.master_pos();
    bt.feed(db, 3, "K");
    let (fk, pk) = bt.master_pos(); // = Xid('Klast') end_pos：末**意图**提交界
    assert_eq!(fk, f0, "本件单档");
    // T6a 关注#1：stop 钉在末提交再一格（PAD 的 end_pos）→ Klast 含、PAD 排
    bt.sql("INSERT INTO p3t6bstp.t_ord (sku, qty, note) VALUES ('STPPAD', 0, 'pad')");
    let (fe, pe) = bt.master_pos();
    assert!(bt.trx_ends(&f0).contains(&pk), "pk 须为真事务收束界");
    assert!(pe > pk);

    let dir_r = bt.sub("repl");
    let ro = dir_r.to_str().unwrap().to_string();
    let window: Vec<String> = vec![
        "--uri".into(),
        bt.uri.clone(),
        "--start-file".into(),
        f0.clone(),
        "--start-pos".into(),
        p0.to_string(),
        "--stop-file".into(),
        fe.clone(),
        "--stop-pos".into(),
        pe.to_string(),
        "--db".into(),
        db.into(),
        "--add-extra-info".into(),
    ];
    let mut args: Vec<&str> = vec!["repl", "--binlog-dir", "/nonused"];
    args.extend(window.iter().map(String::as_str));
    let sidb41 = sid(41);
    args.extend([
        "--output-dir",
        &ro,
        "--server-id",
        &sidb41,
        "--heartbeat-secs",
        "10",
        "--threads",
        "1",
    ]);
    let r = run_bin(&args, "stop-finalize", Duration::from_secs(150));
    assert!(r.status.success(), "stop 到点须优雅 exit 0");
    let s = String::from_utf8_lossy(&r.stdout).into_owned();
    assert!(s.contains("repl done") && s.contains("errors=0"), "{s}");

    // 终档在场 + 恰 = 末提交界 + 对账通过
    let cpr = ro.clone() + "/resume.json";
    checkpoint::read_verify(std::path::Path::new(&cpr), &dir_r)
        .expect("终档 checkpoint 必须在场且过 read_verify");
    let cp = read_cp(&dir_r.join("resume.json")).expect("终档在场");
    assert_eq!(
        (cp.file.as_str(), cp.pos),
        (f0.as_str(), pk),
        "终档 = 末意图提交 end_pos（PAD 排除后）"
    );

    // writer 收尾：摘要 files= 与盘上一一对应；无 torn（末文件完整）
    let n_files = sql_map(&dir_r).len();
    let claimed: usize = s
        .split("files=")
        .nth(1)
        .and_then(|t| t.trim_start().split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|d| d.parse().ok())
        .expect("摘要含 files=<N>");
    assert_eq!(claimed, n_files, "摘要 files= 计数须 = 盘上 .sql 数");
    assert!(n_files >= 1, "单档窗口至少 1 个产物文件");
    for b in dir_blocks(&dir_r) {
        let _ = b; // dir_blocks 自带无-torn 断言
    }

    let text = blocks_text(&dir_blocks(&dir_r));
    assert!(text.contains("'Klast'"), "末意图提交必须在产");
    assert!(
        text.contains("STPPAD"),
        "PAD rows 流式在场（关注#1「再一格」形态本体）"
    );

    cmp_to_file_mode(&bt, &dir_r, &f0, &fe, &window);
    println!(
        "[stop] 终档 {f0}:{pk} == Xid('Klast') end（PAD XID 等号排除、rows 流式在场）；files={claimed} 完整 ≡file"
    );
}

/// 心跳静默耐受：heartbeat 20s、主库静默 60s（> 读超时 2×20+1=41s 的
/// 「无心跳必假死」区间）——零 reconnect 行、进程存活、静默后新流量照常
/// 流式落盘、到 stop 条件干净收尾（假断链 = 把 idle 当死链）。
#[test]
#[ignore = "requires live mysql 8.0 container (make repl-test 门: MY2SQL_TEST_URI+CTR)"]
fn repl_idle_60s_no_false_drop() {
    live_gate();
    let bt = Bt::new("idl");
    let db = "p3t6bidl";
    bt.seed(db);
    let (f0, p0) = bt.master_pos();
    let run = bt.sub("run");
    let ro = run.to_str().unwrap().to_string();
    let stop_unix = chrono::Utc::now().timestamp() + 100;
    let stop = chrono::DateTime::from_timestamp(stop_unix, 0)
        .unwrap()
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let p0s = p0.to_string();
    let mut child = spawn_bin(&[
        "repl",
        "--binlog-dir",
        "/nonused",
        "--uri",
        &bt.uri,
        "--start-file",
        &f0,
        "--start-pos",
        &p0s,
        "--db",
        db,
        "--add-extra-info",
        "--output-dir",
        &ro,
        "--server-id",
        &sid(51),
        "--heartbeat-secs",
        "20",
        "--threads",
        "1",
        "--stop-datetime",
        &stop,
    ]);
    bt.feed(db, 2, "E");
    // ── 静默 60s：每秒验活（假断链会在 ~21/41s 附近爆）──
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(60) {
        assert!(
            child.try_wait().expect("try_wait").is_none(),
            "静默 {}s 时进程退出 = 心跳未续命（假断链，T2 传输缺陷）",
            t0.elapsed().as_secs()
        );
        std::thread::sleep(Duration::from_millis(1000));
    }
    // ── 静默后流量必须照常抓住（流仍是活的）──
    bt.feed(db, 2, "F");
    let tw = Instant::now();
    // 轮询前缀 token：值 = 'Fdoc1中文'（闭引号在中文后），前缀扫足够唯一点验
    // F 段已落盘（精确指纹核点在下方 blocks_text 断言）。
    while !raw_has(&run, b"'Fdoc1") {
        assert!(
            tw.elapsed() < Duration::from_secs(30),
            "静默后 30s 内 F 段落盘？"
        );
        assert!(
            child.try_wait().expect("try_wait").is_none(),
            "F 段等待期进程须存活"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let (fp, pp) = bt.master_pos(); // = Xid('Flast') end（脉冲前最后一提交界）
    assert_eq!(fp, f0, "本件单档");
    let remain = stop_unix + 2 - chrono::Utc::now().timestamp();
    if remain > 0 {
        std::thread::sleep(Duration::from_secs(remain as u64));
    }
    bt.sql("INSERT INTO p3t6bidl.t_ord (sku, qty, note) VALUES ('IDLPULSE', 0, 'stop')");
    let out = wait_bounded(child, "idle-run", Duration::from_secs(120));
    // 日志面 = stdout∪stderr（tracing fmt 默认落 stdout，见重启件 r3 注记）；
    // 静默件是**负断言**，旧形态只扫恒 0B 的 stderr = 空洞真，合并扫才为真。
    let esum = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "idle run 须 exit 0\n日志:\n{esum}");
    let sum = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        sum.contains("repl done") && sum.contains("errors=0"),
        "{sum}"
    );
    assert!(
        !esum.contains("repl: reconnect #"),
        "60s 静默期不得出任何重连行（心跳续命失效）\n日志:\n{esum}"
    );
    let cp = read_cp(&run.join("resume.json")).expect("终档在场");
    assert_eq!(
        (cp.file.as_str(), cp.pos),
        (f0.as_str(), pp),
        "终档 = Flast 提交界（脉冲排除）"
    );
    checkpoint::read_verify(&run.join("resume.json"), &run).expect("read_verify");
    let text = blocks_text(&dir_blocks(&run));
    for tok in ["'Edoc1中文'", "'Elast'", "'Fdoc1中文'", "'Flast'"] {
        assert!(text.contains(tok), "静默前后指纹缺 {tok}");
    }
    assert!(!text.contains("IDLPULSE"), "PULSE 不得入产");
    println!("[idle] 60s 静默零重连；E+F 双段在场；终档 {f0}:{pp} ∈ 事务界");
}

/// threads>1 水位钉（T4 挂账活体版）：--threads 4 + 三股交错流量（双
/// feed_mixed 线程 + 大事务插队）——全程轮询 checkpoint：每次出样的
/// pos ∈ 真事务界集、单调不回退、且**水位声称的每个 (P) 必已全量落盘**
/// （flush-implies-watermark 双向钉：块 stop≤P ⇒ 快照在场）；产物与
/// file 模式同窗逐字节（并行乱序不改出码序）。
#[test]
#[ignore = "requires live mysql 8.0 container (make repl-test 门: MY2SQL_TEST_URI+CTR)"]
fn repl_threads_gt1_watermark_boundaries() {
    live_gate();
    let bt = Bt::new("mtw");
    let db = "p3t6bmtw";
    bt.seed(db);
    let (f0, p0) = bt.master_pos();
    let run = bt.sub("run");
    let ro = run.to_str().unwrap().to_string();
    let stop_unix = chrono::Utc::now().timestamp() + 120;
    let stop = chrono::DateTime::from_timestamp(stop_unix, 0)
        .unwrap()
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let p0s = p0.to_string();
    let mut child = spawn_bin(&[
        "repl",
        "--binlog-dir",
        "/nonused",
        "--uri",
        &bt.uri,
        "--start-file",
        &f0,
        "--start-pos",
        &p0s,
        "--db",
        db,
        "--add-extra-info",
        "--output-dir",
        &ro,
        "--server-id",
        &sid(61),
        "--heartbeat-secs",
        "10",
        "--threads",
        "4",
        "--stop-datetime",
        &stop,
    ]);

    // ── 三股交错流量（X/Y 双 bursts + Z 大事务插队）──
    let alive = Arc::new(AtomicBool::new(true));
    let samples: Samples = Arc::new(Mutex::new(Vec::new()));
    let mon = {
        let (alive, samples, run, cpr) = (
            alive.clone(),
            samples.clone(),
            run.clone(),
            run.join("resume.json"),
        );
        std::thread::spawn(move || {
            let mut last: Option<u32> = None;
            while alive.load(AtomicOrdering::Relaxed) {
                if let Some(cp) = read_cp(&cpr)
                    && last != Some(cp.pos)
                {
                    let (bs, _torn) = dir_blocks_tolerant(&run);
                    samples.lock().expect("samples lock").push((cp.clone(), bs));
                    last = Some(cp.pos);
                }
                std::thread::sleep(Duration::from_millis(40));
            }
        })
    };
    let hz = {
        let c = bt.ctr.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(2200));
            libf_lock_retry(
                "mtw-Z",
                &format!("p3e2e_feed_bigtrx {c} p3t6bmtw 6000 Z"),
                8,
            );
        })
    };
    let hx = {
        let c = bt.ctr.clone();
        std::thread::spawn(move || {
            for i in 1..=7u32 {
                libf_lock_retry("mtw-X", &format!("p3e2e_feed_mixed {c} p3t6bmtw 2 X{i}"), 8);
            }
        })
    };
    let hy = {
        let c = bt.ctr.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(350));
            for i in 1..=7u32 {
                libf_lock_retry("mtw-Y", &format!("p3e2e_feed_mixed {c} p3t6bmtw 2 Y{i}"), 8);
            }
        })
    };
    hx.join().expect("X 灌流线程");
    hy.join().expect("Y 灌流线程");
    hz.join().expect("Z 大事务线程");
    assert!(
        child.try_wait().expect("try_wait").is_none(),
        "灌流期进程必须存活"
    );

    // ── 排水核点：全部提交在场后水位应追至 master pos（marker 界）──
    let (fm, pm) = bt.master_pos();
    assert_eq!(fm, f0, "本件单档");
    let tw = Instant::now();
    while read_cp(&run.join("resume.json")).map(|c| c.pos) != Some(pm) {
        assert!(
            tw.elapsed() < Duration::from_secs(40),
            "40s 内水位须追至 marker 界 {pm}"
        );
        assert!(
            child.try_wait().expect("try_wait").is_none(),
            "追水位期进程须存活"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let remain = stop_unix + 2 - chrono::Utc::now().timestamp();
    if remain > 0 {
        std::thread::sleep(Duration::from_secs(remain as u64));
    }
    bt.sql("INSERT INTO p3t6bmtw.t_ord (sku, qty, note) VALUES ('MTPULSE', 0, 'stop')");
    let out = wait_bounded(child, "mt-run", Duration::from_secs(120));
    alive.store(false, AtomicOrdering::Relaxed);
    mon.join().expect("monitor 线程");
    assert!(out.status.success(), "threads>1 run 须 exit 0");
    let sum = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        sum.contains("repl done") && sum.contains("errors=0"),
        "{sum}"
    );

    // ── 出样核点：界集 ∈、单调、单档 ──
    let ss = samples.lock().expect("final samples").clone();
    assert!(
        ss.len() >= 15,
        "水位出样过少（{}）——交错窗口未张开",
        ss.len()
    );
    let ends = bt.trx_ends(&f0);
    for (cp, _) in &ss {
        assert_eq!(cp.file, f0, "样本公司单档（跨档轮询未建模，拒收）");
        assert!(
            ends.contains(&cp.pos),
            "水位 {} 不在真事务界集（threads>1 越界推水位 = 出账）",
            cp.pos
        );
    }
    for w in ss.windows(2) {
        assert!(
            w[0].0.pos <= w[1].0.pos,
            "水位回退 {} → {}",
            w[0].0.pos,
            w[1].0.pos
        );
    }

    // ── 水位 ⇒ 已落盘（每样自证：stop ≤ P 的基准块必在该样快照内）──
    let final_bs = dir_blocks(&run);
    let mut max_checked = 0u32;
    for (cp, snap) in ss.iter() {
        // 快照必须是终局块列的前缀（append-only 物理事实）
        assert!(
            snap.len() <= final_bs.len() && snap.iter().zip(final_bs.iter()).all(|(a, b)| a == b),
            "水位样快照非终局前缀（乱序出块 = 出码序破坏）"
        );
        if let Some(k) = final_bs.iter().rposition(|b| b.stop <= cp.pos) {
            assert!(
                k < snap.len(),
                "水位声称 pos={}，但 stop≤P 的块到 idx {k} 仅落盘 {}（越水位出账）",
                cp.pos,
                snap.len()
            );
            max_checked = max_checked.max(cp.pos);
        }
    }
    assert_eq!(max_checked, pm, "至少一个样的水位核到 marker 界");

    // ── 终档 + 对账 + 指纹 ──
    let cp = read_cp(&run.join("resume.json")).expect("终档在场");
    assert_eq!(
        (cp.file.as_str(), cp.pos),
        (f0.as_str(), pm),
        "终档 = marker XID end_pos"
    );
    checkpoint::read_verify(&run.join("resume.json"), &run).expect("read_verify");
    let text = blocks_text(&final_bs);
    for tok in [
        "'X1doc1中文'",
        "'X7last'",
        "'Y1trx5'",
        "'Y7last'",
        "'Z1'",
        "'Z6000'",
    ] {
        assert!(text.contains(tok), "交错指纹缺 {tok}");
    }
    assert!(!text.contains("MTPULSE"), "PULSE 排除");
    println!(
        "[mtw] 水位出样 {} 次（全部 ∈ {} 真事务界、单调、水位⇒已落盘）；产物 {} 块 ≡file",
        ss.len(),
        ends.len(),
        final_bs.len()
    );

    // ── file 模式同窗对照：双侧共用同一 stop-datetime 规则（Filters
    // ::pos_stopped 单码），PULSE 首事件 ts ≥ stop 两侧同为界前停 ──
    let fwindow: Vec<String> = vec![
        "--uri".into(),
        bt.uri.clone(),
        "--start-file".into(),
        f0.clone(),
        "--start-pos".into(),
        p0s.clone(),
        "--stop-datetime".into(),
        stop.clone(),
        "--db".into(),
        db.to_string(),
        "--add-extra-info".into(),
    ];
    cmp_to_file_mode(&bt, &run, &f0, &fm, &fwindow);
}

/// 终审 FIX D live 件（空闲 master + SIGINT → exit 130）：默认心跳 30s
/// （不传 `--heartbeat-secs`——本件合同就是「默认心跳为中断延迟兜底」），
/// 开流后主库全程静默（只有心跳帧）。修复前形态：ReplSource 把心跳
/// `continue` 内部消化、帧循环无人看中断旗标，Ctrl-C 只能等传输层读
/// 超时（2d+1s）——exit 130 与 stop 评估在空闲 master 上无限期停摆。
/// 新契约：中断在 ReplSource 循环顶门按帧节奏落地 → 停泵→drain→
/// checkpoint→exit 130，延迟 ≤ 一个心跳周期。硬超时 90s ≫ 30s 周期；
/// resume.json 在场即收尾链完整（水位 = 定位起点、零产物零名单）。
#[test]
#[ignore = "requires live mysql 8.0 container (make repl-test 门: MY2SQL_TEST_URI+CTR)"]
fn repl_sigint_idle_master_exits_130() {
    live_gate();
    let bt = Bt::new("sig");
    let db = "p3tfxdsg";
    bt.seed(db);
    let (f0, p0) = bt.master_pos();
    let run = bt.sub("run");
    let ro = run.to_str().unwrap().to_string();
    let p0s = p0.to_string();
    let mut child = spawn_bin(&[
        "repl",
        "--binlog-dir",
        "/nonused",
        "--uri",
        &bt.uri,
        "--start-file",
        &f0,
        "--start-pos",
        &p0s,
        "--db",
        db,
        "--output-dir",
        &ro,
        "--server-id",
        &sid(71),
        "--threads",
        "1",
    ]);
    // ≥2 个心跳周期（62s）：进程必须存活（心跳续命正常、不误判死链），
    // 同时确证流已进「追平活写尾、按周期收心跳」的稳态形态。
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(62) {
        assert!(
            child.try_wait().expect("try_wait").is_none(),
            "空闲 {}s 时进程自行退出 = 心跳假死断链（非本件合同）",
            t0.elapsed().as_secs()
        );
        std::thread::sleep(Duration::from_millis(1000));
    }
    let pid = child.id().to_string();
    let sig = ProcCommand::new("kill")
        .args(["-INT", &pid])
        .status()
        .expect("spawn kill");
    assert!(sig.success(), "kill -INT {pid} 失败: {sig}");
    let t1 = Instant::now();
    let out = wait_bounded(child, "sigint-idle", Duration::from_secs(90));
    let esum = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        out.status.code(),
        Some(130),
        "空闲 master SIGINT 必须 130 收尾（修复前停摆不返 = 本件红）\n日志:\n{esum}"
    );
    println!(
        "[fixD] 空闲 {}s → SIGINT → exit 130，收尾延迟 {:?}",
        t0.elapsed().as_secs(),
        t1.elapsed()
    );
    let cp = read_cp(&run.join("resume.json")).expect("终档在场（130 收尾链完整）");
    assert_eq!(
        (cp.file.as_str(), cp.pos),
        (f0.as_str(), p0),
        "零事件空闲 run：终档 = 本次定位起点"
    );
    assert!(cp.written_files.is_empty(), "零事件不得有产物名单: {cp:?}");
    checkpoint::read_verify(&run.join("resume.json"), &run).expect("终档自洽");
}

// ────────────────────────────────────────────────────────────────────────────
// P4a Lane D：5.6/5.7 idle 窗心跳帧形 live 件（spec §4，T0 挂账残余半面）。
//
// 形状 = repl_idle_60s_no_false_drop 全形态逐段移植（60s 静默逐秒验活、
// 零 `repl: reconnect #`、静默后 F 段捕获、`repl done errors=0`、终档 =
// Flast 提交界、read_verify、双段指纹 + PULSE 排除），外加**追平判据升格**
// （spec §4）：静默窗 binlog 经 p3e2e_capture_binlogs 取档 → file 模式同窗
// to-sql 基准 → blocks_text 序列对账（T6a 切片段同款比较纪律；此处不逐字
// 节钉文件——版本 server 端注释/DDL 噪音容差按既有切片件口径）。
//
// 帧形观测（真实跑逐字转录，见各件 doc 注记 + task-4 报告）：
//   ① `SHOW GLOBAL VARIABLES LIKE 'binlog_heartbeat%'` 变量面探测（sql_soft
//      容忍失败/空集）；② 心跳帧在场证据 = 60s 静默零假断链（负断言）+
//      同构造 raw 帧流探针（examples/ 一次性观测件，跑后即删不入仓）记录
//      的帧型字节。ReplSource 对 0x1b 无 trace 面（src/repl/source.rs:319
//      静默 continue），按红线**不新增生产日志**。
// ────────────────────────────────────────────────────────────────────────────

/// 两件共享体：`ver` = 容器版本、`db` = 专属库名（p4aidl57/p4aidl56）。
/// server_id 基数 `sidbase` 各件独用（81/91）防并发邻居撞线。
fn idle_heartbeat_case(ver: &str, slug: &str, db: &str, sidbase: u32) {
    live_gate();
    let bt = Bt::new_ver(slug, ver);
    bt.seed(db);
    let server_v = libf(&format!("p3e2e_sql {} -N -e \"SELECT VERSION()\"", bt.ctr))
        .trim()
        .to_string();
    // ── 心跳变量面探测（brief Step 2）：sql_soft 容忍（5.6 无此变量 =
    // 空集/失败均不红，注记走 fallback 文本）──
    let hbprobe = libf_soft(&format!(
        "p3e2e_sql {} -N -e \"SHOW GLOBAL VARIABLES LIKE 'binlog_heartbeat%'\"",
        bt.ctr
    ));
    match &hbprobe {
        Ok(o) if !o.trim().is_empty() => {
            println!(
                "[idle-hb:{ver}] server={server_v} binlog_heartbeat% = {}",
                o.trim()
            );
        }
        _ => {
            if ver == "5.6" {
                // 5.6 面注记（T5 合流裁定：以实测真相替换 brief 冻结的 fallback 措辞）：
                println!(
                    "[idle-hb:{ver}] server={server_v} 5.6 面：SHOW 变量面为空；心跳周期 \
                     实走会话级 SET @master_heartbeat_period 载荷（SHOW GLOBAL VARIABLES \
                     LIKE 'binlog_heartbeat%' 探测: {:?}）",
                    hbprobe.map(|o| o.trim().to_string())
                );
            } else {
                println!(
                    "[idle-hb:{ver}] server={server_v} binlog_heartbeat% 变量面为空 \
                     （SHOW GLOBAL VARIABLES LIKE 'binlog_heartbeat%' 探测: {:?}；\
                     心跳周期实走会话级 SET @master_heartbeat_period 通道，帧形见件注）",
                    hbprobe.map(|o| o.trim().to_string())
                );
            }
        }
    }
    let (f0, p0) = bt.master_pos();
    let run = bt.sub("run");
    let ro = run.to_str().unwrap().to_string();
    let stop_unix = chrono::Utc::now().timestamp() + 100;
    let stop = chrono::DateTime::from_timestamp(stop_unix, 0)
        .unwrap()
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let p0s = p0.to_string();
    let mut child = spawn_bin(&[
        "repl",
        "--binlog-dir",
        "/nonused",
        "--uri",
        &bt.uri,
        "--start-file",
        &f0,
        "--start-pos",
        &p0s,
        "--db",
        db,
        "--add-extra-info",
        "--output-dir",
        &ro,
        "--server-id",
        &sid(sidbase),
        "--heartbeat-secs",
        "20",
        "--threads",
        "1",
        "--stop-datetime",
        &stop,
    ]);
    bt.feed(db, 2, "E");
    // ── 静默 60s：每秒验活（无心跳续命的假断链会在 ~21/41s 附近爆）──
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(60) {
        assert!(
            child.try_wait().expect("try_wait").is_none(),
            "[idle-hb:{ver}] 静默 {}s 时进程退出 = 心跳未续命（假断链）",
            t0.elapsed().as_secs()
        );
        std::thread::sleep(Duration::from_millis(1000));
    }
    println!(
        "[idle-hb:{ver}] 60s 静默验活通过（{}s 全程存活）",
        t0.elapsed().as_secs()
    );
    // ── 静默后流量必须照常抓住（流仍是活的）──
    bt.feed(db, 2, "F");
    let tw = Instant::now();
    while !raw_has(&run, b"'Fdoc1") {
        assert!(
            tw.elapsed() < Duration::from_secs(30),
            "[idle-hb:{ver}] 静默后 30s 内 F 段落盘？"
        );
        assert!(
            child.try_wait().expect("try_wait").is_none(),
            "[idle-hb:{ver}] F 段等待期进程须存活"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let (fp, pp) = bt.master_pos(); // = Xid('Flast') end（脉冲前最后一提交界）
    assert_eq!(fp, f0, "本件单档");
    let remain = stop_unix + 2 - chrono::Utc::now().timestamp();
    if remain > 0 {
        std::thread::sleep(Duration::from_secs(remain as u64));
    }
    bt.sql(&format!(
        "INSERT INTO {db}.t_ord (sku, qty, note) VALUES ('IDLPULSE', 0, 'stop')"
    ));
    let out = wait_bounded(child, &format!("idle-hb-{ver}"), Duration::from_secs(120));
    let esum = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "[idle-hb:{ver}] run 须 exit 0\n日志:\n{esum}"
    );
    let sum = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        sum.contains("repl done") && sum.contains("errors=0"),
        "{sum}"
    );
    let rlines: Vec<&str> = esum
        .lines()
        .filter(|l| l.contains("repl: reconnect #"))
        .collect();
    assert!(
        rlines.is_empty(),
        "[idle-hb:{ver}] 60s 静默期不得出任何重连行（心跳续命失效）\n日志:\n{esum}"
    );
    let cp = read_cp(&run.join("resume.json")).expect("终档在场");
    assert_eq!(
        (cp.file.as_str(), cp.pos),
        (f0.as_str(), pp),
        "终档 = Flast 提交界（脉冲排除）"
    );
    checkpoint::read_verify(&run.join("resume.json"), &run).expect("read_verify");
    let rb = dir_blocks(&run);
    let text = blocks_text(&rb);
    for tok in ["'Edoc1中文'", "'Elast'", "'Fdoc1中文'", "'Flast'"] {
        assert!(text.contains(tok), "静默前后指纹缺 {tok}");
    }
    assert!(!text.contains("IDLPULSE"), "PULSE 不得入产");

    // ── 追平判据升格（spec §4）：窗 binlog → file 模式同窗 to-sql 基准 →
    // blocks_text 序列对账（T6a 切片比较纪律，双侧非空反假绿）──
    let bins = bt.sub("bins-cmp");
    let fout = bt.sub("file-cmp");
    libf(&format!(
        "p3e2e_capture_binlogs {} {f0} {f0} {}",
        bt.ctr,
        bins.display()
    ));
    let window: Vec<String> = vec![
        "--uri".into(),
        bt.uri.clone(),
        "--start-file".into(),
        f0.clone(),
        "--start-pos".into(),
        p0s.clone(),
        "--stop-datetime".into(),
        stop.clone(),
        "--db".into(),
        db.to_string(),
        "--add-extra-info".into(),
    ];
    let mut args: Vec<&str> = vec!["to-sql", "--binlog-dir", bins.to_str().unwrap()];
    args.extend(window.iter().map(String::as_str));
    args.extend(["--output-dir", fout.to_str().unwrap()]);
    let r = run_bin(&args, "idle-hb-file-cmp", Duration::from_secs(180));
    assert!(r.status.success(), "file 模式同窗基准须 exit 0");
    let fs = String::from_utf8_lossy(&r.stdout).into_owned();
    assert!(
        fs.contains("to-sql done") && fs.contains("errors=0"),
        "file 基准摘要: {fs}"
    );
    let fbs = dir_blocks(&fout);
    assert!(!rb.is_empty() && !fbs.is_empty(), "双侧空对账 = 假绿禁止");
    assert_eq!(
        rb.len(),
        fbs.len(),
        "repl {} 块 vs file {} 块（静默窗追平块数分歧）",
        rb.len(),
        fbs.len()
    );
    assert_eq!(
        blocks_text(&rb),
        blocks_text(&fbs),
        "repl 产物 vs file 模式同窗 blocks_text 序列分歧（静默窗后未追平？）"
    );
    println!(
        "[idle-hb:{ver}] 60s 静默零重连；E+F 双段在场；终档 {f0}:{pp} ∈ 事务界；\
         追平对账 repl {} 块 ≡ file {} 块（{}B 语句流）",
        rb.len(),
        fbs.len(),
        text.len()
    );
}

/// 5.7 件帧形注记（2026-09-22 真机首跑逐字，探针 = 生产 transport 同构造
/// 一次性观测件，跑后即删；报告 task-4 留档）：
/// `SHOW GLOBAL VARIABLES LIKE 'binlog_heartbeat%'` = **空集**（5.7.44 无该
/// 全局变量面），心跳周期实走会话级 `SET @master_heartbeat_period`（T7 已证
/// SET 被接受，本件补「帧实际到场」半面）。60s 静默窗探针逐字：
/// ```text
/// [probe] version=5.7.44-log master=mysql-bin.000003:154 heartbeat=20s
/// [probe] t=020.0s kind=0x1b ts=0 log_pos=154 size=39 body=[109, 121, 115, 113, 108, 45, 98, 105, 110, 46, 48, 48, 48, 48, 48, 51, 147, 93, 75, 38]
/// [probe] t=040.0s kind=0x1b ts=0 log_pos=154 size=39 body=[…同上逐帧全等…]
/// [probe] t=060.0s kind=0x1b ts=0 log_pos=154 size=39 …
/// [probe] t=080.0s kind=0x1b ts=0 log_pos=154 size=39 …
/// ```
/// 即 v1 心跳（0x1b）、ts=0、header log_pos=静默期主库活写位点、体 = 16B
/// 日志文件名 + 4B 尾（帧间恒等内容尾 4B 恒定 = CRC32 口径，同 8.0 件
/// spec §2 勘误-4），**非** fake rotate（流首 0x04 合成帧另算）。本件实测：
/// 60s 静默零 `repl: reconnect #`；终档 mysql-bin.000003:14777 = Flast 提交
/// 界；追平对账 repl 38 块 ≡ file 38 块（9795B 语句流）。
#[test]
#[ignore = "requires live mysql 5.7 container (make repl-test 门: MY2SQL_TEST_URI+CTR)"]
fn repl_idle_heartbeat_5_7() {
    live_gate();
    idle_heartbeat_case("5.7", "hb57", "p4aidl57", 81);
}

/// 5.6 件帧形注记（2026-09-22 真机首跑逐字，同 5.7 件探针构造）：
/// `SHOW GLOBAL VARIABLES LIKE 'binlog_heartbeat%'` = **空集**（5.6.51 无该
/// 变量；brief fallback 口径「5.6 面：心跳周期纯客户端 COM_BINLOG_DUMP 载荷，
/// SET 通道不存在」由件内 println 转录——**实测修正**：会话级
/// `SET @master_heartbeat_period` 通道在 5.6.51 真实生效，见下帧流）。60s
/// 静默窗探针逐字：
/// ```text
/// [probe] version=5.6.51-log master=mysql-bin.000004:120 heartbeat=20s
/// [probe] t=020.0s kind=0x1b ts=0 log_pos=120 size=39 body=[109, 121, 115, 113, 108, 45, 98, 105, 110, 46, 48, 48, 48, 48, 48, 52, 123, 241, 67, 155]
/// [probe] t=040.0s kind=0x1b ts=0 log_pos=120 size=39 body=[…逐帧全等…]
/// [probe] t=060.0s kind=0x1b ts=0 log_pos=120 size=39 …
/// [probe] t=080.0s kind=0x1b ts=0 log_pos=120 size=39 …
/// ```
/// 即 5.6 与 5.7 **同帧形**（v1 0x1b、ts=0、活写位点、16B 名 + 4B 尾），
/// 无需 fake-rotate 续命退路；断言面按 brief 冻结 = 零 reconnect + 追平（帧
/// 形只注记不钉）。本件实测：60s 静默零 `repl: reconnect #`；终档
/// mysql-bin.000004:12437 = Flast 提交界；追平对账 repl 38 块 ≡ file 38 块
/// （9847B 语句流）。seed 走 lib 既有 5.6 LONGTEXT 降级（JSON 列探测）。
#[test]
#[ignore = "requires live mysql 5.6 container (make repl-test 门: MY2SQL_TEST_URI+CTR)"]
fn repl_idle_heartbeat_5_6() {
    live_gate();
    idle_heartbeat_case("5.6", "hb56", "p4aidl56", 91);
}
