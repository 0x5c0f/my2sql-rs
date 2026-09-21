# my2sql-rs P3 repl 模式设计（实时复制流 → SQL）

**Goal:** 新增第四子命令 `repl`：以 MySQL 从库协议（COM_REGISTER_SLAVE / COM_BINLOG_DUMP）连接主库，实时拉取 binlog 事件流，复用既有解码/SQL 生成/输出层，按事务边界流式产出 to-sql，支持安全位点 checkpoint、断点接续与自动重连。

**Architecture:** 复制通道采用方案 A —— `mysql` crate（已在树 28.0.2）的 `binlog` feature 提供连接/认证/TLS/`BinlogStream` 帧协议管道；事件字节从 `ReplSource`（实现既有 `pipeline::source::EventSource` trait）喂入，之后与 file 模式**共用同一条解码→过滤→SQL 生成→分组→落盘链路，零分叉**。解码层（`src/binlog/*`）保持 P2 终审冻结的零改动不变量。

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
- **resume 启动**：`--resume-file` 存在 → 校验 JSON 合法 + `written_files` 与目录实际文件一致（不一致 = 上次跑崩在 rename 之前/有人删了产物 → 硬错，拒绝猜测）；请求的 file 若已被主库 purge（1236 错误）→ 明确报「位点已失效，需人工指定新起点」，不自动跳最新（静默丢数据是重罪）。
- 重连后拉流起点 = checkpoint 位点（不用内存中「已读到的更远位置」——内存可能含未落盘事件）。

## 5. 输出文件命名与防覆盖

- 命名族沿用 `to_sql.{[schema.table.]}<N>.sql`，N = binlog 文件序号（去前导零，`output.rs:path_for` 现口径）→ ROTATE 跨文件天然新文件（上游 `com.go:41` 跟文件语义等价）。
- **不重写已存在文件**：本次 run（含 resume run）将要创建的目标文件若已存在 → 启动即硬错并列出冲突名（提示换 `--output-dir`）。**repl 永不 append 既有文件**——接续产物永远新文件，旧产物字节不可变，这是 §4 at-least-once 语义能被人手消费的前提。
  - 边界：resume 起点在文件 N 中间 → 新 run 仍产出 `.{N}` 序号——与上一 run 的 `.{N}` 必冲突 → 规范做法：resume 用新 `--output-dir`（错误信息里直接给这条指引）。

## 6. 心跳、重连与错误面

- **心跳**：`--heartbeat-secs`（缺省 30，0=禁用）→ 服务端 HEARTBEAT_LOG_EVENT；连续 2×间隔无任何事件 → 判定死链（TCP 半开由心跳兜底），走重连路径。心跳事件不产生 SQL、不推 checkpoint。
- **自动重连**：指数退避 1s → 30s 封顶 + 抖动，**无限次**；每次重连日志一行（`repl: reconnect #K in <backoff> at <file:pos>`），同一波故障日志限流。终止条件（不可恢复，立即非零退出并给可操作信息）：认证失败（1045）、缺 `REPLICATION SLAVE/CLIENT` 权限（1227 家族）、位点被 purge（1236）、server-id 冲突特征（表现为对端强制断连循环 → 连续 3 次同因秒断即终止报错，防无限互踢）。主库重启（container restart 级）走正常重连恢复。
- **解码错误策略**：沿用 `--on-error` 语义（repl 面该旗标存在且默认 skip-bad-event；帧协议保证事件完整性，坏事件=真坏数据，与 file 模式同闸）；**解码器不得 panic 红线不变**（repl 直通路径无 catch_unwind 保护的 threads=1 形态同守——事件解析在 worker 线程内 panic 即整 run Err，口径与 P2 一致）。
- Ctrl-C：收到 TERM/INT → 停止拉流 → 落当前完整事务 → checkpoint → exit 130 语义（文档口径）。

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
