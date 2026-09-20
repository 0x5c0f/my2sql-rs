//! EventHeader（公共 19 字节头）解码 + crc32 checksum 剥离/校验。
//!
//! 行为对照 go-mysql-org/go-mysql replication/event.go 的 `EventHeader.Decode`
//! （commonHeader 19 字节，全小端；且要求 `event_size >= 19`）。

// 骨架阶段本模块尚未接入 main 管道（Task 12+ 消费），参照 Task 1 对 config 的处理。
#![allow(dead_code)]

use super::error::BinlogError;

/// 公共事件头长度：timestamp(4)+type(1)+server_id(4)+event_size(4)+log_pos(4)+flags(2)。
pub const EVENT_HEADER_SIZE: usize = 19;

/// 事件类型（u8 newtype，数值与 MySQL 官方 / go-mysql EventType 常量一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EventType(pub u8);

impl EventType {
    pub const QUERY: u8 = 2;
    pub const CREATE_DB: u8 = 3;
    pub const ROTATE: u8 = 4;
    pub const FORMAT_DESC: u8 = 15;
    pub const XID: u8 = 16;
    pub const TABLE_MAP: u8 = 19;
    pub const HEARTBEAT: u8 = 27;
    pub const WRITE_ROWS_V1: u8 = 30;
    pub const UPDATE_ROWS_V1: u8 = 31;
    pub const DELETE_ROWS_V1: u8 = 32;
    pub const GTID_LOG: u8 = 33;
    pub const WRITE_ROWS_V2: u8 = 34;
    pub const UPDATE_ROWS_V2: u8 = 35;
    pub const DELETE_ROWS_V2: u8 = 36;
    pub const PREVIOUS_GTIDS: u8 = 37;
    pub const ANONYMOUS_GTID_LOG: u8 = 119;
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
        let actual = [
            ("QUERY", EventType::QUERY),
            ("ROTATE", EventType::ROTATE),
            ("FORMAT_DESC", EventType::FORMAT_DESC),
            ("XID", EventType::XID),
            ("TABLE_MAP", EventType::TABLE_MAP),
            ("HEARTBEAT", EventType::HEARTBEAT),
            ("WRITE_ROWS_V1", EventType::WRITE_ROWS_V1),
            ("UPDATE_ROWS_V1", EventType::UPDATE_ROWS_V1),
            ("DELETE_ROWS_V1", EventType::DELETE_ROWS_V1),
            ("GTID_LOG", EventType::GTID_LOG),
            ("WRITE_ROWS_V2", EventType::WRITE_ROWS_V2),
            ("UPDATE_ROWS_V2", EventType::UPDATE_ROWS_V2),
            ("DELETE_ROWS_V2", EventType::DELETE_ROWS_V2),
            ("PREVIOUS_GTIDS", EventType::PREVIOUS_GTIDS),
            ("ANONYMOUS_GTID_LOG", EventType::ANONYMOUS_GTID_LOG),
        ];
        let expected: [(&str, u8); 15] = [
            ("QUERY", 2),
            ("ROTATE", 4),
            ("FORMAT_DESC", 15),
            ("XID", 16),
            ("TABLE_MAP", 19),
            ("HEARTBEAT", 27),
            ("WRITE_ROWS_V1", 30),
            ("UPDATE_ROWS_V1", 31),
            ("DELETE_ROWS_V1", 32),
            ("GTID_LOG", 33),
            ("WRITE_ROWS_V2", 34),
            ("UPDATE_ROWS_V2", 35),
            ("DELETE_ROWS_V2", 36),
            ("PREVIOUS_GTIDS", 37),
            ("ANONYMOUS_GTID_LOG", 119),
        ];
        assert_eq!(actual, expected);
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
