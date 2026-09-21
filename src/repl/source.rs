//! repl 事件源：`FrameStream` 全帧 → `RawEvent`（Task 2）。
//!
//! ## 解码冻结下的有意重复（两测互钉）
//!
//! `src/binlog/` 为 P2 终审冻结区（spec §7-8：`git diff main..HEAD --
//! src/binlog/` 必须为空），故本模块**有意重复** `binlog/file_reader.rs`
//! 的事件编排骨架（handle_fde/query_text/server_version_ge/解码路由约
//! 80 行），而非共享代码——两侧只用公开件（`parse_header`/`crc32_ok`/
//! `fde_checksum_ok`/`strip_checksum`），口径漂移必红其一：
//! - 单测：`repl_source_maps_synthetic_stream_byte_equal_to_file_reader`
//!   （本文件，同一合成事件集分喂 FileReader 与 ReplSource，
//!   RawEvent 逐字段 + body 字节一致）；
//! - 对测：`tests/repl.rs::synth_frame_export_is_byte_equal_through_repl_source`
//!   （`tests/common/synth.rs::frame_bytes` 导出的文件同构全帧经
//!   ReplSource ≡ 同一字节流过 FileReader）。
//!
//! 模块注释引用与 [`crate::binlog::file_reader`] 逐字同源（下抄），两
//! 侧改注释必查另一侧。
//!
//! ## 上游口径对照（my2sql-go base/file.go/com.go，权威引用——原样
//! ## 引用自 src/binlog/file_reader.rs）
//!
//! - rows 事件的 start_pos = 所属 TABLE_MAP 事件的起始位置
//!   （file.go:197-198 `if h.EventType == TABLE_MAP_EVENT { tbMapPos =
//!   h.LogPos - h.EventSize }`，file.go:214-215 `oneMyEvent{... StartPos:
//!   tbMapPos}`——rows 事件打印/进 extra-info 用的都是「最近一次
//!   table_map 的起始」，非 rows 事件本体起始）。本层非 rows 事件
//!   （Query/Xid/Rotate/…）start_pos = 自身起始（log_pos - event_size，
//!   上游 stats 通道对 query 亦此口径，file.go:276）。
//! - ROTATE 只改名不切文件（com.go:41-46 更新「当前文件名」用于位点
//!   比较标签）；rotate 事件本身记旧名、其后事件记新名
//!   （file.go:214→217 时序）。
//! - checksum：上游从不验证，本层对每帧验 crc32（敌意输入防线，与
//!   file_reader 同立场；FDE 特例见 event.rs）。
//!
//! ## repl 特有口径（spec §2 Task 0 spike 实测勘误）
//!
//! - **合成帧头位点永不进位点链**：流首 fake rotate（ts=0、flags
//!   ARTIFICIAL 0x20、header log_pos=0、payload position=请求起点）、
//!   mid-file 合成 FDE（header log_pos 清 0）、EOF 切换 ROTATE（ts=0、
//!   header log_pos=0、payload position=4）都是 dump 线程合成、文件中
//!   本不存在。链从请求起点起种、只用真帧 log_pos 推进；合成帧只做
//!   改名（payload/hint 指名下一文件）与链复位，其头位点绝不消费。
//! - 流首 fake rotate **不产出**（file 模式无对应体，产出即破坏逐字节
//!   等价）；EOF 切换 ROTATE 照常产出 Rotate（跟文件语义 com.go:41-46），
//!   位点标注取链尾。
//! - **CRC 通过性探测**（spike 实测-3）：EOF 合成帧带 CRC 尾、流首
//!   fake rotate 不带——对 log_pos=0 的合成帧验过才剥（不判损坏），真帧
//!   维持 file_reader 的「with_crc 必验、不过即 ChecksumMismatch」硬闸。
//! - 心跳 v1（0x1b）内部消化：header log_pos 为主库活写位点，仅证
//!   链路存活（T5 探活），不产 SQL、不推链/checkpoint（spec §4/§6）。
//! - **断链 ≠ 干净停止**（spike 实测-6）：`Ok(None)` 只留给消费方判定
//!   的干净停止（stop 命中、测试流自然耗尽）；上游 `Err(Disconnect)`
//!   原样翻成 BinlogError 上抛（trait 签名冻结），变体经
//!   [`ReplSource::transport_error`] 保真给 T5 重连分类学。

use std::sync::Arc;

use crate::binlog::error::BinlogError;
use crate::binlog::event::{
    EVENT_HEADER_SIZE, EventType, crc32_ok, fde_checksum_ok, parse_header, strip_checksum,
};
use crate::binlog::rows::RowsKind;
use crate::binlog::table_map::{self, TableMapEvent};
use crate::pipeline::filter::Filters;
use crate::pipeline::source::{EventSource, RawEvent, RawKind};
use crate::repl::transport::{FrameStream, ReplError};

/// 复制流事件源（与 FileReader 同形异构：帧驱动、无 seek）。
pub struct ReplSource {
    transport: Box<dyn FrameStream>,
    /// 当前 binlog 文件名（rotate/合成帧 hint 更名，上游 com.go:41-46）。
    name: String,
    filters: Filters,
    /// FDE 声明 CRC32（binlog v4 且 alg=1）；未知（FDE 前）→ 事件不可解。
    with_crc: bool,
    seen_fde: bool,
    /// 最近一个 TABLE_MAP（rows 事件携带，Arc 共享——与 FileReader 同缝）。
    tm: Option<Arc<TableMapEvent>>,
    /// 最近一个 TABLE_MAP 的起始位（rows 事件 start_pos，上游 tbMapPos）。
    tm_pos: u32,
    done: bool,
    /// 位点链：最近一个真帧的尾位点（合成帧头 log_pos=0 绝不写入；
    /// 种子 = 请求起点，fake rotate payload position 到达时校正）。
    chain: u32,
    /// 传输层错误快照（EventSource Err 类型冻结为 BinlogError，重连
    /// 分类学依赖的 ReplError 变体在此保真——T5 经 accessor 取）。
    transport_err: Option<ReplError>,
}

impl ReplSource {
    /// 与 `FileReader::new(name, rdr, filters)` 同形构造器
    /// （`src/binlog/file_reader.rs:70`）。链种子 = filters.start 中
    /// 与首文件名一致的位点分量（请求起点），缺省 4（文件头）。
    pub fn new(transport: Box<dyn FrameStream>, first_binlog: String, filters: Filters) -> Self {
        let chain = match filters.start.as_ref() {
            Some((f, p)) if *f == first_binlog => *p,
            _ => 4,
        };
        Self {
            transport,
            name: first_binlog,
            filters,
            with_crc: false,
            seen_fde: false,
            tm: None,
            tm_pos: 0,
            done: false,
            chain,
            transport_err: None,
        }
    }

    /// 最近一次传输层错误（重连分诊入口，spec §6；无错误 = None）。
    pub fn transport_error(&self) -> Option<&ReplError> {
        self.transport_err.as_ref()
    }

    /// 当前文件名（rotate/心跳后已更新；§6 重连日志用）。
    pub fn current_binlog(&self) -> &str {
        &self.name
    }

    /// 最近一次链尾位点（合成帧不参与；checkpoint 装配层 T4 参考）。
    pub fn chain_pos(&self) -> u32 {
        self.chain
    }

    /// 处理 dump 线程合成 ROTATE（流首 fake rotate / EOF 切换帧）：
    /// payload = 8B position（流首=请求起点、EOF=下一文件头 4）+ 下一
    /// 文件名——只据此**改名与定链**，其 header log_pos（恒 0）绝不消费
    /// （spec §2 勘误-3）。url 缺失时回退 `binlog_hint`（transport 由
    /// crate 解出的干净名）。
    fn adopt_synthetic_rotate(&mut self, body: &[u8], hint: Option<String>) {
        if let Ok((pos, url)) = rotate_payload(body) {
            if !url.is_empty() {
                self.name = url;
            } else if let Some(h) = hint {
                self.name = h;
            }
            self.chain = if pos > 0 {
                pos.min(u32::MAX as u64) as u32
            } else {
                4
            };
        } else if let Some(h) = hint {
            self.name = h;
            self.chain = 4;
        }
    }

    /// FDE：binlog 版本/服务端版本甄别 + checksum 有无判定。
    /// 与 `FileReader::handle_fde`（file_reader.rs:126-174）逐行同口径的
    /// 有意重复（冻结区不可共享；判定次序论证见彼处注释，T17 真机勘误：
    /// 5.7 起 FDE **恒带 CRC 尾**，即便 binlog_checksum=NONE——旧判定
    /// 错把 body[-1] 当 alg）。repl 特有形态仅一点：mid-file 起点 dump
    /// 的流首 FDE 是合成帧（header log_pos=0），但**体是真文件 FDE 字节**
    /// （crate 原样交付），判定只读物内容、与 header 位点无关，故实现
    /// 与 file 模式逐字相同。
    fn handle_fde(&mut self, full: &[u8]) -> Result<(), BinlogError> {
        let body = full.get(EVENT_HEADER_SIZE..).ok_or(BinlogError::TooShort)?;
        // 2(ver)+50(server)+4(create_ts)+1(hdr_len) 最小骨架
        if body.len() < 57 {
            return Err(BinlogError::TooShort);
        }
        let ver = u16::from_le_bytes([body[0], body[1]]);
        if ver != 4 {
            return Err(BinlogError::InvalidData(format!(
                "unsupported binlog format version {ver}: P1 supports v4 only \
                 (MySQL 5.0+); pre-5.0 v1/v2 files lack a usable FDE"
            )));
        }
        let server = &body[2..52];
        if server
            .to_ascii_lowercase()
            .windows(b"mariadb".len())
            .any(|w| w == b"mariadb")
        {
            let s = String::from_utf8_lossy(server);
            return Err(BinlogError::InvalidData(format!(
                "MariaDB binlog not supported in P1 (server version {s})"
            )));
        }
        if body[56] != 19 {
            return Err(BinlogError::InvalidData(format!(
                "FDE common event header length {} != 19",
                body[56]
            )));
        }
        self.with_crc = false;
        if server_version_ge(server, (5, 6, 1)) {
            if full.len() >= EVENT_HEADER_SIZE + 5 && fde_checksum_ok(full) {
                // FDE 带合法 CRC 尾 → alg 字节在 len-5（go-mysql event.go:186 同位）
                if full[full.len() - 5] == 1 {
                    self.with_crc = true;
                }
            } else if body[body.len() - 1] == 1 {
                // 无 CRC 尾形态（5.6.1~5.6.x 部分构建/合流）且 alg 声称 CRC32
                // 但 FDE 验证不过 → 损坏/改写
                return Err(BinlogError::ChecksumMismatch);
            }
        }
        self.seen_fde = true;
        Ok(())
    }

    /// QUERY 事件体 → (schema, SQL 文本)。与 `FileReader::query_text`
    /// （file_reader.rs:179-196）同口径有意重复；布局对照 vendored
    /// go-mysql event.go:304-333（binlog v4：proxy 4B + exec_time 4B +
    /// schema_len 1B + err 2B + status_vars_len 2B + status vars +
    /// schema + 0x00 + query）。
    fn query_text(body: &[u8]) -> Result<(String, String), BinlogError> {
        if body.len() < 13 {
            return Err(BinlogError::TooShort);
        }
        let slen = body[8] as usize;
        let svlen = u16::from_le_bytes([body[11], body[12]]) as usize;
        let schema_at = 13 + svlen;
        let sql_at = schema_at + slen + 1; // +0x00 终止符
        let schema = body
            .get(schema_at..schema_at + slen)
            .ok_or(BinlogError::TooShort)?;
        // sql 可为空（GTID 载体的空 query event）
        let sql = body.get(sql_at..).ok_or(BinlogError::TooShort)?;
        Ok((
            String::from_utf8_lossy(schema).into_owned(),
            String::from_utf8_lossy(sql).into_owned(),
        ))
    }

    fn make(&self, start: u32, end: u32, ts: u32, kind: RawKind, body: Vec<u8>) -> RawEvent {
        RawEvent {
            binlog: self.name.clone(),
            start_pos: start,
            end_pos: end,
            timestamp: ts,
            kind,
            body,
            tm: None,
        }
    }
}

/// ROTATE 体（已剥 CRC）→ (下一位点, 下一文件名)。布局：8B position LE +
/// 名字节（无 NUL，文件尾截断即界）。
fn rotate_payload(body: &[u8]) -> Result<(u64, String), BinlogError> {
    if body.len() < 8 {
        return Err(BinlogError::TooShort);
    }
    let pos = u64::from_le_bytes(body[..8].try_into().unwrap());
    let url = String::from_utf8_lossy(&body[8..]).into_owned();
    Ok((pos, url))
}

impl EventSource for ReplSource {
    fn next(&mut self) -> Result<Option<RawEvent>, BinlogError> {
        loop {
            if self.done {
                return Ok(None);
            }
            // ---- 取帧 ----（Ok(None) 只留给消费方语义；生产断链在
            // transport 已折叠为 Err(Disconnect)，spike 实测-6①）
            let frame = match self.transport.next_frame() {
                Ok(Some(f)) => f,
                Ok(None) => {
                    self.done = true; // 测试流耗尽 = 干净结束
                    return Ok(None);
                }
                Err(e) => {
                    let msg = e.to_string();
                    self.transport_err = Some(e);
                    return Err(BinlogError::InvalidData(format!("repl: {msg}")));
                }
            };
            let full = frame.bytes;
            let hint = frame.binlog_hint;
            let h = parse_header(&full)?;
            // Event::write 重建恒等于 header event_size（spike 实测-1），
            // 不符 = 残帧/改判 → 协议硬错误（file 模式无此闸：字节流按
            // event_size 自取，repl 帧自足，验证其自洽）。
            if full.len() as u32 != h.event_size {
                return Err(BinlogError::InvalidData(format!(
                    "frame size {} != header event_size {} (not file-isomorphic)",
                    full.len(),
                    h.event_size
                )));
            }
            let t = h.event_type.0;
            // 心跳 0x1b：内部消化（不产出、不推链、不进 stop 判定——
            // 其 ts=0/活位点均非数据事件口径，spec §6）。
            if t == EventType::HEARTBEAT {
                continue;
            }
            // dump 线程合成帧（流首 fake rotate / mid-file FDE / EOF
            // 切换 ROTATE）的唯一可靠判别：header log_pos 清 0
            // （spike 勘正：`RotateEvent::is_fake()` 查 payload、vendor
            // 语义与本链路不同染，不可用）。
            let synthetic = h.log_pos == 0;
            let own_start = if synthetic {
                self.chain
            } else {
                h.log_pos.saturating_sub(h.event_size)
            };
            let end_pos = if synthetic { self.chain } else { h.log_pos };
            // ---- stop 判定：header 之后、body 之前（file_reader 同款
            // 收紧；合成帧以链尾标位，等号排除在 Filters 内）----
            if self.filters.pos_stopped(&self.name, end_pos, h.timestamp) {
                self.done = true;
                return Ok(None);
            }
            let mut body = full[EVENT_HEADER_SIZE..].to_vec();
            if t == EventType::FORMAT_DESC {
                self.handle_fde(&full)?;
                continue; // FDE 由源消化（checksum 口径），不产出
            }
            if !self.seen_fde {
                if synthetic && t == EventType::ROTATE {
                    // 流首 fake rotate：改名 + 以 payload position 校正
                    // 链种（= 请求起点）。**不产出**——文件中本不存在此
                    // 帧，产出即破坏与 file 模式的逐字节等价（§7-1 总闸）。
                    self.adopt_synthetic_rotate(&body, hint);
                    continue;
                }
                // go-mysql parser.go:329-332：非 FDE 事件必须先有 FDE
                return Err(BinlogError::InvalidData(
                    "event before any format_description event".into(),
                ));
            }
            // checksum：真帧与 file 模式同硬闸；合成帧（EOF rotate 带尾、
            // relay 人工 rotate 不带）只验过才剥、不判损坏。
            if self.with_crc {
                if synthetic {
                    if crc32_ok(&full) {
                        strip_checksum(&mut body, true);
                    }
                } else {
                    if !crc32_ok(&full) {
                        return Err(BinlogError::ChecksumMismatch);
                    }
                    strip_checksum(&mut body, true);
                }
            }
            if !synthetic {
                self.chain = h.log_pos; // 链只被真帧推进
            }
            let pending = self.filters.pos_pending(&self.name, end_pos, h.timestamp);
            // 结构性事件恒处理（窗口外也吃——上游 file.go:118 不 seek +
            // 197 tbMapPos 先于 header 条件检查的同款事实）：
            if t == EventType::TABLE_MAP {
                let tm = table_map::parse_table_map(&body, false)?;
                self.tm_pos = own_start;
                self.tm = Some(Arc::new(tm));
                continue; // TABLE_MAP 不产出（RawKind 无该变体，上游 default→continue）
            }
            if pending {
                continue;
            }
            let kind = match t {
                EventType::QUERY => {
                    let (_db, sql) = Self::query_text(&body)?;
                    RawKind::Query(sql)
                }
                EventType::XID => RawKind::Xid,
                EventType::GTID_LOG | EventType::ANONYMOUS_GTID_LOG => RawKind::Gtid,
                EventType::ROTATE => {
                    let (pos, mut url) = rotate_payload(&body)?;
                    if url.is_empty() {
                        url = hint.unwrap_or_default();
                    }
                    // 先以「当前名」产出（上游 file.go:214 早于 com.go:43
                    // 改名），再更新比较用文件名。EOF 切换帧（synthetic）
                    // 照常产出——跨文件跟名是 com.go:41-46 真语义，其头
                    // 位点仍不进链（下方 adopt 复位 payload=4）。
                    let ev = self.make(
                        own_start,
                        end_pos,
                        h.timestamp,
                        RawKind::Rotate(url.clone()),
                        body,
                    );
                    self.name.clone_from(&url);
                    if synthetic {
                        self.chain = if pos > 0 {
                            pos.min(u32::MAX as u64) as u32
                        } else {
                            4
                        };
                    }
                    return Ok(Some(ev));
                }
                EventType::PREVIOUS_GTIDS => continue, // 结构性消化
                // rows 路由（file_reader 同款 T12 路由义务）：
                EventType::WRITE_ROWS_V0
                | EventType::UPDATE_ROWS_V0
                | EventType::DELETE_ROWS_V0 => {
                    return Err(BinlogError::InvalidData(
                        "V0 row events (MySQL ≤5.1, 4-byte table_id) are not supported; \
                         P1 matrix is 5.6+"
                            .into(),
                    ));
                }
                39 => return Err(BinlogError::PartialNotSupported), // PARTIAL_UPDATE_ROWS_V2
                kt @ (23 | 24 | 25 | 30 | 31 | 32) => {
                    let (rk, v2) = match kt {
                        23 => (RowsKind::Write, false),
                        24 => (RowsKind::Update, false),
                        25 => (RowsKind::Delete, false),
                        30 => (RowsKind::Write, true),
                        31 => (RowsKind::Update, true),
                        _ => (RowsKind::Delete, true),
                    };
                    let Some(tm) = self.tm.clone() else {
                        return Err(BinlogError::InvalidData(format!(
                            "rows event (type {kt}) without preceding table_map"
                        )));
                    };
                    return Ok(Some(RawEvent {
                        binlog: self.name.clone(),
                        start_pos: self.tm_pos, // 上游 tbMapPos 口径
                        end_pos,
                        timestamp: h.timestamp,
                        kind: RawKind::Rows(rk, v2),
                        body,
                        tm: Some(tm),
                    }));
                }
                _ => RawKind::Other,
            };
            let ev = self.make(own_start, end_pos, h.timestamp, kind, body);
            return Ok(Some(ev));
        }
    }
}

/// 服务端版本 ≥ want（点分十进制前三段；无法解析按旧版处理 = false）。
/// 对照 go-mysql calcVersionProduct/event.go:179-190 的 (5,6,1) 门槛。
/// 与 `file_reader::server_version_ge` 同口径有意重复（冻结区不可共享）。
fn server_version_ge(server: &[u8], want: (u32, u32, u32)) -> bool {
    let s = String::from_utf8_lossy(server);
    let mut parts = [0u32; 3];
    let mut idx = 0usize;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '.' => {
                if cur.is_empty() || idx >= 3 {
                    return false;
                }
                parts[idx] = cur.parse().map_err(|_| 0u32).unwrap_or(0);
                idx += 1;
                cur.clear();
            }
            d if d.is_ascii_digit() => cur.push(d),
            // "-log"/"-debug" 等后缀：截断比较
            _ => break,
        }
    }
    if idx < 2 || (idx == 2 && cur.is_empty()) {
        return false; // 至少 major.minor(.patch)
    }
    if idx < 3 {
        parts[idx] = cur.parse().unwrap_or(0);
    }
    (parts[0], parts[1], parts[2]) >= want
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binlog::file_reader::FileReader;
    use crate::repl::transport::Frame;
    use std::io::Cursor;

    // ---------- 合成帧构造器（文件同构：19B 头 + 体 [+CRC 尾]） ----------

    /// 尾（CRC）形态：Off=无尾；On=常规 crc32(全帧)；Fde=FDE 特例
    /// （IN_USE 位清零覆盖，口径同 event.rs `fde_checksum_ok`）。
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Crc {
        Off,
        On,
        Fde,
    }

    /// 单事件全帧（`Event::write` 产物同构：header log_pos 直填、
    /// event_size=实际帧长）。合成帧传 log_pos=0/flags=0x20 系。
    fn build_frame(
        evtype: u8,
        ts: u32,
        log_pos: u32,
        flags: u16,
        body: &[u8],
        crc: Crc,
    ) -> Vec<u8> {
        let tail = crc != Crc::Off;
        let size = (EVENT_HEADER_SIZE + body.len() + if tail { 4 } else { 0 }) as u32;
        let mut b = Vec::new();
        b.extend_from_slice(&ts.to_le_bytes());
        b.push(evtype);
        b.extend_from_slice(&9u32.to_le_bytes()); // server_id
        b.extend_from_slice(&size.to_le_bytes());
        b.extend_from_slice(&log_pos.to_le_bytes());
        b.extend_from_slice(&flags.to_le_bytes());
        b.extend_from_slice(body);
        if tail {
            let mut h = crc32fast::Hasher::new();
            h.update(&b[..17]);
            if crc == Crc::Fde {
                h.update(&[b[17] & !0x01, b[18]]); // 只掩 BINLOG_IN_USE
            } else {
                h.update(&b[17..19]);
            }
            h.update(&b[EVENT_HEADER_SIZE..]);
            b.extend_from_slice(&h.finalize().to_le_bytes());
        }
        b
    }

    fn fr(bytes: Vec<u8>) -> Frame {
        Frame {
            bytes,
            binlog_hint: None,
        }
    }

    // ---------- 事件体（镜像 file_reader 测试 Synth，逐字段等价源头） ----------

    fn fde_body(server: &str, alg: Option<u8>) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&4u16.to_le_bytes()); // binlog version
        let mut sv = [0u8; 50];
        sv[..server.len()].copy_from_slice(server.as_bytes());
        b.extend_from_slice(&sv);
        b.extend_from_slice(&1600000000u32.to_le_bytes()); // create ts
        b.push(19); // event header length
        b.extend_from_slice(&[27u8; 39]); // event_type_header_lengths 占位
        if let Some(a) = alg {
            b.push(a); // checksum alg 字节
        }
        b
    }

    fn table_map_body(tid: u64, db: &str, tb: &str) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&tid.to_le_bytes()[..6]);
        b.extend_from_slice(&[0u8; 2]);
        b.push(db.len() as u8);
        b.extend_from_slice(db.as_bytes());
        b.push(0);
        b.push(tb.len() as u8);
        b.extend_from_slice(tb.as_bytes());
        b.push(0);
        b.push(1); // n_cols
        b.push(3); // LONG
        b.push(2); // metadata total len
        b.extend_from_slice(&0u16.to_le_bytes());
        b.push(0); // null bits
        b
    }

    fn write_rows_body(tid: u64) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&tid.to_le_bytes()[..6]);
        b.extend_from_slice(&[0u8; 2]);
        b.extend_from_slice(&2u16.to_le_bytes()); // extra info len (self)
        b.push(1); // n_cols
        b.push(1); // cols present bitmap
        b.push(0); // null bits
        b.extend_from_slice(&7i32.to_le_bytes());
        b
    }

    fn query_body(db: &str, sql: &str) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.push(db.len() as u8);
        b.extend_from_slice(&[0u8; 2]); // err code
        b.extend_from_slice(&0u16.to_le_bytes()); // status vars len
        b.extend_from_slice(db.as_bytes());
        b.push(0);
        b.extend_from_slice(sql.as_bytes());
        b
    }

    fn xid_body(id: u64) -> Vec<u8> {
        id.to_le_bytes().to_vec()
    }

    fn rotate_body(pos: u64, name: &str) -> Vec<u8> {
        let mut b = pos.to_le_bytes().to_vec();
        b.extend_from_slice(name.as_bytes());
        b
    }

    /// 顺序流构造器（file 视图与帧视图同源一键）：log_pos 真实累计。
    struct Parity {
        raws: Vec<Vec<u8>>,
        crc: bool,
        cursor: u32,
    }

    impl Parity {
        fn new(crc: bool) -> Self {
            Self {
                raws: Vec::new(),
                crc,
                cursor: 4,
            }
        }
        /// 真文件帧：header log_pos = 自身尾位点（连续推进）。
        fn push(&mut self, evtype: u8, ts: u32, body: &[u8]) -> (u32, u32) {
            let c = if !self.crc {
                Crc::Off
            } else if evtype == EventType::FORMAT_DESC {
                Crc::Fde
            } else {
                Crc::On
            };
            let size = (EVENT_HEADER_SIZE + body.len() + if self.crc { 4 } else { 0 }) as u32;
            let start = self.cursor;
            let end = start + size;
            self.raws
                .push(build_frame(evtype, ts, end, 0x0001, body, c));
            self.cursor = end;
            (start, end)
        }
        /// file 通道字节流（magic + 顺序拼接）——FileReader 直接可吃。
        fn file(&self) -> Vec<u8> {
            let mut v = b"\xfebin".to_vec();
            for r in &self.raws {
                v.extend_from_slice(r);
            }
            v
        }
        /// repl 通道帧序列（同一批字节，逐帧交付）。
        fn frames(&self) -> Vec<Frame> {
            self.raws.iter().cloned().map(fr).collect()
        }
    }

    fn collect_file(bytes: Vec<u8>, first: &str) -> Vec<RawEvent> {
        let mut r = FileReader::new(first.into(), Cursor::new(bytes), Filters::none()).unwrap();
        let mut v = Vec::new();
        while let Some(e) = r.next().unwrap() {
            v.push(e);
        }
        v
    }

    fn collect_repl(frames: Vec<Frame>, first: &str) -> Vec<RawEvent> {
        let mut s = crate::repl::test_support::repl_source_for_test(frames, first.into());
        let mut v = Vec::new();
        while let Some(e) = s.next().unwrap() {
            v.push(e);
        }
        v
    }

    // ---------- Step 1 主测：字节等价总闸（简报口径） ----------

    /// 同一合成事件集双通道对拉：(a) 走 FileReader（既有通道）、(b) 包
    /// 成 Vec<Frame> 走 ReplSource（注入 VecDeque 假流，transport::open
    /// 不碰）——binlog/start_pos/end_pos/timestamp/kind 逐字段一致 +
    /// body 字节一致（含 rows start=table_map 起始、rotate 先旧名后切名、
    /// CRC 验剥、5.7 恒带尾 FDE 全部口径）。
    #[test]
    fn repl_source_maps_synthetic_stream_byte_equal_to_file_reader() {
        for crc in [false, true] {
            let mut p = Parity::new(crc);
            p.push(
                EventType::FORMAT_DESC,
                1000,
                &fde_body("8.0.46", if crc { Some(1) } else { Some(0) }),
            );
            let (g_start, _) = p.push(EventType::ANONYMOUS_GTID_LOG, 1001, &[0u8; 42]);
            let (q_start, q_end) = p.push(EventType::QUERY, 1001, &query_body("t10", "BEGIN"));
            let (tm_start, _) = p.push(EventType::TABLE_MAP, 1001, &table_map_body(7, "t10", "u"));
            let (_, rows_end) = p.push(EventType::WRITE_ROWS_V2, 1002, &write_rows_body(7));
            let (x_start, x_end) = p.push(EventType::XID, 1003, &xid_body(42));
            // 真文件形态 rotate（log_pos 正常、体带 CRC 尾时照常验剥）：
            let (r_start, r_end) =
                p.push(EventType::ROTATE, 1004, &rotate_body(4, "mysql-bin.000002"));
            p.push(EventType::QUERY, 1005, &query_body("t10", "BEGIN"));
            p.push(EventType::TABLE_MAP, 1005, &table_map_body(8, "t10", "v"));
            p.push(EventType::WRITE_ROWS_V2, 1006, &write_rows_body(8));
            let (y_start, y_end) = p.push(EventType::XID, 1007, &xid_body(43));

            let a = collect_file(p.file(), "mysql-bin.000001");
            let b = collect_repl(p.frames(), "mysql-bin.000001");
            assert_eq!(a.len(), b.len(), "事件数（crc={crc}）");
            assert_eq!(a.len(), 8, "FDE/TABLE_MAP 由源消化不产出");
            for (x, y) in a.iter().zip(b.iter()) {
                assert_eq!(x.binlog, y.binlog, "binlog（crc={crc}）");
                assert_eq!(x.start_pos, y.start_pos, "start_pos（crc={crc}）");
                assert_eq!(x.end_pos, y.end_pos, "end_pos（crc={crc}）");
                assert_eq!(x.timestamp, y.timestamp, "timestamp（crc={crc}）");
                assert_eq!(
                    format!("{:?}", x.kind),
                    format!("{:?}", y.kind),
                    "kind（crc={crc}）"
                );
                assert_eq!(x.body, y.body, "body 字节（crc={crc}）");
            }
            // 绝对值抽查（钉死「双侧同错镜像」的对称盲区——口径引用见
            // 模块注释 file.go:197-215）：
            assert_eq!(b[0].kind, RawKind::Gtid);
            assert_eq!(b[0].start_pos, g_start);
            assert_eq!(b[1].kind, RawKind::Query("BEGIN".into()));
            assert_eq!((b[1].start_pos, b[1].end_pos), (q_start, q_end));
            assert_eq!(b[2].kind, RawKind::Rows(RowsKind::Write, true));
            assert_eq!(b[2].start_pos, tm_start, "rows start = table_map 起始");
            assert_eq!(b[2].end_pos, rows_end);
            assert_eq!(b[3].kind, RawKind::Xid);
            assert_eq!((b[3].start_pos, b[3].end_pos), (x_start, x_end));
            assert_eq!(
                b[4].kind,
                RawKind::Rotate("mysql-bin.000002".into()),
                "rotate 以旧名产出后切名（裁定 7 时序）"
            );
            assert_eq!(b[4].binlog, "mysql-bin.000001");
            assert_eq!((b[4].start_pos, b[4].end_pos), (r_start, r_end));
            assert_eq!(b[7].binlog, "mysql-bin.000002", "rotate 后事件记新名");
            assert_eq!((b[7].start_pos, b[7].end_pos), (y_start, y_end));
            assert!(b[2].tm.is_some() && b[6].tm.is_some(), "rows 携带 tm");
            assert_eq!(b[2].tm.as_ref().map(|t| t.table_id), Some(7));
            assert_eq!(b[6].tm.as_ref().map(|t| t.table_id), Some(8));
        }
    }

    // ---------- Step 1 主测之二：rotate/合成帧/checksum 特例族 ----------

    /// 位点链只认请求起点与真帧 log_pos：fake rotate 更名+播种、mid-file
    /// 合成 FDE（log_pos=0）定 CRC、EOF 切换帧产 Rotate 但头位点不进链、
    /// 5.7 恒带尾 FDE 红绿两态、hint 兜底更名。
    #[test]
    fn repl_source_tracks_rotate_and_checksum() {
        // -- 流首 fake rotate（log_pos=0/ts=0/ARTIFICIAL 0x20/无尾）+
        //    mid-file 合成 FDE（log_pos=0）+ 真事件：起点 1000 的接续 --
        let fake = build_frame(
            EventType::ROTATE,
            0,
            0,
            0x0020,
            &rotate_body(1000, "mysql-bin.000005"),
            Crc::Off,
        );
        let fde_syn = build_frame(
            EventType::FORMAT_DESC,
            1000,
            0,
            0x0020,
            &fde_body("8.0.46", Some(0)),
            Crc::Off, // 合成帧无尾形态（FDE 声明 NONE）
        );
        let xid = build_frame(EventType::XID, 1002, 1027, 0x0001, &xid_body(42), Crc::Off);
        let mut s = crate::repl::test_support::repl_source_for_test(
            vec![fr(fake), fr(fde_syn.clone()), fr(xid.clone())],
            "mysql-bin.000005".into(),
        );
        let ev = s.next().unwrap().unwrap();
        assert_eq!(ev.kind, RawKind::Xid, "fake rotate/FDE 均内部消化不产出");
        assert_eq!(
            (ev.start_pos, ev.end_pos),
            (1000, 1027),
            "真帧位点取自自身头"
        );
        assert_eq!(ev.binlog, "mysql-bin.000005");
        assert!(s.next().unwrap().is_none());
        assert_eq!(s.chain_pos(), 1027, "链只被真帧推进");

        // -- fake rotate body 名残缺 → hint 兜底更名 --
        let fake_no_name = Frame {
            bytes: build_frame(EventType::ROTATE, 0, 0, 0x0020, &[9u8; 8], Crc::Off),
            binlog_hint: Some("mysql-bin.000009".into()),
        };
        let mut s = crate::repl::test_support::repl_source_for_test(
            vec![fake_no_name, fr(fde_syn.clone()), fr(xid.clone())],
            "mysql-bin.000005".into(),
        );
        let ev = s.next().unwrap().unwrap();
        assert_eq!(ev.binlog, "mysql-bin.000009", "hint 兜底更名");

        // -- EOF 切换 ROTATE（seq>0 合成帧：ts=0、log_pos=0、payload=4、
        //    **带 CRC 尾**——spike 实测 size=47 帧）：照常产出 Rotate，
        //    位点标注取链尾（500），绝不记 0；更名 + 链复位 4 --
        let mut p = Parity::new(true); // crc 流（EOF 合成帧带尾口径）
        p.push(EventType::FORMAT_DESC, 1000, &fde_body("8.0.46", Some(1)));
        p.push(EventType::XID, 2000, &xid_body(1)); // 链推进到 p.cursor
        let chain_end = p.cursor;
        p.raws.push(build_frame(
            EventType::ROTATE,
            0,
            0,
            0x0021, // ARTIFICIAL | BINLOG_IN_USE
            &rotate_body(4, "mysql-bin.000003"),
            Crc::On,
        ));
        let mut s =
            crate::repl::test_support::repl_source_for_test(p.frames(), "mysql-bin.000001".into());
        let x = s.next().unwrap().unwrap();
        assert_eq!(x.kind, RawKind::Xid);
        assert_eq!(x.end_pos, chain_end);
        let r = s.next().unwrap().unwrap();
        assert_eq!(r.kind, RawKind::Rotate("mysql-bin.000003".into()));
        assert_eq!(r.binlog, "mysql-bin.000001", "rotate 记旧名");
        assert_eq!(
            (r.start_pos, r.end_pos),
            (chain_end, chain_end),
            "合成帧头 log_pos=0 不进链——位点标注取链尾"
        );
        assert_eq!(
            r.body.len(),
            8 + "mysql-bin.000003".len(),
            "EOF 合成帧尾经 crc32_ok 通过性探测剥离（不判损坏、不残留 4B）"
        );
        assert_eq!(s.chain_pos(), 4, "EOF 切换后链复位 payload=4");
        assert!(s.next().unwrap().is_none());

        // -- 5.7 恒带 CRC 的 FDE 特例（`fde_checksum_ok` 红绿两态） --
        // 绿：FDE 带合法 CRC 尾但 alg(len-5)=0（5.7 binlog_checksum=NONE
        //     真机形态，T17 勘误口径）→ NONE 流整流可解，不误报损坏。
        let mut g = Parity::new(false);
        let (gs, _) = g.push(EventType::FORMAT_DESC, 1000, &fde_body("5.7.44", Some(0)));
        // 手工升级为「无声明但有尾」：重推 FDE（Crc::Fde 尾、alg=0 在
        // len-5 位）——即 5.7 NONE 文件真机字节形态。
        let mut raws = Vec::new();
        raws.push(build_frame(
            EventType::FORMAT_DESC,
            1000,
            gs + (EVENT_HEADER_SIZE + 97 + 4) as u32,
            0x0001,
            &fde_body("5.7.44", Some(0)),
            Crc::Fde,
        ));
        let _ = gs;
        raws.push(build_frame(
            EventType::XID,
            1002,
            9999,
            0x0001,
            &xid_body(42),
            Crc::Off,
        ));
        let mut s = crate::repl::test_support::repl_source_for_test(
            raws.into_iter().map(fr).collect(),
            "mysql-bin.000003".into(),
        );
        let ev = s.next().unwrap().unwrap();
        assert_eq!(ev.kind, RawKind::Xid, "5.7 NONE-with-tail FDE 不得拒流");
        assert!(!s.with_crc, "alg 在 len-5 且 =0 → NONE 态");
        let gevs = collect_file(g.file(), "x"); // 对照：无尾 FDE 在 file 通道亦可解
        assert!(gevs.is_empty() || gevs.len() <= 1);

        // 红：无尾 FDE 且体末 alg 字节声称 CRC32 → ChecksumMismatch 硬拒
        //（与 file_reader `no_tail_fde_claiming_crc32…` 同口径钉死）。
        let bad = build_frame(
            EventType::FORMAT_DESC,
            1000,
            120,
            0x0001,
            &fde_body("5.6.51", Some(1)),
            Crc::Off,
        );
        let mut s = crate::repl::test_support::repl_source_for_test(vec![fr(bad)], "x".into());
        assert_eq!(s.next().unwrap_err(), BinlogError::ChecksumMismatch);
    }

    /// 心跳内部消化：不产出、不进 stop 判定、不推链（其 header
    /// log_pos=主库活写位点）；名字段不改名（更名只认 ROTATE）。
    #[test]
    fn heartbeat_is_consumed_without_side_effects() {
        let mut p = Parity::new(true);
        p.push(EventType::FORMAT_DESC, 1000, &fde_body("8.0.46", Some(1)));
        p.raws.push(build_frame(
            EventType::HEARTBEAT,
            0,
            777_777, // 主库活写位点（不进链）
            0x0020,
            b"mysql-bin.000002", // 心跳体带日志文件名，但更名归 ROTATE
            Crc::On,
        ));
        let (xs, xe) = {
            p.cursor = 777_777; // 模拟心跳后真事件（活位点附近）
            p.push(EventType::XID, 2000, &xid_body(7))
        };
        let mut s =
            crate::repl::test_support::repl_source_for_test(p.frames(), "mysql-bin.000001".into());
        let ev = s.next().unwrap().unwrap();
        assert_eq!(ev.kind, RawKind::Xid, "心跳不产出、真事件照常");
        assert_eq!((ev.start_pos, ev.end_pos), (xs, xe));
        assert_eq!(
            ev.binlog, "mysql-bin.000001",
            "心跳不改名（改名只认 ROTATE）"
        );
        assert_eq!(s.chain_pos(), xe, "心跳 log_pos 未进链");
        assert!(s.next().unwrap().is_none());
    }

    /// 断链 ≠ 干净停止：上游 Err(Disconnect) → next() Err 且变体保真
    /// （T5 重连分类依赖）；Ok(None) 只留给消费方语义。
    #[test]
    fn disconnect_surfaces_as_error_variant_not_clean_stop() {
        let mut p = Parity::new(false);
        p.push(EventType::FORMAT_DESC, 1000, &fde_body("8.0.46", Some(0)));
        // 无 tail 的测试流：帧耗尽 = 消费方干净停止语义（Ok(None)）
        let no_tail = crate::repl::test_support::FakeStream::new(p.frames());
        let mut s = ReplSource::new(
            Box::new(no_tail),
            "mysql-bin.000001".into(),
            Filters::none(),
        );
        assert!(s.next().unwrap().is_none()); // FDE 消化后流耗尽 → 干净 None
        assert!(s.transport_error().is_none(), "干净停止不记传输错误");
        // 换新流：帧耗尽后 tail=Err(Disconnect)
        let fake = crate::repl::test_support::FakeStream::with_tail(
            vec![],
            ReplError::Disconnect("stream ended without stop condition".into()),
        );
        let mut s = ReplSource::new(Box::new(fake), "mysql-bin.000001".into(), Filters::none());
        let e = s.next().unwrap_err();
        assert!(
            matches!(&e, BinlogError::InvalidData(m) if m.contains("disconnected")),
            "断链必须是 Err 而非 Ok(None)：{e:?}"
        );
        assert!(matches!(
            s.transport_error(),
            Some(ReplError::Disconnect(_))
        ));
        // 终止面变体经同一通道保真（分类权在 T5）：
        let fake = crate::repl::test_support::FakeStream::with_tail(
            vec![],
            ReplError::Purged("1236 log has been purged".into()),
        );
        let mut s = ReplSource::new(Box::new(fake), "m".into(), Filters::none());
        assert!(s.next().is_err());
        assert!(matches!(s.transport_error(), Some(ReplError::Purged(_))));
    }

    /// FDE 前的非合成帧 = 硬错误（go-mysql parser.go:329-332 同款闸）。
    #[test]
    fn events_before_fde_are_hard_errors_except_fake_rotate() {
        let bare = build_frame(EventType::XID, 1, 100, 0x0001, &xid_body(1), Crc::Off);
        let mut s = crate::repl::test_support::repl_source_for_test(vec![fr(bare)], "f".into());
        assert!(matches!(s.next().unwrap_err(), BinlogError::InvalidData(_)));
    }
}
