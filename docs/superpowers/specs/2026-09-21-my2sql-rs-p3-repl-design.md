# my2sql-rs P3 repl 模式设计（实时复制流 → SQL）

**Goal:** 新增第四子命令 `repl`：以 MySQL 从库协议（COM_REGISTER_SLAVE / COM_BINLOG_DUMP）连接主库，实时拉取 binlog 事件流，复用既有解码/SQL 生成/输出层，按事务边界流式产出 to-sql，支持安全位点 checkpoint、断点接续与自动重连。

**Architecture:** 复制通道采用方案 A —— `mysql` crate（已在树 28.0.2）的 `binlog` feature 提供连接/认证/`BinlogStream` 帧协议管道（勘误：TLS 需额外 feature 且 URI 参不透传，P3 不启用，见 §2 spike 实测-5）；事件字节从 `ReplSource`（实现既有 `pipeline::source::EventSource` trait）喂入，之后与 file 模式**共用同一条解码→过滤→SQL 生成→分组→落盘链路，零分叉**。解码层（`src/binlog/*`）保持 P2 终审冻结的零改动不变量。

**Tech Stack:** Rust std::thread（无 async）、`mysql = { version = "28.0.2", features = ["binlog"] }`、crossbeam-channel、serde_json（checkpoint 文件）、docker（e2e 真容器）。

**Spec 上游依据:** `reference/my2sql-go/`（只读裁判）：repl 入口 `main.go:29-30` → `base/repl.go:13`；file/pos 定位 `repl.go:47`；客户端过滤 `com.go:48-74`；ROTATE 跟文件 `com.go:41-46`；start-datetime 二分 `binlog_scan.go:219-236`。**本设计对上游 repl 是超集**（checkpoint/重连/心跳上游全无），故 repl 无裁判差分（§8 登记）。

## 0. 范围裁决（含 P2 挂账消费）

**做：** repl × to-sql 单形态；启动三定位（now / file+pos / datetime）；事务边界 checkpoint + `--resume-file` 接续；心跳探活 + 指数退避自动重连；stop 条件优雅收尾；5.6/5.7/8.0/8.4 真容器矩阵；P2 挂账两项：stats Err 路径 JSONL 头收口（HANDOVER:1188）+ `--dml`×stats 裁判维度（HANDOVER:1458）。

**不做（明确登记）：** flashback/stats×repl（用户裁决 2026-09-21：仅 to-sql 实时流）；GTID 自动定位（上游 repl 也仅 file/pos，`repl.go:47`；`gtid_mode=ON` 主库照常可用——我们只是按 file/pos 请求，不消费 GTID）；具名时区扩展（P4）；bench 判定工装（P4）；repl×Go 裁判差分（§8）。

## 1. CLI 面

```
my2sql-rs repl --uri mysql://repl:pw@host:3306 --server-id <N>
               [--resume-file <PATH>]
               [--start-file <F>] [--start-pos <P>] [--start-datetime <T>]
               [--stop-file <F>] [--stop-pos <P>] [--stop-datetime <T>]
               [--heartbeat-secs <S=30>]
               [输出/过滤参数与 to-sql 全同集：--output-dir/--file-per-table/
                --add-extra-info/--db/--table/--ignore-*/--threads ... ]
```

- **`Command::Repl(ReplArgs)`** 第四子命令；`ReplArgs` = `CommonArgs` + 文本输出面（复用 `SqlTextArgs`）+ repl 专属：`--uri`（必填）、`--server-id`（必填，无默认——server-id 冲突静默丢事件，拒绝代答）、`--resume-file`、`--heartbeat-secs`。
- **启动位点三态互斥**：①缺省 = 跑时 `SHOW MASTER STATUS` 取当前位点（只看新流量）；②file+pos 显式给（pos 缺省 4）；③`--start-datetime` = 二分 `SHOW BINARY LOGS` 定位候选文件后从文件头拉流、客户端逐事件过滤至 ts 达标（上游 `binlog_scan.go:219` 语义；被滤过的事件照常喂事务状态机与 checkpoint 位点推进，只不出 SQL）。与 `--resume-file`（文件存在且含合法位点）两两共存 → validate 硬错「位点来源歧义」。
- **stop 条件**：`--stop-file/--stop-pos/--stop-datetime` 到点 → 刷完已缓冲完整事务 → 写 checkpoint → exit 0（对齐 file 模式 stop 语义 `com.go` 客户端比较口径）。
- **validate 拒绝清单**（validate 期拒，不留运行中惊喜）：`--to-stdout` 与 `--resume-file` 并存（stdout 无法回滚重放段、checkpoint 无意义）；`--schema-dump` **允许**（与三形态同构收口的延续）。
- 表结构：`--uri` 双用——同一 DSN 既做复制连接也做元数据连接（内部两条独立连接，元数据走既有 `metadata::Store`）。认证失败/权限缺失错误映射见 §6。

## 2. 复制通道（方案 A + Task 0 spike 闸）

`mysql` crate `binlog` feature：`Conn::get_binlog_dump(BinlogRequest{file, pos, server_id})` → `BinlogStream`。心跳经 `register_slave`/dump flags 口径由 spike 定稿。**Spike（计划内 Task 0，半天，产物标 throwaway）**在真 8.0 容器上验证四件事，任一不通即触发 §9 降级预案：
1. caching_sha2 与 native 双认证过（复用 crate 既有路径）；
2. `BinlogStream` 交给我们的字节是**去掉 19 字节 packet 头与 ok 字节的事件体**（含 19 字节公共事件头），与我方 `RawEvent` 解析入口 `binlog::event` 逐级对齐——若 crate 已解析成 `mysql_common` 结构而非裸字节，则取其 `EventData`/原始 bytes 出口，仍以**我方解码器**为唯一解码权威（不许引入第二解码路径）；
3. ROTATE / HEARTBEAT / 伪 BINLOG 事件（`mysql-bin.000001 pos 4` 重启头）透传形态；
4. URI query 参数（如 `ssl-mode`）是否原样进 `Opts`（TLS 能力口径登记进 README）。

### Spike 实测（2026-09-21, mysql 28.0.2 @ 8.0.46，`examples/repl_spike.rs` throwaway）

**API 面勘误（以实测为准，本 spec 其余条目已按此口径）**：
- 入口实为 `Conn::get_binlog_stream(self, BinlogRequest) -> Result<BinlogStream>`（**无 `get_binlog_dump`**）；**消耗 Conn**（repl 连接与元数据连接必须物理两条，§1 既定成立且被强制）。
- `BinlogStream: Iterator<Item = mysql::Result<Event>>`；事件类型是 `mysql::binlog::events::Event`（mysql_common 0.37.3），**`SlicedEvent` 在本链路不存在**（brief 假设有误）。
- `BinlogRequest` 由 `mysql` 直接 re-export：`BinlogRequest::new(server_id).with_filename(Vec<u8>).with_pos(u64)`（pos 缺省 4）。
- 连接构造：`Conn::new(Opts::from_url(url)?)`（`Conn::from_url` 在 28.0.2 不存在）。
- server 侧自动 `SET @master_binlog_checksum='ALL'` + `COM_REGISTER_SLAVE`（`register_as_slave` 私有、`get_binlog_stream` 内置，无需也不能手动重复）。

**六问结论**：
1. **完整原始事件字节（19B 头+体+CRC）可及——无需 §9 降级**。crate 将事件拆存 `header`(19B, Copy, 可序列化) + `data()`(CRC 已剥) + `checksum()`(原 4B) + `footer()`；`Event::write(Version4, w)` 整事件重序列化（CRC32 重算、FDE 的 checksum-alg desc 字节还原）。**实测重建字节与容器内 binlog 文件 `dd`+`od` 逐字节相同**（TABLE_MAP/WRITE_ROWS/QUERY 均验证）。已知例外仅两类合情场景：fake rotate 系 dump 线程合成（文件中本不存在）、流首 FDE 的 `BINLOG_IN_USE` 标志与闭档后文件态不同（crate 的 `calc_checksum` 已按官方口径处理该位）。→ `src/repl/` 喂我方解码器就用 `Event::write` 产物，file 模式解码链零分叉。
2. **CRC32 剥离是 crate 做的**：`data()` 不含尾 4B，原值在 `checksum()`（`Option<[u8;4]>`，`footer().get_checksum_enabled()` 判开关）。但因结论 1 走 `Event::write` 重建**含 CRC 的完整文件同构字节**，repl/source.rs **不调 strip_checksum**，与我方 file 路径（含 CRC 校验）直接兼容。
3. **FDE 恒在流首，且每连接先跟一个 fake ROTATE**：mid-file 起点实测流首 = `[ROTATE(合成"连接事件"): ts=0, hdr_flags=0x20(ARTIFICIAL), **header log_pos=0**（非 4）, 无 CRC, payload name=请求文件, **payload position=请求起点**（fix 轮实测 292528；pos=4 请求实测 4）, FDE(合成帧, **头 log_pos 清 0**), 首个真事件(恰接请求 pos，实测 59438→+79B→59517；fix 轮复测 292528→+71B→292599 严丝合缝), …]`。**字段口径（fix 轮勘正）**：vendor `RotateEvent::is_fake()` 查的是 **payload position==0**——合成流首帧 payload=请求起点≠0，故 `is_fake()` 实测为 **false**（原勘误"is_fake()=true"系 payload 与 header 字段混染），判别只能靠 ARTIFICIAL+ts=0+**header** log_pos=0+流首位置。EOF 跨文件 ROTATE（流中 seq>0、带 CRC、size=47）**同为 dump 线程到文件尾追加的合成帧**（闭档后 000001 真实 180B、尾无 rotate 记录），头 log_pos 亦为 0、payload position=4——跟文件语义照常，但其 header log_pos 同样不得进位点链。脚注：capture 行首个真事件的 start_pos 列=0 非矛盾——其前一帧为合成帧（header log_pos=0，链无从衔接），起点算术以 request 行 pos + 磁盘对字节为准。从文件头 dump 时 FDE/PREVIOUS_GTIDS 为文件真字节（log_pos=126 正常）。**`--start-pos` 语义勘定（不变）**：位点链从 requested pos 起、合成两帧（fake rotate + mid-stream FDE）的 header log_pos **不得**用于 checkpoint 推进/链衔接；推进只用首个真事件之后各帧的 `next_log_pos`。
4. **heartbeat 请求面：BinlogRequest/BinlogDumpFlags 无入口**（flags 仅 NON_BLOCK/THROUGH_POSITION/THROUGH_GTID，非 GTID 请求只剩 NON_BLOCK）。实测退路**可用且优于预期**：`get_binlog_stream` 前在同一 Conn 上 `SET @master_heartbeat_period = <secs×1e9>`（会话级，服务端读回 2000000000 确认）。到流形态：`EventType::HEARTBEAT_EVENT`(0x1b)、ts=0、body=日志文件名（16B）、含 CRC；**header log_pos=非零活位点**（发送时刻主库当前 binlog 写位点；fix 轮实测 573729=当时 SHOW MASTER STATUS，原始实测 157 同）——**非**合成帧那种清 0，但心跳非数据事件，checkpoint 推进仍不消费其 log_pos（§4 事务边界口径不变）；crate 不透明吞，原样交付（EventData::HeartbeatEvent），客户端自决跳过。注：mysql_common 0.37.3 不支持 HEARTBEAT_LOG_EVENT_V2(0x29)，但本 crate 的 ComRegisterSlave 注册形态下 8.0.46 实发 v1(0x1b)——5.6/5.7 矩阵件复核。→ `--heartbeat-secs` 即该 SET + 「连续 2×间隔无事件判死链」定时器（§6 口径成立）。
5. **认证双过：caching_sha2 与 mysql_native_password（TCP）query+dump 全通**（replsha2/replnative 双用户实测）。**但 URI query 参不透传（§2-4 假设不成立）**：mysql 28.0.2 的 `Opts::from_url` 参数白名单（user/password/host/port/socket/db_name/prefer_socket/enable_cleartext_plugin/secure_auth/tcp_*/compress/stmt_cache_size/reset_connection）**无任何 ssl 项**，且**未知参数直接硬错 `Unknown URL parameter`**（`ssl-mode=PREFERRED` 实测被拒；ssl 需 crate feature native-tls/rustls + 程序化 SslOpts，默认 features 不含）。→ `--uri` 解析层自持 query 参白名单并自行剥离/映射，P3 不提供 TLS（README 差异清单如实登记，追加 23+ 一条）。
6. **断链两形态，分类必须都覆盖**：①**优雅终止**（docker restart，服务端发流终止包）→ 迭代器**直接 `None`、无 Err**（与「正常到 stop 条件」不可分——**任何 None 且未达 stop 一律按断链进重连**，不得当自然 EOF 收尾）；②**硬断**（docker kill，TCP 断）→ `Some(Err(Error::IoError("server disconnected")))` 一条，随后流中毒（crate 内部 conn=None，后续恒 `None`）。`mysql::Error::is_connectivity_error()` 可作 §6 重连/终止分诊钩子（IoError/DriverError/CodecError=true，MySqlError=false——1045/1227/1236 落 MySqlError 走终止面）。

**遗留登记**：spike 件 `examples/repl_spike.rs` 标 throwaway（Task 8 决定去留，`src/` 零引用已验）；本小节结论即 Task 1/2 字段形状依据。

## 3. 数据流与既有链路的接缝

```
ReplSource(EventSource) ──RawEvent──▶ pump（过滤/事务状态机/分发, 现 pipeline/mod.rs）
   │                                        │
   │ checkpoint 推进                        ▼
   │ (事务边界)                        既有 worker/SQL 生成/Reorder
   ▼                                        │
 resume.json (tmp+rename)                   ▼
                            流式输出层：每事务提交边界 flush（§4）
```

- `ReplSource::next()`：喂 `RawEvent{binlog_file, pos, kind, ts, ...}`（与 file 模式同型——file_reader 给什么字段语义，repl 就给什么，**extra-info 注释头逐字节同构**由 e2e 等价性证明，§7）。
- 文件模式与 repl 的唯一管道差异：**刷出时机**。file 模式在通道关闭统一落盘（P1 既有行为不动）；repl 在 `TrxStatus::Commit/Rollback` 边界把已完成事务组落盘（`--to-stdout` 逐语句即时 flush）。实现为 `Writer` 新增流式模式开关，不改 file 模式路径字节。
- `--threads` 在 repl：并行解码语义保留（事件无 seek，仅解码并行 + Reorder 保序），缺省同 file。

## 4. checkpoint / resume

- **内容**：`{"file":"mysql-bin.000007","pos":<已完整落盘事务的提交后位点>,"ts":"2026-09-21_15:04:05","written_files":["to_sql.7.sql", ...]}`（serde_json 单对象，tmp 写 + rename 原子替换；`written_files` 供 §5 防覆盖闸核对）。
- **推进点**：与 §3 事务边界 flush 同点——该事务 SQL 已 write 完成后才写 checkpoint。**语义 = 每事务至少一次（at-least-once per transaction）**：崩溃重放最多重复 checkpoint 之后的完整事务，绝不半途切开一条事务；输出目录使用者按 `written_files` 名单整文件处置。
- **resume 启动**：`--resume-file` 存在 → 校验 JSON 合法 + `written_files` 承诺的产物全部在盘（承诺而缺失 = 有人删了产物/档被外来篡改 → 硬错，拒绝猜测；盘上**多出**未登记实物 = at-least-once 崩溃的预期残骸（撕裂事务半块等）→ `warn` 放行，不死锁恢复——终审 FIX B 精确化，原文「不一致即硬错」对"多"侧 overstate 已纠正）；请求的 file 若已被主库 purge（1236 错误）→ 明确报「位点已失效，需人工指定新起点」，不自动跳最新（静默丢数据是重罪）。
- 重连后拉流起点 = checkpoint 位点（不用内存中「已读到的更远位置」——内存可能含未落盘事件）。

## 5. 输出文件命名与防覆盖

- 命名族沿用 `to_sql.{[schema.table.]}<N>.sql`，N = binlog 文件序号（去前导零，`output.rs:path_for` 现口径）→ ROTATE 跨文件天然新文件（上游 `com.go:41` 跟文件语义等价）。
- **不重写已存在文件**：无启动一次性预检；闸在**首个冲突目标的创建时刻**原子生效——写文件走 `create_new`（O_EXCL），盘上已存在同名文件即该次创建失败 → 硬错并报出该冲突名（提示换 `--output-dir`），逐次一个、race 安全（预检式方案在 TOCTOU 窗口下不可保证，实现口径以 O_EXCL 为准）。**repl 永不 append 既有文件**——接续产物永远新文件，旧产物字节不可变，这是 §4 at-least-once 语义能被人手消费的前提。
  - 边界：resume 起点在文件 N 中间 → 新 run 仍产出 `.{N}` 序号——写到该文件首次创建时与上一 run 留存的 `.{N}` 必冲突（硬错终止）→ 规范做法：resume 用新 `--output-dir`（错误信息里直接给这条指引）。

## 6. 心跳、重连与错误面

- **心跳**：`--heartbeat-secs`（缺省 30，0=禁用）→ 服务端 HEARTBEAT_LOG_EVENT；连续 2×间隔无任何事件 → 判定死链（TCP 半开由心跳兜底），走重连路径。心跳事件不产生 SQL、不推 checkpoint。
- **自动重连**：指数退避 1s → 30s 封顶 + 抖动，**无限次**；每次重连日志一行（`repl: reconnect #K in <backoff> at <file:pos>`），同一波故障日志限流。终止条件（不可恢复，立即非零退出并给可操作信息）：认证失败（1045）、缺 `REPLICATION SLAVE/CLIENT` 权限（1227 家族）、位点被 purge（1236）、server-id 冲突特征（表现为对端强制断连循环 → 连续 3 次同因秒断即终止报错，防无限互踢）。主库重启（container restart 级）走正常重连恢复。
- **解码错误策略**：沿用 `--on-error` 语义（repl 面该旗标存在且默认 skip-bad-event；帧协议保证事件完整性，坏事件=真坏数据，与 file 模式同闸）；**解码器不得 panic 红线不变**（repl 直通路径无 catch_unwind 保护的 threads=1 形态同守——事件解析在 worker 线程内 panic 即整 run Err，口径与 P2 一致）。
- Ctrl-C：收到 TERM/INT → 停止拉流 → 落当前完整事务 → checkpoint → exit 130 语义（文档口径）。**空闲期延迟上界 = 心跳周期**（终审 FIX D）：心跳被解码环内部消化，中断检查钉在帧循环顶门——写入静默的主库上，下一个心跳帧到流即停泵；`--heartbeat-secs 0` 同时禁用死链探测与空闲期即时停泵（此时 Ctrl-C 停摆直到有真事件，属旗标语义自负）。stop-datetime/stop-position 同口径：只随**数据事件**生效，空闲 master 上命中要等下一个事件到流（登记行为，不修）。

## 7. 测试策略（全真实件）

1. **等价性总闸（P3 的核心不变量）**：同容器建库灌混合 DML → 同时开 repl（start=库当前位点）与事后 file 模式跑同一 binlog 段 → **断言两路产出逐字节一致**（repl 侧以 `--stop-datetime` 优雅收尾后，取两路覆盖段的交集比对；flush 时机差异不改变文件内容字节）。5.6/5.7/8.0/8.4 矩阵各跑一次。
2. **resume 正确性**：流中 `kill -9` repl → 检查 checkpoint 与文件末事务对齐 → `--resume-file` 接续 → 两段产物合并后与不中断基准比对：已提交事务零丢失、重复仅允许整事务且可由 written_files 界定。
3. **重连**：`docker restart` mysqld → repl 端不退出、退避重连、从 checkpoint 续拉，最终产物与基准等价（同 1 的比对器）。
4. **位点三态**：now（只收新流量）/ file+pos 精确回放 / start-datetime 二分+过滤各一 e2e；stop 三态各一；歧义组合 validate 拒。
5. **心跳/死链**：idle 60s>2×heartbeat 不误断；拔网（`docker network disconnect`）→ 探活生效重连。
6. **协议 spike 件（Task 0）** 标 throwaway，结论回填本 spec §2 条目 2/3 的字节口径。
7. 单元层：checkpoint 文件原子性（tmp+rename 双形态红例）、written_files 校验闸、1236/1227/1045 错误映射、日志限流计数器。TDD 全程先红。
8. 回归面：file 模式三形态既有 296 测试 + P1/P2 差分全绿（repl 为纯增量）；`git diff main..HEAD -- src/binlog/` 必须为空（冻结不变量升格入门禁）。

## 8. 有意超越与差异登记（README 差异清单续 23 起）

- repl 无裁判差分（上游 repl：无优雅停止、`log.Fatalf` 即崩、syncer 泄漏、无 checkpoint/重连/心跳——不可作 oracle；以 §7-1 内部等价性替代，理由如实入 README）。
- checkpoint/resume/自动重连/心跳 = 对上游的功能超集（上游 `-mode repl` 断线即终）。
- server-id 强制显式（上游有默认值——冲突静默危险，我方拒代答）。
- resume 防覆盖闸（§5）为自设安全语义，上游无对应物。
- 上游 quirk 不继承清单：`repl.go:96` Fatalf 吞错误参数、start-pos 无 table-map 时 `tbMapPos=0`、RawData 丢弃、charset 硬编码 utf8——均采我方 file 模式既有正确行为。

## 9. 降级预案（spike 失败时）

若 `mysql` crate binlog feature 不满足 §2-2/§2-3（如强塞 `mysql_common` 解析结构、无原始字节出口）：改**方案 B 收敛版**——仅 COM_REGISTER_SLAVE/COM_BINLOG_DUMP/心跳 ack 三段自研（约 <400 行），复用 crate 完成握手认证后取其 OpaquePacket 读写环（spike 同场验证可达性）；若连该缝也无 → 全自研握手走 `mysql_common` 原语（成本 +2~3 天，届时重开裁决并勘误本 spec）。降级决定由计划 Task 0 的评审点做出，不留到实现中途。

## 10. 验收标准（DoD）

1. §7-1 等价性 e2e 在 8.0 绿 + 矩阵 4 版本 repl 件全绿（tsv 逐字入 docs/compat）。
2. resume（kill -9）与重连（容器重启）两专项测试绿；checkpoint 单测族绿。
3. `cargo test`/`clippy -D`/`fmt` 三门 + P1/P2 全量回归绿；`src/binlog/` 零 diff 门禁绿。
4. README（矩阵行、快速上手实跑例句、差异登记 23+）、HANDOVER（每任务节点 + P3 DoD 节 + 挂账消费）落账，计数逐字如实（禁虚账）。
5. reference/ 零改动；spec §2 字节口径经 Task 0 spike 实证回填（勘误随实现同批提交）。
