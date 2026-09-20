//! 集成测试共享常量（Task 15 差分测试基建 / 真实 binlog 样本构造用）。
//!
//! 注意：本工程当前为 bin-only crate，`tests/` 下子目录文件不会被 cargo 编译为
//! 测试目标；待后续任务引入 lib 目标后由 `mod fixtures;` 引用。单元测试常量请
//! 直接写在 `src/binlog/event.rs` 等的 `#[cfg(test)] mod tests` 内（TDD 约定）。

/// 公共事件头长度（字节）。与 `src::binlog::event::EVENT_HEADER_SIZE` 保持一致。
pub const EVENT_HEADER_SIZE: usize = 19;

/// 已知的合法 19 字节头字段值（小端序列化见 `known_header_bytes`）：
/// ts=0x5F8A1B2C, type=19(TABLE_MAP), server_id=1, event_size=100, log_pos=200, flags=0。
pub const KNOWN_TS: u32 = 0x5F8A_1B2C;
pub const KNOWN_TYPE: u8 = 19;
pub const KNOWN_SERVER_ID: u32 = 1;
pub const KNOWN_EVENT_SIZE: u32 = 100;
pub const KNOWN_LOG_POS: u32 = 200;
pub const KNOWN_FLAGS: u16 = 0;

/// 手工构造上述合法 19 字节头（小端），供集成测试复用。
pub fn known_header_bytes() -> Vec<u8> {
    let mut b = Vec::with_capacity(EVENT_HEADER_SIZE);
    b.extend_from_slice(&KNOWN_TS.to_le_bytes());
    b.push(KNOWN_TYPE);
    b.extend_from_slice(&KNOWN_SERVER_ID.to_le_bytes());
    b.extend_from_slice(&KNOWN_EVENT_SIZE.to_le_bytes());
    b.extend_from_slice(&KNOWN_LOG_POS.to_le_bytes());
    b.extend_from_slice(&KNOWN_FLAGS.to_le_bytes());
    b
}
