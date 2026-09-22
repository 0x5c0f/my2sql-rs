use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::binlog::error::BinlogError;
use crate::config::Config;
use crate::metadata::store::SchemaStore;
use crate::output::Writer;
use crate::pipeline::filter::Filters;
use crate::pipeline::source::{EventSource, RawEvent};
use crate::pipeline::{Emitter, PipelineError, REPL_INTERRUPT, RunSummary, Runner, open_store};
use crate::repl::ReplSource;
use crate::repl::checkpoint::{self, Checkpoint};
use crate::repl::transport::{self, Frame, FrameStream, ReplError};
use crate::sqlopen::dml::{DmlBuilder, SqlOpts};

// ────────────────────────────────────────────────────────────────────────────
// P3 T5：repl 装配——三态定位 / checkpoint 接续 / 封顶退避重连 / SIGINT 收尾
// ────────────────────────────────────────────────────────────────────────────

/// 位点被主库 purge 的终止文案（简报逐字钉；`...` 为固定占位不是格式串）。
pub(crate) const PURGED_HINT: &str =
    "replication position ... does not exist on master (binlog purged): choose a newer start";
/// 权限缺失的终止文案（简报逐字钉；1227 家族。fix round M1：1045 认证失败
/// 自 spec §6 起即为另一终止类，不再共用本文案——见 [`AUTH_HINT`]）。
pub(crate) const PRIV_HINT: &str = "user lacks REPLICATION SLAVE/CLIENT privilege";
/// 认证失败（1045）的专属终止文案（fix round M1，spec §6 两终止类分立）：
/// 动作是修 --uri 凭据，与 GRANT 无关——混写会把改密码的人引去改权限。
pub(crate) const AUTH_HINT: &str = "authentication failed: the server rejected the credentials in --uri \
                                    (wrong password, unknown user, or host not granted); fix the connection URI";
/// resume 起点被 purge 的前缀（+ [`PURGED_HINT`] 逐字）。
pub(crate) const RESUME_GONE_PREFIX: &str = "resume point is gone: ";
/// 退避封顶秒（1s 起翻倍）。
const BACKOFF_CAP_SECS: f64 = 30.0;
/// 同因快速失败的连发窗口（毫秒）：两次同因失败间隔 ≥ 本窗即重置计数。
/// 终审 FIX E 精细化后的真实角色：ServerIdConflict（1236 文案互踢形态）
/// 仍由本窗独立兜底（旧口径逐字不变）；Disconnect 闸改按**零进度秒断**
/// 计连发（[`FailureTracker::observe`]）——主库重启的失败-失败间隔 = 退避
/// 本身（1s/2s/4s…封顶 30s×1.25=37.5s，恒 < 本窗），旧注释「退避自然拉长
/// 后间隔必超窗」对封顶退避不成立，故重启恢复改由进度信号保护；本窗对
/// Disconnect 退化为「间隔超窗 = 新一段故障」的第二道重置（互踢循环连发
/// 间隔永不超窗，正常永不触发）。
pub(crate) const SERVER_ID_GRACE_MS: u64 = 60_000;
/// 同因秒断终止阈值（spec §6「连续 3 次同因秒断即终止报错」；适用范围
/// ServerIdConflict + Disconnect〔零进度口径，FIX E 精细化〕，终审 FIX E）。
const FAST_FAIL_LIMIT: u32 = 3;

/// 位点三态 + resume 的判定结果（纯函数可测，控制器裁定：start_file 空
/// 即 now 哨兵，**无论 start_pos**——clap 默认 4 不得把裸默认带进直连路径）。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Locate {
    Resume,
    Now,
    FilePos,
    Datetime,
}

/// 定位判定纯函数（优先级：resume > datetime > now 哨兵 > file+pos 直给）。
/// `start_pos` 在 now 分支**有意不消费**（签名保留以显式钉死裁定）。
pub(crate) fn decide_locate(
    has_resume: bool,
    start_file: &str,
    _start_pos: u32,
    has_datetime: bool,
) -> Locate {
    if has_resume {
        Locate::Resume
    } else if has_datetime {
        Locate::Datetime
    } else if start_file.is_empty() {
        Locate::Now
    } else {
        Locate::FilePos
    }
}

/// [`ReplError`] 的重连分类学归档（spec §6 终止/重连两分诊的装配侧落点）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailKind {
    /// 可重连：断链/本地 IO。
    Disconnect,
    /// 终止：位点被 purge（1236 非 server-id 文案）。
    Purged,
    /// 终止：认证失败（1045）。
    Auth,
    /// 终止：权限缺失（1227 家族）。
    Priv,
    /// 可重连（但受同因秒断终止闸管辖）：1236 双面的 server-id 形态。
    ServerIdConflict,
    /// 可重连：其余服务端码/协议错（首因留痕，连续风暴由 tracker 兜底）。
    Other,
}

/// 一轮失败的结构化摘要（泵侧经 sink 上报；开流侧就地构造）。
#[derive(Debug, Clone)]
pub(crate) struct FailReport {
    pub kind: FailKind,
    pub cause: String,
}

/// 1236 双面（裁定）：同码不同因——文案含 server_id/server-uuid 特征即
/// server-id 冲突（可重连，交给秒断终止闸）；否则真 purge（终止）。
pub(crate) fn classify_repl_failure(e: &ReplError) -> (FailKind, String) {
    match e {
        ReplError::Purged(m) => {
            let lm = m.to_ascii_lowercase();
            if lm.contains("server_id") || lm.contains("server-uuid") || lm.contains("server id") {
                (
                    FailKind::ServerIdConflict,
                    format!("server-id conflict suspected: {m}"),
                )
            } else {
                (FailKind::Purged, m.clone())
            }
        }
        ReplError::Auth(m) => (FailKind::Auth, m.clone()),
        ReplError::MissingPriv(m) => (FailKind::Priv, m.clone()),
        ReplError::Disconnect(_) | ReplError::Io(_) => (FailKind::Disconnect, e.to_string()),
        ReplError::Server { .. } | ReplError::Protocol(_) => (FailKind::Other, e.to_string()),
    }
}

/// 同因连发计数器（换因重置；间隔超 grace 窗重置）。observe 返回含本次
/// 在内的当前同因连发数；终止判定（≥3 且 kind∈{ServerIdConflict,
/// Disconnect}）在调用方。分形态语义（终审 FIX E 精细化）：
/// - `ServerIdConflict`（1236 文案）等：同因 + 间隔 < grace → +1，否则
///   重置为 1（T5 原判逻辑，逐字保留）；
/// - `Disconnect`：只计**连续零进度秒断**——该尝试 open 成功且一帧
///   RawEvent 都未投递（register/dump 期同 id 互踢循环的签名，spec §6
///   「连续 3 次同因秒断」）。投递过事件的串中断路径、以及开流即被拒
///   （主库重启窗口形态：从未建立流，非「秒断」）一律把连发清零——
///   重启恢复不误杀；真互踢恒为 3 连零事件秒断 → 仍第 3 连终止。
#[derive(Debug, Default)]
pub(crate) struct FailureTracker {
    last: Option<(FailKind, u64, u32)>,
}

impl FailureTracker {
    pub(crate) fn new() -> Self {
        Self::default()
    }
    /// `bare_break` = 本尝试开流成功且投递 **0** 个 RawEvent（裸秒断）。
    /// 仅对 `Disconnect` 有意义；其余 kind 忽略该参（保持 T5 原逻辑）。
    pub(crate) fn observe(&mut self, kind: FailKind, now_ms: u64, bare_break: bool) -> u32 {
        let streak = match self.last {
            Some((k, at, s)) if k == kind && now_ms.saturating_sub(at) < SERVER_ID_GRACE_MS => {
                if kind == FailKind::Disconnect {
                    if bare_break { s + 1 } else { 0 }
                } else {
                    s + 1
                }
            }
            // 换因/宽间隔重开一段；Disconnect 非秒断臂不占连发位（清零）。
            _ if kind == FailKind::Disconnect && !bare_break => 0,
            _ => 1,
        };
        self.last = Some((kind, now_ms, streak));
        streak
    }
}

/// 指数退避：`attempt`（1 起）→ base = min(2^(attempt-1), 30)s，乘子由
/// `unit`∈[0,1) 线性映到 ±25% 抖动带 [0.75, 1.25)。纯函数（unit 注入
/// 即为钉死测试的确定性面）。
pub(crate) fn reconnect_backoff_secs(attempt: u32, unit: f64) -> f64 {
    let exp = attempt.saturating_sub(1).min(64);
    let base = (2f64.powi(exp as i32)).min(BACKOFF_CAP_SECS);
    base * (0.75 + 0.5 * unit)
}

/// 生产抖动源：SystemTime 亚秒纳秒过 splitmix64 混洗 → [0,1)。
fn jitter_unit() -> f64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x5DEECE66D);
    let mut z = nanos.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    ((z >> 40) as f64) / ((1u64 << 24) as f64)
}

/// 重连 warn 行（简报逐字钉）。
pub(crate) fn reconnect_warn(k: u32, backoff_secs: f64, cause: &str) -> String {
    format!("repl: reconnect #{k} in {backoff_secs:.1}s (cause: {cause})")
}

/// datetime 二分的纯核（上游 `binlog_scan.go` BinarySearchBinlogReplMode
/// 口径）：探测失败/**首事件 ts=0**/ts≤目标 → 候选右移（result=mid）；
/// ts>目标 → 向左收；全大于目标 → 0（最老档，客户端 start_ts 过滤兜住）。
/// `probe` 返回 None = 探测失败（同左收，上游 err → hi=mid-1）。
pub(crate) fn bisect_index(
    n: usize,
    want: u32,
    probe: &mut dyn FnMut(usize) -> Option<u32>,
) -> Option<usize> {
    if n == 0 {
        return None;
    }
    let (mut lo, mut hi, mut result) = (0usize, n - 1, 0usize);
    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        match probe(mid) {
            None => {}
            Some(ts) if ts == 0 || ts <= want => {
                result = mid;
                lo = mid + 1;
                continue;
            }
            Some(_) => {}
        }
        if mid == 0 {
            break;
        }
        hi = mid - 1;
    }
    Some(result)
}

/// 帧流开缝类型（clippy type_complexity 解构；生产 = `transport::open`
/// 就地闭包，单测 = fake 工厂——重连纪律的被钉对象）。
pub(crate) type Opener<'a> = dyn FnMut(&str, u32) -> Result<Box<dyn FrameStream>, ReplError> + 'a;

/// datetime 探针单文件墙钟硬顶（fix round I2）。缺省 heartbeat=30s 时
/// 首个心跳 ≤30s 必达、探针即时落地；90s 覆盖「丢一轮心跳 + 读超时
/// 2d+1s」的余量。超顶按探测失败（None → 二分左收，[`bisect_index`]
/// 与上游 err→hi=mid-1 同型）——宁可定位保守，不可卡死整场 bisect。
const PROBE_FIRST_TS_CAP: Duration = Duration::from_secs(90);

/// 心跳事件 kind 字节：v1 0x1b（8.0.46 实测发送形态，spec §2 勘误-4）
/// 与 v2 0x29（mysql_common 0.37.3 不解析，防御性一并认）。
const HEARTBEAT_KINDS: [u8; 2] = [crate::binlog::event::EventType::HEARTBEAT, 41];

/// 探针用流抽头（fix round I2 关键）：心跳帧被 [`ReplSource`] 内部消化、
/// 泵外侧不可见，而「是否已追平活写尾部」正是由心跳报知的——抽头在帧
/// 进入解码前窥探 kind 字节（19B 公共头 offset 4，与 parse_header 同源）：心跳 → 置旗标并以
/// `Ok(None)` 终结本流。判别依据：服务端心跳只在 dump 追到实时写尾后
/// 按周期发出；已闭档文件的事件是背靠背瞬发的——「第一帧即心跳」⟺
/// 查询区间内不存在数据事件（≙ 空档，ts=0 右移语义）。其余帧原样透传，
/// 解码权威仍是唯一的 ReplSource 链（零分叉不变）。
struct ProbeTap {
    inner: Box<dyn FrameStream>,
    saw_heartbeat: Arc<AtomicBool>,
}

impl FrameStream for ProbeTap {
    fn next_frame(&mut self) -> Result<Option<Frame>, ReplError> {
        let frame = match self.inner.next_frame()? {
            Some(frame) => frame,
            None => return Ok(None),
        };
        if frame
            .bytes
            .get(4)
            .is_some_and(|k| HEARTBEAT_KINDS.contains(k))
        {
            self.saw_heartbeat.store(true, Ordering::Relaxed);
            return Ok(None);
        }
        Ok(Some(frame))
    }
}

/// 工作线程侧消费环：抽头整体作为 transport 装进 ReplSource（解码权威
/// 不变、心跳经抽头转成干净流终并留旗标）。首个数据事件 ts → Some(ts)；
/// 流终/硬错 → 见过心跳即 Some(0)（活写尾部无数据 ≙ 空档右移），
/// 否则 None（开流/断链失败左收）。
fn probe_consume(tapped: ProbeTap, file: String) -> Option<u32> {
    let saw_heartbeat = tapped.saw_heartbeat.clone();
    let mut src = ReplSource::new(Box::new(tapped), file, Filters::none(), None);
    loop {
        match src.next() {
            Ok(Some(ev)) => {
                if ev.timestamp > 0 {
                    return Some(ev.timestamp);
                }
            }
            Ok(None) | Err(_) => {
                return if saw_heartbeat.load(Ordering::Relaxed) {
                    Some(0)
                } else {
                    None
                };
            }
        }
    }
}

/// datetime 定位的单文件探测：从 4 拉流，**首个数据事件** ts 定档；
/// 首帧即心跳 = 已追平活写尾且区间无数据事件 → Some(0)（空档同权右移）。
/// 开流失败/流断且未见过心跳 → None（交 [`bisect_index`] 左收）。
///
/// 上界钉死（fix round I2，替换本函数旧注释的「repl 心跳在场时由读超时
/// 兜底」——该句是**反的**：heartbeat>0 时服务端每 d 有帧，2d+1s socket
/// 读超时永不触发；heartbeat=0 时连接根本没设读超时，空闲主库静默即
/// 无限阻塞）。真实上界 = 首帧落地（数据/心跳二分类）+ 工作线程隔离的
/// `recv_timeout(cap)` 墙钟硬顶：对「一个字节都没有」的形态唯一有效的
/// 就是后者。超顶场景（heartbeat=0 且主库全静默）残留一个阻塞读的工作
/// 线程——至多各占一条复制连接，随本 run 后续同 server-id 建连/进程退出
/// 而终结；用有界的连接冗余换「bisect 绝不被卡死」。
fn probe_first_ts(open: &mut Opener<'_>, file: &str, cap: Duration) -> Option<u32> {
    let stream = open(file, 4).ok()?;
    let saw_heartbeat = Arc::new(AtomicBool::new(false));
    let tapped = ProbeTap {
        inner: stream,
        saw_heartbeat,
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let worker_file = file.to_string();
    let _ = std::thread::spawn(move || {
        let _ = tx.send(probe_consume(tapped, worker_file));
    });
    match rx.recv_timeout(cap) {
        Ok(v) => v,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            tracing::warn!(
                "repl: first-ts probe of {file} exceeded {}s wall-clock cap \
                 — treating as probe failure (bisect goes left)",
                cap.as_secs()
            );
            None
        }
        // 工作线程 panic/静默丢发送（消费环无 panic 路径，理论不可达）同按失败。
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => None,
    }
}

/// 事件泵包裹件（T5 装配私有）：①Ctrl-C 旗标在**事件间隙**检查——置位
/// 即 `Ok(None)` 干净停泵（run_live 收尾链照常：末事务 drain→flush→
/// checkpoint）。FIX D 后中断**主门**已前置到 ReplSource 事件循环的帧顶
/// （空闲 master 恒心跳流上事件间隙永不到来，源级旗标随构造注入），
/// 本处检查留作解码后间隙的第二道兜底；②源侧传输错误快照进 sink
/// （`BinlogError` 抹平了变体，重连分类学从 [`ReplSource::transport_error`]
/// 取回）。
struct Pumper {
    src: ReplSource,
    sink: Arc<Mutex<Option<FailReport>>>,
    interrupt: Arc<AtomicBool>,
    /// 本尝试是否已向消费侧投递过 ≥1 个 RawEvent（FIX E 精细化的逐尝试
    /// 进度信号：`run_repl_with` 循环体每次尝试新建、随该次失败观测消费）。
    progressed: Arc<AtomicBool>,
}

impl EventSource for Pumper {
    fn next(&mut self) -> Result<Option<RawEvent>, BinlogError> {
        if self.interrupt.load(Ordering::Relaxed) {
            return Ok(None);
        }
        match self.src.next() {
            Err(e) => {
                if let Some(te) = self.src.transport_error() {
                    let (kind, cause) = classify_repl_failure(te);
                    *self.sink.lock().unwrap_or_else(|p| p.into_inner()) =
                        Some(FailReport { kind, cause });
                }
                Err(e)
            }
            Ok(Some(ev)) => {
                self.progressed.store(true, Ordering::Relaxed);
                Ok(Some(ev))
            }
            Ok(None) => Ok(None),
        }
    }
}

/// 可注入运行面（无服务器单测缝；生产参全部由 `run_repl` 就地薄接）：
/// `open` 只暴露**定位面参数**（uri/server-id/heartbeat 是 cfg 常量，
/// 归生产闭包），开流起点 (file, pos) 正是重连纪律的被钉对象。
pub(crate) struct ReplEnv<'a> {
    pub open: &'a mut Opener<'a>,
    pub wait: &'a mut dyn FnMut(Duration),
    pub now_ms: &'a mut dyn FnMut() -> u64,
    pub interrupt: Arc<AtomicBool>,
}

/// 重连起点（§4 铁律）：盘上 checkpoint 优先；无档/半截（本工具外的
/// 篡改等罕见态）回退**上一次尝试的起点**（内存里更远的位点绝不用——
/// 它可能含未落盘事件，方向性重复才可接受）。
fn cp_start_for_retry(cp_path: Option<&Path>, fb_file: &str, fb_pos: u32) -> (String, u32) {
    let Some(p) = cp_path else {
        return (fb_file.to_string(), fb_pos);
    };
    match std::fs::read(p) {
        Ok(raw) => serde_json::from_slice::<Checkpoint>(&raw)
            .map(|cp| (cp.file, cp.pos))
            .unwrap_or_else(|e| {
                tracing::warn!(
                    "repl: checkpoint {} unreadable mid-run ({e}) — retrying from previous attempt start {fb_file}:{fb_pos}",
                    p.display()
                );
                (fb_file.to_string(), fb_pos)
            }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (fb_file.to_string(), fb_pos),
        // fix round M2：非 NotFound（权限/IO 抖动/EISDIR 等）静默回退会掩盖
        // 盘上档位不可读的事实——与下方 serde 失败臂同响度留痕。
        Err(e) => {
            tracing::warn!(
                "repl: checkpoint {} unreadable mid-run ({e}) — retrying from previous attempt start {fb_file}:{fb_pos}",
                p.display()
            );
            (fb_file.to_string(), fb_pos)
        }
    }
}

/// 常规定位三态（now/file+pos/datetime；resume 命中时不走此处）。
fn locate_fresh(
    cfg: &Config,
    store: &mut SchemaStore,
    open: &mut Opener<'_>,
    filters: &Filters,
) -> Result<(String, u32), PipelineError> {
    match decide_locate(
        false,
        &cfg.start_file,
        cfg.start_pos,
        cfg.start_datetime.is_some(),
    ) {
        Locate::Datetime => {
            let want = filters.start_ts.ok_or_else(|| {
                PipelineError::Config("internal: datetime locate without start_ts".into())
            })?;
            let files: Vec<String> = store
                .list_binlogs()?
                .into_iter()
                .map(|(n, _, _)| n)
                .collect();
            let idx = bisect_index(files.len(), want, &mut |i| {
                probe_first_ts(open, &files[i], PROBE_FIRST_TS_CAP)
            })
            .ok_or_else(|| {
                PipelineError::Config(
                    "repl --start-datetime locate: SHOW BINARY LOGS returned no files (is log-bin enabled on the master?)".into(),
                )
            })?;
            Ok((files[idx].clone(), 4))
        }
        Locate::Now => {
            let (f, p) = store.master_status()?;
            Ok((f, p.max(4) as u32))
        }
        _ => Ok((cfg.start_file.clone(), cfg.start_pos.max(4))),
    }
}

/// repl 主装配（可测核）。生命周期纪律（简报钉）：
/// - **一个 Runner 跨重连复用**（seq/writer/水位队列连续），每尝试只换
///   装箱的源（T4 接口注记）；
/// - `run_live` 从不 `Writer::finish`——收尾（drain→flush→终档→finish）
///   归本函数的 epilogue（含终止 Err 路径）；
/// - pump Err 与 drain Err 并发时 pump 为主（T4 已序），drain 失败由
///   `run_live` 内部降为 operator warn 行。
pub(crate) fn run_repl_with(
    cfg: &Config,
    mut store: SchemaStore,
    env: &mut ReplEnv<'_>,
) -> Result<RunSummary, PipelineError> {
    let mut filters = Filters::from_config(cfg);
    let stop_wired = filters.stop.is_some() || filters.stop_ts.is_some();
    let resume_file = cfg.resume_file.clone();
    // P3 fix round（I1）：**读/写 checkpoint 路径分离**。写档恒为
    // {output-dir}/resume.json（缺目录 = 无写档，同 --to-stdout 形态）；
    // 读档 = 显式 --resume-file 优先。resume run 里消费的档是上一 run 的
    // 审计产物，按 §5「旧产物字节不可变」须原样保留——若续写它，首个事务
    // 水位就会把 run1 的 written_files 改写成 run2 清单：旧 manifest 蒸发，
    // 第二跳的 `read_verify(rf, rf.parent())` 对账随即失去审计意义。启动时以
    // 消费档的位点给新目录**播种**一份 fresh 档，此后 mid-run 水位、每次
    // 重连的 `cp_start_for_retry` 读取、epilogue 终档全部只认新目录路径。
    let dir_cp: Option<PathBuf> = cfg.output_dir.as_ref().map(|d| d.join("resume.json"));
    let cp_path: Option<PathBuf> = dir_cp.clone();

    // ── 定位：resume 优先（read_verify 对账），否则三态 ──
    // 对账目录 = checkpoint 的**所在目录**（written_files 描述的上一段产物
    // 与档共存一处；resume 的新产物按 §5 进新 --output-dir）。
    // FIX B 契约：硬错仅限 manifest 承诺而盘上缺失（Missing）与档损坏/
    // 畸形；盘上多出未登记实物（崩溃残骸）由 read_verify warn 放行。
    let mut is_resume_run = false;
    let (mut file, mut pos) = match &resume_file {
        Some(rf) => match checkpoint::read_verify(rf, rf.parent().unwrap_or(Path::new("."))) {
            Ok(cp) => {
                is_resume_run = true;
                // 播种写档（fix round I1）：新目录 fresh 档继承消费档的位点，
                // written_files 清空——那是 run2 自己的账，与 A 目录实物无关。
                // 同路径（用户故意把 resume-file 摆进新输出目录）跳过播种：
                // 该布局下「就地续写覆盖」是用户自己的选择，不算破坏他人审计。
                if let Some(wp) = cp_path.as_deref()
                    && wp != rf.as_path()
                {
                    let mut seed = cp.clone();
                    seed.written_files = Vec::new();
                    checkpoint::write_atomic(wp, &seed)?;
                }
                (cp.file, cp.pos)
            }
            Err(checkpoint::CpError::Io(ref e)) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!(
                    "repl: resume file {} not found — falling through to fresh locating",
                    rf.display()
                );
                locate_fresh(cfg, &mut store, env.open, &filters)?
            }
            Err(e) => {
                let wp_hint = match cp_path.as_deref() {
                    Some(wp) => format!(
                        "this run's own checkpoint would be written to {} (the consumed file stays untouched)",
                        wp.display()
                    ),
                    None => "this run has no on-disk checkpoint (--to-stdout shape)".to_string(),
                };
                return Err(PipelineError::Config(format!(
                    "repl resume check of {} failed: {e} — hard failure only when the checkpoint \
                     promises artifacts that are absent on disk, or the checkpoint itself is \
                     corrupt/malformed (untracked leftovers beside it are warned, not fatal); \
                     repl never appends to existing .sql artifacts; resume into a FRESH \
                     --output-dir (keep the consumed checkpoint beside the previous run's \
                     output — it is never rewritten; {wp_hint})",
                    rf.display()
                )));
            }
        },
        None => locate_fresh(cfg, &mut store, env.open, &filters)?,
    };
    filters.start = Some((file.clone(), pos));

    // ── 跨重连复用的装配状态（一份 Writer/Runner；§5 防覆盖闸永闭）──
    let writer = Writer::with_live(
        cfg.output_dir.clone().unwrap_or_default(),
        cfg.to_stdout,
        cfg.file_per_table,
        cfg.add_extra_info,
        cfg.time_zone,
        "to_sql".into(),
        false,
        true,
        true,
    );
    let mut runner = Runner::new(
        cfg,
        filters.clone(),
        store,
        DmlBuilder::new(SqlOpts::from_config(cfg)),
        Emitter::Sql(writer),
    );

    let mut tracker = FailureTracker::new();
    let mut attempts: u32 = 0;
    let mut reconnects: u32 = 0;
    let run_res: Result<(), PipelineError> = loop {
        attempts += 1;
        // 失败摘要：本轮的终止/重连分诊输入。None = 本轮无传输层失败。
        // 本轮是否「裸秒断」= open 成功且 0 事件投递（互踢签名，进
        // Disconnect 连发计数）；开流被拒恒 false（从未建立流，非秒断）。
        let opened = (env.open)(&file, pos);
        let mut bare_break = false;
        let report: Option<FailReport> = match opened {
            Ok(stream) => {
                let sink: Arc<Mutex<Option<FailReport>>> = Arc::new(Mutex::new(None));
                let progressed = Arc::new(AtomicBool::new(false));
                // FIX D：中断旗标直达解码环——空闲 master 恒心跳流上
                // Pumper 的事件间隙检查无间隙可看，帧顶检查把 Ctrl-C
                // 延迟钉在 ≤ 心跳周期（run_live 照常收尾：停泵→drain→
                // flush→checkpoint→exit 130 语义）。
                let src = ReplSource::new(
                    stream,
                    file.clone(),
                    filters.clone(),
                    Some(env.interrupt.clone()),
                );
                let pumper = Box::new(Pumper {
                    src,
                    sink: sink.clone(),
                    interrupt: env.interrupt.clone(),
                    progressed: progressed.clone(),
                });
                match runner.run_live(pumper, &file, cp_path.as_deref()) {
                    Ok(_) => {
                        bare_break = !progressed.load(Ordering::Relaxed);
                        if env.interrupt.load(Ordering::Relaxed) || stop_wired {
                            break Ok(()); // stop 命中 / Ctrl-C：优雅收尾出口①
                        }
                        // 生产源未达 stop 绝无 Ok(None)（T2 不变式）；测试流
                        // 自然耗尽同型——一律按断链进重连（spec §2 勘误-6①）。
                        Some(FailReport {
                            kind: FailKind::Disconnect,
                            cause: "stream ended without stop condition".into(),
                        })
                    }
                    Err(pe) => {
                        bare_break = !progressed.load(Ordering::Relaxed);
                        let mut rep = sink.lock().unwrap_or_else(|p| p.into_inner()).take();
                        if let Some(r) = rep.as_mut() {
                            // 传输错误的 BinlogError 包装文本并进 cause（保真
                            // 1236 双面判定所依的原串在 classify 已入）。
                            r.cause.push_str(&format!(" [{pe:#}]"));
                        }
                        match rep {
                            Some(r) => Some(r),
                            None => break Err(pe), // 解码/写盘级硬错 = 真坏数据
                        }
                    }
                }
            }
            Err(e) => {
                let (kind, cause) = classify_repl_failure(&e);
                Some(FailReport { kind, cause })
            }
        };
        let rep = report.expect("上面 match 的非重连臂均已 break，此际必为 Some");
        // ── 终止面（spec §6：立即非零退出 + 可操作信息）──
        match rep.kind {
            FailKind::Purged => {
                let text = if is_resume_run && attempts == 1 {
                    format!("{RESUME_GONE_PREFIX}{PURGED_HINT}")
                } else {
                    PURGED_HINT.to_string()
                };
                break Err(PipelineError::Config(format!(
                    "{text} (server: {})",
                    rep.cause
                )));
            }
            FailKind::Auth => {
                break Err(PipelineError::Config(format!(
                    "{AUTH_HINT} (server: {})",
                    rep.cause
                )));
            }
            FailKind::Priv => {
                break Err(PipelineError::Config(format!(
                    "{PRIV_HINT} (server: {})",
                    rep.cause
                )));
            }
            _ => {}
        }
        // ── 可重连面：同因秒断终止闸 → 退避 → checkpoint 起点重开 ──
        let streak = tracker.observe(rep.kind, (env.now_ms)(), bare_break);
        // 带 1236 特征的 server-id 冲突 3 连即终止（T5 原判，间隔窗口径
        // 不变）；终审 FIX E：真实互踢常是**无特征的干净强制断连**
        // （Disconnect）；FIX E 精细化（终审复评阻断缺陷修正）：Disconnect
        // 只计**连续零进度秒断**（bare_break = open 成功且 0 事件投递）——
        // 主库重启时间线（串流中断带进度 + 重启窗口内开流被拒）会把连发
        // 清零，走正常重连恢复；旧口径「同因 3 连即终止」在退避间隔恒
        // < 60s 窗下于第 3 次失败即误杀（旧注释「退避拉长后间隔必超窗」
        // 对封顶 37.5s 的退避不成立）。真互踢 = 3 连零事件秒断，仍终止。
        if matches!(rep.kind, FailKind::ServerIdConflict | FailKind::Disconnect)
            && streak >= FAST_FAIL_LIMIT
        {
            let (lead, trail) = if rep.kind == FailKind::ServerIdConflict {
                ("server-id conflict suspected", " (master kicks both)")
            } else {
                (
                    "repeated master-initiated disconnects — server-id conflict is the primary hypothesis",
                    " (a mutual kick usually presents as a clean forced shutdown without any 1236 feature; verify network and master `SHOW SLAVE HOSTS` before concluding otherwise)",
                )
            };
            break Err(PipelineError::Config(format!(
                "repl: {streak} consecutive same-cause disconnects within {SERVER_ID_GRACE_MS}ms \
                 of each other — {lead}: another slave shares --server-id {}{trail}. \
                 Fix the id and restart (last cause: {})",
                cfg.server_id.unwrap_or(0),
                rep.cause
            )));
        }
        (file, pos) = cp_start_for_retry(cp_path.as_deref(), &file, pos);
        reconnects += 1;
        let backoff = reconnect_backoff_secs(reconnects, jitter_unit());
        tracing::warn!("{}", reconnect_warn(reconnects, backoff, &rep.cause));
        (env.wait)(Duration::from_secs_f64(backoff));
        if env.interrupt.load(Ordering::Relaxed) {
            break Ok(()); // 退避途中 Ctrl-C：不再重连，就地收尾
        }
    };

    // ── epilogue（终止面 Err 路径同样过一遍：产物与终档完整，禁半成品）──
    let epilogue: Result<RunSummary, PipelineError> = (|| {
        // 终档 = 盘上最后水位（缺档回退本次定位起点）+ 刷新 written_files。
        if let Some(p) = cp_path.as_deref() {
            let base = std::fs::read(p)
                .ok()
                .and_then(|raw| serde_json::from_slice::<Checkpoint>(&raw).ok());
            let (f2, p2, ts) = match base {
                Some(cp) => (cp.file, cp.pos, cp.ts),
                None => (
                    file.clone(),
                    pos,
                    crate::output::datetime_str(
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_secs() as u32)
                            .unwrap_or(0),
                        cfg.time_zone,
                    ),
                ),
            };
            let cp = Checkpoint {
                file: f2,
                pos: p2,
                ts,
                written_files: runner.created_names(),
            };
            checkpoint::write_atomic(p, &cp)?;
        }
        let mut sum = runner.summary;
        sum.files = runner.finish_live()?;
        Ok(sum)
    })();

    let out = match (run_res, epilogue) {
        (Ok(()), e) => e,
        (Err(run), Ok(_)) => Err(run),
        (Err(run), Err(ep)) => {
            // pump/drain 并发时 pump 为主（简报钉）；收尾失败降为 operator warn。
            tracing::warn!("repl: finalize also failed after primary error: {ep:#}");
            Err(run)
        }
    };
    if let Err(ref _e) = out {
        tracing::debug!("repl run terminated with error (surfaced to main)");
    }
    if out.is_ok() && env.interrupt.load(Ordering::Relaxed) {
        REPL_INTERRUPT.store(true, Ordering::Relaxed); // main → exit(130)
    }
    out
}

/// repl 生产入口：ctrlc 桥 + 生产 [`ReplEnv`]（transport::open 直连）→
/// [`run_repl_with`] 装配核。uri/server-id 必填由 `validate_repl` 保证；
/// 心跳 `Some(d)` 同时驱动服务端 HEARTBEAT 与客户端读超时探活
/// （2d+1s 无字节即断，§6 死链探测，见 transport 注释）。
pub fn run_repl(cfg: &Config) -> Result<RunSummary, PipelineError> {
    let uri = cfg.uri.clone().ok_or_else(|| {
        PipelineError::Config("internal: repl requires --uri (validate_repl gates)".into())
    })?;
    let server_id = cfg.server_id.ok_or_else(|| {
        PipelineError::Config("internal: repl requires --server-id (validate_repl gates)".into())
    })?;
    let heartbeat =
        (cfg.heartbeat_secs > 0).then(|| Duration::from_secs(cfg.heartbeat_secs as u64));
    let interrupt = Arc::new(AtomicBool::new(false));
    if let Err(e) = ctrlc::set_handler({
        let i = interrupt.clone();
        move || i.store(true, Ordering::Relaxed)
    }) {
        // 处理器装不上（已被占/平台不支持）：Ctrl-C 回退为默认直杀，
        // 数据面仍有 checkpoint（水位在提交边界）兜底，但 130 收尾语义失效。
        tracing::warn!(
            "repl: SIGINT handler install failed ({e}) — Ctrl-C will not drain gracefully"
        );
    }
    let mut open = |file: &str, pos: u32| transport::open(&uri, server_id, file, pos, heartbeat);
    // 退避睡眠按 100ms 切片：Ctrl-C 在退避途中也即时响应（不再叠最长
    // 37.5s 的整睡——SIGINT 语义是「下一次检查点停」，睡死违背初衷）。
    let mut wait = {
        let i = interrupt.clone();
        move |d: Duration| {
            let t0 = Instant::now();
            while t0.elapsed() < d && !i.load(Ordering::Relaxed) {
                let rem = d - t0.elapsed().min(d);
                thread::sleep(rem.min(Duration::from_millis(100)));
            }
        }
    };
    let mut now_ms = || {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    };
    let store = open_store(cfg)?;
    let mut env = ReplEnv {
        open: &mut open,
        wait: &mut wait,
        now_ms: &mut now_ms,
        interrupt,
    };
    run_repl_with(cfg, store, &mut env)
}

#[cfg(test)]
mod repl_tests {
    //! P3 T5：`run_repl` 装配单测（简报 Step 1 红件）——假 `FrameStream`
    //! 注入（无服务器）钉：裸默认位点=now 哨兵、重连退避/抖动、同因 3
    //! 秒断终止、1236 双面分诊、终止文案逐字、resume 对账四臂、重连起点
    //! = checkpoint（绝不用内存更远位点）、Ctrl-C 旗标干净收尾。

    use std::path::{Path, PathBuf};
    use std::process;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use clap::Parser;

    use super::{
        AUTH_HINT, FailKind, Locate, PRIV_HINT, PURGED_HINT, RESUME_GONE_PREFIX, ReplEnv,
        SERVER_ID_GRACE_MS, bisect_index, classify_repl_failure, decide_locate,
        reconnect_backoff_secs, reconnect_warn, run_repl_with,
    };
    use crate::binlog::event::EventType;
    use crate::config::{Cli, Command, Config};
    use crate::metadata::store::SchemaStore;
    use crate::repl::checkpoint::{self, Checkpoint};
    use crate::repl::test_support::FakeStream;
    use crate::repl::transport::{Frame, FrameStream, ReplError};

    fn tmpdir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let d = std::env::temp_dir().join(format!(
            "my2sql-p3t5-asm-{}-{}-{}",
            tag,
            process::id(),
            nanos
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn schema_file(dir: &std::path::Path) -> PathBuf {
        let p = dir.join("schema.json");
        std::fs::write(
            &p,
            r#"{"version":1,"tables":[{"db":"t10","table":"a","cols":[{"name":"id","type_name":"int","unsigned":false}],"pk":["id"],"uks":[]}]}"#,
        )
        .unwrap();
        p
    }

    /// repl 子命令 Config（validate_repl 全过；输出 (dir, out_dir, cfg)）。
    fn repl_cfg(extra: &[&str]) -> (PathBuf, PathBuf, Config) {
        let dir = tmpdir("cfg");
        let schema = schema_file(&dir);
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let mut argv: Vec<String> = vec![
            "my2sql-rs".into(),
            "repl".into(),
            "--binlog-dir".into(),
            dir.join("binlog").to_str().unwrap().into(),
            "--uri".into(),
            "mysql://root@127.0.0.1:1".into(),
            "--server-id".into(),
            "77".into(),
            "--schema-file".into(),
            schema.to_str().unwrap().into(),
            "--output-dir".into(),
            out.to_str().unwrap().into(),
            "--threads".into(),
            "1".into(),
        ];
        argv.extend(extra.iter().map(|s| s.to_string()));
        let cli = Cli::try_parse_from(&argv).expect("repl cli parse");
        let Command::Repl(a) = cli.cmd else {
            panic!("repl expected")
        };
        let cfg = Config::validate_repl(a).expect("repl validate");
        (dir, out, cfg)
    }

    fn store_for(dir: &Path) -> SchemaStore {
        SchemaStore::offline(&schema_file(dir)).unwrap()
    }

    /// 裸假流终结件：open 记录参数后置中断旗标 → 泵在事件间隙看到旗标
    /// → 干净收尾（避免任何单测进重连循环）。
    fn stop_after(
        n: usize,
        opens: &Arc<Mutex<Vec<(String, u32)>>>,
        flag: &Arc<AtomicBool>,
        tail_msg: Option<&str>,
    ) -> impl FnMut(&str, u32) -> Result<Box<dyn FrameStream>, ReplError> {
        let opens = opens.clone();
        let flag = flag.clone();
        let tail_msg = tail_msg.map(|s| s.to_string());
        move |f: &str, p: u32| {
            let mut v = opens.lock().unwrap();
            v.push((f.to_string(), p));
            let call = v.len();
            drop(v);
            if call == n {
                flag.store(true, Ordering::Relaxed);
                Ok(Box::new(FakeStream::new(vec![])) as Box<dyn FrameStream>)
            } else {
                Ok(Box::new(FakeStream::with_tail(
                    vec![],
                    ReplError::Disconnect(
                        tail_msg
                            .clone()
                            .unwrap_or_else(|| "fake stream ended".to_string()),
                    ),
                )) as Box<dyn FrameStream>)
            }
        }
    }

    /// ①定位三态判定纯函数 + ②控制器裁定的「裸默认位点=now」哨兵：
    /// start_file 为空即 now，**无论 start_pos**（clap 默认 4 不得把
    /// 空文件名的直连路径带偏）；且 now 路径必走在线 store 的
    /// SHOW MASTER STATUS（离线 store 硬错、零次拉流）。
    #[test]
    fn bare_default_position_is_now_sentinel_regardless_of_start_pos() {
        assert_eq!(decide_locate(false, "", 4, false), Locate::Now);
        assert_eq!(decide_locate(false, "", 0, false), Locate::Now);
        assert_eq!(
            decide_locate(false, "mysql-bin.000001", 4, false),
            Locate::FilePos
        );
        assert_eq!(decide_locate(false, "", 4, true), Locate::Datetime);
        assert_eq!(
            decide_locate(false, "mysql-bin.000001", 4, true),
            Locate::Datetime,
            "datetime 在场即优先于 file+pos 直给"
        );
        assert_eq!(decide_locate(true, "", 0, false), Locate::Resume);
        assert_eq!(
            decide_locate(true, "mysql-bin.000001", 4, false),
            Locate::Resume,
            "resume 在场即最高优先"
        );

        // 管道面：--start-file ""（start_pos 保持 clap 默认 4）→ now →
        // 离线 store 无 SHOW MASTER STATUS → 硬错且从未开流。
        let (dir, _out, cfg) = repl_cfg(&["--start-file", "", "--start-pos", "4"]);
        assert_eq!(cfg.start_file, "");
        assert_eq!(cfg.start_pos, 4, "陷阱前提：clap 默认 4 在场");
        let opens: Arc<Mutex<Vec<(String, u32)>>> = Arc::new(Mutex::new(vec![]));
        let flag = Arc::new(AtomicBool::new(false));
        let mut opener = {
            let opens = opens.clone();
            move |f: &str, p: u32| {
                opens.lock().unwrap().push((f.to_string(), p));
                Ok(Box::new(FakeStream::new(vec![])) as Box<dyn FrameStream>)
            }
        };
        let mut wait = |_| {};
        let mut clock = || 0u64;
        let mut env = ReplEnv {
            open: &mut opener,
            wait: &mut wait,
            now_ms: &mut clock,
            interrupt: flag.clone(),
        };
        let e = run_repl_with(&cfg, store_for(&dir), &mut env).expect_err("offline → hard err");
        let s = format!("{e:#}");
        assert!(
            s.contains("offline"),
            "应报离线 store 不支持服务端命令，got: {s}"
        );
        assert!(
            opens.lock().unwrap().is_empty(),
            "now 定位失败前不得开流，got: {opens:?}"
        );

        // datetime → list_binlogs 同走在线 store（离线硬错、零次开流）。
        let (dir, _out, mut cfg) = repl_cfg(&["--start-file", ""]);
        cfg.start_datetime = Some(
            chrono::DateTime::parse_from_str("2026-01-01 00:00:00 +0000", "%Y-%m-%d %H:%M:%S %z")
                .unwrap(),
        );
        let opens2: Arc<Mutex<Vec<(String, u32)>>> = Arc::new(Mutex::new(vec![]));
        let mut opener2 = {
            let opens = opens2.clone();
            move |f: &str, p: u32| {
                opens.lock().unwrap().push((f.to_string(), p));
                Ok(Box::new(FakeStream::new(vec![])) as Box<dyn FrameStream>)
            }
        };
        let mut env2 = ReplEnv {
            open: &mut opener2,
            wait: &mut wait,
            now_ms: &mut clock,
            interrupt: flag.clone(),
        };
        let e = run_repl_with(&cfg, store_for(&dir), &mut env2).expect_err("offline → hard err");
        assert!(
            format!("{e:#}").contains("offline"),
            "datetime 定位应走 list_binlogs（在线专属）: {e:#}"
        );
        assert!(opens2.lock().unwrap().is_empty());
    }

    /// 指数退避：1s 起翻倍、封顶 30s、±25% 抖动（unit∈[0,1) 线性映射
    /// 到 [0.75,1.25) 乘子）。
    #[test]
    fn reconnect_backoff_caps_and_jitters() {
        // 中心无偏（unit=0.5 → 乘子 1.0）
        let mid: Vec<f64> = (1..=7u32).map(|k| reconnect_backoff_secs(k, 0.5)).collect();
        assert_eq!(mid, vec![1., 2., 4., 8., 16., 30., 30.]);
        for k in 1..=60u32 {
            let base = 2f64.powi((k - 1).min(5) as i32).min(30.0);
            let lo = reconnect_backoff_secs(k, 0.0);
            let hi = reconnect_backoff_secs(k, 0.999);
            assert!(
                (lo - 0.75 * base).abs() < 1e-6,
                "k={k} unit=0 应 0.75x base={base}，got {lo}"
            );
            assert!(
                hi < 1.25 * base + 1e-6 && hi > 1.24 * base - 1e-6,
                "k={k} unit→1 应逼近 1.25x base={base}，got {hi}"
            );
        }
        assert!(
            reconnect_backoff_secs(u32::MAX, 0.999) <= 37.5 + 1e-6,
            "封顶"
        );
    }

    /// 重连 warn 行逐字钉（简报口径）。
    #[test]
    fn reconnect_warn_line_format_pinned() {
        assert_eq!(
            reconnect_warn(3, 4.0, "link dropped"),
            "repl: reconnect #3 in 4.0s (cause: link dropped)"
        );
        assert_eq!(
            reconnect_warn(1, 0.75, "io: boom"),
            "repl: reconnect #1 in 0.8s (cause: io: boom)"
        );
    }

    /// MySQL 1236 双面（裁定）：同码不同因——server_id/server-uuid 文案
    /// → 可重连的 ServerIdConflict；其余 1236 → 终止面 Purged。
    #[test]
    fn repl_error_taxonomy_splits_1236_double_duty() {
        let (k, cause) = classify_repl_failure(&ReplError::Purged(
            "A slave with the same server_uuid/server_id as this slave has connected to the master"
                .into(),
        ));
        assert_eq!(k, FailKind::ServerIdConflict);
        assert!(cause.contains("server_id"), "cause 保真原文: {cause}");
        let (k, _) = classify_repl_failure(&ReplError::Purged(
            "Same MySQL server_id has connected to master".into(),
        ));
        assert_eq!(k, FailKind::ServerIdConflict);
        assert_eq!(
            classify_repl_failure(&ReplError::Purged(
                "client wants to read log that has been deleted".into()
            ))
            .0,
            FailKind::Purged
        );
        assert_eq!(
            classify_repl_failure(&ReplError::Auth("Access denied".into())).0,
            FailKind::Auth
        );
        assert_eq!(
            classify_repl_failure(&ReplError::MissingPriv("need REPLICATION".into())).0,
            FailKind::Priv
        );
        assert_eq!(
            classify_repl_failure(&ReplError::Disconnect("x".into())).0,
            FailKind::Disconnect
        );
        let io: ReplError = std::io::Error::other("local").into();
        assert_eq!(classify_repl_failure(&io).0, FailKind::Disconnect);
        assert_eq!(
            classify_repl_failure(&ReplError::Server {
                code: 1146,
                msg: "m".into()
            })
            .0,
            FailKind::Other
        );
        assert_eq!(
            classify_repl_failure(&ReplError::Protocol("p".into())).0,
            FailKind::Other
        );
    }

    /// 同因 3 连秒断（server-id 互踢形态）→ 终止报错；纯计数臂（异因/
    /// 宽间隔重置、FIX E 精细化的零进度口径）一并钉死。
    #[test]
    fn same_cause_fast_fail_three_terminates() {
        use super::FailureTracker;
        let mut t = FailureTracker::new();
        assert_eq!(t.observe(FailKind::ServerIdConflict, 1_000, true), 1);
        assert_eq!(t.observe(FailKind::ServerIdConflict, 2_000, true), 2);
        assert_eq!(
            t.observe(FailKind::Disconnect, 3_000, true),
            1,
            "换因即重置（裸秒断占 1 位）"
        );
        assert_eq!(t.observe(FailKind::ServerIdConflict, 4_000, true), 1);
        assert_eq!(
            t.observe(FailKind::ServerIdConflict, 4_000 + SERVER_ID_GRACE_MS, true),
            1,
            "宽间隔（≥grace）重置——主库重启级故障不误杀"
        );
        assert_eq!(t.observe(FailKind::ServerIdConflict, 5_000, true), 2);
        assert_eq!(t.observe(FailKind::ServerIdConflict, 6_000, true), 3);
        // FIX E 精细化：Disconnect 只计连续零进度秒断——非秒断（开流被拒/
        // 有进度投递）清零；主库重启的 3 连 refused（间隔全 < grace 窗）
        // 绝不触闸，真互踢的 3 连裸秒断仍在第 3 连到 3。
        let mut d = FailureTracker::new();
        assert_eq!(
            d.observe(FailKind::Disconnect, 0, false),
            0,
            "串流中断带进度（重启时间线 F1）不种连发"
        );
        for t_ms in [1_000u64, 3_000, 7_000] {
            assert_eq!(
                d.observe(FailKind::Disconnect, t_ms, false),
                0,
                "开流被拒非秒断（t={t_ms}）——重启窗口连发恒 0，间隔全 < 窗也不误杀"
            );
        }
        assert_eq!(
            d.observe(FailKind::Disconnect, 9_000, true),
            1,
            "清零后秒断重计数"
        );
        assert_eq!(d.observe(FailKind::Disconnect, 10_000, true), 2);
        assert_eq!(
            d.observe(FailKind::Disconnect, 11_000, false),
            0,
            "连发中断（有进度/被拒）即清零"
        );
        assert_eq!(d.observe(FailKind::Disconnect, 12_000, true), 1);
        assert_eq!(d.observe(FailKind::Disconnect, 13_000, true), 2);
        assert_eq!(
            d.observe(FailKind::Disconnect, 14_000, true),
            3,
            "3 连裸秒断到闸"
        );
        assert_eq!(
            d.observe(FailKind::Disconnect, 14_000 + SERVER_ID_GRACE_MS, true),
            1,
            "宽间隔（≥grace）同因重开一段"
        );

        // 装配面：假 transport 每次都以 server-id 1236 秒杀 → 第 3 次终止。
        let (_dir, _out, cfg) = repl_cfg(&["--start-file", "mysql-bin.000001"]);
        let opens: Arc<Mutex<Vec<(String, u32)>>> = Arc::new(Mutex::new(vec![]));
        let flag = Arc::new(AtomicBool::new(false));
        let mut opener = {
            let opens = opens.clone();
            move |f: &str, p: u32| {
                opens.lock().unwrap().push((f.to_string(), p));
                Ok(Box::new(FakeStream::with_tail(
                    vec![],
                    ReplError::Purged(
                        "A slave with the same server_uuid/server_id as this slave has connected"
                            .into(),
                    ),
                )) as Box<dyn FrameStream>)
            }
        };
        let mut waits: Vec<Duration> = vec![];
        let mut wait = |d: Duration| waits.push(d);
        let mut t_ms = 0u64;
        let mut clock = || {
            t_ms += 300;
            t_ms
        };
        let mut env = ReplEnv {
            open: &mut opener,
            wait: &mut wait,
            now_ms: &mut clock,
            interrupt: flag.clone(),
        };
        let e = run_repl_with(&cfg, store_for(&_dir), &mut env).expect_err("3 连同因秒断必须终止");
        let s = format!("{e:#}");
        assert!(
            s.contains("server-id"),
            "终止文案须点明 server-id 冲突，got: {s}"
        );
        assert_eq!(
            *opens.lock().unwrap(),
            vec![("mysql-bin.000001".to_string(), 4u32); 3],
            "恰 3 次开流"
        );
        assert_eq!(waits.len(), 2, "仅前两次失败后进退避");
        assert!(!flag.load(Ordering::Relaxed), "终止路径不走中断旗标");
    }

    /// 终审 FIX E 红件（互踢 = 裸断连形态），精细化后仍是**互踢钉**：
    /// 真实 server-id 冲突常不发 1236 特征包而是**干净强制断连**（主库踢
    /// 双方）——旧闸（864c4a9 前）只认 FailKind::ServerIdConflict（:835
    /// 修复前），此形态退避重试无限循环、永不报错。契约：同因 Disconnect
    /// 3 连**零进度秒断**（每次 open 成功且 0 事件投递——本测试的三连全部
    /// 满足，`FakeStream::with_tail(vec![], …)` 即开流即死）终止，文案点名
    /// server-id 冲突为主假设。FIX E 精细化后主库重启不误杀的保障已移交给
    /// **进度信号**（带事件投递的断流/开流被拒均清零连发，见
    /// `master_restart_timeline_not_fast_failed`），而非旧注释宣称的宽间隔
    /// 重置（退避封顶 37.5s < 60s 窗，对重启不成立）。假流在第 4 次 open
    /// 设逃逸门：修复前必然走到（expect 到 Ok → expect_err 红）；修复后
    /// 恰 3 开 2 退避，绝不触及第 4。
    #[test]
    fn bare_disconnect_streak_three_terminates_as_serverid_suspect() {
        let (_dir, _out, cfg) = repl_cfg(&["--start-file", "mysql-bin.000001"]);
        let opens: Arc<Mutex<Vec<(String, u32)>>> = Arc::new(Mutex::new(vec![]));
        let flag = Arc::new(AtomicBool::new(false));
        let mut opener = {
            let opens = opens.clone();
            let flag = flag.clone();
            move |f: &str, p: u32| {
                let mut v = opens.lock().unwrap();
                v.push((f.to_string(), p));
                let call = v.len();
                drop(v);
                if call >= 4 {
                    // 逃逸门（仅旧形态可及——其「永不终止」缺陷的证据位）
                    flag.store(true, Ordering::Relaxed);
                    return Ok(Box::new(FakeStream::new(vec![])) as Box<dyn FrameStream>);
                }
                Ok(Box::new(FakeStream::with_tail(
                    vec![],
                    ReplError::Disconnect(
                        "connection killed by master: forced shutdown of slave".into(),
                    ),
                )) as Box<dyn FrameStream>)
            }
        };
        let mut waits: Vec<Duration> = vec![];
        let mut wait = |d: Duration| waits.push(d);
        let mut t_ms = 0u64;
        let mut clock = || {
            t_ms += 300;
            t_ms
        };
        let mut env = ReplEnv {
            open: &mut opener,
            wait: &mut wait,
            now_ms: &mut clock,
            interrupt: flag.clone(),
        };
        let e = run_repl_with(&cfg, store_for(&_dir), &mut env)
            .expect_err("同因 3 连**裸**秒断必须终止（修复前形态无限循环永不报错）");
        let s = format!("{e:#}");
        assert!(
            s.contains("server-id"),
            "终止文案须点名 server-id 冲突为主假设，got: {s}"
        );
        assert_eq!(
            *opens.lock().unwrap(),
            vec![("mysql-bin.000001".to_string(), 4u32); 3],
            "恰 3 次开流，不得触及逃逸门"
        );
        assert_eq!(waits.len(), 2, "仅前两次失败后进退避");
    }

    /// 终审 FIX E **精细化**红→绿（合并阻断缺陷：主库重启恢复被快速终止闸
    /// 误杀）：重启时间线 = 尝试 1 串流中（FDE+XID 有事件投递）断流
    /// Disconnect（t=0）→ docker 重启窗口内尝试 2-4 开流即被拒
    /// （零进度但非「秒断」；失败-失败间隔 = 退避本身 1s/2s/4s，全
    /// < 60s 窗——旧注释「退避自然拉长后间隔必超窗」对封顶退避数学上
    /// 不成立）→ 尝试 5 主库回、置中断旗标干净收尾。旧闸口径（同因
    /// Disconnect 3 连即终止）在尝试 3 即误杀本时间线（对现码必红）；
    /// 新契约：Disconnect 只计**连续零进度秒断**（open 成功且 0 事件
    /// 投递），有进度的尝试或开流被拒一律清零 → 正常重连恢复、Ok 收尾。
    #[test]
    fn master_restart_timeline_not_fast_failed() {
        let (_dir, _out, cfg) = repl_cfg(&["--start-file", "mysql-bin.000001"]);
        let opens: Arc<Mutex<Vec<(String, u32)>>> = Arc::new(Mutex::new(vec![]));
        let flag = Arc::new(AtomicBool::new(false));
        let mut opener = {
            let opens = opens.clone();
            let flag = flag.clone();
            move |f: &str, p: u32| {
                let call = {
                    let mut v = opens.lock().unwrap();
                    v.push((f.to_string(), p));
                    v.len()
                };
                match call {
                    // 尝试 1：投递 ≥1 事件后断流（串中 kill，有进度——不得种下连发）
                    1 => Ok(Box::new(FakeStream::with_tail(
                        vec![
                            synth_frame(EventType::FORMAT_DESC, 1000, 116, &fde_body()),
                            synth_frame(EventType::XID, 1002, 143, &[7u8; 8]),
                        ],
                        ReplError::Disconnect("master restarted: forced shutdown of slave".into()),
                    )) as Box<dyn FrameStream>),
                    // 尝试 2-4：重启窗口 = 开流被拒（零进度但从未建立流，非秒断）
                    2..=4 => Err(ReplError::Io(std::io::Error::other(
                        "Connection refused (os error 111)",
                    ))),
                    // 尝试 5：主库回，起手置中断旗标 → 干净收尾出口
                    _ => {
                        flag.store(true, Ordering::Relaxed);
                        Ok(Box::new(FakeStream::new(vec![])) as Box<dyn FrameStream>)
                    }
                }
            }
        };
        let mut waits: Vec<Duration> = vec![];
        let mut wait = |d: Duration| waits.push(d);
        // observe 只在失败尝试发生：t = 0 / 1s / 3s / 7s（间隔全 < 60s 窗，
        // 复刻真实退避节奏——宽间隔重置在这里救不了场，被钉的是零进度口径）。
        let mut seq = [0u64, 1_000, 3_000, 7_000, 9_000, 12_000].into_iter();
        let mut clock = move || seq.next().unwrap_or(12_000);
        let mut env = ReplEnv {
            open: &mut opener,
            wait: &mut wait,
            now_ms: &mut clock,
            interrupt: flag.clone(),
        };
        let sum = run_repl_with(&cfg, store_for(&_dir), &mut env)
            .expect("主库重启级 refused 风暴必须走正常重连恢复（不得按互踢误杀）");
        assert_eq!(
            opens.lock().unwrap().len(),
            5,
            "4 次失败 + 第 5 次成功收尾（误杀形态到不了第 5 次开流）"
        );
        assert_eq!(waits.len(), 4, "每次失败后各一次退避");
        assert!(sum.files <= 1, "收尾产物完整（epilogue 照常过）: {sum:?}");
    }

    /// 终止面文案逐字（简报钉）：purge / auth / privilege；零重连零退避。
    #[test]
    fn terminal_error_texts_are_verbatim() {
        // purge（非 resume run）
        let (dir, _out, cfg) = repl_cfg(&["--start-file", "mysql-bin.000001"]);
        let opens: Arc<Mutex<Vec<(String, u32)>>> = Arc::new(Mutex::new(vec![]));
        let flag = Arc::new(AtomicBool::new(false));
        let mut opener = {
            let opens = opens.clone();
            move |f: &str, p: u32| {
                opens.lock().unwrap().push((f.to_string(), p));
                Err(ReplError::Purged(
                    "Could not find first log file name in binary log index file".into(),
                ))
            }
        };
        let mut wait = |_| {};
        let mut clock = || 0u64;
        let mut env = ReplEnv {
            open: &mut opener,
            wait: &mut wait,
            now_ms: &mut clock,
            interrupt: flag.clone(),
        };
        let e = run_repl_with(&cfg, store_for(&dir), &mut env).expect_err("purge → 终止");
        assert!(
            e.to_string().contains(PURGED_HINT),
            "须含逐字 purge 文案「{PURGED_HINT}」，got: {e}"
        );
        assert_eq!(opens.lock().unwrap().len(), 1, "终止面不重连");

        // auth（1045，fix round M1：专属可操作文案，不再与权限共用）
        let (dir, _out, cfg) = repl_cfg(&["--start-file", "mysql-bin.000001"]);
        let opens2: Arc<Mutex<Vec<(String, u32)>>> = Arc::new(Mutex::new(vec![]));
        let mut opener2 = {
            let opens = opens2.clone();
            move |f: &str, p: u32| {
                opens.lock().unwrap().push((f.to_string(), p));
                Err(ReplError::Auth("Access denied for user".into()))
            }
        };
        let mut env2 = ReplEnv {
            open: &mut opener2,
            wait: &mut wait,
            now_ms: &mut clock,
            interrupt: flag.clone(),
        };
        let e = run_repl_with(&cfg, store_for(&dir), &mut env2).expect_err("auth → 终止");
        let s = e.to_string();
        assert!(
            s.contains(AUTH_HINT),
            "须含逐字认证文案「{AUTH_HINT}」，got: {s}"
        );
        assert!(
            !s.contains(PRIV_HINT),
            "1045 文案不得再混入权限串误导排障方向，got: {s}"
        );
        assert_eq!(opens2.lock().unwrap().len(), 1);

        // 权限缺失（1227 家族）：PRIV_HINT 原文不动（live 组钉过同款）
        let (dir, _out, cfg) = repl_cfg(&["--start-file", "mysql-bin.000001"]);
        let opens3: Arc<Mutex<Vec<(String, u32)>>> = Arc::new(Mutex::new(vec![]));
        let mut opener3 = {
            let opens = opens3.clone();
            move |f: &str, p: u32| {
                opens.lock().unwrap().push((f.to_string(), p));
                Err(ReplError::MissingPriv(
                    "Access denied; you need (at least one of) the REPLICATION SLAVE privilege"
                        .into(),
                ))
            }
        };
        let mut env3 = ReplEnv {
            open: &mut opener3,
            wait: &mut wait,
            now_ms: &mut clock,
            interrupt: flag.clone(),
        };
        let e = run_repl_with(&cfg, store_for(&dir), &mut env3).expect_err("priv → 终止");
        assert!(
            e.to_string().contains(PRIV_HINT),
            "须含逐字权限文案「{PRIV_HINT}」，got: {e}"
        );
        assert_eq!(opens3.lock().unwrap().len(), 1);
    }

    /// resume 起点被主库 purge → 逐字 purge 文案前缀 "resume point is gone: "；
    /// 并钉 resume 位点确实成为开流起点（file/pos 来自 checkpoint）。
    #[test]
    fn resume_point_purged_gets_prefixed_text() {
        let (dir, out, cfg) = repl_cfg(&["--start-file", ""]);
        let rf = out.join("resume.json");
        std::fs::write(out.join("to_sql.1.sql"), b"x").unwrap();
        checkpoint::write_atomic(
            &rf,
            &Checkpoint {
                file: "mysql-bin.000007".into(),
                pos: 2222,
                ts: "2026-09-21_12:00:00".into(),
                written_files: vec!["to_sql.1.sql".into()],
            },
        )
        .unwrap();
        let mut cfg = cfg;
        cfg.resume_file = Some(rf.clone());
        let opens: Arc<Mutex<Vec<(String, u32)>>> = Arc::new(Mutex::new(vec![]));
        let flag = Arc::new(AtomicBool::new(false));
        let mut opener = {
            let opens = opens.clone();
            move |f: &str, p: u32| {
                opens.lock().unwrap().push((f.to_string(), p));
                Err(ReplError::Purged("no such log on master".into()))
            }
        };
        let mut wait = |_| {};
        let mut clock = || 0u64;
        let mut env = ReplEnv {
            open: &mut opener,
            wait: &mut wait,
            now_ms: &mut clock,
            interrupt: flag.clone(),
        };
        let e =
            run_repl_with(&cfg, store_for(&dir), &mut env).expect_err("resume 位点被 purge → 终止");
        let s = e.to_string();
        assert!(
            s.contains(&format!("{RESUME_GONE_PREFIX}{PURGED_HINT}")),
            "须含「{RESUME_GONE_PREFIX}{PURGED_HINT}」，got: {s}"
        );
        assert_eq!(
            *opens.lock().unwrap(),
            vec![("mysql-bin.000007".to_string(), 2222u32)],
            "resume 位点即开流起点"
        );
    }

    /// resume 启动自检四臂（CpError 全覆盖，含 Malformed）：BadJson/Missing/
    /// Malformed → 硬错且文案含文件名与新 --output-dir 指引；Io(NotFound) →
    /// 落空放行到常规定位。
    #[test]
    fn resume_checkpoint_reconciliation_arms() {
        // (a) BadJson
        let (dir, out, base) = repl_cfg(&["--start-file", ""]);
        let rf = out.join("resume.json");
        std::fs::write(&rf, b"{oops").unwrap();
        let mut cfg = base.clone();
        cfg.resume_file = Some(rf.clone());
        let opens: Arc<Mutex<Vec<(String, u32)>>> = Arc::new(Mutex::new(vec![]));
        let flag = Arc::new(AtomicBool::new(false));
        let mut opener = stop_after(1, &opens, &flag, None);
        let mut wait = |_| {};
        let mut clock = || 0u64;
        let mut env = ReplEnv {
            open: &mut opener,
            wait: &mut wait,
            now_ms: &mut clock,
            interrupt: flag.clone(),
        };
        let e = run_repl_with(&cfg, store_for(&dir), &mut env).expect_err("坏 JSON → 硬错");
        let s = e.to_string();
        assert!(s.contains("resume.json"), "文案含档名: {s}");
        assert!(s.contains("--output-dir"), "须给新输出目录指引: {s}");
        assert!(opens.lock().unwrap().is_empty(), "自检失败不得开流");

        // (b) Missing：登记了盘上不存在的产物
        checkpoint::write_atomic(
            &rf,
            &Checkpoint {
                file: "mysql-bin.000001".into(),
                pos: 4,
                ts: "t".into(),
                written_files: vec!["to_sql.9.sql".into()],
            },
        )
        .unwrap();
        let e = {
            let mut env = ReplEnv {
                open: &mut opener,
                wait: &mut wait,
                now_ms: &mut clock,
                interrupt: flag.clone(),
            };
            run_repl_with(&cfg, store_for(&dir), &mut env).expect_err("缺实物 → 硬错")
        };
        assert!(
            e.to_string().contains("to_sql.9.sql"),
            "Missing 含文件名: {e}"
        );

        // (c) Malformed：条目非单一文件名
        checkpoint::write_atomic(
            &rf,
            &Checkpoint {
                file: "mysql-bin.000001".into(),
                pos: 4,
                ts: "t".into(),
                written_files: vec!["../evil.sql".into()],
            },
        )
        .unwrap();
        let e = {
            let mut env = ReplEnv {
                open: &mut opener,
                wait: &mut wait,
                now_ms: &mut clock,
                interrupt: flag.clone(),
            };
            run_repl_with(&cfg, store_for(&dir), &mut env).expect_err("畸形条目 → 硬错")
        };
        let s = e.to_string();
        assert!(s.contains("../evil.sql"), "Malformed 含条目名: {s}");
        assert!(s.contains("--output-dir"), "硬错须给操作指引: {s}");

        // (d) NotFound → 落空到常规定位（validate 拦 resume+start 组合，
        // 这里构造直给位点验证 fall-through 后走 FilePos）。
        let (dir, _out, mut cfg) = repl_cfg(&["--start-file", "mysql-bin.000002"]);
        cfg.start_pos = 8;
        cfg.resume_file = Some(_out.join("nowhere.json"));
        let opens: Arc<Mutex<Vec<(String, u32)>>> = Arc::new(Mutex::new(vec![]));
        let flag = Arc::new(AtomicBool::new(false));
        let mut opener = stop_after(1, &opens, &flag, None);
        let mut env = ReplEnv {
            open: &mut opener,
            wait: &mut wait,
            now_ms: &mut clock,
            interrupt: flag.clone(),
        };
        run_repl_with(&cfg, store_for(&dir), &mut env).expect("NotFound → 常规定位可跑通");
        assert_eq!(
            *opens.lock().unwrap(),
            vec![("mysql-bin.000002".to_string(), 8u32)]
        );
    }

    /// 终审 FIX B 契约（resume 面）：上一 run 目录含 manifest 登记的
    /// `to_sql.1.sql` **加上**未登记的崩溃残骸（撕裂事务半块 `to_sql.9.sql`）
    /// → resume 启动自检 **放行续跑**（旧契约为多实物硬 Stale → 崩溃恢复
    /// 在最需要续跑时死锁 = 修复前红点）；manifest 承诺而盘上缺失仍硬错
    /// （上臂 (b) 钉）。
    #[test]
    fn resume_run_proceeds_with_untracked_extras() {
        let (dir, out, base) = repl_cfg(&["--start-file", ""]);
        let prev = dir.join("prev");
        std::fs::create_dir_all(&prev).unwrap();
        std::fs::write(prev.join("to_sql.1.sql"), b"run1-artifact").unwrap();
        std::fs::write(prev.join("to_sql.9.sql"), b"torn-trx-residue").unwrap();
        let rf = prev.join("resume.json");
        checkpoint::write_atomic(
            &rf,
            &Checkpoint {
                file: "mysql-bin.000001".into(),
                pos: 4,
                ts: "t".into(),
                written_files: vec!["to_sql.1.sql".into()],
            },
        )
        .unwrap();
        let mut cfg = base;
        cfg.resume_file = Some(rf.clone());
        let opens: Arc<Mutex<Vec<(String, u32)>>> = Arc::new(Mutex::new(vec![]));
        let flag = Arc::new(AtomicBool::new(false));
        let mut opener = stop_after(1, &opens, &flag, None);
        let mut wait = |_| {};
        let mut clock = || 0u64;
        let mut env = ReplEnv {
            open: &mut opener,
            wait: &mut wait,
            now_ms: &mut clock,
            interrupt: flag.clone(),
        };
        run_repl_with(&cfg, store_for(&dir), &mut env)
            .expect("未登记残骸只 warn，resume 必须续跑（修复前：硬 Stale 死锁）");
        assert_eq!(
            *opens.lock().unwrap(),
            vec![("mysql-bin.000001".to_string(), 4u32)],
            "resume 位点即开流起点（自检放行后照常消费档）"
        );
        // 消费档字节不变（I1 审计面不回归）：written_files 仍是 run1 的账
        let consumed: Checkpoint = serde_json::from_slice(&std::fs::read(&rf).unwrap()).unwrap();
        assert_eq!(consumed.written_files, vec!["to_sql.1.sql".to_string()]);
        assert!(
            out.join("resume.json").is_file(),
            "I1 播种/写档恒在新输出目录"
        );
    }

    /// I1（fix round）：resume run 的写档与读档分离——消费的 `--resume-file`
    /// 是上一 run 的审计产物，字节不可变（§5「旧产物字节不可变」+ 第二跳
    /// 仍可对其 `read_verify` 对账）；本 run 的水位/重连起点/终档全部落
    /// **新输出目录** `{output-dir}/resume.json`（启动即以消费档位点播种）。
    /// 修复前：首个事务水位即把 run1 档的 written_files 改写为 run2 清单
    /// ——旧 manifest 蒸发、第二跳对账失去审计意义（FIX B 契约下多实物仅
    /// warn，恰须靠档本身不可变守住 run1 的账）。
    #[test]
    fn resume_run_never_rewrites_consumed_checkpoint() {
        // 「run1 产物」手工落盘：A=out（to_sql.1.sql + 与其对账的 resume.json，
        // 位点 000003:456）——与 run1 epilogue 落档字节同型（同型已由
        // ctrlc/水位族钉，此处不重复起真 run）。
        let (dir, a_dir, base) = repl_cfg(&["--start-file", ""]);
        std::fs::write(a_dir.join("to_sql.1.sql"), b"old-run-artifact").unwrap();
        let rf = a_dir.join("resume.json");
        checkpoint::write_atomic(
            &rf,
            &Checkpoint {
                file: "mysql-bin.000003".into(),
                pos: 456,
                ts: "2026-09-21_12:00:00".into(),
                written_files: vec!["to_sql.1.sql".into()],
            },
        )
        .unwrap();
        let before = std::fs::read(&rf).unwrap();

        // run2：从 A 的档接续，产物与写档落进新目录 B。
        let b_dir = dir.join("out2");
        std::fs::create_dir_all(&b_dir).unwrap();
        let mut cfg = base;
        cfg.resume_file = Some(rf.clone());
        cfg.output_dir = Some(b_dir.clone());
        let opens: Arc<Mutex<Vec<(String, u32)>>> = Arc::new(Mutex::new(vec![]));
        let flag = Arc::new(AtomicBool::new(false));
        let mut opener = stop_after(1, &opens, &flag, None);
        let mut wait = |_| {};
        let mut clock = || 0u64;
        let mut env = ReplEnv {
            open: &mut opener,
            wait: &mut wait,
            now_ms: &mut clock,
            interrupt: flag.clone(),
        };
        run_repl_with(&cfg, store_for(&dir), &mut env).expect("resume run 正常收尾");
        assert_eq!(
            *opens.lock().unwrap(),
            vec![("mysql-bin.000003".to_string(), 456u32)],
            "读起点仍取自 --resume-file"
        );
        // 消费档字节不可变，旧实物不受碰。
        assert_eq!(
            std::fs::read(&rf).unwrap(),
            before,
            "consumed resume.json 必须字节不变"
        );
        assert_eq!(
            std::fs::read(a_dir.join("to_sql.1.sql")).unwrap(),
            b"old-run-artifact"
        );
        // 新目录自有终档：位点=resume 起点（本 run 无新水位），
        // written_files=run2 自己的空账（而非 A 清单的拷贝）。
        let raw = std::fs::read(b_dir.join("resume.json")).expect("run2 终档必须落在新输出目录");
        let cp: Checkpoint = serde_json::from_slice(&raw).unwrap();
        assert_eq!((cp.file.as_str(), cp.pos), ("mysql-bin.000003", 456));
        assert!(
            cp.written_files.is_empty(),
            "run2 账本不含 A 的产物: {:?}",
            cp.written_files
        );
        // 两跳自洽：对 A（run1 档+目录）与对 B（run2 档+目录）的双向对账都过
        // ——修复前 A 被 run2 epilogue 改写（written_files 蒸发）则第一行必红。
        checkpoint::read_verify(&rf, &a_dir).expect("A 的审计档须仍可整档复用（第二跳前提）");
        checkpoint::read_verify(&b_dir.join("resume.json"), &b_dir).expect("B 档/目录自洽");
    }

    /// I2（fix round）：空闲主库 + heartbeat 在场——流里只有心跳帧。
    /// 心跳被 ReplSource 内部消化（外部不可见），修复前探针自旋等「下一个
    /// 数据事件」：heartbeat>0 时服务端每 d 有帧、读超时 2d+1s 永不触发
    /// （旧注释「由读超时兜底」恰好说反）→ 无界挂起。修复后：首心跳即
    /// 判定「查询区间已到活写尾部且无数据事件」≙ 空档 ts=0（右移语义）。
    #[test]
    fn probe_first_ts_classifies_heartbeat_only_stream_as_tail() {
        let mut opener = |_: &str, _: u32| -> Result<Box<dyn FrameStream>, ReplError> {
            Ok(Box::new(FakeStream::new(vec![heartbeat_frame()])) as Box<dyn FrameStream>)
        };
        let got = super::probe_first_ts(&mut opener, "mysql-bin.000099", Duration::from_secs(5));
        assert_eq!(
            got,
            Some(0),
            "首心跳=活写尾部无数据事件，须即时落地而非等下一事件"
        );
    }

    fn heartbeat_frame() -> Frame {
        // 19B 公共头最小帧：kind=HEARTBEAT_LOG_EVENT(0x1b)、event_size=19、ts=0。
        let mut b = vec![0u8; 19];
        b[4] = EventType::HEARTBEAT;
        b[9..13].copy_from_slice(&19u32.to_le_bytes());
        Frame {
            bytes: b,
            binlog_hint: None,
        }
    }

    /// 数据事件在场形态（修复不吞既有语义）：FDE 源侧消化不产出 → 其后
    /// 首个真事件的 ts 即探测结果；尾随心跳不得抢先把结果改判成 Some(0)
    /// （先数据后心跳的到达序 = 判定序）。
    #[test]
    fn probe_first_ts_returns_first_data_event_ts() {
        let mut opener = |_: &str, _: u32| -> Result<Box<dyn FrameStream>, ReplError> {
            Ok(Box::new(FakeStream::new(vec![
                synth_frame(EventType::FORMAT_DESC, 1000, 116, &fde_body()),
                synth_frame(EventType::XID, 1002, 143, &[7u8; 8]),
                heartbeat_frame(),
            ])) as Box<dyn FrameStream>)
        };
        let got = super::probe_first_ts(&mut opener, "mysql-bin.000099", Duration::from_secs(5));
        assert_eq!(
            got,
            Some(1002),
            "首个数据事件 ts 定档（心跳在其后，不参与）"
        );
    }

    /// 墙钟硬顶钉死：一个字节都不发的静默流（heartbeat=0 空闲主库 + 连接
    /// 无读超时的实况形态）→ 探针必须在 cap 量级内返回 None 脱身。
    /// 修复前形态=在此永挂（循环版探针没有任何可触发检查点的输入）。
    #[test]
    fn probe_first_ts_is_hard_bounded_by_wall_clock() {
        struct SilentStream {
            silence: Duration,
            done: bool,
        }
        impl FrameStream for SilentStream {
            fn next_frame(&mut self) -> Result<Option<Frame>, ReplError> {
                if !self.done {
                    self.done = true;
                    std::thread::sleep(self.silence); // 模拟无字段的阻塞读
                }
                Ok(None)
            }
        }
        let cap = Duration::from_millis(80);
        let mut opener = move |_: &str, _: u32| -> Result<Box<dyn FrameStream>, ReplError> {
            Ok(Box::new(SilentStream {
                silence: Duration::from_secs(2),
                done: false,
            }) as Box<dyn FrameStream>)
        };
        let t0 = std::time::Instant::now();
        assert_eq!(
            super::probe_first_ts(&mut opener, "mysql-bin.000099", cap),
            None,
            "静默超时按探测失败（二分左收）"
        );
        assert!(
            t0.elapsed() < Duration::from_millis(1000),
            "硬顶须快败，got {:?}",
            t0.elapsed()
        );
    }

    /// 文件同构无 CRC 帧（19B 公共头 + 体；event_size 自洽）。
    fn synth_frame(kind: u8, ts: u32, log_pos: u32, body: &[u8]) -> Frame {
        let size = (19 + body.len()) as u32;
        let mut b = Vec::new();
        b.extend_from_slice(&ts.to_le_bytes());
        b.push(kind);
        b.extend_from_slice(&9u32.to_le_bytes()); // server_id
        b.extend_from_slice(&size.to_le_bytes());
        b.extend_from_slice(&log_pos.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes()); // flags
        b.extend_from_slice(body);
        Frame {
            bytes: b,
            binlog_hint: None,
        }
    }

    /// 最小合法 FDE 体（v4 / "8.0.46" / hdr_len 19 / alg=NONE，与
    /// src/repl/source.rs 测试族的 fde_body 同构——那边冻结面不可共享）。
    fn fde_body() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&4u16.to_le_bytes());
        let mut sv = [0u8; 50];
        sv[..6].copy_from_slice(b"8.0.46");
        b.extend_from_slice(&sv);
        b.extend_from_slice(&1600000000u32.to_le_bytes());
        b.push(19);
        b.extend_from_slice(&[27u8; 39]);
        b.push(0); // checksum alg = NONE
        b
    }

    /// §4 铁律：重连拉流起点 = checkpoint 位点，**绝不用内存中已读到的
    /// 更远位置**（预植盘上档即可证伪——内存里根本没有推进过）。
    #[test]
    fn reconnect_resumes_from_checkpoint_not_memory_position() {
        let (dir, out, cfg) = repl_cfg(&["--start-file", "mysql-bin.000001"]);
        let rf = out.join("resume.json");
        checkpoint::write_atomic(
            &rf,
            &Checkpoint {
                file: "mysql-bin.000007".into(),
                pos: 2222,
                ts: "2026-09-21_12:00:00".into(),
                written_files: vec![],
            },
        )
        .unwrap();
        let opens: Arc<Mutex<Vec<(String, u32)>>> = Arc::new(Mutex::new(vec![]));
        let flag = Arc::new(AtomicBool::new(false));
        let mut opener = stop_after(2, &opens, &flag, Some("boom"));
        let mut waits: Vec<Duration> = vec![];
        let mut wait = |d: Duration| waits.push(d);
        let mut t = 0u64;
        let mut clock = || {
            t += 1_000;
            t
        };
        let mut env = ReplEnv {
            open: &mut opener,
            wait: &mut wait,
            now_ms: &mut clock,
            interrupt: flag.clone(),
        };
        run_repl_with(&cfg, store_for(&dir), &mut env).expect("一次断链一次重连后干净收尾");
        assert_eq!(
            *opens.lock().unwrap(),
            vec![
                ("mysql-bin.000001".to_string(), 4u32),
                ("mysql-bin.000007".to_string(), 2222u32),
            ],
            "第二次开流必须落在盘上 checkpoint 位点"
        );
        assert_eq!(waits.len(), 1);
        // 收尾终档：水位无新推进时保持盘上旧值（file/pos），written_files
        // 刷新为本次实物清单（空——没有事件落盘）。
        let raw = std::fs::read(&rf).unwrap();
        let cp: Checkpoint = serde_json::from_slice(&raw).unwrap();
        assert_eq!((cp.file.as_str(), cp.pos), ("mysql-bin.000007", 2222));
        assert!(cp.written_files.is_empty());
    }

    /// Ctrl-C：旗标置位 → 泵在事件间隙停 → 收尾完整事务 + flush + 终档 +
    /// Writer::finish → Ok（run_repl 另置 REPL_INTERRUPT 静态，main 退 130）。
    #[test]
    fn ctrlc_flag_stops_pump_and_finalizes_checkpoint() {
        let (dir, out, cfg) = repl_cfg(&["--start-file", "mysql-bin.000001"]);
        let opens: Arc<Mutex<Vec<(String, u32)>>> = Arc::new(Mutex::new(vec![]));
        let flag = Arc::new(AtomicBool::new(true)); // 起手即已中断
        let mut opener = {
            let opens = opens.clone();
            move |f: &str, p: u32| {
                opens.lock().unwrap().push((f.to_string(), p));
                Ok(Box::new(FakeStream::new(vec![])) as Box<dyn FrameStream>)
            }
        };
        let mut wait = |_| {};
        let mut clock = || 0u64;
        let mut env = ReplEnv {
            open: &mut opener,
            wait: &mut wait,
            now_ms: &mut clock,
            interrupt: flag.clone(),
        };
        super::REPL_INTERRUPT.store(false, Ordering::Relaxed);
        let sum = run_repl_with(&cfg, store_for(&dir), &mut env).expect("中断 = 干净收尾 Ok");
        // 130 出口接线（main.rs 读同一静态）：Ok+中断 必置 REPL_INTERRUPT。
        // 其余测试并发只可能置 true、绝不假阴；本测试独占置 false 窗口。
        assert!(
            super::REPL_INTERRUPT.load(Ordering::Relaxed),
            "中断收尾后 main 须读到 130 旗标"
        );
        assert_eq!(sum.events, 0);
        assert_eq!(sum.files, 0);
        assert_eq!(opens.lock().unwrap().len(), 1, "中断后不再重连");
        // 终档落在默认路径 {output-dir}/resume.json：无水位时记本次定位起点。
        let raw = std::fs::read(out.join("resume.json")).expect("收尾必写终档");
        let cp: Checkpoint = serde_json::from_slice(&raw).unwrap();
        assert_eq!((cp.file.as_str(), cp.pos), ("mysql-bin.000001", 4));
        assert!(cp.written_files.is_empty());
    }

    /// 非传输层错误（帧解码硬错，transport_error()=None）= 真坏数据 →
    /// 直接终止，不进重连分类学（spec §6「坏事件=真坏数据」）。
    #[test]
    fn non_transport_pump_error_is_fatal_not_reconnected() {
        let (dir, _out, cfg) = repl_cfg(&["--start-file", "mysql-bin.000001"]);
        let bad = Frame {
            bytes: vec![0u8; 19], // 全零头：event_size=0 与帧长 19 不自洽
            binlog_hint: None,
        };
        let opens: Arc<Mutex<Vec<(String, u32)>>> = Arc::new(Mutex::new(vec![]));
        let flag = Arc::new(AtomicBool::new(false));
        let mut opener = {
            let opens = opens.clone();
            move |f: &str, p: u32| {
                opens.lock().unwrap().push((f.to_string(), p));
                Ok(Box::new(FakeStream::new(vec![bad.clone()])) as Box<dyn FrameStream>)
            }
        };
        let mut waits = 0usize;
        let mut wait = |_| waits += 1;
        let mut clock = || 0u64;
        let mut env = ReplEnv {
            open: &mut opener,
            wait: &mut wait,
            now_ms: &mut clock,
            interrupt: flag.clone(),
        };
        let e = run_repl_with(&cfg, store_for(&dir), &mut env).expect_err("解码硬错 → 终止");
        assert!(format!("{e:#}").contains("event_size"), "got: {e:#}");
        assert_eq!(opens.lock().unwrap().len(), 1, "不得重连");
        assert_eq!(waits, 0);
    }

    /// datetime 二分（上游 BinarySearchBinlogReplMode 口径）：探测失败/
    /// 首 ts>目标 → 向左收；ts==0（空文件）或 ts<=目标 → 候选右移；全不
    /// 命中回落最老档；空清单 None。
    #[test]
    fn datetime_bisect_picks_latest_file_not_after_target() {
        let ts = [10u32, 20, 30, 40, 50];
        let mut probe = |i: usize| Some(ts[i]);
        assert_eq!(bisect_index(5, 25, &mut probe), Some(1));
        assert_eq!(bisect_index(5, 100, &mut probe), Some(4));
        assert_eq!(
            bisect_index(5, 5, &mut probe),
            Some(0),
            "全大于目标 → 最老档"
        );
        assert_eq!(bisect_index(0, 25, &mut probe), None);
        let mut probe_fail_mid = |i: usize| if i == 2 { None } else { Some(ts[i]) };
        assert_eq!(
            bisect_index(5, 25, &mut probe_fail_mid),
            Some(1),
            "探测失败向左收"
        );
        let mut probe_zero = |i: usize| if i == 2 { Some(0) } else { Some(ts[i]) };
        assert_eq!(
            bisect_index(5, 25, &mut probe_zero),
            Some(2),
            "ts=0 视作可达右移"
        );
    }
}
