//! binlog 解码统一错误类型（Task 3-12 各解码函数均以 `Result<_, BinlogError>` 返回）。

// ChecksumMismatch / UnexpectedEof 的构造点在后续任务，骨架阶段参照 Task 1 允许死代码。

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BinlogError {
    /// 输入缓冲不足以容纳当前结构的最小长度（如 event header < 19 字节）。
    #[error("buffer too short")]
    TooShort,
    /// CRC32 校验和不匹配（binlog 开启 checksum 后尾部 4 字节小端与实算不符）。
    #[error("checksum mismatch")]
    ChecksumMismatch,
    /// 读取流时在预期位置之前遇到 EOF（如 event_size 声称有 N 字节但流只剩 M < N）。
    #[error("unexpected end of input")]
    UnexpectedEof,
    /// 字段值本身非法（如 event_size < header 长度、枚举值未知等），携带描述信息。
    #[error("invalid data: {0}")]
    InvalidData(String),
    /// rows 事件携带 P1 不支持的 partial/未知附加语义（如 extra-info 未知
    /// typecode；扩展口径：事件码 39 PARTIAL_UPDATE 由 T12 路由层同判），
    /// 不做猜测性解码（D5）。构造点见 binlog::rows（Task 10）。
    #[error("partial rows / unknown row-event extras are not supported in P1")]
    PartialNotSupported,
}
