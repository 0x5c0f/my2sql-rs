//! repl 模式复制通道（P3 Task 2）：
//!
//! - [`transport`]：`mysql` crate（binlog feature）薄适配器 —— 只做
//!   连接/dump 升级/`Event → Frame` 全帧重建（spec §2 spike 实测-1：
//!   `Event::write(Version4, …)` 产物与盘上事件字节逐字节同构），
//!   所有判定逻辑（位点链/更名/CRC）在 [`source`]，合成字节单测可达。
//! - [`source`]：`ReplSource`（实现既有 `pipeline::source::EventSource`），
//!   与 FileReader 同套公开解码件——`src/binlog/` 冻结下的**有意编排
//!   重复**，两测互钉（详见 source.rs 模块注释）。
//!
//! 注：本文件与 checkpoint lane 的合并由 controller 取 `pub mod` 并集。

pub mod checkpoint;
pub mod source;
pub mod transport;

pub use source::ReplSource;
pub use transport::{Frame, FrameStream, ReplError};

/// 无服务器单测注入件（简报 Step 1 口径）：`VecDeque` 假 `FrameStream`
/// + 模块级 `repl_source_for_test` 构造器（`transport::open` 不碰）。
#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::VecDeque;

    use crate::pipeline::filter::Filters;
    use crate::repl::source::ReplSource;
    use crate::repl::transport::{Frame, FrameStream, ReplError};

    /// 帧队列假流：耗尽后 `Ok(None)`（消费方干净停止语义）；`tail` 可挂
    /// 一条 `ReplError` 模拟断链（§2 勘误-6①：None-drop = Disconnect）。
    pub(crate) struct FakeStream {
        queue: VecDeque<Frame>,
        tail: Option<ReplError>,
    }

    impl FakeStream {
        pub(crate) fn new(frames: Vec<Frame>) -> Self {
            Self {
                queue: frames.into(),
                tail: None,
            }
        }
        pub(crate) fn with_tail(frames: Vec<Frame>, err: ReplError) -> Self {
            Self {
                queue: frames.into(),
                tail: Some(err),
            }
        }
    }

    impl FrameStream for FakeStream {
        fn next_frame(&mut self) -> Result<Option<Frame>, ReplError> {
            match self.queue.pop_front() {
                Some(f) => Ok(Some(f)),
                None => match self.tail.take() {
                    Some(e) => Err(e),
                    None => Ok(None),
                },
            }
        }
    }

    /// 单测构造器（`test_support::repl_source_for_test`：帧队列 +
    /// 首文件名，过滤全放行），供 source.rs 测试模块跨模块调用。
    pub(crate) fn repl_source_for_test(frames: Vec<Frame>, first_binlog: String) -> ReplSource {
        ReplSource::new(
            Box::new(FakeStream::new(frames)),
            first_binlog,
            Filters::none(),
            None,
        )
    }
}
