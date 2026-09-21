//! `mysql` crate（28.0.2，binlog feature）复制流薄适配层（P3 Task 2）。
//!
//! 本层**刻意做薄**：只做连接、dump 升级（`Conn::get_binlog_stream`，
//! 消耗 Conn——spec §2 勘误：repl 连接与元数据连接必须物理两条）与
//! `Event → Frame` 全帧重建；所有判定逻辑（位点链、ROTATE 更名、CRC
//! 口径、心跳消化）归 [`crate::repl::source`]，合成字节单测可达面。
//!
//! 字节口径（spec §2 spike 实测-1/2，Task 0 dd/od 验证）：
//! `Event::write(Version4, …)` 重建 **19B 公共头 + 事件体 + CRC32 尾**
//! 的文件同构全帧，与盘上事件逐字节相同（例外仅 dump 线程合成帧——
//! 文件中本不存在）。故 [`Frame::bytes`] **不剥 CRC**，`ReplSource` 走
//! FileReader 同款公开件（`parse_header`/`crc32_ok`/`strip_checksum`/
//! `fde_checksum_ok`）解码——file 模式解码链零分叉。
//!
//! 断链形态（spec §2 勘误-6）：①优雅终止（服务端关 dump）→ 迭代器直接
//! `None` 无 Err → 本层映射为 [`ReplError::Disconnect`]（**repl 无自然
//! EOF**，任何未达 stop 的终止都是断链，T5 按可重连处理）；②硬断 →
//! `Some(Err(IoError))` → `is_connectivity_error()` 亦归 Disconnect。

use std::time::Duration;

use mysql::prelude::Queryable;
use mysql::{BinlogRequest, BinlogStream, Conn, Error as MysqlError, Opts};
use thiserror::Error;

/// 事件全帧（19B 头 + 体 + CRC 尾未剥，文件同构字节）。
#[derive(Debug, Clone)]
pub struct Frame {
    pub bytes: Vec<u8>,
    /// ROTATE（含流首 fake rotate / EOF 合成帧）payload 指名的下一文件。
    /// 源侧以帧内 body 为准、本字段为兜底（body 名缺失/损坏时）。
    pub binlog_hint: Option<String>,
}

/// 复制事件流抽象：生产实现包 `BinlogStream`，单测注入 `VecDeque` 假流。
///
/// `Ok(None)` 保留给**消费方干净停止**（测试注入流的自然耗尽）；生产
/// 实现永不自返 `Ok(None)`——流终止一律 [`ReplError::Disconnect`]。
pub trait FrameStream: Send {
    fn next_frame(&mut self) -> Result<Option<Frame>, ReplError>;
}

/// 复制通道错误面（T5 重连分类学的依据，spec §6 终止/重连两分诊）。
#[derive(Debug, Error)]
pub enum ReplError {
    /// 认证失败（1045，终止面：不可恢复）。
    #[error("authentication failed: {0}")]
    Auth(String),
    /// 缺 REPLICATION SLAVE/CLIENT 权限（1227 家族，终止面）。
    #[error("missing REPLICATION privileges: {0}")]
    MissingPriv(String),
    /// 请求位点已被主库 purge（1236，终止面：需人工指定新起点）。
    #[error("binlog position purged on master: {0}")]
    Purged(String),
    /// 协议/帧重建/URL 解析错误。
    #[error("replication protocol error: {0}")]
    Protocol(String),
    /// 其余服务端错误（原码透传，分类权归 T5）。
    #[error("server error {code}: {msg}")]
    Server { code: u16, msg: String },
    /// 本地 IO（读包超时等；连接级断裂归 Disconnect）。
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// 流断链（未达 stop 的终止，spike 勘误-6 两形态归一）——T5 视为
    /// 可重连。**固定变体集无法表达 None-drop 断链，此为接口增补。**
    #[error("replication stream disconnected: {0}")]
    Disconnect(String),
}

/// 建立注册从库 + BINLOG_DUMP 升级，返回帧流（消耗内部 Conn）。
///
/// - `heartbeat = Some(d)`：dump 升级前同连接 `SET @master_heartbeat_period
///   = <ns>`（spec §2 勘误-4：BinlogRequest/flags 无心跳入口，唯一实测通路）。
///   SET 失败**不致命**——静默降级为无心跳流，Option 原样保留给 T5 用读
///   超时兜底（§6 死链探测）。
/// - TLS 不提供（spec §2 勘误-5：`Opts::from_url` 白名单无 ssl 项，未知
///   query 参硬错），uri 原样透传。
pub fn open(
    uri: &str,
    server_id: u32,
    file: &str,
    pos: u32,
    heartbeat: Option<Duration>,
) -> Result<Box<dyn FrameStream>, ReplError> {
    let opts = Opts::from_url(uri).map_err(|e| ReplError::Protocol(format!("invalid uri: {e}")))?;
    let mut conn = Conn::new(opts).map_err(map_mysql_error)?;
    if let Some(d) = heartbeat {
        let _ = conn.query_drop(format!("SET @master_heartbeat_period = {}", d.as_nanos()));
    }
    let req = BinlogRequest::new(server_id)
        .with_filename(file.as_bytes().to_vec())
        .with_pos(u64::from(pos));
    let stream = conn.get_binlog_stream(req).map_err(map_mysql_error)?;
    Ok(Box::new(MysqlFrameStream { stream }))
}

/// `BinlogStream` → `FrameStream` 适配（逐事件 `Event::write` 全帧重建）。
struct MysqlFrameStream {
    stream: BinlogStream,
}

impl FrameStream for MysqlFrameStream {
    fn next_frame(&mut self) -> Result<Option<Frame>, ReplError> {
        match self.stream.next() {
            Some(Ok(event)) => {
                let mut bytes = Vec::new();
                event
                    .write(mysql::binlog::BinlogVersion::Version4, &mut bytes)
                    .map_err(|e| ReplError::Protocol(format!("event rebuild failed: {e}")))?;
                let binlog_hint = match event.read_data() {
                    Ok(Some(mysql::binlog::events::EventData::RotateEvent(re))) => {
                        Some(re.name().into_owned())
                    }
                    _ => None,
                };
                Ok(Some(Frame { bytes, binlog_hint }))
            }
            Some(Err(e)) => Err(map_mysql_error(e)),
            // 服务端优雅终止 dump（docker restart 等）：迭代器 None 无 Err。
            // repl 语义下这不是自然 EOF → Disconnect（T5 重连路径）。
            None => Err(ReplError::Disconnect(
                "stream ended without stop condition (server closed dump)".into(),
            )),
        }
    }
}

/// `mysql::Error` → 重连分类学映射（spec §2 勘误-6②：`is_connectivity_error`
/// 为分诊钩子；1045/1227/1236 落 MySqlError 走终止面）。
fn map_mysql_error(e: MysqlError) -> ReplError {
    if let MysqlError::MySqlError(m) = &e {
        return match m.code {
            1045 => ReplError::Auth(m.message.clone()),
            1227 => ReplError::MissingPriv(m.message.clone()),
            1236 => ReplError::Purged(m.message.clone()),
            other => ReplError::Server {
                code: other,
                msg: m.message.clone(),
            },
        };
    }
    if e.is_connectivity_error() {
        return ReplError::Disconnect(e.to_string());
    }
    match e {
        MysqlError::IoError(io) => ReplError::Io(io),
        other => ReplError::Protocol(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mysql::MySqlError;

    fn server_err(code: u16, msg: &str) -> MysqlError {
        MysqlError::MySqlError(MySqlError {
            state: "HY000".into(),
            code,
            message: msg.into(),
        })
    }

    /// §6 终止面三变体逐一钉死（T5 分类学依赖）。
    #[test]
    fn terminal_server_errors_map_to_dedicated_variants() {
        assert!(matches!(
            map_mysql_error(server_err(1045, "Access denied")),
            ReplError::Auth(m) if m == "Access denied"
        ));
        assert!(matches!(
            map_mysql_error(server_err(1227, "Access denied; you need REPLICATION")),
            ReplError::MissingPriv(_)
        ));
        assert!(matches!(
            map_mysql_error(server_err(1236, "client wants log that has been deleted")),
            ReplError::Purged(_)
        ));
        // 其余码原样透传 Server{code,msg}（分类权归 T5，不在此预判）
        assert!(matches!(
            map_mysql_error(server_err(1146, "no such table")),
            ReplError::Server { code: 1146, .. }
        ));
    }

    /// 断链两形态（spike 勘误-6）归一 Disconnect；本地 IO 保 Io 变体。
    #[test]
    fn connectivity_errors_are_disconnect() {
        let io = MysqlError::IoError(std::io::Error::other("server disconnected"));
        assert!(matches!(map_mysql_error(io), ReplError::Disconnect(_)));
        let d = MysqlError::DriverError(mysql::DriverError::UnexpectedPacket);
        assert!(matches!(map_mysql_error(d), ReplError::Disconnect(_)));
        // IoError 走 is_connectivity_error=true 臂（上方），Io 变体保留给
        // 调用方构造的本地 IO（from<std::io::Error>）—— 烟雾：
        let local: ReplError = std::io::Error::other("local").into();
        assert!(matches!(local, ReplError::Io(_)));
        // Display 面（日志可读性）
        assert!(
            ReplError::Disconnect("x".into())
                .to_string()
                .contains("disconnected")
        );
    }

    /// 编译期钉死契约形状（T5 依赖的 pinned 接口签名）。
    #[test]
    fn pinned_interface_shapes_compile() {
        type OpenFn =
            fn(&str, u32, &str, u32, Option<Duration>) -> Result<Box<dyn FrameStream>, ReplError>;
        let _: OpenFn = open as OpenFn;
        let f = Frame {
            bytes: vec![0u8; 19],
            binlog_hint: Some("mysql-bin.000002".into()),
        };
        assert_eq!(f.bytes.len(), 19);
        assert!(f.binlog_hint.is_some());
    }
}
