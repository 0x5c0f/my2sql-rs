//! EventHeader（公共 19 字节头）解码 + crc32 checksum 剥离/校验。
//!
//! 行为对照 go-mysql-org/go-mysql replication/event.go 的 `EventHeader.Decode`
//! （commonHeader 19 字节，全小端；body 长度非空 → 要求 `event_size > 19`，
//! 对齐上游 my2sql-go base/file.go:162 对 `<=19` 的 fatal 判定，T14 Step-0）。

// 骨架阶段本模块尚未接入 main 管道（Task 12+ 消费），参照 Task 1 对 config 的处理。

use super::error::BinlogError;

/// 公共事件头长度：timestamp(4)+type(1)+server_id(4)+event_size(4)+log_pos(4)+flags(2)。
pub const EVENT_HEADER_SIZE: usize = 19;

/// 事件类型（u8 newtype，数值与 MySQL 官方 log_event.h / go-mysql
/// `replication/const.go:54-87` 一致——T9 校准修正，见 `event_type_constants_match_mysql`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EventType(pub u8);

impl EventType {
    pub const QUERY: u8 = 2;
    /// 原表误标 `CREATE_DB=3`；const.go:54-87 中 3 为 STOP_EVENT（v3 时代停用，
    /// 无消费者），本补丁按权威勘误为 STOP，值不变。
    pub const STOP: u8 = 3;
    pub const ROTATE: u8 = 4;
    pub const FORMAT_DESC: u8 = 15;
    pub const XID: u8 = 16;
    pub const TABLE_MAP: u8 = 19;
    pub const WRITE_ROWS_V0: u8 = 20;
    pub const UPDATE_ROWS_V0: u8 = 21;
    pub const DELETE_ROWS_V0: u8 = 22;
    pub const WRITE_ROWS_V1: u8 = 23;
    pub const UPDATE_ROWS_V1: u8 = 24;
    pub const DELETE_ROWS_V1: u8 = 25;
    pub const HEARTBEAT: u8 = 27;
    pub const WRITE_ROWS_V2: u8 = 30;
    pub const UPDATE_ROWS_V2: u8 = 31;
    pub const DELETE_ROWS_V2: u8 = 32;
    pub const GTID_LOG: u8 = 33;
    pub const ANONYMOUS_GTID_LOG: u8 = 34;
    pub const PREVIOUS_GTIDS: u8 = 35;
    /// 8.0.1+ 事务上下文事件（T12 Step-0 补录：原表为审阅过的子集，
    /// 常量按官方 log_event.h / go-mysql const.go 序补齐）。
    pub const TRANSACTION_CONTEXT: u8 = 36;
    /// 8.0.1+ 视图变更事件。
    pub const VIEW_CHANGE: u8 = 37;
}

/// binlog 事件公共头（19 字节，小端）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventHeader {
    pub timestamp: u32,
    pub event_type: EventType,
    pub server_id: u32,
    pub event_size: u32,
    pub log_pos: u32,
    pub flags: u16,
}

/// 最大事件大小限制（4MB），用于防止 DoS 攻击（MySQL 官方限制）
const MAX_EVENT_SIZE: u32 = 4 * 1024 * 1024;

/// 从 `buf` 前 19 字节解析公共头；`buf.len() < 19` 时报 [`BinlogError::TooShort`]。
pub fn parse_header(buf: &[u8]) -> Result<EventHeader, BinlogError> {
    if buf.len() < EVENT_HEADER_SIZE {
        return Err(BinlogError::TooShort);
    }
    // 长度已验证，切片转数组必然成功。
    let le32 = |s: usize| u32::from_le_bytes(buf[s..s + 4].try_into().unwrap());
    let header = EventHeader {
        timestamp: le32(0),
        event_type: EventType(buf[4]),
        server_id: le32(5),
        event_size: le32(9),
        log_pos: le32(13),
        flags: u16::from_le_bytes(buf[17..19].try_into().unwrap()),
    };
    // T14 Step-0 修正：NONE+Stop 场景下 Stop 事件恰为 19B 纯头部（无 CRC32），
    // 原 `<= 19` 判定过严导致解析失败（见 BUGS.md B013）。允许 `>= 19` 但需限制上限防 DoS。
    if header.event_size < EVENT_HEADER_SIZE as u32 || header.event_size > MAX_EVENT_SIZE {
        return Err(BinlogError::InvalidData(format!(
            "event_size {} out of valid range [{}, {}]",
            header.event_size, EVENT_HEADER_SIZE, MAX_EVENT_SIZE
        )));
    }
    Ok(header)
}

/// 事件尾部携带 4 字节 crc32 时（`with_crc`）就地剥掉，否则原样保留。
pub fn strip_checksum(payload: &mut Vec<u8>, with_crc: bool) {
    if with_crc && payload.len() >= 4 {
        let keep = payload.len() - 4;
        payload.truncate(keep);
    }
}

/// **FDE 校验和特例**（T12 Step-0 账载项 + T14 Step-0 精修，MySQL 规范行为）：
/// `Format_description_log_event` 的 CRC32 覆盖 `[0, size-4)` 时，公共头 flags
/// 中**仅 `LOG_EVENT_BINLOG_IN_USE_F`（bit 0）按零参与计算**
/// （mysqld log_event.cc:1324-1338 在写 FDE 时先算校验、后置该位；其余 flags
/// 位照常入校验——T14 前旧口径整 2 字节清零，对带其他位的 FDE 必假阴，
/// 真机件 flags 恰为 0x01 故从未暴露，规范以 [`LOG_EVENT_BINLOG_IN_USE_F`] 为准），
/// 真机验证：
/// 本仓库全部 4 个 8.0.46 fixture 的 FDE（flags=0x01、log_pos=126 非零参与
/// 计算）按本规则逐字节吻合；普通 [`crc32_ok`] 口径对其必失败
/// （rows.rs `walk_events` 当年被迫 `type != 15` 绕行，本函数补上正解）。
/// 注：go-mysql（vendored parser.go:238-247）**完全不校验 FDE 的 crc**（FDE
/// 分支只解析不验证）；上游 my2sql-go 更从不验证。本层取「验证」立场，
/// 特例口径按 MySQL 写盘实现实证钉死（bytes 13..17 的 log_pos 参与计算由
/// fixture log_pos=0x7E 非零事实锁定，不是整头清零）。
pub fn fde_checksum_ok(ev: &[u8]) -> bool {
    /// `LOG_EVENT_BINLOG_IN_USE_F`（log_event.h，值 1；小端下落在 flags 低字节）。
    const LOG_EVENT_BINLOG_IN_USE_F: u8 = 0x01;
    if ev.len() < EVENT_HEADER_SIZE + 4 {
        return false;
    }
    let split = ev.len() - 4;
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&ev[..17]);
    // 仅掩 IN_USE 位（T14 Step-0 精修；旧口径整 flags 字段清零）
    hasher.update(&[ev[17] & !LOG_EVENT_BINLOG_IN_USE_F, ev[18]]);
    hasher.update(&ev[EVENT_HEADER_SIZE..split]);
    hasher.finalize() == u32::from_le_bytes(ev[split..].try_into().unwrap())
}

/// 校验 `body_with_crc`：前 `len-4` 字节的 crc32（crc32fast，即 IEEE CRC-32）
/// 是否等于尾部 4 字节小端；`len < 4` 时返回 false。
pub fn crc32_ok(body_with_crc: &[u8]) -> bool {
    if body_with_crc.len() < 4 {
        return false;
    }
    let split = body_with_crc.len() - 4;
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&body_with_crc[..split]);
    hasher.finalize() == u32::from_le_bytes(body_with_crc[split..].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Step 1 的合法 19 字节头（小端）：ts=0x5F8A1B2C, type=19(TABLE_MAP),
    /// server_id=1, event_size=100, log_pos=200, flags=0。
    fn known_header_bytes() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&0x5F8A1B2Cu32.to_le_bytes());
        b.push(19);
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&100u32.to_le_bytes());
        b.extend_from_slice(&200u32.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes());
        b
    }

    #[test]
    fn parses_known_header() {
        let b = known_header_bytes();
        let h = parse_header(&b).unwrap();
        assert_eq!(
            (h.timestamp, h.event_type.0, h.event_size, h.log_pos),
            (0x5F8A1B2C, 19, 100, 200)
        );
        assert_eq!((h.server_id, h.flags), (1, 0));
    }

    #[test]
    fn short_buffer_errors() {
        assert!(parse_header(&[0u8; 18]).is_err());
        assert_eq!(parse_header(&[0u8; 18]).unwrap_err(), BinlogError::TooShort);
    }

    #[test]
    fn event_size_smaller_than_header_is_invalid() {
        // event_size < 19 仍然是非法的（字节数不足头部）
        let mut b = known_header_bytes();
        b[9..13].copy_from_slice(&18u32.to_le_bytes()); // event_size = 18
        assert!(matches!(parse_header(&b), Err(BinlogError::InvalidData(_))));
    }

    #[test]
    fn stop_event_19_bytes_none_format_acceptable() {
        // T14 Step-0: NONE 格式下 STOP 事件恰 19 字节必须合法（BUGS.md B013 根因修正）
        // 原逻辑 `<= 19` 拒绝零体事件，现已改为 `< 19 || > MAX_EVENT_SIZE`
        let mut ev = vec![0u8; EVENT_HEADER_SIZE];
        ev[4] = EventType::STOP; // type=3 (const.go:54)
        ev[9..13].copy_from_slice(&19u32.to_le_bytes()); // event_size = 19
        assert!(
            parse_header(&ev).is_ok(),
            "NONE Stop(19B) should pass after B013 fix"
        );
    }

    #[test]
    fn heartbeat_event_19_bytes_valid() {
        // HEARTBEAT(27) 也是常见零体事件，应同样通过（用于 repl 心跳检测）
        let mut ev = vec![0u8; EVENT_HEADER_SIZE];
        ev[4] = EventType::HEARTBEAT;
        ev[9..13].copy_from_slice(&19u32.to_le_bytes());
        assert!(parse_header(&ev).is_ok(), "HEARTBEAT(19B) should be valid");
    }

    #[test]
    fn event_size_too_large_is_rejected() {
        // MAX_EVENT_SIZE=4MB 防护 DoS 攻击
        let mut b = known_header_bytes();
        b[9..13].copy_from_slice(&(MAX_EVENT_SIZE + 1).to_le_bytes());
        assert!(matches!(parse_header(&b), Err(BinlogError::InvalidData(_))));
    }

    #[test]
    fn fde_checksum_masks_only_binlog_in_use_bit() {
        // T14 Step-0 账载（log_event.cc:1324-1338）：FDE 校验输入只把
        // LOG_EVENT_BINLOG_IN_USE_F（bit 0）置零，其余 flags 位照常参与——
        // 整 2 字节清零的旧口径对带其他位的 FDE 必假阴。
        let mut ev = vec![0u8; EVENT_HEADER_SIZE + 40 + 4];
        let split = ev.len() - 4;
        ev[9..13].copy_from_slice(&((EVENT_HEADER_SIZE + 40 + 4) as u32).to_le_bytes());
        ev[17] = 0x01; // IN_USE
        ev[18] = 0x02; // 另一个任意位（非 IN_USE）：必须保留参与校验
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&ev[..17]);
        hasher.update(&[0x00, 0x02]); // 仅掩 bit 0
        hasher.update(&ev[EVENT_HEADER_SIZE..split]);
        let crc = hasher.finalize();
        ev[split..].copy_from_slice(&crc.to_le_bytes());
        assert!(
            fde_checksum_ok(&ev),
            "只 bit0 置零口径必须过（整 flags 清零必 RED）"
        );
        // bit0 未置位的普通 FDE 同口径自洽
        ev[17] = 0x00;
        let mut h2 = crc32fast::Hasher::new();
        h2.update(&ev[..17]);
        h2.update(&[0x00, 0x02]);
        h2.update(&ev[EVENT_HEADER_SIZE..split]);
        let crc2 = h2.finalize();
        ev[split..].copy_from_slice(&crc2.to_le_bytes());
        assert!(fde_checksum_ok(&ev));
    }

    #[test]
    fn event_type_constants_match_mysql() {
        // 数值逐一对照 MySQL 官方 log_event.h（enum Log_event_type）与 vendored
        // go-mysql replication/const.go:54-87（iota 序）：rows V0=20/21/22、
        // V1=23/24/25、V2=30/31/32、GTID=33、ANONYMOUS_GTID=34、
        // PREVIOUS_GTIDS=35（T9 审阅校准：原表把 V2 标成 V1、ANON 写成 119 均系误标）。
        let actual = [
            ("QUERY", EventType::QUERY),
            ("ROTATE", EventType::ROTATE),
            ("FORMAT_DESC", EventType::FORMAT_DESC),
            ("XID", EventType::XID),
            ("TABLE_MAP", EventType::TABLE_MAP),
            ("WRITE_ROWS_V0", EventType::WRITE_ROWS_V0),
            ("UPDATE_ROWS_V0", EventType::UPDATE_ROWS_V0),
            ("DELETE_ROWS_V0", EventType::DELETE_ROWS_V0),
            ("WRITE_ROWS_V1", EventType::WRITE_ROWS_V1),
            ("UPDATE_ROWS_V1", EventType::UPDATE_ROWS_V1),
            ("DELETE_ROWS_V1", EventType::DELETE_ROWS_V1),
            ("HEARTBEAT", EventType::HEARTBEAT),
            ("WRITE_ROWS_V2", EventType::WRITE_ROWS_V2),
            ("UPDATE_ROWS_V2", EventType::UPDATE_ROWS_V2),
            ("DELETE_ROWS_V2", EventType::DELETE_ROWS_V2),
            ("GTID_LOG", EventType::GTID_LOG),
            ("ANONYMOUS_GTID_LOG", EventType::ANONYMOUS_GTID_LOG),
            ("PREVIOUS_GTIDS", EventType::PREVIOUS_GTIDS),
            ("TRANSACTION_CONTEXT", EventType::TRANSACTION_CONTEXT),
            ("VIEW_CHANGE", EventType::VIEW_CHANGE),
        ];
        let expected: [(&str, u8); 20] = [
            ("QUERY", 2),
            ("ROTATE", 4),
            ("FORMAT_DESC", 15),
            ("XID", 16),
            ("TABLE_MAP", 19),
            ("WRITE_ROWS_V0", 20),
            ("UPDATE_ROWS_V0", 21),
            ("DELETE_ROWS_V0", 22),
            ("WRITE_ROWS_V1", 23),
            ("UPDATE_ROWS_V1", 24),
            ("DELETE_ROWS_V1", 25),
            ("HEARTBEAT", 27),
            ("WRITE_ROWS_V2", 30),
            ("UPDATE_ROWS_V2", 31),
            ("DELETE_ROWS_V2", 32),
            ("GTID_LOG", 33),
            ("ANONYMOUS_GTID_LOG", 34),
            ("PREVIOUS_GTIDS", 35),
            ("TRANSACTION_CONTEXT", 36),
            ("VIEW_CHANGE", 37),
        ];
        assert_eq!(actual, expected);
    }

    /// T9 校准回归：真实 8.0.46 抓包（binlog_row_metadata=MINIMAL，crc32 on）
    /// 逐事件走读，断言事件类型序列与常量表一致。序列由 fixture 实测推得
    /// （非简报猜测值）：FDE → PREVIOUS_GTIDS(35) → ANON_GTID(34) → QUERY(2)
    /// → ANON_GTID(34) → QUERY(2,BEGIN) → TABLE_MAP(19) → WRITE_ROWS_V2(30) → XID(16)。
    #[test]
    fn fixture_8_0_event_type_sequence() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/capture_8.0_minimal/mysql-bin.000003"
        );
        let data = std::fs::read(path).expect("fixture must be committed");
        assert_eq!(&data[..4], b"\xfebin");
        let mut pos = 4usize;
        let mut types = Vec::new();
        while pos < data.len() {
            let h = parse_header(&data[pos..]).unwrap();
            let size = h.event_size as usize;
            // 注：不对逐事件做 crc32_ok——FDE 的 CRC 校验区须再排除公共头末 4B
            // （MySQL 规范），现 crc32_ok 不覆盖该特例（既有 T9 事实，非本补丁范围）。
            types.push(h.event_type);
            pos += size;
        }
        assert_eq!(pos, data.len());
        assert_eq!(
            types,
            vec![
                EventType(EventType::FORMAT_DESC),
                EventType(EventType::PREVIOUS_GTIDS),
                EventType(EventType::ANONYMOUS_GTID_LOG),
                EventType(EventType::QUERY),
                EventType(EventType::ANONYMOUS_GTID_LOG),
                EventType(EventType::QUERY),
                EventType(EventType::TABLE_MAP),
                EventType(EventType::WRITE_ROWS_V2),
                EventType(EventType::XID),
            ]
        );
    }

    /// T12 Step-0 账载项（FDE crc 特例）：真机 8.0.46 fixture 的 FDE 必须
    /// 过 [`fde_checksum_ok`] 且**不过**普通 [`crc32_ok`]（钉死特例真实存在），
    /// 其余事件反向（普通口径过）。篡改后双双失败。
    #[test]
    fn fixture_fde_needs_flags_zeroed_coverage() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/capture_8.0_minimal/mysql-bin.000003"
        );
        let data = std::fs::read(path).expect("fixture must be committed");
        let h = parse_header(&data[4..]).unwrap();
        assert_eq!(h.event_type.0, EventType::FORMAT_DESC);
        assert_eq!(h.flags, 0x01, "真机 FDE 落盘 flags=IN_USE（计算时为零）");
        let fde = &data[4..4 + h.event_size as usize];
        assert!(fde_checksum_ok(fde));
        assert!(!crc32_ok(fde), "普通口径对 FDE 必失败——特例存在的负证明");
        let mut bad = fde.to_vec();
        bad[30] ^= 0x80;
        assert!(!fde_checksum_ok(&bad));
        // 非 FDE 事件：普通口径过、无特例需求
        let h2 = parse_header(&data[4 + h.event_size as usize..]).unwrap();
        assert_eq!(h2.event_type.0, EventType::PREVIOUS_GTIDS);
        let off = 4 + h.event_size as usize;
        let ev2 = &data[off..off + h2.event_size as usize];
        assert!(crc32_ok(ev2));
    }

    #[test]
    fn crc32_ok_accepts_valid_tail() {
        let mut b = known_header_bytes(); // header 19B
        b.extend_from_slice(&[1, 2, 3]); // body 3B
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&b);
        let crc = hasher.finalize();
        b.extend_from_slice(&crc.to_le_bytes());
        assert!(crc32_ok(&b));
    }

    #[test]
    fn crc32_ok_rejects_tampered_byte() {
        let mut b = known_header_bytes();
        b.extend_from_slice(&[1, 2, 3]);
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&b);
        let crc = hasher.finalize();
        b.extend_from_slice(&crc.to_le_bytes());
        b[20] ^= 0x80; // 篡改 body 中 1 字节
        assert!(!crc32_ok(&b));
    }

    #[test]
    fn strip_checksum_removes_tail_only_when_enabled() {
        let mut v = vec![1, 2, 3, 4, 5, 6, 7, 8];
        strip_checksum(&mut v, true);
        assert_eq!(v, vec![1, 2, 3, 4]);
        let mut w = vec![1, 2, 3, 4, 5, 6, 7, 8];
        strip_checksum(&mut w, false);
        assert_eq!(w, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn binlog_error_display() {
        assert_eq!(BinlogError::TooShort.to_string(), "buffer too short");
        assert_eq!(
            BinlogError::ChecksumMismatch.to_string(),
            "checksum mismatch"
        );
        assert_eq!(
            BinlogError::UnexpectedEof.to_string(),
            "unexpected end of input"
        );
        assert_eq!(
            BinlogError::InvalidData("x".into()).to_string(),
            "invalid data: x"
        );
    }
}
