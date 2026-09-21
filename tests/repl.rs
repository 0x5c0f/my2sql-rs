//! P3 T2 对测（byte-parity 双通道闸）：`tests/common/synth.rs::frame_bytes`
//! 导出的文件同构全帧经 `ReplSource` ≡ 同一字节流过 `FileReader`。
//!
//! 与 src/repl/source.rs 内置单测互为两测（冻结区下的有意重复由双侧
//! 同红钉死）：本文件走 **tests 公共夹具** 通道（P2 起的 e2e 事件族，
//! 含 rows 解码实义体），源内单测走 build_frame 手搓通道（合成帧/心跳/
//! 断链形态）。CRC 两态（无 checksum 5.6 流 / CRC32 8.0 流含 FDE 特例
//! 掩位）各拉一遍：binlog 名（含 rotate 更名时序）/start_pos/end_pos/
//! timestamp/kind/body 逐字段一致。

#![cfg_attr(test, allow(unused))]

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
