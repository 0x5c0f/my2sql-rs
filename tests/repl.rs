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
