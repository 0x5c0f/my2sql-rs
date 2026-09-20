# Task 12 report — 事件源层（FileReader + Filters + 事务状态机）

分支 `feat/p1`，基线 HEAD `86b36f1`。两提交：
`1484cdd` fix: rows decode livelock guard + tm-shape hardening（step-0 携带项）
+ `227ba1f` feat: file event source, filters, trx state machine（功能 + FDE CRC）。

## TDD 证据

- **step-0 RED（1484cdd 前置）**：
  - `all_zero_present_bitmap_errors_instead_of_hanging`：修复前**挂死**
    （present==0 时 null bitmap 游标零推进死循环，timeout 强杀为证）；
    修复后 → `Err(InvalidData("rows image has zero present columns
    (livelock guard)"))`，GREEN。
  - `short_tm_arrays_error_not_panic`：修复前 panic
    `index out of bounds: the len is 1 but the index is 1`（rows.rs:236
    直索引 `tm.column_type[i]`/`column_meta[i]`）；修复后 `.get(i).ok_or(
    TooShort)`，GREEN。
- **功能 RED**（36 个新测试 + `todo!()` 骨架先行，file_reader/filter/source
  三模块）：RED 轮新测试全部以 `not yet implemented` panic 失败（≥19 个
  显式列出，其余同类），既有 154 测试保持绿。
- **GREEN 迭代记录**（非逻辑 RED，属 API/事实探测，备查）：
  - 编译错 2 轮：`table_map::parse_table_map` 路径未导入（E0433）、
    `EventHeader` 未导入（E0425）、`make` 签名按引用而调用按值——修至通过。
  - **`die()` 杀测试进程**：`from_config_maps_real_cli_fields` 首跑让
    整个测试二进制 exit 2——`Config::validate` 失败路径走
    `eprintln! + process::exit(2)`（config.rs:157-160/201），测试参数缺
    `--uri/--schema-file` 即死。补 `--uri mysql://x@y` 后绿。此坑已写入
    HANDOVER 遗留节（后续凡调 validate 的测试必带 schema 源）。
  - clippy 收尾 4 项：无意义 `SeekFrom::Current(0)`（删除，`Seek` 约束
    留给 T14）、`matches!(x, Some(_))`→`is_some()`、3 处单变体
    `match cli.cmd`→不可拒绝 `let Command::ToSql(args) = cli.cmd;`
    （edition-2024 let/irrefutable）。
- **全量三门**：`cargo test` → **191 passed; 0 failed; 1 ignored**
  （基线 154+2(step-0) → 本任务 +36 模块测试 +1 event.rs fixture 回归）；
  `cargo clippy --all-targets -- -D warnings` 干净；`cargo fmt --check` 干净。

## 真实 fixture 端到端（capture_8.0_minimal/mysql-bin.000003）

- 事件序列 [15,35,34,2,34,2,19,30,16] → 产出标签
  `["Gtid","Query(DDL)","Gtid","Query(BEGIN)","Rows(Write,true)","Xid"]`
  （FDE/PREVIOUS_GTIDS/TABLE_MAP 结构性消化不产出，镜像上游 default→continue）。
- start_pos 断言：WRITE_ROWS_V2 `start_pos=1020` = TABLE_MAP 自身起始、
  `end_pos=3358`；BEGIN(start 939)；tm 携带 `t9.probe`/26 列；`with_crc==true`
  由 FDE 实证探测；body 直喂 `decode_rows` 冒烟通过（接缝钉死）。
- 窗口：stop@2000 → 4 事件、末件 Query(BEGIN)（header 后 body 前断）；
  start@1020 → [BEGIN,Rows,Xid] 且 FDE/TM 仍被消费；时间窗 stop 用 `>=`
  等号排除断言通过。

## 裁定引证（重点 1/2/4/7）

1. **Gtid = marker only**：上游 com.go `CheckEventCondition` 对
   GTID_LOG_EVENT(33)/ANONYMOUS_GTID(34) 走 default→`C_reContinue`——
   完全忽略 GTID 内容（并行/过滤都不看）。本层 `RawKind::Gtid` 纯标记，
   状态机透明（`gtid_and_misc_are_state_transparent` 测试）；匿名 GTID
   在 fixture 序列里出现两次（34 前后夹 DDL/BEGIN），不影响 trx_id。
2. **Filters/pos 窗口 = end_pos 口径**：`CheckBinHeaderCondition`
   （com.go:163-224）用 `header.LogPos`（事件**结束**位点）构造
   `mysql.Position` 比较，名先字典序；`(name,end)<start → skip`（跨界
   包含）、`(name,end)>=stop → break`（等号排除）。`Filters::accept`
   的 db/table 取 config.rs 真实字段（`db: Vec<String>` 白名单、
   `ignore_db` 黑名单等），行为面镜像。start_pos 语义（rows=TM 起始）
   权威：file.go:197-198 `tbMapPos = h.LogPos - h.EventSize` + :214-215。
4. **时间窗**：com.go:63-74 `ts < start → continue`、`ts >= stop → break`
   （u32 unix 秒）；`Filters::from_config` 把 T1 的
   `DateTime<FixedOffset>` 对映射 `.timestamp().max(0) as u32`。
   fixture 时间窗测试钉死 `>=` 等号方向。
7. **跨文件 = MIRROR UPSTREAM（简报「+06d 跨文件」部分失真）**：
   - 上游 file 模式**默认单文件**：EOF 后续读下一文件仅当设置了
     stop-file 或 stop-datetime（file.go:74-85 `!IfSetStopParsPoint &&
     !IfSetStopDateTime → break`）。
   - 下一文件名 = 末段十进制序号 +1、`%06d` 最小宽度（funcs.go:98-103
     `GetNextBinlog`；999999→1000000，无截断/十六进制回绕）——helper
     `FileReader::next_binlog_name` 同款并测（000003→000004、
     000009→000010、999999→1000000、无序号/非数字尾→None）。
   - rotate 事件 url **从不切文件**：com.go:41-46 仅用 url 更新
     「当前文件名」标签供位点比较；且 rotate 事件本体在改名前已用旧名
     构造（file.go:214 早于 com.go:43）——`rotate_rename_timing_after_emit`
     测试复刻该时序。
   - 结论：本层 FileReader 恒单文件（EOF→`Ok(None)`），多文件续读编排
     归 T14（且须遵守「仅 stop 条件下续读」的上游语义，已写入 HANDOVER
     节点防 T14 顺手做默认续读）。

## 简报 vs 现实对照（本任务新增失真/偏差清单）

| # | 简报/假定 | 现实（权威） | 处置 |
|---|---|---|---|
| 1 | rows start_pos=行事件自身起始（可自然推断） | = 最近 TABLE_MAP 自身起始（file.go:197-198/214-215） | 镜像上游，fixture 断言 1020 |
| 2 | 「rotate/+06d 跨文件」暗示默认自动续读+rotate 切文件 | 默认单文件；+06d 仅在 stop 条件设置时用于推导下一名；rotate 只改标签名（file.go:74-85、funcs.go:98-103、com.go:41-46） | 单文件 + helper，T14 编排 |
| 3 | FDE 也走通用 crc32_ok | **实证**：真机 FDE 通用 crc32 恒假——mysqld 算 FDE CRC 时尚未置 LOG_EVENT_BINLOG_IN_USE_F，需把 header flags（字节 17..19）置零再算（log_pos 字节照常参与）；4 个 8.0.46 fixture 全中，暴力穷举唯一零覆盖窗口=flags。go-mysql 直接跳过 FDE 验证（parser.go:238-243 FDE 分支不碰 verify） | `fde_checksum_ok` 专函数 + `fixture_fde_needs_flags_zeroed_coverage` 回归（断言其**不过**通用 crc32_ok，防回归混淆） |
| 4 | DDL=独立事务应出 SQL | 上游 file 模式只把行事件送 SQL 生成（file.go:245-268），DDL Query 不出 SQL | 状态机保留 DDL 独立提交语义（简报绑定），是否出 SQL 归 T13 决策 |
| 5 | `--table` 收 db.table | 上游 -tables 是裸表名 "DONOT prefix with schema"（context.go:196） | 双形态兼容（含 `.` 精确、不含只比表名）——上游语义超集 |
| 6 | stop_pos 独立可用 | 上游 StopFilePos 仅随 -stop-file 生效（context.go:325-334） | 本层以 start_file 回退（本工具 CLI 契约，文档化） |
| 7 | （裁定 3 自身）stop 判定 header 后 body 前 | 上游实为 ParseEvent 之后才查（file.go:203 后） | 收紧为省 IO 优化，停止**语义**（end_pos+等号排除）不变，代码注释注明 |
| 8 | 上游有 checksum 校验（隐含） | my2sql-go **从不验证**，仅按 FDE 声明剥 4B | 本层逐事件强制验证 = 文档化的更严立场；人工 rotate（log_pos=0 无 CRC 尾）豁免 |

## 文件

- `src/binlog/file_reader.rs`（新建，~700 行含测试：20 测试 + Synth 构造器
  真实 log_pos 累计 + fixture e2e 5 组）
- `src/pipeline/source.rs`（新建：RawEvent/RawKind/EventSource/TrxStatus/
  TrxStateMachine + 7 测试）
- `src/pipeline/filter.rs`（新建：Filters + pos_cmp/entry_match/Dml + 9 测试，
  Cli::try_parse_from 走真实 CLI）
- `src/binlog/event.rs`（`fde_checksum_ok` + fixture 回归测试；step-0 的
  36/37 常量在 1484cdd）
- `src/binlog/rows.rs`（step-0：活锁守卫 + `.get()` 加固 + 2 RED→GREEN 测试）
- `src/config.rs`（`Config::validate` 放宽为 pub + 文档）；
  `src/binlog/mod.rs`、`src/pipeline/mod.rs`（接线）

## 顾虑 / 遗留

- **BinlogError 无 Io 变体**：读错误统一映射 `InvalidData("io: {e}")`，
  真 IO 故障（如 EIO）与格式损坏不可程序化区分；EOF 判定靠「0 字节=干净
  结束、1..18=UnexpectedEof」约定。若 T14/T15 需要重试语义，加 Io 变体
  是低成本改法（未做：扩 thiserror 面超出本任务授权）。
- `Config::validate` 的 die/exit 路径对库化（future `--dry-run`、测试）不
  友好——本任务以「测试必带 --uri」绕过，改 Result 返回属 T14/T16 重构候选。
- TRANSACTION_CONTEXT(36)/VIEW_CHANGE(37) 常量已定义但无消费（走
  `RawKind::Other` 产出）；上游对两者亦忽略。P2 flashback 若支持 TXA 再路由。
- `Filters::accept` 对无 tm 的行事件 fail-closed（拒），FileReader 对无 tm
  行事件硬错误——双层防御，语义已注释。
- 三新模块 `#![allow(dead_code)]` 保留至 T14 生产接线（bin crate 中 pub
  项无消费者即触发 dead_code，参照 T10/T11 先例）。
- MariaDB GTID（type 16 与 XID 冲突的 5.5+ 形态）不在 P1 矩阵：
  `handle_fde` 已对 server 串含 "mariadb" 显式报错（裁定 6 pre-5.0 同款
  立场），不会静默误解码。
- 注入式 stop 指令：本轮收到 harness 的 **compact/summary 请求**（会话中途
  注入的停机指令，已按规程压缩续作，未中断交付）；另有常规任务清单提醒，
  非注入。无改稿/隐藏行为的注入指令。
