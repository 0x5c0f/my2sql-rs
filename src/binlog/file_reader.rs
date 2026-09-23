//! 文件事件源：magic 校验 → FDE（checksum 有无）→ 逐事件头/体读取 → RawEvent
//! （Task 12）。泛型 `R: Read + Seek`：生产用 `File`，单测喂 `Cursor<Vec<u8>>`
//! （简报 Step 1 口径，无临时文件）。
//!
//! ## 上游口径对照（my2sql-go base/file.go/com.go，权威引用）
//!
//! - 固定从偏移 4 顺序读，**绝不 seek 到 start_pos**（file.go:118-122 原注释
//!   "must not seek to other position, otherwise the program may panic because
//!   formatevent, table map event is skipped"）——start 窗口只做「读了但不产出」
//!   的跳过（FDE/TABLE_MAP 恒处理，见 next() 注释）。
//! - stop-file/stop-pos/stop-datetime 判定在 **header 之后、body 之前**
//!   （upstream 实际在 file.go:203 于 ParseEvent 之后才查
//!   `CheckBinHeaderCondition`；简报 Step 2 的「可跳过读」是有意的更省 IO
//!   收紧，停止语义（end_pos 口径 + `>=` 等号排除）与上游一致）。
//! - **跨文件真相（裁定 7）**：上游 file 模式默认**单文件**——EOF 后仅当设置了
//!   stop-file/stop-datetime 才续下一个文件（file.go:74-85 `!IfSetStopParsPoint
//!   && !IfSetStopDateTime → break`），且文件名推进用 **+06d 十进制序号**
//!   （funcs.go:98-103 `GetNextBinlog`），**不用 rotate url 切文件**；rotate url
//!   只在 com.go:41-46 更新「当前文件名」用于位点比较标签。本层镜像：FileReader
//!   恒单文件（EOF → None），rotate 只改名不切文件；多文件迭代归 T14 装配层，
//!   [`FileReader::next_binlog_name`] 提供同款推导（999999 → 1000000，%06d
//!   是最小宽度格式，无截断/十六进制回绕）。
//! - V0 行事件（20/21/22，4B table_id）与 39（PARTIAL_UPDATE）在本路由层
//!   硬错误（rows.rs 模块注释的 T12 路由义务；上游 go-mysql 支持 V0、拒绝
//!   39——P1 目标 5.6+，V0 不出现在支持矩阵，报错优于误解码）。
//! - 与上游的另一处有意差异：上游 my2sql-go **从不校验** checksum（仅按 FDE
//!   声明剥 4B），本层对每个事件验 crc32（敌意输入防线，FDE 特例见 event.rs）。

use std::io::{Read, Seek};
use std::path::Path;
use std::sync::Arc;

use crate::binlog::error::BinlogError;
use crate::binlog::event::{
    EVENT_HEADER_SIZE, EventHeader, EventType, crc32_ok, fde_checksum_ok, parse_header,
    strip_checksum,
};
use crate::binlog::rows::RowsKind;
use crate::binlog::table_map::{self, TableMapEvent};
use crate::pipeline::filter::Filters;
use crate::pipeline::source::{EventSource, RawEvent, RawKind};

/// binlog 文件魔数 `fe 'bin'`（go-mysql replication.BinLogFileHeader 同值）。
pub const BINLOG_MAGIC: [u8; 4] = [0xfe, b'b', b'i', b'n'];

/// 单事件字节上限（T14 Step-0 账载：header 谎报巨形 `event_size` 时
/// `vec![0u8; body_len]` 预分配即 OOM）。MySQL 官方事件无此量级
/// （max_allowed_packet 域 ≤1GB 且行事件分片）；取 2GiB（`1<<31`）为
/// 「任何合法 binlog 事件都远小于此」的宽松天花板，超出 = 损坏/敌意 →
/// InvalidData 硬错误，先于任何分配。
pub const MAX_EVENT_SIZE: u32 = 1 << 31;

/// 文件事件源（单文件；rotate 只改名不切文件）。
pub struct FileReader<R: Read + Seek> {
    name: String,
    rdr: R,
    filters: Filters,
    /// FDE 声明 CRC32（binlog v4 且 alg=1）；未知（FDE 前）→ 事件不可解。
    with_crc: bool,
    seen_fde: bool,
    /// 最近一个 TABLE_MAP（rows 事件携带，Arc 共享——简报绑定）。
    tm: Option<Arc<TableMapEvent>>,
    /// 最近一个 TABLE_MAP 的起始位（rows 事件 start_pos，上游 tbMapPos）。
    tm_pos: u32,
    done: bool,
}

impl FileReader<std::fs::File> {
    /// 打开 `dir/name` 生产文件源（IO 错误映射 [`BinlogError::InvalidData`]）。
    pub fn open(dir: &Path, name: &str, filters: Filters) -> Result<Self, BinlogError> {
        let path = dir.join(name);
        let f = std::fs::File::open(&path)
            .map_err(|e| BinlogError::InvalidData(format!("open {}: {e}", path.display())))?;
        Self::new(name.to_string(), f, filters)
    }
}

impl<R: Read + Seek> FileReader<R> {
    /// magic 校验 + 定位 4（上游语义：位置过滤靠比较不靠 seek，见模块注释）。
    pub fn new(name: String, mut rdr: R, filters: Filters) -> Result<Self, BinlogError> {
        let mut magic = [0u8; 4];
        let n = read_up_to(&mut rdr, &mut magic)?;
        if n < 4 || magic != BINLOG_MAGIC {
            return Err(BinlogError::InvalidData(
                "not a valid binlog file (head 4 bytes must be fe'bin')".into(),
            ));
        }
        // magic 已读满 4B，流自然停在 4——上游语义固定从偏移 4 顺序读，绝不 seek
        // 到位点参数（file.go:118-122）；`Seek` 约束为 T14 多文件/复位留口。
        Ok(Self {
            name,
            rdr,
            filters,
            with_crc: false,
            seen_fde: false,
            tm: None,
            tm_pos: 0,
            done: false,
        })
    }

    /// 上游 GetNextBinlog 同款（funcs.go:98-103）：末段十进制序号 +1、最小宽 6
    /// （999999 → 1000000，%06d 无截断）。非 `<base>.<decimal>` 形态 → None。
    pub fn next_binlog_name(name: &str) -> Option<String> {
        let (base, idx) = name.rsplit_once('.')?;
        let n: u32 = idx.parse().ok()?;
        Some(format!("{base}.{:06}", n + 1))
    }

    /// 读满 buf 或遇 EOF；返回实际读取数（0 = 干净结束）。
    fn fill(&mut self, buf: &mut [u8]) -> Result<usize, BinlogError> {
        read_up_to(&mut self.rdr, buf)
    }

    /// FDE：binlog 版本/服务端版本甄别 + checksum 有无判定（FDE crc 特例）。
    ///
    /// 判定次序：binlog version 必须 v4；server 含 "MariaDB" → P1 拒
    /// （上游 mariadb 支持走独立事件码语义，D5 不猜测）；随后镜像
    /// go-mysql event.go:179-190 的版本门槛（≥5.6.1 才可能有 checksum），
    /// 用 [`fde_checksum_ok`]（flags 置零特例，T2 账载）实证探测 CRC 有无：
    /// FDE 校验通过 → alg 字节取 len-5（5.7+ 真机：FDE 恒带 CRC 尾，NONE 态亦
    /// 如此，T17 勘误）；校验不过 → FDE 无尾，body 末字节按 alg 解读，声称
    /// CRC32 即判损坏。
    /// 注：go-mysql 对 FDE 本身**不验证** checksum（parser.go:238-247 FDE
    /// 分支绕开 verify），我们取验证立场并以真机 fixture 钉死口径。
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
                // FDE 带合法 CRC 尾 → alg 字节在 len-5（go-mysql event.go:186 同位）。
                // T17 真机勘误：5.7 起 FDE **恒带 CRC 尾**，即便 binlog_checksum=NONE
                // （mysqld 对 FDE 总是 `checksum_event`——声明位随体，校验尾不缺席）；
                // 旧判定错把 body[-1]（=CRC 尾字节）当 alg，NONE 文件会被误判损坏。
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

    /// QUERY 事件体 → (schema, SQL 文本)。布局对照 vendored go-mysql
    /// event.go:304-333（binlog v4：proxy 4B + exec_time 4B + schema_len 1B +
    /// err 2B + status_vars_len 2B + status vars + schema + 0x00 + query）。
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
}

/// 读满 buf 或 EOF（0 字节起读 = 干净结束）；Interrupted 重试，其余 IO 错
/// 映射 InvalidData（BinlogError 无 Io 变体，T2 契约不改——映射理由记录报告）。
fn read_up_to<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<usize, BinlogError> {
    let mut total = 0usize;
    while total < buf.len() {
        match r.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(io_err(e)),
        }
    }
    Ok(total)
}

fn io_err(e: std::io::Error) -> BinlogError {
    BinlogError::InvalidData(format!("io: {e}"))
}

/// 服务端版本 ≥ want（点分十进制前三段；无法解析按旧版处理 = false）。
/// 对照 go-mysql calcVersionProduct/event.go:179-190 的 (5,6,1) 门槛。
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

impl<R: Read + Seek> EventSource for FileReader<R> {
    fn next(&mut self) -> Result<Option<RawEvent>, BinlogError> {
        loop {
            if self.done {
                return Ok(None);
            }
            // ---- header ----
            let mut hb = [0u8; EVENT_HEADER_SIZE];
            let n = self.fill(&mut hb)?;
            if n == 0 {
                self.done = true; // 干净 EOF（与 IO 错误区分，裁定 5）
                return Ok(None);
            }
            if n < EVENT_HEADER_SIZE {
                return Err(BinlogError::UnexpectedEof); // 半截头 = 截断文件
            }
            let h = parse_header(&hb)?;
            // 事件体预分配前的尺寸闸门（T14 Step-0：谎报巨形 header 不得先 alloc）
            if h.event_size > MAX_EVENT_SIZE {
                return Err(BinlogError::InvalidData(format!(
                    "event_size {} exceeds cap {MAX_EVENT_SIZE} (corrupt or hostile header)",
                    h.event_size
                )));
            }
            let own_start = h.log_pos.saturating_sub(h.event_size);
            // ---- stop 判定：header 之后、body 之前（简报 Step 2 / 裁定 3）----
            if self.filters.pos_stopped(&self.name, h.log_pos, h.timestamp) {
                self.done = true;
                return Ok(None);
            }
            // ---- body ----
            let body_len = h.event_size as usize - EVENT_HEADER_SIZE;
            let mut body = vec![0u8; body_len];
            let n = self.fill(&mut body)?;
            if n < body_len {
                return Err(BinlogError::UnexpectedEof);
            }
            let t = h.event_type.0;
            if t == EventType::FORMAT_DESC {
                let mut full = hb.to_vec();
                full.extend_from_slice(&body);
                self.handle_fde(&full)?;
                continue; // FDE 由源消化（checksum 口径），不产出
            }
            if !self.seen_fde {
                // go-mysql parser.go:329-332：非 FDE 事件必须先有 FDE
                return Err(BinlogError::InvalidData(
                    "event before any format_description event".into(),
                ));
            }
            // checksum：人工 rotate（relay 产物：log_pos=0、无 CRC 尾）豁免，
            // binlog 文件真 rotate（文件尾、带 CRC）走常规验证。
            let artificial_rotate = t == EventType::ROTATE && h.log_pos == 0;
            if self.with_crc && !artificial_rotate {
                let mut full = hb.to_vec();
                full.extend_from_slice(&body);
                if !crc32_ok(&full) {
                    return Err(BinlogError::ChecksumMismatch);
                }
                strip_checksum(&mut body, true);
            }
            let pending = self.filters.pos_pending(&self.name, h.log_pos, h.timestamp);
            // 结构性事件恒处理（窗口外也吃——上游 file.go:118 不 seek + 197
            // tbMapPos 先于 header 条件检查的同款事实）：
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
                    if body.len() < 8 {
                        return Err(BinlogError::TooShort);
                    }
                    let url = String::from_utf8_lossy(&body[8..]).into_owned();
                    // 先以「当前名」产出（上游 file.go:214 早于 com.go:43 改名），
                    // 再更新比较用文件名（裁定 7：只改名、不切文件）。
                    let ev = self.make(own_start, h, RawKind::Rotate(url.clone()), body);
                    self.name = url;
                    return Ok(Some(ev));
                }
                EventType::PREVIOUS_GTIDS => continue, // 结构性消化
                // rows 路由（rows.rs 模块注释的 T12 义务：V0/39 硬错误）：
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
                    let ev = RawEvent {
                        binlog: self.name.clone(),
                        start_pos: self.tm_pos, // 上游 tbMapPos 口径
                        end_pos: h.log_pos,
                        timestamp: h.timestamp,
                        kind: RawKind::Rows(rk, v2),
                        body,
                        tm: Some(tm),
                    };
                    return Ok(Some(ev));
                }
                _ => RawKind::Other,
            };
            let ev = self.make(own_start, h, kind, body);
            return Ok(Some(ev));
        }
    }
}

impl<R: Read + Seek> FileReader<R> {
    fn make(&self, start: u32, h: EventHeader, kind: RawKind, body: Vec<u8>) -> RawEvent {
        RawEvent {
            binlog: self.name.clone(),
            start_pos: start,
            end_pos: h.log_pos,
            timestamp: h.timestamp,
            kind,
            body,
            tm: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // ---------- 合成 binlog 构造器（log_pos = 真实累计字节位） ----------

    struct Synth {
        bytes: Vec<u8>,
        crc: bool,
    }
    impl Synth {
        /// 空流（仅 magic，8.0 crc 关）。
        fn start() -> Self {
            Synth {
                bytes: b"\xfebin".to_vec(),
                crc: false,
            }
        }
        /// 带 FDE v4 的流：crc=true 时 FDE alg=1 且各事件带 CRC32。
        fn with_fde(crc: bool) -> Self {
            let mut s = Synth {
                bytes: b"\xfebin".to_vec(),
                crc,
            };
            s.push_fde(4, "8.0.46", if crc { 1 } else { 0 });
            s
        }
        /// 手工 FDE（版本/服务端口径负用例）。
        fn push_fde(&mut self, ver: u16, server: &str, alg: u8) {
            let mut b = Vec::new();
            b.extend_from_slice(&ver.to_le_bytes()); // binlog version
            let mut sv = [0u8; 50];
            sv[..server.len()].copy_from_slice(server.as_bytes());
            b.extend_from_slice(&sv);
            b.extend_from_slice(&1600000000u32.to_le_bytes());
            b.push(19); // event header length
            b.extend_from_slice(&[27u8; 39]); // event_type_header_lengths 占位
            if alg != 0xFF {
                b.push(alg); // checksum alg byte（5.6.1+ 才有；0xFF=无此字节模拟）
            }
            self.push(15, 1000, &b);
        }
        /// 追加事件，返回 (start_pos, end_pos)。
        fn push(&mut self, evtype: u8, ts: u32, body: &[u8]) -> (u32, u32) {
            let size = 19 + body.len() as u32 + if self.crc { 4 } else { 0 };
            let start = self.bytes.len() as u32;
            let end = start + size;
            let mut b = Vec::new();
            b.extend_from_slice(&ts.to_le_bytes());
            b.push(evtype);
            b.extend_from_slice(&9u32.to_le_bytes());
            b.extend_from_slice(&size.to_le_bytes());
            b.extend_from_slice(&end.to_le_bytes());
            b.extend_from_slice(&0x01u16.to_le_bytes()); // BINLOG_IN_USE
            b.extend_from_slice(body);
            if self.crc {
                let mut h = crc32fast::Hasher::new();
                if evtype == 15 {
                    // FDE 特例：flags 置零后覆盖 [0, size-4)
                    h.update(&b[..17]);
                    h.update(&[0u8; 2]);
                    h.update(&b[19..]);
                } else {
                    h.update(&b);
                }
                b.extend_from_slice(&h.finalize().to_le_bytes());
            }
            assert_eq!(b.len(), size as usize);
            self.bytes.extend_from_slice(&b);
            (start, end)
        }
        fn table_map(&mut self, tid: u64, db: &str, tb: &str, ts: u32) -> (u32, u32) {
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
            self.push(19, ts, &b)
        }
        fn write_rows(&mut self, tid: u64, ts: u32) -> (u32, u32) {
            let mut b = Vec::new();
            b.extend_from_slice(&tid.to_le_bytes()[..6]);
            b.extend_from_slice(&[0u8; 2]);
            b.extend_from_slice(&2u16.to_le_bytes()); // extra info len (self)
            b.push(1); // n_cols
            b.push(1); // cols present bitmap
            b.push(0); // null bits
            b.extend_from_slice(&7i32.to_le_bytes());
            self.push(30, ts, &b)
        }
        fn query(&mut self, db: &str, sql: &str, ts: u32) -> (u32, u32) {
            let mut b = Vec::new();
            b.extend_from_slice(&0u32.to_le_bytes());
            b.extend_from_slice(&0u32.to_le_bytes());
            b.push(db.len() as u8);
            b.extend_from_slice(&[0u8; 2]); // err code
            b.extend_from_slice(&0u16.to_le_bytes()); // status vars len
            b.extend_from_slice(db.as_bytes());
            b.push(0);
            b.extend_from_slice(sql.as_bytes());
            self.push(2, ts, &b)
        }
        fn xid(&mut self, ts: u32) -> (u32, u32) {
            self.push(16, ts, &42u64.to_le_bytes())
        }
        fn anon_gtid(&mut self, ts: u32) -> (u32, u32) {
            self.push(34, ts, &[0u8; 42])
        }
        fn rotate(&mut self, next: &[u8], ts: u32) -> (u32, u32) {
            let mut b = vec![0u8; 8]; // next log pos u64
            b.extend_from_slice(next);
            self.push(4, ts, &b)
        }
    }

    fn reader(s: &Synth, f: Filters) -> FileReader<Cursor<Vec<u8>>> {
        FileReader::new("mysql-bin.000001".into(), Cursor::new(s.bytes.clone()), f).unwrap()
    }

    fn collect(r: &mut dyn EventSource) -> Vec<RawEvent> {
        let mut v = Vec::new();
        while let Some(e) = r.next().unwrap() {
            v.push(e);
        }
        v
    }

    // ---------- Step 1（简报）：三事件流 → RawEvent，start_pos = table_map 起始 ----------

    #[test]
    fn emits_raw_events_with_tablemap_start_pos() {
        let mut s = Synth::with_fde(false);
        let (tm_start, _tm_end) = s.table_map(7, "t10", "u", 1001);
        let (_, rows_end) = s.write_rows(7, 1002);
        let (xid_start, xid_end) = s.xid(1003);
        let mut r = reader(&s, Filters::none());
        let evs = collect(&mut r);
        assert_eq!(evs.len(), 2, "FDE/TABLE_MAP 由源消费，不产出");
        assert_eq!(evs[0].kind, RawKind::Rows(RowsKind::Write, true));
        assert_eq!(
            evs[0].start_pos, tm_start,
            "rows start = table_map 起始（上游 file.go:198）"
        );
        assert_eq!(evs[0].end_pos, rows_end);
        assert_eq!(evs[0].binlog, "mysql-bin.000001");
        assert_eq!(evs[0].timestamp, 1002);
        // body = 剥头剥 CRC 的事件体，可直接喂 decode_rows（接缝）
        use crate::binlog::rows::decode_rows;
        use crate::metadata::schema::{SchemaCol, TableSchema};
        let tm = evs[0].tm.clone().unwrap();
        assert_eq!(tm.table_id, 7);
        let sc = TableSchema {
            db: "t10".into(),
            table: "u".into(),
            cols: (0..1)
                .map(|i| SchemaCol {
                    name: format!("c{i}"),
                    type_name: "int".into(),
                    unsigned: false,
                })
                .collect(),
            pk: vec![],
            uks: vec![],
        };
        let rows = decode_rows(&evs[0].body, &tm, &sc, RowsKind::Write, true).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            evs[1].kind,
            RawKind::Xid,
            "非 rows 事件 start_pos = 自身起始（上游 stats 口径 file.go:276）"
        );
        assert_eq!(evs[1].start_pos, xid_start);
        assert_eq!(evs[1].end_pos, xid_end);
        assert_eq!(evs[1].timestamp, 1003);
    }

    #[test]
    fn query_gtid_rotate_events_carry_text_marker_and_url() {
        let mut s = Synth::with_fde(false);
        let (g_start, _) = s.anon_gtid(1001);
        let (q_start, _) = s.query("t10", "BEGIN", 1002);
        let _ = (g_start, q_start);
        let (r_start, r_end) = s.rotate(b"mysql-bin.000002", 1003);
        let (x_start, _) = s.xid(1004);
        let mut r = reader(&s, Filters::none());
        let evs = collect(&mut r);
        assert_eq!(
            evs.iter().map(|e| e.kind.clone()).collect::<Vec<_>>(),
            vec![
                RawKind::Gtid,
                RawKind::Query("BEGIN".into()),
                RawKind::Rotate("mysql-bin.000002".into()),
                RawKind::Xid
            ]
        );
        // rotate 后「当前文件名」更新（上游 com.go:41-46 仅改名不切文件）：
        // rotate 事件本身记旧名，其后事件记新名（上游 file.go:214→217 时序）。
        assert_eq!(evs[2].binlog, "mysql-bin.000001");
        assert_eq!(evs[2].start_pos, r_start);
        assert_eq!(evs[2].end_pos, r_end);
        assert_eq!(evs[3].binlog, "mysql-bin.000002");
        assert_eq!(evs[3].start_pos, x_start);
    }

    // ---------- magic / 截断 / 无 FDE ----------

    #[test]
    fn rejects_bad_magic() {
        let r = FileReader::new(
            "f".into(),
            Cursor::new(b"XXXX....".to_vec()),
            Filters::none(),
        );
        assert!(matches!(r, Err(BinlogError::InvalidData(_))));
    }

    #[test]
    fn truncated_event_body_errors_unexpected_eof() {
        let mut s = Synth::with_fde(false);
        s.table_map(7, "t10", "u", 1001);
        s.write_rows(7, 1002);
        s.xid(1003);
        s.bytes.truncate(s.bytes.len() - 5); // 砍掉最后一个事件的尾部
        let mut r = reader(&s, Filters::none());
        assert!(r.next().unwrap().is_some()); // rows 先出
        assert_eq!(r.next().unwrap_err(), BinlogError::UnexpectedEof);
    }

    #[test]
    fn empty_file_after_magic_is_clean_end() {
        let s = Synth::start();
        let mut r = reader(&s, Filters::none());
        assert!(r.next().unwrap().is_none());
    }

    // ---------- crc：校验+剥离、篡改检测、FDE 特例、无 FDE 先行报错 ----------

    #[test]
    fn crc_stream_verifies_strips_and_fde_special_case_passes() {
        // FDE 事件体（alg+crc 后）在剥 4B 后按未剥态解 TABLE_MAP 会多出 1B；
        // 源内部先剥再解——rows 事件 body 应为纯体（长度自洽）。
        let mut s = Synth::with_fde(true);
        let (tm_start, _) = s.table_map(7, "t10", "u", 1001);
        let (_, rows_end) = s.write_rows(7, 1002);
        let mut r = reader(&s, Filters::none());
        let evs = collect(&mut r);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].start_pos, tm_start);
        assert_eq!(evs[0].end_pos, rows_end);
        // body 剥 CRC：v2 头 8B + extra 2B + 1+1+1+4 = 17B
        assert_eq!(evs[0].body.len(), 17);
        use crate::binlog::rows::decode_rows;
        use crate::metadata::schema::{SchemaCol, TableSchema};
        let tm = evs[0].tm.clone().unwrap();
        let sc = TableSchema {
            db: "t10".into(),
            table: "u".into(),
            cols: vec![SchemaCol {
                name: "c0".into(),
                type_name: "int".into(),
                unsigned: false,
            }],
            pk: vec![],
            uks: vec![],
        };
        assert!(decode_rows(&evs[0].body, &tm, &sc, RowsKind::Write, true).is_ok());
    }

    #[test]
    fn tampered_body_fails_checksum_before_emit() {
        let mut s = Synth::with_fde(true);
        s.table_map(7, "t10", "u", 1001);
        let (rs, _) = s.write_rows(7, 1002);
        let _ = rs;
        // 翻最后一个 rows 事件 body 中间 1 字节（crc 之前）
        let n = s.bytes.len();
        s.bytes[n - 10] ^= 0x80;
        let mut r = reader(&s, Filters::none());
        assert_eq!(r.next().unwrap_err(), BinlogError::ChecksumMismatch);
    }

    // ---------- T17 修复轮 1：无尾 FDE 兜底分支钉死（handle_fde else 臂） ----------

    #[test]
    fn no_tail_fde_with_none_alg_decodes_as_checksumless_stream() {
        // 钉死语义（log_event.cc FDE 行为的本仓库立场，b8f401c 判定次序）：
        // 5.6.1~5.6.x 部分构建的 FDE **不带 CRC 尾**（checksum_event 未随体）
        // → `fde_checksum_ok` 不过 → 回退按「无尾形态」解读：body 末字节即
        // checksum alg 字节。alg=0（NONE）→ 流按无校验解码（with_crc=false），
        // 后续事件照常产出——不得因探针失败误报 ChecksumMismatch。
        // （合成件 = 97B FDE 体 [ver4|server|ts|hdr_len 19|ethl×39|alg=0]，无尾。）
        let mut s = Synth::start();
        s.crc = false;
        s.push_fde(4, "5.6.51", 0);
        let (tm_start, _) = s.table_map(7, "t10", "u", 1001);
        s.write_rows(7, 1002);
        let mut r = reader(&s, Filters::none());
        let evs = collect(&mut r);
        assert_eq!(evs.len(), 1, "无尾 NONE FDE 必须整流可解");
        assert_eq!(evs[0].kind, RawKind::Rows(RowsKind::Write, true));
        assert_eq!(evs[0].start_pos, tm_start);
        assert!(!r.with_crc, "alg 字节（无尾体末）=0 → NONE 态");
    }

    #[test]
    fn no_tail_fde_claiming_crc32_is_rejected_as_checksum_mismatch() {
        // 钉死语义（alg mismatch 路径）：FDE 验证不过（无合法 CRC 尾）而体末
        // alg 字节声称 CRC32(=1) → mysqld 的 CRC32 流 FDE 恒带尾（T17 真机
        // 5.6.51/5.7.44/8.x 实测 + log_event.cc 写盘次序），无尾+声称 CRC32
        // 只可能是损坏/改写件 → 必须 BinlogError::ChecksumMismatch 硬拒，
        // 不得静默降级成 NONE 流丢弃全文件校验保护。
        let mut s = Synth::start();
        s.crc = false;
        s.push_fde(4, "5.6.51", 1); // 无尾，但 alg 字节 = CRC32
        let mut r = reader(&s, Filters::none());
        assert_eq!(r.next().unwrap_err(), BinlogError::ChecksumMismatch);
    }

    #[test]
    fn fixture_5_7_checksum_none_fde_carries_crc_tail() {
        // T17 真机回归（mysql:5.7.44, --binlog-checksum=none, mysql-bin.000003
        // 首件 FDE，119B 逐字节捕获）：5.7 的 FDE **恒带 4B CRC 尾**——即便
        // binlog_checksum=NONE（alg 字节在 len-5 处=0；go-mysql event.go:186 亦
        // 无条件取 data[len-5]）。旧判定把 body[-1]（实为 crc 尾字节，此处恰为
        // 0x01）当 alg 字节 → 误判"声称 CRC32 而验证失败" → ChecksumMismatch
        // （RED = T17 矩阵 5.7-none 用例整跑挂，out/compat-5.7-none.log）。
        let mut bytes = b"\xfebin".to_vec();
        bytes.extend_from_slice(&[
            0xe0, 0x2b, 0xb0, 0x6a, 0x0f, 0x01, 0x00, 0x00, 0x00, 0x77, 0x00, 0x00, 0x00, 0x7b,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x35, 0x2e, 0x37, 0x2e, 0x34, 0x34, 0x2d,
            0x6c, 0x6f, 0x67, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0xe0, 0x2b, 0xb0, 0x6a, 0x13, 0x38, 0x0d, 0x00, 0x08, 0x00, 0x12, 0x00, 0x04,
            0x04, 0x04, 0x04, 0x12, 0x00, 0x00, 0x5f, 0x00, 0x04, 0x1a, 0x08, 0x00, 0x00, 0x00,
            0x08, 0x08, 0x08, 0x02, 0x00, 0x00, 0x00, 0x0a, 0x0a, 0x0a, 0x2a, 0x2a, 0x00, 0x12,
            0x34, 0x00, 0x00, 0x3d, 0x4f, 0x03, 0x01,
        ]);
        let mut r = FileReader::new(
            "mysql-bin.000003".into(),
            Cursor::new(bytes),
            Filters::none(),
        )
        .unwrap();
        assert!(
            r.next().unwrap().is_none(),
            "FDE 由源消化后干净 EOF；不得报 ChecksumMismatch"
        );
        assert!(!r.with_crc, "alg 字节（len-5）=0 → NONE 态");
    }

    #[test]
    fn oversized_event_header_errors_before_alloc() {
        // T14 Step-0 账载：event_size > MAX_EVENT_SIZE（2GiB）的谎报头必须在
        // vec![0;body_len] 之前被拒（RED 形态：旧实现先分配再 EOF 报错/卡内存）。
        let mut bytes = b"\xfebin".to_vec();
        let mut hb = Vec::new();
        hb.extend_from_slice(&1u32.to_le_bytes()); // ts
        hb.push(2); // QUERY
        hb.extend_from_slice(&1u32.to_le_bytes()); // server_id
        hb.extend_from_slice(&(MAX_EVENT_SIZE + 8).to_le_bytes()); // event_size 越界
        hb.extend_from_slice(&126u32.to_le_bytes()); // log_pos
        hb.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&hb);
        let mut r = FileReader::new(
            "mysql-bin.000001".into(),
            Cursor::new(bytes),
            Filters::none(),
        )
        .unwrap();
        assert!(matches!(
            r.next(),
            Err(BinlogError::InvalidData(m)) if m.contains("valid range")
        ));
    }

    #[test]
    fn events_before_fde_are_hard_errors() {
        // go-mysql parser.go:329-332：非 FDE 事件必须先有 FDE
        let mut s = Synth::start();
        s.push(16, 1, &42u64.to_le_bytes()); // 裸 XID
        let mut r = reader(&s, Filters::none());
        assert!(matches!(r.next(), Err(BinlogError::InvalidData(_))));
    }

    #[test]
    fn non_v4_binlog_version_rejected() {
        let mut s = Synth::start();
        s.push_fde(3, "5.0.95", 0xFF); // binlog v3（5.0 时代）无 checksum alg 字节
        let mut r = reader(&s, Filters::none());
        assert!(matches!(
            r.next(),
            Err(BinlogError::InvalidData(m)) if m.contains("version")
        ));
    }

    #[test]
    fn mariadb_fde_rejected_explicitly() {
        let mut s = Synth::start();
        s.crc = false;
        s.push_fde(4, "10.6.5-MariaDB", 0xFF);
        let mut r = reader(&s, Filters::none());
        assert!(matches!(
            r.next(),
            Err(BinlogError::InvalidData(m)) if m.contains("MariaDB")
        ));
    }

    // ---------- 路由：V0/39/无 tm ----------

    #[test]
    fn v0_and_partial39_are_routed_out_with_clear_errors() {
        let mut s = Synth::with_fde(false);
        s.push(20, 1002, &[0u8; 8]); // WRITE_ROWS_V0
        let mut r = reader(&s, Filters::none());
        assert!(matches!(
            r.next(),
            Err(BinlogError::InvalidData(m)) if m.contains("V0")
        ));

        let mut s = Synth::with_fde(false);
        s.table_map(7, "t10", "u", 1001);
        s.push(39, 1002, &[0u8; 8]); // PARTIAL_UPDATE_ROWS_V2
        let mut r = reader(&s, Filters::none());
        assert_eq!(r.next().unwrap_err(), BinlogError::PartialNotSupported);
    }

    #[test]
    fn rows_without_table_map_is_hard_error() {
        let mut s = Synth::with_fde(false);
        s.write_rows(7, 1002); // 无 table_map
        let mut r = reader(&s, Filters::none());
        assert!(matches!(r.next(), Err(BinlogError::InvalidData(_))));
    }

    #[test]
    fn v1_rows_maps_to_not_v2() {
        let mut s = Synth::with_fde(false);
        s.table_map(7, "t10", "u", 1001);
        let mut b = Vec::new();
        b.extend_from_slice(&7u64.to_le_bytes()[..6]);
        b.extend_from_slice(&[0u8; 2]);
        b.push(1);
        b.push(1);
        b.push(0);
        b.extend_from_slice(&7i32.to_le_bytes());
        s.push(23, 1002, &b); // WRITE_ROWS_V1
        let mut r = reader(&s, Filters::none());
        let ev = r.next().unwrap().unwrap();
        assert_eq!(ev.kind, RawKind::Rows(RowsKind::Write, false));
    }

    // ---------- 窗口（stop 在 header 后 body 前；start 只挡产出） ----------

    #[test]
    fn stop_pos_breaks_before_body_read() {
        let mut s = Synth::with_fde(false);
        s.table_map(7, "t10", "u", 1001);
        let (rows_start, _) = s.write_rows(7, 1002);
        // stop = rows 起始：rows 尾必然 >= stop → header 后立停，body 不读
        // （若读了 body 也不产出，语义同；此测试钉死「不 panic、无事件」）
        let f = Filters {
            stop: Some(("mysql-bin.000001".into(), rows_start)),
            ..Filters::none()
        };
        let mut r = reader(&s, f);
        assert!(r.next().unwrap().is_none());
    }

    #[test]
    fn start_pos_window_skips_emission_but_consumes_fde_and_tablemap() {
        let mut s = Synth::with_fde(false);
        let (tm_start, tm_end) = s.table_map(7, "t10", "u", 1001);
        let (_, rows_end) = s.write_rows(7, 1002);
        s.xid(1003);
        // start = tm_end+1：tm 尾 < start → 窗口外（但仍被消费）；rows 尾在窗口内
        let f = Filters {
            start: Some(("mysql-bin.000001".into(), tm_end + 1)),
            ..Filters::none()
        };
        let mut r = reader(&s, f);
        let evs = collect(&mut r);
        assert_eq!(evs.len(), 2, "rows + xid");
        assert_eq!(evs[0].kind, RawKind::Rows(RowsKind::Write, true));
        assert_eq!(
            evs[0].start_pos, tm_start,
            "窗口外消费的 tm 仍是 rows 的 start_pos 来源"
        );
        assert_eq!(evs[1].end_pos, rows_end + 27);
    }

    // ---------- +06d 推导（上游 funcs.go:98-103） ----------

    #[test]
    fn next_name_uses_decimal_six_width_like_upstream() {
        type F = FileReader<std::fs::File>;
        assert_eq!(
            F::next_binlog_name("mysql-bin.000003").as_deref(),
            Some("mysql-bin.000004")
        );
        assert_eq!(
            F::next_binlog_name("binlog.000009").as_deref(),
            Some("binlog.000010")
        );
        // %06d 是最小宽度：999999 → 1000000（7 位，无截断/十六进制回绕——上游同款）
        assert_eq!(
            F::next_binlog_name("mysql-bin.999999").as_deref(),
            Some("mysql-bin.1000000")
        );
        assert_eq!(F::next_binlog_name("no-index-here"), None);
        assert_eq!(F::next_binlog_name("mysql-bin.000abc"), None);
    }

    // ---------- 真机 fixture 端到端（capture_8.0_minimal） ----------

    fn fixture_reader(filters: Filters) -> FileReader<std::fs::File> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/capture_8.0_minimal");
        FileReader::open(&dir, "mysql-bin.000003", filters).unwrap()
    }

    #[test]
    fn fixture_minimal_end_to_end() {
        let mut r = fixture_reader(Filters::none());
        let evs = collect(&mut r);
        // 真机事件序 [15,35,34,2,34,2,19,30,16]：FDE/35/19 被源吞掉；
        // 其余全产出（34→Gtid 标记，上游忽略 33/34 的裁定 1 对应）。
        let labels: Vec<String> = evs
            .iter()
            .map(|e| match &e.kind {
                RawKind::Query(q) if q.starts_with("CREATE") => "Query(DDL)".into(),
                RawKind::Query(q) => format!("Query({q})"),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            labels,
            vec![
                "Gtid",
                "Query(DDL)",
                "Gtid",
                "Query(BEGIN)",
                "Rows(Write, true)",
                "Xid"
            ]
        );
        assert_eq!(
            evs[4].start_pos, 1020,
            "rows start = table_map 起始（真机钉死）"
        );
        assert_eq!(evs[4].end_pos, 3358);
        assert_eq!(evs[3].start_pos, 939, "BEGIN query 自身起始");
        assert_eq!(evs[3].binlog, "mysql-bin.000003");
        let tm = evs[4].tm.clone().unwrap();
        assert_eq!(
            (tm.schema.as_str(), tm.table.as_str(), tm.n_cols),
            ("t9", "probe", 26)
        );
        // FDE 的 CRC 特例判定在真机件上成立（with_crc=true → 全事件 crc 通过）
        assert!(r.with_crc);
        // 产物体可直接 decode_rows（与 rows.rs 真机结论同缝）
        use crate::binlog::rows::decode_rows;
        use crate::metadata::schema::{SchemaCol, TableSchema};
        let sc = TableSchema {
            db: tm.schema.clone(),
            table: tm.table.clone(),
            cols: (0..26)
                .map(|i| SchemaCol {
                    name: format!("c{i}"),
                    type_name: "int".into(),
                    unsigned: false,
                })
                .collect(),
            pk: vec![],
            uks: vec![],
        };
        let rows = decode_rows(&evs[4].body, &tm, &sc, RowsKind::Write, true).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cols.len(), 26);
    }

    #[test]
    fn fixture_stop_mid_file_breaks_after_header() {
        // stop_pos=2000：rows(尾 3358) 前停 → 最后产出 Query(BEGIN)@1020
        let f = Filters {
            stop: Some(("mysql-bin.000003".into(), 2000)),
            ..Filters::none()
        };
        let mut r = fixture_reader(f);
        let evs = collect(&mut r);
        assert_eq!(evs.len(), 4);
        assert_eq!(evs[3].kind, RawKind::Query("BEGIN".into()));
        assert_eq!(evs[3].end_pos, 1020);
    }

    #[test]
    fn fixture_start_pos_window_still_consumes_fde() {
        // start=1020（tm 起始=BEGIN 尾）：FDE(126)/35(157)/34(236)/DDL(860)/34(939)
        // 尾均 < 1020 → 不产出；BEGIN 尾 =1020 → 进入（>= start 保留）。
        let f = Filters {
            start: Some(("mysql-bin.000003".into(), 1020)),
            ..Filters::none()
        };
        let mut r = fixture_reader(f);
        let evs = collect(&mut r);
        assert_eq!(
            evs.iter().map(|e| e.kind.clone()).collect::<Vec<_>>(),
            vec![
                RawKind::Query("BEGIN".into()),
                RawKind::Rows(RowsKind::Write, true),
                RawKind::Xid
            ]
        );
        assert_eq!(evs[1].start_pos, 1020);
    }

    #[test]
    fn fixture_time_window_uses_ge_for_stop() {
        let mut p = fixture_reader(Filters::none());
        let ts = p.next().unwrap().unwrap().timestamp;
        // stop_ts = 首事件 ts → 立即停（上游 com.go:70 `>=` 排除）
        let f = Filters {
            stop_ts: Some(ts),
            ..Filters::none()
        };
        let mut r = fixture_reader(f);
        assert!(r.next().unwrap().is_none());
        // stop_ts = ts+1 → 全部放行到 EOF
        let f = Filters {
            stop_ts: Some(ts + 1),
            ..Filters::none()
        };
        let mut r = fixture_reader(f);
        assert_eq!(r.next().unwrap().unwrap().kind, RawKind::Gtid);
    }
}
