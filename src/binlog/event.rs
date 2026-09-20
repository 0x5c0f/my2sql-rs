//! EventHeader（公共 19 字节头）解码 + crc32 checksum 剥离/校验。
//!
//! 行为对照 go-mysql-org/go-mysql replication/event.go 的 `EventHeader.Decode`
//! （commonHeader 19 字节，全小端；且要求 `event_size >= 19`）。

// 骨架阶段本模块尚未接入 main 管道（Task 12+ 消费），参照 Task 1 对 config 的处理。
#![allow(dead_code)]

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
    // 对照 go-mysql：event_size 小于头长度视为坏数据，防止下游按错误长度切片。
    if header.event_size < EVENT_HEADER_SIZE as u32 {
        return Err(BinlogError::InvalidData(format!(
            "event_size {} < header size {EVENT_HEADER_SIZE}",
            header.event_size
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
        // 对照 go-mysql：event_size 必须 >= 19，否则视为坏数据。
        let mut b = known_header_bytes();
        b[9..13].copy_from_slice(&18u32.to_le_bytes()); // event_size = 18
        assert!(matches!(parse_header(&b), Err(BinlogError::InvalidData(_))));
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
        ];
        let expected: [(&str, u8); 18] = [
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
