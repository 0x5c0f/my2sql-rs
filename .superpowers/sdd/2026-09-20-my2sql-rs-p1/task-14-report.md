# Task 14 报告：流水线装配 + output writer（端到端首交付）

状态：完成（收尾者复核后提交）。测试 236+3+4=243 全绿（1 ignored=真库 live）、
clippy -D warnings 干净、fmt 干净。step-0 修复已先行提交（5fed174）。

## 交付物

- 新建：`src/pipeline/order.rs`（Reorder）、`src/pipeline/worker.rs`
  （SqlGroup/Job/build_groups/worker_loop）、`src/output.rs`（Writer +
  path_for + datetime_str + extra-info 字节面）、`src/lib.rs`（薄库根）、
  `tests/e2e.rs`（4 个集成测试）。
- 改写：`src/main.rs`（占位 → 真实装配：Config::from_args → tracing init →
  run_to_sql → 摘要行 stdout）；`src/pipeline/mod.rs`（Runner/dispatcher：
  FileReader→filter→trx 机→编号→threads 分派→reorder→Writer）。
- 清理：11 个模块的 `#![allow(dead_code)]` 骨架豁免全部移除；
  `Config::dml_enabled` 删除（DML 过滤唯一入口 = `Filters::dml_ok`）。

## 简报符合性核对（收尾者逐项验证）

| 简报要求 | 实现 | 结论 |
|---|---|---|
| Reorder `push`/`pending`，HashMap 缓冲连续弹出 | order.rs:29-47，签名与简报一致；另有 `drain_remaining` 收尾防御（seq 断流时升序强制吐出不丢数） | ✅ |
| 反压 pending > 2×threads 阻塞 dispatcher | mod.rs:327-337：入队前 `reap()` 非阻塞清收，随后 `while pending > threads*2 { res_rx.recv() }` 阻塞收取；worker 只发 unbounded 结果通道，无死锁 | ✅ |
| SqlGroup 字段 | worker.rs:35-47：binlog/start_pos/end_pos/timestamp/db/table/trx_id/sqls 八字段与简报逐一对应（trx_id P1 透传、P2 消费） | ✅ |
| path_for `to_sql.{schema.table.}<N>.sql` | output.rs:51-58；N=binlog 数字后缀去前导零（对齐上游 `%d`，events.go:286-299 的 forward 命名族换为 spec 的 to_sql 前缀）；`--file-per-table` 决定 db.table 段有无 | ✅ |
| SET NAMES utf8mb4 文件头 | output.rs:36 `FILE_HEADER`，每新文件首建写入一次，e2e 逐字节钉死 | ✅ |
| threads=1 直通 + 与并行字节等价单测 | mod.rs:184 直通分支（无通道无线程，reorder 恒零滞留）；`e2e_single_thread_matches_parallel_byte_for_byte` 实跑两路径读文件比对 | ✅ |
| decode 错误策略（skip+count） | worker_loop（worker.rs:96-122）：错误 → 原子计数 + `tracing::error!` + 投**空批填洞**（否则 reorder 永挂）；dispatcher 侧 schema 缺失/无 tm 同口径计入 `RunSummary.errors`；threads=1 路径同策略内联计数（mod.rs:283-295） | ✅ |
| Config::validate Result 化调用点 | main 走 `Config::from_args`（内部 expect），e2e 走 `Cli::try_parse_from → Config::validate().expect()`（tests/e2e.rs:213-218），无遗漏旧签名调用点 | ✅ |
| threads clamp(1..=64) | mod.rs:138 `cfg.threads.clamp(1, 64)`；validate 另拦 0 | ✅ |

## TDD / 测试证据

`cargo test`：**236 passed; 0 failed; 1 ignored**（单元，含 order 5 + worker 2 +
output 5 新增）+ CLI 3 绿 + e2e 4 绿 = **243 总绿**。四个 e2e：

```
test e2e_pipeline_produces_expected_sql_bytes ... ok      // 4 事件×2 表×多行事务，全文件逐字节（头+extra-info+保序+位置）
test e2e_single_thread_matches_parallel_byte_for_byte ... ok // threads=1 vs 4 输出文件逐字节相等
test e2e_file_per_table_splits_by_table ... ok            // to_sql.t10.a.1.sql / to_sql.t10.b.1.sql 拆分与命名
test e2e_schema_miss_counts_errors_and_continues ... ok   // robust-continue：b 表 2 事件计错跳过、a 表 3 条照常出货、exit 非 Err
```

fixture 为合成 5.6 语义 binlog（无 CRC，镜像 file_reader 测试 Synth）+ 离线
schema JSON v1，全链路走真实 CLI 解析/校验/装配路径。

## extra-info 上游字节对照（前实施者声明「模板+下划线 datetime 字节平价」——收尾者独立复核：**成立**）

上游发射点 `reference/my2sql-go/base/events.go:322-326`
（`GetForwardRollbackContentLineWithExtra`）：

```go
"# datetime=%s database=%s table=%s binlog=%s startpos=%d stoppos=%d\n%s;\n"
```

本侧 `src/output.rs:145`：

```rust
"# datetime={} database={} table={} binlog={} startpos={} stoppos={}\n"
```

字段名、顺序、单空格、`\n` 收尾逐字节镜像（注意上游第 7 个 `%s` 是语句体，
注释行本身到 `\n` 为止——本侧语句逐句 `+"\n"` 写出，与上游
`Join(sqls,";\n")+";\n"` 字节等价）。

datetime 分量：上游 events.go:170
`GetDatetimeStr(int64(ev.Timestamp), 0, constvar.DATETIME_FORMAT_NOSPACE)`，
constvar.go:6 = `"2006-01-02_15:04:05"`——**下划线形**（带空格的
`DATETIME_FORMAT` 仅用于 CLI start/stop 输入解析 context.go:292/303；
`DATETIME_FORMAT_NOSPACE_FILE` 在 Go 代码中零引用，仅文件名场景预留常量）。
本侧 `datetime_str`（output.rs:44）`%Y-%m-%d_%H:%M:%S` 镜像下划线形。**平价
声明确认**。唯一有意分歧：上游 `time.Unix().Format()` 走**运行主机时区**
（funcs.go:105-107），本侧把 `--time-zone` 固定偏移施加于 unix 秒——输出
确定性、不依赖跑分机器 TZ（T15 裁判固定 TZ 即可复现等价）。

## 决策记录（本层定夺，已入代码注释）

1. **逐事件错误 = robust-continue（skip+count）**：上游同类错误多走
   `log.Fatalf` 全进程终止；本侧仅**文件级**损坏（checksum/截断，源层 Err）
   终止，单事件 decode/build 错误计数续跑，汇入摘要 `errors=` 字段。差分
   夹具均良构，T15 期望输出无分歧。
2. **schema 获取/缓存全部在 dispatcher**：`SchemaStore::get` 是 `&mut`
   天然单线程；新 table_id/tm 更替时取一次、`Arc<TableSchema>` 随 Job 下发，
   worker 零共享零连库。Align 对账每 (tm,schema) 对一次（strict 甄别 +
   Padded 去重告警）；每事件 SQL 内仍由 DmlBuilder::plan 消费对齐结果
   （T13 审定 API 不变，O(列数) 纯计算重复成本可忽略）。
3. **datetime 时区**：见上节——固定偏移替代主机 TZ，确定性优先。
4. **lib.rs 引入（架构偏差，控制器裁定采案 a）**：原 spec 为 bin-only crate；
   集成测试（tests/）无 lib 目标不可编译，e2e 需直呼 `run_to_sql`。
   `src/lib.rs` 为**薄模块根**（仅 6 行 `pub mod` 声明 + 层序注释，零逻辑），
   全部实现仍在原模块；`main.rs` 降为 22 行薄壳（from_args→run_to_sql→打印），
   `Cargo.toml` 未动（src/lib.rs+src/main.rs 自动发现，包名 my2sql-rs →
   外部名 `my2sql_rs`）。此后各层测试可上移集成层。已记 HANDOVER Task 14 节点。
5. **stdout 模式**：与文件模式统一发 SET NAMES 头 + extra-info（上游屏幕模式
   仅打语句）——偏差入 T15 白名单挂账。

## dead_code 豁免盘点（移除 11 处后，src/ 残留 4 处，全部有终态理由）

- `binlog/field_types.rs`（模块级）：完整类型码参考表，未消费码为 P2/穷举预留。
- `binlog/value.rs` `ColCtx::new`：测试便构（生产走完整字面量注入 tz）；T14
  后仍仅测试消费，豁免定为终态（本次收尾更新过期注释）。
- `binlog/proto.rs` `BitmapCursor.bit_width`：构造期契约字段，行重置归调用方。
- `binlog/json.rs` `Frame.header_size`：冗余调试字段，校验用局部变量完成。

rows→T14 接缝豁免（rows.rs/file_reader.rs/store.rs/sqlopen 等）已随生产者
接入全部移除；filter.rs 中 `Config::dml_enabled` 过期引用注释同步修正。

## 遗留/关注

- 真实 binlog 冒烟按简报 Step 3 推迟至 T15（docker-mysql 夹具）；本任务以
  e2e fixture 为准。
- bounded 作业队列容量 threads×2 与反压阈值 pending>2×threads 双保险并存：
  前者限在飞作业、后者限乱序滞留结果，语义独立均按简报/spec §5.1 落地。
- `drain_remaining` 触发 = seq 流有洞（worker 全体消亡等病理态），当前仅
  warn+强制吐出不丢数；P2 keep-trx/回滚配对将依赖 trx_id 透传字段。
