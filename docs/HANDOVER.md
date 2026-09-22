# my2sql-rs 交接文档（HANDOVER）

> 每完成一个节点统一更新本文件。目标读者：接手项目的任何人（含未来子代理/人类协作者）。
> 设计权威：`docs/superpowers/specs/2026-09-20-my2sql-rust-design.md`；P1 执行计划：`docs/superpowers/plans/2026-09-20-my2sql-rs-p1.md`。

## 项目一句话

Rust 独立重写 MySQL binlog 解析工具（to-sql / flashback / stats / repl），
能力对齐 Go 版 my2sql 但 CLI 全新设计；`reference/my2sql-go/` 为行为参考与
差分测试裁判（不入库、勿改动）；repl 无 Go 裁判（上游不可作 oracle，
README 差异 25），以 repl==file 逐字节等价性为正确性总闸。

## 关键决策记录（不可回退项）

| # | 决策 | 理由 |
|---|---|---|
| D1 | 解码层全自研（方案 A），仅复用 mysql_common 协议原语 | 市面 Rust 库解码覆盖参差，静默错 SQL 不可接受 |
| D2 | 无 async：std::thread + crossbeam-channel | 批处理+单连接拉流，tokio 无收益 |
| D3 | 值保真总原则：列值全链路 `Vec<u8>`，datetime 族解码为字符串，不经 chrono/sqltypes 式转换 | 防 UTF-8/时区静默损坏 |
| D4 | ENUM/SET 输出序号（与 Go 裁判一致）；DECIMAL 精确解码（超越项，差分白名单） | 可比性+正确性 |
| D5 | 一期不做：DDL 回滚、--apply、MariaDB、8.0.1 default_metadata（检测到报错不猜） | YAGNI/安全 |
| D6 | 兼容矩阵含 MySQL 5.6/5.7/8.0/8.4（5.6 需显式 row 格式，报错给指引） | 用户验收要求 |
| D7 | 保序=reorder buffer 单线程刷出，反压阈值 2×threads | 替代上游自旋锁 |

## 当前进度

- **当前分支：`worktree-feat-p4b`（base `main@aba8293` = v0.4.0-p4a；
  P4b 计划 = `docs/superpowers/plans/2026-09-22-my2sql-rs-p4b-performance.md`，
  spec = `docs/superpowers/specs/2026-09-22-my2sql-rs-p4b-performance-design.md`；
  SDD 台账 `.superpowers/sdd/2026-09-22-my2sql-rs-p4b-performance/`）
- **P4b「性能面」计划 5 任务（T1 工装 / T2 profile / T4 搬运 并行 → T3 优化 →
  T5 合流）全部完成**：`tools/bench-ab.sh` A/B 判定工装（median+MAD，selftest 8 例
  + 恒等 A/A 冒烟）；`docs/bench/p4b-profile.md` profile 普查（threads 曲线 +
  假设判定 + 排序表）；mimalloc 全局分配器（glibc A/B 显著快 26.561% + musl 悬崖
  3.3→64.2 MiB/s 消账）；`src/repl/assembly.rs` 装配块纯搬运（move-only + 逐字节 +
  350 计数）；T5 落 `make bench-ab`/`make bench-profile` + `docs/bench/p4b.md`
  新权威基线（threads=8 median **127.59 MiB/s**，回归闸 vs 103.85 **+22.86% 更快
  GREEN**）+ 挂账 #7 P1→P2 复测**钉死不显著**（+2.812% < 0.5714s，N=5 不升级）+
  全量回归六闸逐字台账。§0 七条挂账全部销账/书面处置（见「P4b DoD 对账」与
  挂账清单 P4b 消费注）。**收口 pending = controller 的 merge/tag/push（本轮
  T5 lane 不并入 main、不打 tag、不 push）**。
- 分支（P4a 史）：`worktree-feat-p4a`（base `main@2149ce1`（= P3 终审后合入态
  `v0.3.0-p3`）；P4a 计划 =
  `docs/superpowers/plans/2026-09-22-my2sql-rs-p4a-quality-lanes.md`，
  spec = `docs/superpowers/specs/2026-09-22-my2sql-rs-p4a-quality-lanes-design.md`；
  SDD 台账 `.superpowers/sdd/2026-09-22-my2sql-rs-p4a-quality-lanes/progress.md`）
- **P4a 计划 5 任务（T1–T4 并行 + T5 串行合流）全部完成**：fuzz 正式接入
  （两靶 300s 闸 + 实抓 2 发解码器 panic，开闸条款 3 处 `checked_add` 闸）、
  影子库三段闸（8.0 spec 原形态全绿 + 5.7 REF-clone 锚裁定入册 + negcheck
  验钞机）、difftest P4A 三列形真机捕获（三形全 Go 支持，测试债销账）、
  5.6/5.7 idle 心跳 live 件（repl live 家族 11→13，帧形实测钉死）；
  T5 落 `make fuzz-min`/`make shadow-test` 直通行 + 全量回归逐字台账，
  DoD 对账见「P4a DoD 对账」节。全分支终审已做（With-fixes → FIX A–F
  单修复轮落地 `8130c6b`）+ 合流亲跑三闸全绿（见 T5 节点尾「合流亲跑
  节点」）；收口 = ff main + tag `v0.4.0-p4a` + push（本轮）。
- 前史（P3）：`worktree-feat+p3`（worktree `.qoder/worktrees/feat+p3`，base
  `main@0905368`（= feat/p2 终审后合入态 v0.2.0-p2）；
  P3 计划 = `docs/superpowers/plans/2026-09-21-my2sql-rs-p3-repl.md`，
  spec = `docs/superpowers/specs/2026-09-21-my2sql-rs-p3-repl-design.md`；
  SDD 台账 `.superpowers/sdd/2026-09-21-my2sql-rs-p3-repl/progress.md`）
- **P3 计划 9 任务（T0–T8）全部完成（T0–T7 节点 + T8 收口齐）**：repl
  （伪装 replica 拉流，to-sql 流式形态）交付——超集四件（checkpoint/resume、
  自动重连、心跳探活、resume 防覆盖闸）+ 等价性总闸（repl==file 逐字节）；
  live 套件 10/10、compat 矩阵 18/18，DoD 对账见「P3 DoD 对账」节。
  全分支终审已做（六件 A–F，两轮终审修复 + 合流亲跑 11/11·349 绿，
  见该节第 7 条），已合入 main（`2149ce1`，tag `v0.3.0-p3`）。
- 前史（P2）：`feat/p2`（worktree `.qoder/worktrees/feat+p2`，base
  `main@205512b`；P2 计划 = `docs/superpowers/plans/2026-09-21-my2sql-rs-p2-flashback-stats.md`，
  spec = `docs/superpowers/specs/2026-09-21-my2sql-rs-p2-flashback-stats-design.md`；
  SDD 台账 `.superpowers/sdd/2026-09-21-my2sql-rs-p2-flashback-stats/progress.md`）
- **P2 计划 9 任务全部完成（T1–T9 节点齐）**：flashback（记录原子逆序 +
  keep-trx + 完整性硬规则）与 stats（两报表 + JSONL + tick 对齐）交付，
  差分/矩阵/活库对账/bench 闸全走查，DoD 对账见「P2 DoD 对账」节。
  已合入 main（`0905368`，tag `v0.2.0-p2`）。
- 前史（P1）：`main` 分支（`feat/p1` 已于终审修复后合入并删除，merge commit `62d9f6a`）
- 里程碑：P1 计划 17 任务（执行序 1..15, 17, 16）——**全部完成；全分支终审
  已做，唯一一轮终审修复（#1 decimal panic 闸 / #2 SHOW 标识符转义 / #3 本文
  档口径修正）见 Task 16 节点「终审修复轮」与挂账清单**。
- **P1 DoD 对账**（证据= Task 16 节点 + docs/bench/p1.md + docs/compat/matrix.md）：
  ① `make difftest` exit 0（8.0 矩阵 21/21 绿 + 离线回放逐字节，T16 复跑）；
  ② file 模式双 schema 源一致（`--uri` 在线 vs `--schema-file` 离线，
  difftest 第 7 步 `diff -r` 硬闸 + T17 矩阵 8 用例全过）；
  ③ 吞吐基线 ≥500MB @ threads=8 ≥40MB/s：**实测 108.9 MB/s（2.7×）**，
  release 态 criterion（`cargo bench --bench decode`，数据 `tools/gen-bench-binlog.sh`）；
  ④ `tests/fuzz_seed/` 4 件坏事件语料（T16 三件 + 终审 #1 补 DECIMAL 满组
  溢出件），解码层断言 Err-不-panic（兼 P4 corpus）；
  ⑤ test/clippy/fmt 三门全绿 + README 落地（本节点收尾复跑）。
  前序：Task 17 全版本兼容矩阵（8 用例全绿，唯一真解码器 bug 修复 `b8f401c`）；
  Task 15 golden 差分基建（21/21 绿，工具链 + 白名单）；Task 14 端到端装配
  （275e1a6）；Task 13 sqlopen（ebf8a10）。

## 任务节点日志

（每任务完成追加一节：做了什么/关键接口/遗留项/对后续任务的影响）

### Task 1: 项目脚手架与 CLI 骨架

- 做了什么：`cargo init --name my2sql-rs`（crate 在仓库根，非嵌套目录）；按白名单引入依赖
  （clap/derive、crossbeam-channel、mysql 28 默认特性即 TLS off、mysql_common 0.38、crc32fast、
  simdutf8、serde/derive、serde_json、thiserror、chrono、tracing、tracing-subscriber）；
  `[profile.release] lto = true`；TDD 先写 `tests/cli.rs` 3 个失败测试（RED：3 failed）后实现转绿。
- 关键接口（Task 9-14 消费）：
  - `config::Cli` / `config::Command::ToSql(config::ToSqlArgs)`（clap derive，参数名/默认值与 brief 一致：
    `--start-pos` 默认 4、`--threads` 默认 available_parallelism、`--dml` 为 `Dml{Insert,Update,Delete}` 逗号分隔、空=全部）。
  - `config::Config::from_args() -> Config`：解析+校验，失败 `eprintln!` + `exit(2)`。校验规则：
    (1) `--uri`/`--schema-file` 至少其一；(2) threads>=1；(3) start/stop_datetime 成对时 start<stop
    （格式 `YYYY-MM-DD HH:MM:SS`，按 `--time-zone` 解释）；(4) stop_pos 与 start 同文件时 stop_pos>start_pos。
  - `Config::dml_enabled(Dml) -> bool`（空列表=全部启用）。
- 与 brief 的偏差/细化：
  - `Config` 不持有 `ToSqlArgs` 而是平铺字段，且 datetime/time_zone 已归一化为
    `DateTime<FixedOffset>` / `FixedOffset`（`--time-zone` 接受 `+HH:MM`、`UTC`、`SYSTEM`；
    不支持具名时区——chrono 无 chrono-tz 白名单外依赖，P3 若需要再议）。
  - 占位 main：打印 `to-sql: not wired yet (start_file=..., start_pos=..., threads=...)` + exit 1。
  - `src/{binlog,metadata,pipeline,sqlopen}/mod.rs` 为空壳（仅注释），main.rs 已声明四个 mod。
- 遗留：`Config` 与 `dml_enabled` 上有临时 `#[allow(dead_code)]`（骨架阶段字段无消费者），
  Task 12/13/14 接入后应移除。
- 注意：mysql 28 默认特性含 `flate2/zlib`（构建需系统 zlib/cmake，本机已验证可编译）。

### Task 2: event header 解码 + checksum

- 做了什么：新增 `src/binlog/error.rs`（`BinlogError`：TooShort / ChecksumMismatch /
  UnexpectedEof / InvalidData(String)，thiserror）与 `src/binlog/event.rs`
  （`EVENT_HEADER_SIZE=19`、`EventType(pub u8)` newtype + u8 关联常量表、`EventHeader`、
  `parse_header` / `strip_checksum` / `crc32_ok`）；`binlog/mod.rs` 声明两个子模块。
  TDD：先写 8 个单测（RED：6 failed，todo!() 断言），实现后全绿。行为对照
  go-mysql replication/event.go `EventHeader.Decode`（19 字节全小端；event_size<19 判
  InvalidData，防下游按错误长度切片——此校验为对照参考实现补充，brief 未列）。
- 关键接口（Task 3-12 消费）：
  - `parse_header(&[u8]) -> Result<EventHeader, BinlogError>`（<19 字节 → TooShort）。
  - `strip_checksum(&mut Vec<u8>, with_crc: bool)`（尾部剥 4 字节；<4 字节时不动）。
  - `crc32_ok(&[u8]) -> bool`（crc32fast 前 len-4 比对尾 4 字节小端；<4 字节 → false）。
  - `EventType` 常量：QUERY=2 STOP=3 ROTATE=4 FORMAT_DESC=15 XID=16 TABLE_MAP=19
    HEARTBEAT=27 GTID_LOG=33；rows 三代 V0=20/21/22、V1=23/24/25、V2=30/31/32、
    ANONYMOUS_GTID=34、PREVIOUS_GTIDS=35（T9 后校准补丁定稿，逐一对照
    log_event.h / const.go:54-87；原表 30/31/32=V1、34/35/36=V2、37=PREVIOUS_GTIDS、
    ANON=119、CREATE_DB=3 均为误标，已勘误）。
- 依赖调整（Task 1 遗留债务）：移除了直接依赖 `mysql_common 0.38.2`（src 内零引用，
  与 mysql 28 传递依赖的 0.37.3 双版本共存）。Task 3 若需 LNE 等原语，按 mysql 28
  对齐补 `mysql_common 0.37` 或直接手写，勿再引入 0.38。
- 与 brief 的偏差/细化：`tests/fixtures/events.rs` 仅存测试常量占位（bin-only crate 的
  tests/ 子目录不会被 cargo 编译为测试目标；单测常量按 brief 要求在 event.rs 的
  `#[cfg(test)] mod tests` 内自足）；Task 15 引入 lib 目标后可 `mod fixtures;` 复用。
- 遗留：event.rs / error.rs 顶部有临时 `#![allow(dead_code)]`（骨架阶段无生产消费者，
  沿 Task 1 惯例），Task 12+ 接入管道后移除。
- （T9 后校准补丁）ALARM A 勘误：原常量表 rows V1=30/31/32、V2=34/35/36、
  PREVIOUS_GTIDS=37、ANONYMOUS_GTID=119 全部错档（源自简报误抄，T2 当时未对照
  const.go 实码）。已改为 V0=20/21/22、V1=23/24/25、V2=30/31/32、GTID=33、
  ANON=34、PREVIOUS_GTIDS=35，CREATE_DB=3 更名 STOP=3；新增 fixture 回归
  `fixture_8_0_event_type_sequence`（真实 8.0 抓包走读，序列 15,35,34,2,34,2,19,30,16）。
  附带发现（未改，非本补丁范围）：FDE 的 crc32 校验区须排除公共头末 4B，
  现 `crc32_ok` 对 FDE 整件校验必失败（fixture 实测其余 8 件全过）——T12 接入
  逐事件校验时处理。

### Task 3: 协议原语（LNE / bitmap cursor）

- 做了什么：新增 `src/binlog/proto.rs`——`read_lne`（1/2/3/8 字节前缀 length-encoded
  整数）、`read_lns`（length-encoded 切片）、`bit_width`（`(n_cols+7)/8`，用 `div_ceil`）、
  `BitmapCursor`（NULL 位游标，bit==1 为 NULL，LSB first，连续推进不重置）。
  TDD：先写 11 个失败测试（RED：11 failed, todo!()），实现后全绿。未引入 mysql_common
  （0.38 直接依赖已在 T2 移除，LNE 约 60 行自写）。
- 关键接口（Task 4-10 消费）：
  - `read_lne(&[u8], &mut usize) -> Result<u64, BinlogError>`：0xFB 按数值 251 返回
    （NULL 哨兵语义由行解码层判定），0xFF → InvalidData，越界 → TooShort。
  - `read_lns<'a>(&'a [u8], &mut usize) -> Result<&'a [u8], BinlogError>`。
  - `BitmapCursor::new(bits, bit_width)` + `next_is_null(&mut self) -> bool`：
    游标按读取次数线性推进，**换行不重置**——行边界由调用方控制（每行读 n_cols 次）；
    位下标越出 bits 长度时返回 false 且仍推进（截断由上层校验）。
- 与 brief 的对齐：Step1 跨字节用例——8 列 bitmap `[0b10000001, 0b00000010]`，
  两行共 9 次读取断言列 0/列 7 为 NULL、第二行列 0 落入新字节 bit0。
- 遗留：proto.rs 顶部 `#![allow(dead_code)]`（T5+ 消费后移除）；`BitmapCursor.bit_width`
  字段当前仅 new() 存入、不参与位运算（预留行对齐）。

### Task 4: TABLE_MAP 解码（含 STRING meta 还原）

- 做了什么：新增 `src/binlog/table_map.rs`——`TableMapEvent`（table_id/schema/table/
  n_cols/column_type/column_meta/null_bits/charset，**无 signed 字段**，unsigned 判定
  归 Task 11 metadata 层）、`parse_table_map(body, with_crc)`、`real_string_type(tp, meta)`。
  TDD：先写 8 个失败测试（RED：8 failed, todo!()），实现后全绿。
- 关键接口（Task 10/11 消费）：`parse_table_map` 输入为剥掉 19B 公共头后的 body；
  `with_crc=true` 时先剥尾 4B CRC 再解字段；`real_string_type(0xFE, (0x06<<8)|2)=0x36`、
  `real_string_type(0xFE, (0xF6<<8)|6)=0xF6`（brief 写死的两对）。
- 与简报布局的偏差（按 ledger Ruling 允许 T15 真机校准，此处主动先对齐 go-mysql）：
  - **字段顺序**：column_types → metadata（LNE 总长+逐列 meta）→ null_bits bitmap，
    与简报正文「bitmap 在前、meta_len 2B LE 在后」相反；以 go-mysql
    `TableMapEvent.Decode` 为准（真实 MySQL 5.6+ 即此顺序，meta_len 为 LNE）。
  - 每列 meta 宽度表照抄 go-mysql `decodeMeta`：STRING/NEWDECIMAL 2B（高字节在前）、
    VAR_STRING/VARCHAR/BIT 2B LE、BLOB/FLOAT/DOUBLE/GEOMETRY/JSON/时间2族 1B、其余 0。
  - 字符集段按简报的 5.6.3 WL#6494 布局**严格**解析（审查修正：原宽松读法已废弃）：
    零尾随字节 → `Ok(空)`（pre-8.0/无 metadata 合法）；否则要求长度前缀
    （1B，=255 转义为后随 2B LE）+ **恰好 n_cols 条**完整 LNE 且无多余尾随字节，
    任一不满足 → `Err(InvalidData("unsupported table_map optional metadata / charset
    section: …"))`（D5：不支持的元数据必须报错、不得猜测）。~~MySQL 8.0
    `binlog_row_metadata=FULL` 的 TLV optional metadata（2B LE total_length +
    type/LNE长/值 条目）会被明确拒绝，完整 TLV 解析留 Task 15~~（T9 后校准补丁
    已实现 TLV 解析，见下）。
- （T9 后校准补丁）ALARM B 修复：真机 8.0.46（MINIMAL）null_bits 后为**无前导
  total_length** 的 TLV 流（fork `decodeOptionalMeta` row_event.go:241-321 即如此，
  原文档「2B LE total_length」表述系臆断，已删）。现两形态分流：legacy 精确覆盖
  → 原严格数组解析（5.6/5.7 既有测试全保留）；否则 TLV 迭代到 body 末尾——
  #1 signedness 消费不存（P1 裁定 unsigned 来自 schema DDL）、#2/#10 默认字符集
  奇数项校验、#3 column charset 存入 `charset`、#4/#5/#6/#7/#8/#9/#11 结构校验、
  未知 type 跳过（vendored fork default 臂 "Ignore for future extension"，
  my2sql-go 无任何 ignore 开关调用，参考工具有效行为即跳过）；截断一律报错。
  Fixture 回归 `fixture_8_0_table_map_parses_with_real_tlv_opt_meta` 钉死真机件
  （26 列 meta/null_bits 全量断言，TLV `01 01 40|02 0d …|07 01 00` 完整消费）。
- 遗留：table_map.rs 顶部 `#![allow(dead_code)]`；schema/table 用 `String::from_utf8`
  （非法 UTF-8 → InvalidData，真机库表名均为 UTF-8 可行）。

### Task 5: 定宽列值解码（整型 + BIT/YEAR/FLOAT/DOUBLE）+ ColumnValue

- 做了什么：新增 `src/binlog/int.rs`——`ColumnValue` 枚举（Null/Int/UInt/Double/
  Decimal/Str/Bytes/Json/Missing，derive Debug+Clone+PartialEq+Eq，后续任务按变体
  精确匹配）与 `decode_int` / `decode_float`；`binlog/mod.rs` 声明 `pub mod int`；
  dev-dependency 增加 `proptest 1.11`（简报 Step3 要求往返测试）。
  TDD：先写 16 个失败测试（RED：16 failed, todo!()），实现后 52/52 绿。
- 关键接口（Task 9/10 消费）：
  - `decode_int(buf, &mut pos, tp, unsigned, meta) -> Result<ColumnValue, BinlogError>`：
    TINY/SHORT/INT24/LONG/LONGLONG 均 LE，signed 用 `^(1<<(w*8-1))` 翻符号位；
    YEAR/BIT 走本入口特例。**签名比简报多一个 `meta: u16`**——BIT 的存储字节数
    只能从 meta 推（`nbits=(meta>>8)*8+(meta&0xFF)`，`n=ceil(nbits/8)`），简报签名
    与之矛盾，属必要偏差（go-mysql `decodeValue(tp, meta)` 同样带 meta）。
  - `decode_float(buf, &mut pos, tp) -> Result<ColumnValue, BinlogError>`：
    FLOAT=4B f32 / DOUBLE=8B f64 IEEE754 LE → `Double(最短往返十进制文本)`
    （Rust `f32/f64::to_string()` 即 shortest-roundtrip，非 from_utf8_lossy，
    与 Task 9 计划文本「FLOAT→4B f32文本(最短表示)」一致）。
- **YEAR 偏差（重要，已按权威纠正简报）**：简报称「官方 2 字节 LE 直存年份」。
  实测 docker mysql:8.0.46 / 5.7.44 真机 binlog（探针表 YEAR+BIT(9)+FLOAT+DOUBLE+
  TINY+MEDIUMINT，插入 2026/2000/0）：YEAR 为 **1 字节，值=年份−1900**（0x7E→2026），
  与 go-mysql `decodeValue`（n=1, +1900, 0 原样）完全一致；2 字节是 MariaDB 变体
  （D5 不支持 MariaDB）。已按实测+裁判实现：`[byte] → UInt(year+1900)`，0→UInt(0)。
  BIT 实测 2B **大端**（b'110000000'=384 → `01 80`），即简报「字节逆序按 LE 读」
  口径，断言 `UInt(0x0102)` 用例通过。
- 与 T4 接口衔接的实测观察（留给 T9/T15，不影响 T5）：8.0.17+/5.7.43+ 起
  FLOAT/DOUBLE 的 TABLE_MAP meta 从 0 变为 packlength(4/8)；真机 table_map 的
  charset 段字节形态需 T15 用真实 fixture 回归核对 T4 的严格解析。
- 类型常量：int.rs 内 `mod tp` 用官方值 FLOAT=4/DOUBLE=5（注意 table_map.rs 的
  私有 `mod tp` 把 FLOAT/DOUBLE 命名互换了——数值相同、meta 宽度一致，无功能
  影响，T4 已审不改，此处不触碰）。
- 遗留：int.rs 顶部 `#![allow(dead_code)]`（Task 9/10 消费后移除）。

### Task 6: 时间族解码（DATE2/DATETIME2/TIMESTAMP2/TIME2 → 字符串保真）

- 做了什么：新增 `src/binlog/time.rs`——`decode_date2` / `decode_datetime2` /
  `decode_timestamp2` / `decode_time2`，全部 `Result<String, BinlogError>`，
  位级算式逐项对照 go-mysql `row_event.go`（`decodeDatetime2`/`decodeTimestamp2`/
  `decodeTime2`/`timeFormat`/`MYSQL_TYPE_DATE` 分支），chrono-free（epoch→历法用
  Hinnant civil_from_days 手工换算）。TDD：先写 11 个失败测试（RED：11 failed,
  todo!()），实现后 60+3/63 全绿。
- 真机回验（T5 同款探针法）：docker mysql:8.0.46 与 mysql:5.7.44 ROW binlog 抓包
  表 t.t6（DATE / DATETIME(3)/(2) / TIMESTAMP(3) / TIME(0)/(3)/(6)，含零值、
  ±838:59:59、>24h、跨 1970 等），**全部测试 fixture 直接取自真机字节**，两版本
  逐字节一致。
- 关键接口（Task 9 消费）与读长：DATE 3B；DATETIME2 `5+(fsp+1)/2`B；TIMESTAMP2
  `4+(fsp+1)/2`B；TIME2 `3+(fsp+1)/2`B。fsp>6 → InvalidData；短缓冲 TooShort 且
  不污染 pos。
- **简报偏差（按权威纠正，证据= vendored go-mysql + 真机抓包）**：
  1. DATE2 实为 **3 字节小端位域 `year*512+month*32+day`**（[y:15][m:4][d:5]），
     零值 0x000000 → "0000-00-00"。简报「3B 大端、减 0x800000」两处均不成立
     （0x800000 是 TIME2 的 `TIMEF_INT_OFS`；DATE 无任何 bias；dispatch 猜测的
     0x8000 亦不成立——真机 '2020-07-16' = `f0 c8 0f` LE = 1034480 = 2020<<9|7<<5|16）。
  2. `decode_timestamp2` 签名增 `tz_offset_secs: i32`（控制器裁定；CLI
     --time-zone 的 FixedOffset 秒数在 T9/T14 传入，解码层不引 chrono）。
  3. **TIMESTAMP2 秒=0 输出 `1970-01-01 00:00:00`（+tz 偏移）**——此处是
     控制器裁定**覆盖** go-mysql（go 的 `formatZeroTime` 特例输出
     "0000-00-00 00:00:00"）。MySQL 服务器语义上 TIMESTAMP 0 即零值日期，
     若 T15 差分对 go-mysql 裁判产生差异，回看此条。
- 输出文本细节（与 go-mysql 完全一致）：DATETIME/TIMESTAMP fsp>0 恒输出 fsp 位
  小数（含零值日期）；TIME2 仅当小数非 0 才输出小数段；负 TIME2 的 fsp≤4 小数段
  反向存储需 `intPart++ / frac−=0x100^k` 补偿；TIME hour 为 10 bit（838 上限，
  无 24h 钳制）。
- 遗留：time.rs 顶部 `#![allow(dead_code)]`（Task 9 消费后移除）；DATE 型在
  table_map metadata 中 meta 恒 0（真机确认），T9 分发时 date2 无需 fsp 参数。

### Task 7: DECIMAL（MYSQL_TYPE_NEWDECIMAL）精确解码 → 保真文本

- 做了什么：新增 `src/binlog/decimal.rs`——`decode_decimal(buf, pos, precision,
  scale) -> Result<String, BinlogError>`，算式逐字镜像 go-mysql `row_event.go`
  `decodeDecimal` + `decodeDecimalDecompressValue`（`compressedBytes`
  `[0,1,1,2,2,3,3,4,4,4]`、首字节高 0x80 符号折叠、满 9 位大端 u32 组 ^mask、
  负值**逐字节/逐组 one's complement，无 +1**——真机抓包证实，非二补码）。
  纯 u32 组算术 + 字符串拼接，无新依赖、无浮点。TDD：先写 16 个失败测试
  （RED：16 failed, todo!()），实现后 76+3 全绿。
- 真机回验：docker mysql:8.0.46 + 5.7.44 ROW binlog 探针表 t.p_a..p_k
  （DECIMAL (10,2)/(3,0)/(9,0)/(5,5)/(30,10)/(65,30)/(20,20)/(1,0)/(4,2)/
  (18,6)/(2,1)，58 行含 ±99999999.99、全组边界、1e-30、纯小数、负零舍入等），
  两版本抓包**逐字节一致**；再以独立 Python 镜像核对「字节→文本 == 服务器
  存储值」58/58 全等后才作为 fixture（fixture=真机编码，非实现回声）。
- 接缝核验（控制器要求）：T4 `table_map.rs::decode_meta` 对 NEWDECIMAL 存
  2B **大端对**（高字节=precision），与 go-mysql 消费点 `prec=meta>>8;
  scale=meta&0xFF` 完全一致——**无缝隙**，T9 可直接按该式传参。
- 简报偏差（权威修正）：① tiny_int_len 表简报 9 项 vs 权威 10 项（公共部分
  数值相同，索引 9 恒不可达），取权威；② 简报「首组 ^0x80000000」不确——
  权威/真机是**首字节** ^0x80（符号位属于数值最高字节的最高位，无论首组是
  余数组还是满组，(9,0) 类 `bb 9a c9 ff` 即满组带符号位）；③ 余数组读取为
  **大端**逐字节 XOR，非 LE。
- 负零裁定核验（权威=真机+go-mysql）：**MySQL 不落盘 -0.00**——
  `CAST('-0.001' AS DECIMAL(10,2))` 抓包 = `80 00 00 00 00`，与 +0.00 全等
  （舍入到零丢符号）；理论上全取反的负零字节形态（`7f ff ff ff ff`）go-mysql
  输出 "-0.00"，本实现逐式一致（合成用例备案，真机不可达路径）。
- 输出保真：小数恒 scale 位（尾零保留 "0.00"/"...9900"）、整数无前导零
  （"0.05"，中间/末尾组零按 9 位补位 `0000000001`）、负值 '-' 前缀；
  scale=0 无小数点。TooShort 时 pos 不动；precision∉1..=65 或 scale>precision
  → InvalidData（go-mysql precision=0 会越界 panic，此处显式报错，与 T6
  fsp>6 同口径）。
- 遗留/挂账：decimal.rs 顶部 `#![allow(dead_code)]`（T9 消费后移除）；
  (65,30) 探针插入字面量小数 28 位→存储尾组 "…900" 形态保留于 fixture
  （验证的是编码往返，非任意值全覆盖）；T15 差分若启用 useDecimal=false
  路径，本实现即 go-mysql 字符串分支输出。

### Task 8: JSON 二进制 → 紧凑文本（json_binary_to_text）

- 做了什么：新增 `src/binlog/json.rs`——`json_binary_to_text(&[u8]) ->
  Result<String, BinlogError>`（无空格紧凑 JSON 文本，手工渲染、零新依赖，
  serde_json 未引入）。结构/算式逐字镜像 go-mysql
  `replication/json_binary.go`（decodeJsonBinary/decodeValue/
  decodeObjectOrArray/isInlineValue/decodeLiteral/decodeInt*/decodeOpaque/
  decodeDecimal/decodeTime/decodeDateTime/decodeVariableLength），
  容器解析用**显式栈 Frame 迭代**（非递归）。TDD：先写 17 个失败测试
  （RED：17 failed, todo!()），实现后 93+3 全绿。类型字节全覆盖：
  0x00/0x01 小/大对象、0x02/0x03 小/大数组、0x04 字面量(null/true/false)、
  0x05-0x0a 五档整型、0x0b double、0x0c 字符串、0x0f opaque
  （NEWDECIMAL→复用 T7 `decode_decimal`；DATE/TIME/DATETIME/TIMESTAMP→
  8B LE i64 位域解码；其余按严格 UTF-8 字符串）。
- 真机回验：docker mysql:8.0.46(t7cap)+5.7.44(t7cap57) ROW binlog 抓包
  j_probe/d_probe/t_probe 共 60+ 条 JSON 列原文（对象/数组/转义/UTF-8/整型
  边界/大对象 100KB/全部 opaque 类型/时间零值/28+6 组 double 位↔文本对），
  12 行公共样本两版本**逐字节一致**；double 渲染规则另以独立 Python 镜像
  对全部 bit↔text 对核验后才作 fixture。
- 简报偏差（权威=go-mysql+真机，均已按权威实现并实证）：
  1. 偏移/计数宽度非 u8/u16 二态，而是随类型字节 small=u16/large=u32；
     **KEY 长度恒 u16**（go:184，id13 100KB 大对象实抓证实）。
  2. 键序：简报称保持原文插入序——实为服务器按 **(字节长度, memcmp)** 重排，
     `{"b":1,"aa":"x"}` 真机输出 b 在前（现实优先，见 fixture id4）。
  3. double：简报称 12→"12"——真机恒带 `.0`（"12.0"）；科学计数当且仅当
     定位点 pp>15 或 pp<-14，格式 `d[.ddd]e{n}`（无 +、无零填充）；
     NaN/±Inf MySQL 不落盘，本实现报 InvalidData。
  4. opaque 布局简报未提长度字节：实为 `[内部类型1B][varint长][payload]`
     （id14/15/16 顶层 opaque 解码核实）。
  5. 嵌套容器子值在值偏移处**不含类型字节**（类型在值表项中，go:225）。
- 加固偏差（相对 go-mysql，均为 brief 硬性要求）：① 显式栈+深度上限
  `MAX_DEPTH=100`（超限 InvalidData，go 无上限递归）；② 全量边界校验，
  任何截断/畸形输入 → TooShort/InvalidData，**无 panic 路径**（go 的
  decodeDecimal 无守卫取下标、varint 截断等已封堵；size>data_len、
  header>size、key_offset<header_size、key 区越界均显式拒绝）；③ 严格
  UTF-8：键与字符串非法字节 → InvalidData（go 用 hack.String lossy）；
  ④ 空输入 → `Ok("")`（与 go decodeJsonBinary:76 一致，brief 未提，
  从权威）。
  **终审勘误（决策日志续笔）**：上面「decodeDecimal 无守卫取下标…已封堵」
  在写就时只覆盖了 json 复用路径的截断面——**残余洞**：被 T8/T12 复用的
  `decode_decimal` 自身，损坏 4B 满组 XOR 还原出 ≥10 位 u32（如
  0xFFFFFFFF^0x80000000）时组内左补零 `9 − t.len()` 下溢，debug
  （subtract overflow）/release（repeat capacity）双 panic。终审（review
  569d74e..a97f010）发现，fix 8bfe73a 封堵：满组值 > 999999999 →
  InvalidData（DECIMAL(19,9) 敌意字节 `81 00 00 00 01 FF FF FF FF`，
  单测双点位 + threads=1 e2e 直通泵回归 + `tests/fuzz_seed/` 第 4 件
  `decimal_full_group_overflow.bin`）。「无 panic 路径」自该修复起方为
  全真。
- 与 MySQL 渲染语义的已核实细节：转义仅 `"` `\` 与 <0x20（\b\f\n\r\t，
  其余 `\u00xx` 小写 hex）；DEL/<>&/UTF-8 原样输出（id7 实抓）；TIME v==0
  → "00:00:00.000000"、DATETIME 零值 → "0000-00-00 00:00:00.000000"、
  DATE 恒只渲染日期段（含零值 "0000-00-00"）——go-mysql 这三处输出
  "00:00:00"/"0000-00-00 00:00:00"/完整 datetime 串，**均与真机不符**，
  本实现从真机（T15 差分白名单素材）。
- 服务器怪癖备案（非本层缺陷）：文本 999999999999999.9 入库即 1e15 位型；
  TIME 838:59:59 经 JSON 往返显示 630:59:59、532:10:20→404:10:20；
  `CAST(x AS TIMESTAMP)` 非法（JSON 内时间戳只能经列隐式转）。
- 关键接口（Task 9/10 消费）：`json_binary_to_text` 收 JSON 列的 binlog
  原字节（即 T9 分发到 Json 变体的 Vec<u8> 全量），输出即最终 SQL 文本。
- 遗留：json.rs 顶部 `#![allow(dead_code)]`（T9 消费后移除）；深度 100
  上限、严格 UTF-8、非有限 double 报错是对 go-mysql 的行为差异，
  T15 差分需列入白名单。

### Task 9: 值分发 decode_value（value.rs / schema.rs / rows.rs 占位）

- 做了什么：TDD（RED：todo!() 桩下 19/19 value 测试 panic 捕获后实现）；
  `src/binlog/value.rs` 落地 `ColCtx{tp,meta,schema,tz_offset_secs}`（第 4 字段
  = 裁定 1 偏差）、`decode_value`、`utf8_safe`、私有 `mod tp`（官方
  const.go:102-140 码值）；`src/metadata/schema.rs` 仅定义
  `SchemaCol{name,type_name,unsigned}` + `TableSchema{db,table,cols,pk,uks}`
  （存取 T11）；`src/binlog/rows.rs` 占位（T10 接口草案，仅文档）；mod 接线。
  fixture 全部取自 docker mysql:8.0.46 真机 26 列单行（2186B 行体逐列精确消费，
  go-mysql 裁判逐列比对通过；捕获件留在 /tmp/t9probe 会话级，仓库未收）。
- tp→变体映射表（分支图镜像 go-mysql row_event.go `decodeValue` :1004-1170；
  权威行号附注）：

  | tp | 变体 | 权威依据 |
  |---|---|---|
  | 6 NULL | `Null`，0B | :1167 default 之外特判（T2/裁判确认） |
  | 1/2/3/8/9 整型 | `Int`/`UInt`(schema.unsigned) | decode_int（T5，ParseBinaryInt*） |
  | 4/5 FLOAT/DOUBLE | `Double`（最短往返文本） | :1024-1033（8.0.17 起 meta=4/8，分发忽略） |
  | 13 YEAR | `UInt` 1B+1900 | 真机实测（简报 2B 系 MariaDB，T5 已裁定） |
  | 16 BIT | `UInt` 大端、meta 推宽 | decodeBit（T5） |
  | 246 NEWDECIMAL | `Decimal`（meta=prec<<8\|scale） | :1036-1041（直存，T4 实测 0x0502） |
  | 17/18/19 TIMESTAMP2/DATETIME2/TIME2、10 DATE | `Str`（T6 打包历法） | fsp=meta（1B TLV） |
  | 7 TIMESTAMP(V1) | `Str` 4B **LE 秒**+tz | :1065-1072 ParseBinaryUint32（真机 LE 证实） |
  | 12 DATETIME(V1) | `Str` 8B LE `YYYYMMDDHHMMSS` **无微秒** | :1073-1092（简报 micro*1e6 猜测被否） |
  | 11 TIME(V1) | `Str` 3B LE `HHMMSS`（>24h 支持） | :1098-1106 |
  | 252 BLOB | type_name 含 "text" → `Str`(过 utf8_safe) 否则 `Bytes`；前缀 = meta∈1..4 字节 LE | decodeBlob :1539-1563；判别谓词 sqlgen.go:114-118 / events.go:102-104 |
  | 15/253 VARCHAR/VAR_STRING、254→CHAR 分支 | `Str`（过闸），前缀 = max_len<256?1B:2B LE | decodeString :1173-1185（meta=300→2B 双类型已测） |
  | 245 JSON | `Json`（meta 宽 LE 定长前缀 + JSONB→T8） | FixedLengthInt=LE util.go:121-127；**裁定 6「LNE/read_lns」与权威不符，按权威** |
  | 255 GEOMETRY | `Bytes` 原样（SRID+WKB 保真） | :1053-1061 同 blob 形态（裁定 7） |
  | 254→247/248 ENUM/SET（STRING 伪装经前奏还原） | `UInt` 序号/位图（名称留 P2） | :1042-1051；真机 `fe..f701`/`f801` 证实 |
  | 0/14/249-251/未知码 | `InvalidData` + `tracing::warn!`，pos 不动 | default 臂 :1167-1169；简报「Bytes 兜底」被否（宽度不可知必错行，兜底仅对宽度可知类型安全而它们已全覆盖） |

- STRING 前奏（:1007-1022）关键发现：0xFE(CHAR) 与 0xF5-0xF8 均满足
  `b0&0x30==0x30` → else 分支（length=b1=pack_length、1B 前缀）；此前速算
  0xFE&0x30=0x20 有误，裁判运行+真机抓包双重否证并确认 else 形态。
- 其余偏差/裁定：V1 TIMESTAMP 零秒输出 epoch 文本（T6 裁定延至 V1 保 V1/V2
  一致，go-mysql formatZeroTime 差异入白名单①）；`utf8_safe` 用
  `simdutf8::basic::from_utf8`（0.1.5 无 `validate` API，等价校验零拷贝）。
- 豁免调整：移除 time.rs/decimal.rs/json.rs 顶部 `#![allow(dead_code)]`
  （消费者已就位；json.rs 仅存 `Frame.header_size` 一处定点 allow）；
  int.rs 保留（`ColumnValue::Missing` 待 T10 构造）；value.rs 保留至 T10、
  schema.rs 保留至 T11。
- 对后续任务的影响：见遗留清单新增三条（T2 事件码表勘误、T4 TLV 拒绝的
  真机字节样本、varbinary→Str 白名单候选）。另：value.rs 引入**第三份**私有
  `mod tp`（T5 台账曾要求不得出现）——未做统一合并，因 T4 的 tp 命名与
  decode_meta 匹配臂经真机校验、动它需独立评审轮；建议 T10 统一 int.rs+value.rs
  两份（官方值），table_map.rs 一份保留或同轮处理。

### Task 10: ROWS 事件行解码（rows.rs + 类型码统一表）

- 做了什么（两提交）：
  - `228d00e` refactor: single field-type constant table——Step 0 绑定先行：
    新建 `src/binlog/field_types.rs`（官方 MYSQL_TYPE_* 唯一表），删除
    int.rs/value.rs/table_map.rs 三份私有 `mod tp`（含 T5/T9 挂账的
    FLOAT/DOUBLE 误名表），三处 `use super::field_types as tp` 统一；
    118 项既有测试零改动全绿。
  - `1f59064` feat: rows event decoding with bitmap cursors——
    `decode_rows(body, tm, schema, kind, v2)` + `Row{cols}`/`RowsKind`；
    `BinlogError::PartialNotSupported` 新变体；真机 fixture
    `tests/fixtures/capture_8.0_rows/`（000002 全镜像 W+U、000003 MINIMAL
    镜像 U、000004 JSON 表 W/2×39/D）；capture_8.0_minimal 26 列行 P1 首个
    真实 binlog 端到端行值断言。TDD：RED 15 失败（todo!()）→ GREEN 133+3。
- 关键接口（T12/T13 消费）：
  - `rows::decode_rows(body, tm, schema, kind: RowsKind, v2: bool) ->
    Result<Vec<Row>, BinlogError>`。body 须已剥 CRC（`event::strip_checksum`）；
    **签名无 `with_crc`**（剥除后无真实用途——裁定 2「去掉并记录」分支，报告
    差异表 #5）。UPDATE 返回 2n 交错 `[before,after,…]`；`Row.cols.len()
    == tm.n_cols` 恒成立（schema 更宽时不补齐——T13 责任，接缝）。
  - 行布局（双权威+真机钉死）：tid6B+flags2B+[v2: extra_info_len u16 含自身,
    整段跳过]+n_cols LNE+bm1(+bm2)；**每镜像独立字节对齐 null 区，宽
    bit_width(present)**，present 按前 n_cols 位掩蔽（真机 padding 位=1）、
    null 位按 present 序数推进。简报「双镜像共用游标」shorthand 被否
    （T6-T9 先例第 3 例，报告差异表 #1）。
  - 路由约束（T12）：事件码 39 PARTIAL_UPDATE 与 V0 rows 20/21/22 **不得**
    送入本函数（39 无法从 body 判别，误送必 TooShort/InvalidData——真机
    两件 39 fixture 已钉）；v2 = 事件码 ∈ {30,31,32}。
  - 值语义：present 0 位 → `ColumnValue::Missing`（≠NULL，简报绑定；与
    go-mysql nil 的差异入 T15 白名单候选）；dropped 列（binlog 宽于 schema）
    用占位 `{name:"dropped_column", type_name:"unknown_type"}` 解码照常
    （对齐 my2sql-go context.go:26-27，per-index 名 T13 拼）；空行区 →
    TooShort（比 go-mysql 收紧，T3 校验纪律）；extra-info 首字节 typecode
    ∉ {0,1,2} → `PartialNotSupported`（D5；真机 ROWS_QUERY 实为独立事件
    type 29，8.0.46 extra_info_len 恒 2）。
  - `field_types`（pub(crate)）：binlog 层类型码唯一来源，新代码不得再立私有表。
- 豁免审计：value.rs 模块级 `#![allow(dead_code)]` 移除（`ColCtx::new` 转定点
  豁免，暂仅测试消费）；rows.rs 保留模块级豁免至 T12 生产接入。
- 遗留/对后续影响：tz_offset_secs 本层恒 0，T14 `--time-zone` 需经参数注入
  （接缝已记录）；flags 2B 消费性跳过，STMT_END_F 归 T12 事务机自 body[6..8]
  读取；挂账清单「T2 勘误残余子项归 T10（extra-info 跳过）」已销账。

### Task 11: metadata 层（SchemaStore 在线/离线 + 列数对账 + 键映射）

- 做了什么：`src/metadata/schema.rs` 扩展（serde derive 上表结构、`norm_type`
  Type 列归一化、`Align`/`align_cols`、`key_indexes`）+ 新建
  `src/metadata/store.rs`（`MetaError`、`SchemaStore` offline/online/get/dump、
  `parse_columns`/`parse_keys`）+ `mod.rs` 接线。两文件拆分采纳简报建议
  （store.rs 独立）。TDD：RED 14 失败（todo!()）→ GREEN 152+3（1 ignored）。
  **真库证明**：`#[ignore]` 的 `online_store_live` 双实例实跑通过——docker
  mysql:8.0.46（t1：unsigned/decimal unsigned/复合 uk/非唯一键 + 生成列/
  不可见列 ALTER 成功收录）与 mysql:5.6.51（无此类列，ALTER 失败自动降级、
  absent-safe 证实）；均自建 `my2sql_t11` 库（含 odd 表：无主键、uk 名
  `fake_primary_idx`），SHOW 两查询→解析断言→dump→offline 闭环→删库，
  throwaway 容器用毕即删。
- 顺带修复（既有潜伏）：table_map.rs:385/529/615 三处
  `assert_eq!(e.charset, Vec::new())` 限定为 `Vec::<u64>::new()`。原因：
  bin 此前从未引用 serde_json（依赖虽在 Cargo.toml，trait impl 不进选型）；
  store.rs 首次使用 serde_json 后 `impl PartialEq<serde_json::Value> for u64`
  参与竞争，旧断言类型推断歧义（E0282/0283 编译失败）。最小限定、零行为改动。
- **JSON schema 文件格式**（裁定 1，键面绑定——T14 CLI 与用户手编以本文为准）：
  `{"version":1,"tables":[{"db":"…","table":"…","cols":[{"name":"…",
  "type_name":"…","unsigned":false}],"pk":["…"],"uks":[["…"]]}]}`。
  `type_name` 小写、无括号、无 unsigned/zerofill 后缀词。tables 按
  `db.table` 字典序（BTreeMap，dump 字节稳定）；读入重复 db.table →
  最后生效 + `tracing::warn`；version≠1 → `MetaError::BadFile`。
- 权威对照表（reference/my2sql-go，简报 shorthand 裁定见报告）：
  | 行为点 | 上游实码 | 本层实现 |
  |---|---|---|
  | 列查询 | `SHOW COLUMNS`（mysqlFuncs.go:254），Field/Type 前两列 | `SHOW FULL COLUMNS` 按列名取 Field/Type（简报措辞；输出前两列同序同值，有效一致） |
  | type 归一化 | GetFiledType `(` 前首段（funcs.go:86-92）+ IsUnsigned 含 "unsigned"（:94-96）；8.0.19+ `int unsigned` 无括号形态会把 " unsigned" 留在 type_name（上游 quirk，上游消费不受影响） | norm_type 首段后再剥尾部 unsigned/signed/zerofill 词 → 对齐上游 5.7 形态有效输出与 T9 契约 |
  | 生成/不可见列过滤 | **零过滤**，吃服务器所给（8.0.46 实测 SHOW FULL COLUMNS 含 STORED GENERATED 与 INVISIBLE 列） | 同样零过滤；5.6 无此类列自然 absent-safe |
  | PK 判定 | 键名小写**含 "primary" 即主键**（:194，简报此点核实为真非 shorthand）；多 primary 名键：map 随机序最后生效、前者整体丢弃（:221-238） | parse_keys 确定序复刻「最后生效 + 前者丢弃」；真库实测 `fake_primary_idx` 被提成 pk |
  | 多列/表达式索引 | Seq_in_index 保序 + ContainsString 去重（:184-192）；表达式索引 NULL Column_name→"" （:189） | 照抄（空串名进组；下游 key_indexes 因匹配不到列而整键降级） |
  | 键名→序号 | GetColIndexFromKey（:337-348）找不到时**静默置 0**（零值 bug，WHERE 用错列）；仅 PK 名列表为空才 pk=[]（events.go:139-143）；序号≥行宽 → 上游后续访问越界 panic | key_indexes 整键丢弃（pk→[]、uk 剔除），刻意偏离上游 bug，T15 白名单候选 |
  | 列数·扩宽(binlog>schema) | sqlgen.go:23-33 补 `dropped_column_i`/unknown_type（命名 :19-21、context.go:26-27），但 events.go:87 随即**无条件 Fatalf**——补位从不产出 SQL | 非 strict → `Padded{dropped:["dropped_column_0"…]}`；strict → `Err(ColCountFatal)` 复现 fatal。**T15 差分：此情形上游恒 fatal，对账需 strict=true** |
  | 列数·收窄(binlog<schema) | events.go:83 rowLen≤len → 静默取 schema 前 binlog 列 | `Truncated(schema宽-binlog宽)` 恒返回（含 strict——上游该方向从不 fatal，strict 覆盖两向是本层语义扩展） |
- 关键接口（T12/T13/T14 消费）：
  - `SchemaStore::{offline(&Path), online(uri)->, get(&mut,db,tb)->Result<&TableSchema,MetaError>, dump(&self,&Path)}`；
    online 构造建连一次、get 懒查+缓存、无重试（裁定 2，上游同）；空库/表名 →
    `EmptyIdent`；offline 缺表 → `NotFound`。`MetaError`（thiserror，
    Db/Io/Json/NotFound/BadFile/EmptyIdent/ColCountFatal）——**签名偏差**：
    align_cols 错误类型由简报 `BinlogError` 改 `MetaError`（列数对账属
    metadata 层语义，不向 binlog 层塞变体；依赖面不扩，mysql/serde/serde_json
    既白名单）。
  - `Align::{Ok, Truncated(尾列个数), Padded{dropped:Vec<String>}}`：目标宽度
    可推导（Truncated：cols.len()-n；Padded：cols.len()+dropped.len()，均等
    binlog_cols）。T10 decode_rows 已按静态 dropped 占位处理超宽列，
    Padded.dropped 供 T13 在 SQL 文本面拼 per-index 真名（T10 节点接缝）。
  - `key_indexes(&TableSchema,&TableMapEvent)->(Vec<usize>,Vec<Vec<usize>>)`。
- 遗留/对后续影响：T13 strict 语义接线时决定默认值——与上游有效行为等价
  要求扩宽方向 fatal（见对照表「列数·扩宽」行）；`--schema-dump` 在 T14 接
  online 时仅含已缓存表（懒查语义，上游 GetTableInfoJson 亦惰性）。
  schema.rs/store.rs 模块级 `#![allow(dead_code)]` 保留至 T13/T14 生产接线。

### Task 12: 事件源层（FileReader + Filters + 事务状态机）

- 做了什么（提交 1484cdd 前置修复 + 227ba1f 功能）：
  - `src/binlog/file_reader.rs`：`FileReader<R: Read+Seek>` 实现
    `EventSource`（生产 `File` 经 `open()`，测试 `Cursor` 零临时文件）。
    流程：magic `fe bin` 校验（错→InvalidData）→ FDE 消化（binlog version
    ≠4 拒、server 串含 mariadb 拒——P1 矩阵外；`with_crc` 以
    `fde_checksum_ok` 实证探测而非仅信声明字节）→ 逐事件
    parse_header → **stop 判定在 header 后 body 前**（省 IO 收紧，停止
    语义与上游一致）→ 常规事件 crc32 验证+剥 4B（人工 rotate log_pos=0
    豁免）→ start 窗口「读了不产出」→ 分发 RawEvent。V0 行事件
    （20/21/22）与 39（PARTIAL_UPDATE）路由层硬错误；rows 缺前置
    TABLE_MAP 硬错误；PREVIOUS_GTIDS 结构性消化。
  - `src/pipeline/source.rs`：`RawEvent{binlog,start_pos,end_pos,timestamp,
    kind,body,tm:Option<Arc<TableMapEvent>>}`、`RawKind`
    （Query/Xid/Gtid/Rows(RowsKind,v2)/Rotate/Other）、
    `TrxStateMachine::feed()->(trx_id,TrxStatus)`。
  - `src/pipeline/filter.rs`：`Filters::{none,from_config,accept,
    pos_stopped,pos_pending}` + 库表/DML 名单。
  - step-0（1484cdd）：rows.rs present==0 活锁守卫 + tm 数组 `.get()`
    加固（RED=零位图挂死/越界 panic，GREEN=两测试）；event.rs 增补
    TRANSACTION_CONTEXT=36、VIEW_CHANGE=37。
- **上游对账真相**（本任务核心产出，裁定 2/4/7 的权威结论）：
  - rows 事件 `start_pos` = **最近 TABLE_MAP 事件自身起始**（file.go:197-198
    `tbMapPos = h.LogPos - h.EventSize`，:214-215 赋给行事件；非行事件
    = 自身起始，file.go:276）。真实 fixture 钉死：WRITE_ROWS start=1020
    = TABLE_MAP 起始，非简报可推断的行事件起点。
  - 位点窗口按 **end_pos（header.LogPos）** 比较：`(name,end)<start` 跳过
    （跨 start 的事件被包含），`(name,end)>=stop` 停止——**等号排除**；
    名先字典序再位点（com.go:163-224 mysql.Position.Compare）。
  - 时间窗口 `ts<start→续`、`ts>=stop→断`，unix u32 秒（com.go:63-74）。
  - **跨文件真相**：上游默认**单文件**——EOF 后续读仅当设置 stop-file/
    stop-datetime（file.go:74-85）；下一文件名 `%06d` 十进制推进
    （funcs.go:98-103，999999→1000000 无截断）；rotate url **从不切文件**，
    只更新比较用文件名标签（com.go:41-46），且 rotate 事件本身归属旧名
    产出（file.go:214 造事件早于 com.go:43 改名）。简报「+06d 跨文件」
    为部分真实（推导式存在，但非默认行为）。本层镜像：FileReader 恒
    单文件（EOF→`Ok(None)`），rotate 只改名；多文件迭代归 T14，
    `FileReader::next_binlog_name` helper 已就位。
  - 上游**绝不 seek 到 start_pos**（file.go:118-122 原注释：seek 会因跳过
    FDE/TABLE_MAP 而 panic）；本层同——窗口只过滤产出，FDE/TABLE_MAP 恒
    消费。
  - **FDE CRC 规范特例**：mysqld 计算 FDE 校验和时尚未写入
    LOG_EVENT_BINLOG_IN_USE_F，故 FDE 的 CRC = crc32(event[0..len-4) 且
    **header flags 字节 17..19 置零**；log_pos 字节照常参与。通用
    `crc32_ok` 对真机 FDE 恒假（4 个 8.0.46 fixture 实证），`fde_checksum_ok`
    专函数 + fixture 回归钉死；go-mysql 干脆跳过 FDE 验证
    （parser.go:238-243 FDE 分支不触 verify），上游 my2sql-go **从不校验**
    任何 checksum——
    本层逐事件校验是文档化的更严立场（敌意输入防线）。
- 与上游的有意偏差（均入 T15 白名单候选）：
  - DDL Query 事件：上游 file 模式只把行事件送 SQL 生成（file.go:245-268），
    Query/DDL 不出 SQL；本层状态机把非事务 Query 标记为独立已提交事务
    （feed: begin→trx_id+1 Begin；其他 SQL→+1 Commit；XID→Commit；
    ROLLBACK→Rollback 不改 id；GTID 33/34 透明——上游 com.go default→
    C_reContinue 全忽略，裁定 1 保持 marker-only），出不出 SQL 归 T13。
  - `--db/--table` 双形态：条目含 `.` → db.table 精确；无 `.` → 仅比表名
    （兼容上游 bare-table 语义 context.go:196 的超集）。
  - `stop_pos` 无 `stop_file`：上游 StopFilePos 仅随 -stop-file 生效
    （context.go:325-334）；本层 from_config 以 start_file 回退名字、
    缺位点取 u32::MAX（本工具 CLI 语义）。
  - checksum 逐事件强制验证（上游零验证，见上）。
- 关键接口（T13/T14 消费）：
  - `FileReader::open(name:String, path:&Path, filters)->FileReader<File>`；
    `FileReader::new(name, rdr:R, filters)`（R: Read+Seek）；
    `EventSource::next()->Result<Option<RawEvent>,BinlogError>`，
    `None`=干净终点（EOF 0B），截断 header（1..18B）=UnexpectedEof，
    其余 IO 错映射 InvalidData("io: ..")（BinlogError 无 Io 变体）。
  - rows 事件 `body` 剥头剥 CRC 后可直接 `decode_rows`（fixture 冒烟已
    接）；`tm` 随事件携带 `Arc<TableMapEvent>`。
  - `Filters::accept(&RawEvent)`（非行事件只过窗口；行事件无 tm →
    fail-closed 拒）；`pos_stopped` 供主循环硬停。
- 遗留/对后续影响：
  - `Config::validate` 已 pub（Filters::from_config 测试需要）；失败路径
    仍 `die()`→`process::exit(2)`——**凡测试调 validate 必须带 --uri 或
    --schema-file**，否则整个测试进程被杀且只留一行 stderr（本任务实踩）。
  - 36/37 常量本任务仅定义未消费（归 Other）；P2 flashback 若做 TXA 再消费。
  - file_reader/filter/source 三模块 `#![allow(dead_code)]` 保留至 T14 接线。
  - T14 若做多文件续读：仅当 stop 条件存在时（镜像 file.go:74-85 语义），
    文件名推进用 `next_binlog_name`（十进制 %06d），不信任 rotate url 切文件。

### Task 13: sqlopen（值编码 + DML 构建器）

- 做了什么（提交 ebf8a10）：`src/sqlopen/{mod,encode,dml}.rs`。
  `quote_ident`（反引号、内部 `` ` `` 双写，裁定 8——库/表/列名进 SQL 文本的
  唯一通道）；`encode_value`（Null→`NULL`、Int/UInt→十进制、Double/Decimal→
  预渲染文本原样透传、Str/Json→单引号+上游全等转义集、Bytes→`0xUPPERHEX`、
  Missing→`InvalidData` 硬错误〔裁定 3，partial rows 已在 T10 拒收，非出货
  路径〕）；`DmlBuilder::{inserts,deletes,updates}` 返回
  `Result<Vec<String>, SqlError>`（简报 `-> Vec<String>` 速记不承载裁定 2/3
  的错误传播——失真续例）；`SqlOpts` 六字段与 T1 config.rs flag 一一对应
  （db_prefix←`--no-db-prefix` 取反；`from_config` 就位，T14 零新字段接线）。
- 权威行为对照（本任务行号全部实读自 reference/my2sql-go）：
  - **转义集**：字面量渲染链 = `SQL.Literal`→`sqltypes.BuildValue`
    （sqltypes.go:217-278）→`String.encodeSql`（:548-566）按 `SqlEncodeMap`
    逐字节。映射由 `encodeRef`（:611-621）定义，**恰为 9 项**：`0x00→\0`
    `'→\'` `"→\"` `0x08→\b` `0x0A→\n` `0x0D→\r` `0x09→\t` `0x1A→\Z`
    `\→\\`；简报/控制器备忘猜测的「%, 反斜杠?/ctrl-S?」不在集内——以代码
    为准。com.go/funcs.go **无** Escape 函数（简报定位失真续例）。另有 LIKE
    例外（:556-561）：`\` 后随 `%`/`_` 不双写（MySQL 5.7 string-literals
    文档注），本层逐字节复刻。%/_ 本身不转义。
  - **WHERE NULL**：`sqlbuilder.Eq`（expression.go:441-447）对 NULL 右值把
    `=` 算子换成 ` IS `，右值渲染 `null`（sqltypes.go:27 `nullstr`）——
    即上游产出 `col IS null`，**从不** `col = null`（恒假、行不可定位）。
    本层镜像为 `col IS NULL`（大写，无语义差）。无键表全列 WHERE 同规则。
  - **键选择**：`GetOneUniqueKey`（mysqlFuncs.go:322-335）：uniqueFirst∧uk
    非空→uk[0]；否则 pk；否则 uk[0]；皆空→`GenEqualConditions`
    （sqlgen.go:269-282）full 分支全列等值。`--full-columns`
    （context.go:215）短路为恒全列 WHERE + SET 全列。dropped 位（schema
    无名）永不入键（key_indexes 按名解析构造保证）。
  - **SET 差异**：`GenUpdateSetPart`（sqlgen.go:336-381）非 full 时逐列比较
    解码值：字节族（blob/json/geometry/unknown 且非 text）按原字节
    `CompareEquelByteSlice`，其余 Go `==`（值等值）。本层用 ColumnValue
    derive PartialEq（int.rs:30，T10 已派生——裁定 6 核实**无需新增**）：
    Str/Bytes 按字节、文本族按预渲染文本严格比较。因 T5-T8 渲染确定性，
    「比解码值」与「比编码文本」效果等价（裁定 6 结论）；采前者的镜像。
  - **批量 INSERT**：`GenInsertSqlsForOneRowsEvent`（sqlgen.go:165-187）按
    rowsPerSql 切分；上游调用面 events.go:150 **恒传 1**（无 CLI batch
    flag——`--insert-batch` 是本工具重设计），`insert_batch=None`→1 行/句。
    DELETE/UPDATE 上游无批量路径（一行对/一行一语句）。
  - **ignore_pk_for_insert**：仅 INSERT——sqlgen.go:159-164（pk 空自动失效）
    + `ConvertRowToExpressRow` :204-217 确认列清单与 VALUES **双位置**剔除；
    `GenUpdateSqlsForOneRowsEvent`（:288-334）签名无该参数——UPDATE 的
    SET/WHERE 完全不受影响（裁定 7 核实毕）。
- 裁定 2 定稿（销账下方 checklist「T13 决策点」行）：`SqlOpts.strict_schema`
  **默认 false（非 strict）**。非 strict：`Align::Padded` 的 dropped 位从列
  清单/全列 WHERE 中**省略**并每事件 `tracing::warn!` 一次（合成名
  `dropped_column_i` 非真列，引用必错；binlog 序号保留仅作位置映射）；
  `Truncated` 静默取 schema 前缀（events.go:83 口径）。strict=true：
  align_cols 任何失配升 `ColCountFatal` → 逐事件 `SqlError::Meta`。与上游
  有效行为等价性：上游扩宽方向恒 `log.Fatalf`（events.go:87）、pad 列从不
  出 SQL，故非 strict 的「子集出货」只在上游崩溃的场景里多产出，等宽场景
  两家逐字节一致——T15 差分纪律（宽出跑 strict=true）不变。
- 与上游的有意偏差（T15 白名单候选，blob 项已挂账下方清单）：
  - **Blob 字面量**：`Bytes`→`0xHEX`（大写），上游非 utf8 String→
    `X'lowerhex'`（sqltypes.go:567-570 + hex.go:19 `%02x`）——前缀与大小写
    双分歧，SQL 语义等价，比较器须双解（裁定 1 重设计）。
  - Str 的**契约兜底**：utf8_safe 后仍构造非法 UTF-8 Str（违约输入）→
    降级 `0xHEX`（不 lossy 重写，D3），测试钉死。
  - **无变化行对**（FULL 镜像 matched-update）：上游空 SET → sqlbuilder 报错
    → log.Fatalf 进程死；本层跳过该语句 + warn（无操作重放语义等价）。
  - NULL 渲染大写 `NULL`/`IS NULL`（上游 `null`/`IS null`）、WHERE 结合子
    ` AND `（上游同）——仅大小写面差异。
- 关键接口（T14 消费）：`DmlBuilder::new(SqlOpts)`；
  `inserts/deletes/updates(&self,&TableMapEvent,&TableSchema,&[Row])
  ->Result<Vec<String>,SqlError>`（updates 传 decode_rows 的交错对，奇数长
  →InvalidData）；`SqlOpts::from_config(&Config)`；表名取 tm.schema/tm.table
  （上游 rEv.Table 同源，binlog 面真名）。DmlBuilder 无跨事件状态（简报接口
  块 `..,` 参数简写 = 与 inserts 同参 tm/schema/rows，文档化）。
- 遗留/对后续影响：sqlopen 三模块 `#![allow(dead_code)]` 保留至 T14 接线；
  add_extra_info 注释头（`--add-extra-info`）非本层职责、T14 包表层做；
  rollback（flashback）方向语义（P2）本层已按上游结构预留对偶性——WHERE
  恒取 before 镜像，P2 翻转时交换 before/after 即可复用。

### Task 14: 流水线装配 + output writer（端到端首交付）

- 做了什么（代码+文档本次单提交）：`src/pipeline/order.rs`（Reorder：
  HashMap 缓冲+连续弹出，pending>2×threads 反压对齐 D7）、
  `src/pipeline/worker.rs`（SqlGroup/Job/build_groups/worker_loop）、
  `src/output.rs`（Writer：path_for `to_sql.{schema.table.}<N>.sql`、
  SET NAMES utf8mb4 头、extra-info 注释行）、`src/pipeline/mod.rs`
  （Runner 装配：FileReader→filter→trx 机→编号→threads=1 直通或并行
  worker 池→reorder→Writer；schema 配对/Arc 下发在 dispatcher）、
  `src/main.rs` 薄壳化（from_args→run_to_sql→摘要行）、`tests/e2e.rs`
  （合成 binlog 全链路 4 测试，threads=1 vs 4 输出逐字节等价钉死）。
- T13/T11/T12/T10 carry-ins **销账**：sqlopen/filter/file_reader/store/rows
  等 11 处模块级 `#![allow(dead_code)]` 随生产接线全部移除（残留 4 处字段级
  豁免均有终态理由，见 task-14-report.md 盘点）；add-extra-info 包表层归本
  任务已交付；`SqlOpts::from_config`/`DmlBuilder` 零新字段消费落地；
  `--time-zone` 注入路径落地（Config.time_zone: FixedOffset → Writer，
  兑现 T10 节点「tz_offset_secs 恒 0、T14 经参数注入」承诺——**仅输出边沿
  使用 chrono**，列值解码链不引 chrono，D3 不破）；`--schema-dump` 经
  Runner::dump_schema 接 T11 API；多文件续读按 T12 裁定 7 落地（仅 stop
  条件存在时跨文件，镜像 file.go:74-85）；`Config::dml_enabled` 删除，
  DML 过滤唯一入口 `Filters::dml_ok`。
- 上游对照结论（收尾者独立复核，前实施者声明成立）：extra-info 模板
  `# datetime=%s database=%s table=%s binlog=%s startpos=%d stoppos=%d\n`
  （events.go:322-326）与 datetime 下划线形 `DATETIME_FORMAT_NOSPACE =
  "2006-01-02_15:04:05"`（events.go:170 + constvar.go:6）**字节平价**；
  带空格 `DATETIME_FORMAT` 仅上游 CLI 输入解析（context.go:292/303），
  `..._NOSPACE_FILE` 上游零引用——不落入 extra-info。保序机制以 Reorder+
  反压替代上游自旋锁（events.go:176-193），语义等价（D7）。
- **架构偏差（控制器裁定，采案 a）**：spec 原为 bin-only crate，集成测试
  无 lib 目标不可编译 → 新增 `src/lib.rs` 作**薄模块根**（仅 `pub mod`
  声明+层序注释，零逻辑）；`main.rs` 为唯一二进制且仅薄壳委派
  `my2sql_rs::pipeline::run_to_sql`；`Cargo.toml` 未动（src/lib.rs +
  src/main.rs 自动发现，包名 my2sql-rs → 外部名 `my2sql_rs`）。
- 决策：逐事件错误 = robust-continue（skip+原子计数+空批填洞，仅文件级
  损坏终止；上游多数同类 Fatalf）；schema 获取/缓存全部在 dispatcher
  （SchemaStore `&mut` 单线程），worker 零共享；Align 每 (tm,schema) 对一
  次甄别/告警去重，每事件对账仍走 DmlBuilder::plan（T13 API 不变）；
  datetime 用固定偏移而非主机 TZ（确定性，T15 裁判固定 TZ 复现）；
  stdout 模式与文件模式统一字节面（含 SET NAMES 头与 extra-info）。
- 遗留/对后续影响：**P2 回滚配对 seam**：SqlGroup.trx_id 已透传、
  TrxStateMachine 状态在 prepare 处可得，flashback 的 before/after 交换
  复用面已留（T13 节点「对偶性」段）；**真实 binlog 冒烟归 T15**
  （tools/docker-mysql.sh 建成后自动化差分，本任务以 e2e fixture 为准，
  简报 Step 3 口径）；reorder `drain_remaining` 为 seq 断流病理防御，
  正常路径恒空。
- **T14 审阅后修复（本节点追加，单提交）**：
  ① Finding 1 worker panic 挂死——`worker_loop` 单作业处理抽为
  `process_job` 并包 `catch_unwind(AssertUnwindSafe(..))`；panic 走与 Err
  分支**完全相同的填洞契约**（errors 计数 + 投 (seq, 空批)）。此前单 worker
  panic 留 seq 空洞，存活 worker 持有 sender 使 dispatcher `res_rx.recv()`
  永久挂死。测试 `worker_loop_panics_are_caught_and_hole_filled`：双 worker +
  `#[cfg(test)]` PANIC_MARKER 注入缝（按 binlog 名匹配，零全局态、不扰并行
  测试），recv_timeout 保证失败模式为 FAIL 非挂起。
  ② Finding 2 输出路径穿越——`path_for` 对 db/table 新增
  `sanitize_for_path`（`/`、`\`、NUL → `?`；整段 `..` → `?`），此前敌意
  TABLE_MAP 名经 `dir.join` 插值 + `sink_for` 的 create_dir_all 可越界建目录
  写文件。**有意偏离上游**（上游 my2sql 同款缺陷可越界写；本项目立场=敌意
  输入安全）。净化**仅作用于路径面**——SqlGroup.db/table 原字节不动，
  extra-info 注释与反引号 SQL 文本继续消费原始值（测试钉死两侧）。
  RED→GREEN 测试：`path_for_sanitizes_traversal_names_into_dir`、
  `writer_traversal_names_never_escape_output_dir`（temp root 递归清点 = 2
  文件全在 dir 内）。三门禁（test 246 绿/clippy -D/fmt）复跑通过。

### Task 15: golden 差分基建 vs Go 裁判（8.0 矩阵全绿）

- 交付物：`tools/docker-mysql.sh`（版本参数化 5.6/5.7/8.0/8.4，
  `--binlog-format=row --binlog-row-image=full --server-id=1`，挂载
  `data/<ver>/`，8.x 强制 mysql_native_password——8.4 用
  `--authentication-policy`，容器 TZ=UTC）、`tools/gen-data.sql`（全类型
  ×{normal,NULL,zero,boundary} 矩阵 + unsigned + emoji + 非UTF8 blob +
  嵌套 JSON(HTML 字符/\u2028/大整数) + DECIMAL(65,30) + 多行事务 +
  单语句 autocommit + utf8mb3/GBK 表 + 1 个仅-UK 表 + 1 个无键表）、
  `tools/run-difftest.sh`（6 步：selftest→双构建→容器+灌数据→oracle
  file 模式→本侧 to-sql→比较；trap 清理容器，KEEP=1 失败时保留调试）、
  `tools/comparator/compare.py`（147 行 ≤150 约束达标；环境无
  sqlparse/pip，走简报预案手写 stdlib 解析：引号态机 _scan +
  split_top/find_kw/split_kw + canon 字面量打标 + 组内多重集配对）、
  `tools/comparator/selftest.py`（8 组 plain-assert：简报绑定 4 例含 2 例
  故意不等 + 新增三规则各 1 正 1 反回归；初版标称「9 组」系计数虚高，
  审阅后如实修正）、`tools/difftest-allowlist.txt`
  （挂账清单逐条运营化，本节点下表）、Makefile `difftest` 目标。
- 对齐口径（非放宽）：双方单行语句（上游恒 1 行/句=本侧 insert_batch
  缺省）；extra-info (binlog,startpos,stoppos) 为对齐键，startpos 双方均
  TABLE_MAP 事件起点；oracle 进程 TZ=UTC + 本侧 `--time-zone +00:00` +
  行解码链恒 UTC（T10 纪律），datetime 字段不进对齐键；本侧 schema 走
  `--uri` 活库，与 oracle 同源，等宽无 strict 干扰；SET NAMES 头/
  空白/括号/引号形态由结构解析吸收。
- 结果：`make difftest` 洁净态 exit 0，groups A=20 B=20 aligned=20
  **green=20 red=0**（修订后矩阵含 JSON 变更 UPDATE → 21/21，见下）；
  三门禁保持：cargo test 239+3+4=246 绿（1 ignored）、
  clippy -D warnings、fmt 干净。
- **解码器 bug：零**。首跑 17/20，三处红灯逐条溯源后全部裁定为渲染/上游
  行为差异（非 T4–T7 布局假设错误），证据链：① 组 7797（t_all zero 行）
  c_ts3 本侧 `1970-01-01 00:00:00` vs go-mysql formatZeroTime
  `0000-00-00 00:00:00`——T6 既有裁定，扩展为零日期对 + fsp 后缀逐字符
  一致方可判等；② 组 8317（boundary 行）c_float=FLOAT 最大值：oracle
  按 f64 全展开 `34028234663852886000…`、本侧 f32 最短 `34028235e38`，
  双方 f32-pack 同 bits → 白名单规则，且以「双方有效数字≤17」闸口
  保护 DECIMAL(65,30) 严格性（自测第 5 组钉死不泄漏）；③ 组 23618
  （t_json UPDATE）上游 sqlgen.go GenUpdateSetPart 对 decoded JSON 的
  `[]byte` 断言失败 → 「恒视为变更」写进 SET，本侧按实际 diff 省略 →
  seteq 规则：多出的 SET 项仅当值为 JSON 文本才容忍、交集严格判等，
  JSON 解码覆盖仍由同组 INSERT 语句全量保真。若任一规则将来把真值差
  放绿，selftest 反例（第 5/6/7 组）先红。
- **审阅两轮修订**（审阅者判「20/20 绿」本身可信，但白名单闸口不具对抗性）：
  ① Important：ALW-FLOAT-WIDTH 原实现类型盲——f32 bits 兜底对任意 num/text
  对生效，DECIMAL(10,2) 相邻值 12345678.90/.91 会漏绿（正是 T7 bug 形态）→
  4613e0f 限定「一侧 f32-canonical 且同 bits」；② 审阅复测残留低精度漏洞
  （16777216/16777217、12345679.0/.9、8388609.0/.6 一侧恰为 f32 精确值）→
  01edac7 再收紧：canonical 侧有效数字须**严格多于**对侧（真 Go f64 展开必
  满足，短文本相邻 DECIMAL/BIGINT 值必红），TDD 先红后绿，产物复跑 21/21；
  ③ Minor：gen-data 原无 JSON 值变更 UPDATE，ALW-JSON-IN-SET 对本侧漏报
  SET 变更不设防 → 4613e0f 在真实事务内补 `UPDATE t_json SET j=…`，本侧
  正确发出变更列、进入交集严格比较（未暴露解码 bug），矩阵 20→21 组。
- 挂账→机制运营化对照（权威登记 = tools/difftest-allowlist.txt）：
  ALW-JSON-KEYORDER/DOUBLE/HTMLESC→jeq 深比较；ALW-DECIMAL-TEXT→num/text
  桥 Decimal；ALW-BLOB-HEX/ALW-VARBINARY-STR→text↔bytes 双向桥；
  ALW-ZERO-TIMESTAMP→零日期对(含fsp)；ALW-INT-SIGN-WRAP→整数 mod 2^64；
  ALW-FLOAT-WIDTH→≤17 位闸口 + f64/f32 同bits；ALW-JSON-IN-SET→seteq；
  ALW-WHERE-PARENS/ALW-IDENT-BACKTICK→cond/idn 结构解析；
  ALW-SETNAMES-HEADER→load 跳头；ALW-EXTRAINFO-DTZ→对齐键不含
  datetime；ALW-COLCOUNT-STRICT/EXCL ALW-NOCHANGE-UPDATE/ALW-MULTI-UK/
  ALW-EXPR-INDEX/ALW-DROP-COL-ALTER/ALW-JSON-OPAQUE-TIME/ALW-TS-V1-LEGACY
  →gen-data.sql 头部注释钉死的矩阵排除；NOTE ALW-56-JSON→T17 预案。
- 环境事实（容器侧，T17 复用）：本机 uid 999 已被 dnsmasq 占用 →
  datadir chown/chmod 走 `docker run --rm -u 0 --entrypoint chown/sh`
  助手容器；binlog 文件 640/uid999 须 `chmod a+r mysql-bin*`+`a+rX` 否则
  两侧读档均失败（oracle 读到不可读时**静默 exit 0**，run 脚本以
  forward*.sql 存在性硬护栏兜底）；`mysqladmin ping` 走 socket 会误中
  临时初始化服务（skip-networking）→ 就绪探测必须 TCP
  `-h127.0.0.1 -P3306`；mysql:8.0 容器 `--memory=1g` 会 OOM 静默断连 →
  2g；Go 侧 `go build` 需 PATH 加 /opt/go/bin。
- T17 预备：5.6 无 JSON 类型 → 差分需表/列过滤（ALW-56-JSON，矩阵
  t_json + t_all.c_json）；8.4 `--default-authentication-plugin` 已移除，
  docker-mysql.sh 已按版本切换 `--authentication-policy=mysql_native_password`；
  5.7 signedness bitmap 与 TLV 差异预期由本比较器结构层吸收（num/text
  桥），真机跑通即销账 690 行残余。

### Task 16: 发布收尾——吞吐基线 / fuzz 语料 / README / 发布构建（P1 DoD 全项达成）

- 交付物：① criterion 吞吐基线 `benches/decode.rs`（harness=false，端到端子进程
  测 file 模式 to-sql；`[profile.bench] inherits="release"` 保证发布态口径；
  输入缺失或 debug 档**自动跳过 exit 0**，洁净克隆 `cargo test --all-targets`
  不受影响）+ 生成器 `tools/gen-bench-binlog.sh` 与 `tools/bench-growth.sql`
  （服务端 INSERT…SELECT 放大 + W/U/D 混合，缓存 data/bench/ 带 marker+尺寸闸，
  FORCE=1 重生成）；② `tests/fuzz_seed/`（3 个坏事件件：table_map 元数据截断、
  rows present=0 livelock 形态、JSON 深度炸弹）+ `tests/fuzz_seed.rs`
  （解码层走读断言 **Err 而非 panic**；FUZZ_SEED_REGEN=1 可再生 + 磁盘/构造器
  漂移硬assert；头注释标明 P4 corpus 来源）；③ `README.md`（功能矩阵、
  **实测过**的 docker 产数→to-sql 快速上手、差分说明、15 条与上游行为差异
  摘录——白名单/决策条目curate）；④ `docs/bench/p1.md`（机器上下文、输入
  溯源、criterion 原文、复现命令、发布构建记录、musl 性能悬崖实测量表）。
- 吞吐结果（DoD-3 通过，release 态，528.8 MiB 单文件，errors=0）：
  **threads=8 中位 103.85 MiB/s ≈ 108.9 MB/s（阈值 40MB/s 的 2.7 倍）**；
  threads=1 41.37 MiB/s（扩展比 2.5×，串行点=reorder 刷出+写盘放大 ~871MB，
  如实记录）。threads=1/8 输出 1,757,221 条语句逐字节确定一致。
- 简报勘误（诚实口径）：「gen-data.sql ×20 ≈ 500MB」不成立——单次回放仅
  ~37KB（difftest 000002=3MB 是初始化容器系统 DDL，非用户数据）；改用
  服务端存储过程放大 62 轮 ≈ 554MB（2 分钟），max_binlog_size>1GB 会被
  mysqld 静默截顶（MY-000081）。
- 发布构建：`cargo build --release` 绿；`cargo build --target
  x86_64-unknown-linux-musl`（debug+release）绿、static-pie 实证可运行——
  musl **构建销账**；但同负载吞吐崩塌至 ~3.4 MiB/s（musl malloc arena 竞争，
  8 线程 32×变慢），**性能挂账 P4**（候选 mimalloc/glibc-static），DoD 基线
  口径 = glibc release。详见 docs/bench/p1.md。
- 三门禁（本任务收尾复跑）：`make difftest` rc=0（21/21 绿 + 回放逐字节）、
  `cargo test --all-targets` 绿（242+3+4+2，bench SKIP 路径验证）、
  `cargo clippy --all-targets -- -D warnings` 零告警、`cargo fmt --check` 干净。
- 遗留（不阻塞 P1）：吞吐 2.5× 扩展上限的画像优化（P4+，先 profile 再动）；
  bench 输入生成器依赖本机 docker + mysql:8.0 镜像（CI 化归 P4）。
- **终审修复轮**（全分支终审 review 569d74e..a97f010 后唯一一轮，TDD 先红后绿）：
  ① `fix(binlog)` 8bfe73a——`decode_decimal` 满组越界 `9 − t.len()` 下溢
  panic 封堵（>999999999 → InvalidData，pos 不动），RED=敌意字节
  `81 00 00 00 01 FF FF FF FF` 单测双点位 + threads=1 e2e 直通泵 abort +
  fuzz 第 4 件种子，三处先红后绿（详注见 Task 9 节点「终审勘误」段）；
  ② `fix(metadata)` 29bcab2——online `fetch_online` 两条 SHOW 语句的
  db/tb 裸插值改走 `quote_ident` 单通道（裁定 8，终审 #2 注入面：表名
  含反引号即 breakout），纯函数 `show_columns_sql`/`show_index_sql` 抽出，
  公开 API 不变；③ `docs`——本档与 README 加固口径按实修正（含下方挂账
  四条终审登记项）。矩阵缺口（ENUM>255/GEOMETRY/LONGBLOB>64K）等四项
  登记挂账，不阻塞 P1。

### Task 17: 全版本兼容矩阵 5.6/5.7/8.0/8.4（全绿）

- 交付物：`tools/compat-matrix.sh`（8 用例编排 + 8.4 caching_sha2 探针，
  复用 run-difftest 入口不复制步骤）、`docs/compat/matrix.md`（结果表 +
  排除清单 + 真机勘误）、Makefile `compat` 目标。run-difftest.sh 扩展
  （全 env 开关，默认行为与 T15 一致）：`CKSUM=none|crc32`（透传
  docker-mysql）、`V1ROWS=1`（5.6 真机 V1 rows 事件用例）、
  `gen-data-$VER.sql` 存在则优先（5.6 JSON 裁剪版 19 组）、步骤 7 =
  离线 schema 回放（`--schema-dump` → `--schema-file` 重跑 →
  与在线输出 `diff -r` 逐字节）；产物目录带 CKSUM/V1ROWS 后缀；
  容器就绪后回显 `@@binlog_checksum` 等真机事实入日志。
  docker-mysql.sh 扩展：CKSUM/V1ROWS/AUTH(stock)/DT_NAME 开关 +
  **8.4 真机勘误**（见下）。
- 结果（commit b8f401c，2026-09-21，全绿细节与复现 = docs/compat/matrix.md）：
  5.6.51×3（默认 CRC32、NONE、V1rows）、5.7.44×2（CRC32、NONE）、
  8.0.46、8.4.11 各 1 + 8.4 stock caching_sha2 在线元数据探针
  （产物与 native 跑逐字节一致）。差分全对 19~21 组/用例，回放全等。
- **唯一真解码器 bug（fix(task-12)，b8f401c）**：5.7+ mysqld 的 FDE
  **恒带 4B CRC 尾**（即便 binlog_checksum=NONE，alg 字节在 len-5=0，
  go-mysql event.go:186 同位读取）；旧 `handle_fde` 判定把 body[-1]
  （实为 CRC 尾字节）当 alg——5.7-none 真机件尾字节恰 0x01 →
  误判"声称 CRC32 但验证失败" → ChecksumMismatch 整跑挂。
  RED=真机捕获件单测 `fixture_5_7_checksum_none_fde_carries_crc_tail`
  （119B 逐字节 mysql:5.7.44 --binlog-checksum=none FDE）修复前必红。
  三门禁复跑：cargo test 240+3+4 绿 / clippy -D / fmt。
- 实测勘误（brief 假设 vs 真机）：① 5.6.51 默认 **CRC32**（非 NONE）且默认
  产 **V2** rows 事件（V1 需 log_bin_use_v1_row_events=1——矩阵加测该用例，
  事件普查 10W/6U/3D 全 V1 确认（修复轮 1：U/D 曾误记为 D/U；普查现为
  run-difftest 步骤 3.5 实证硬门，产物 out/difftest-5.6-v1rows/EVENT_CENSUS.txt））；② 8.4.11
  `--authentication-policy=mysql_native_password` **启动失败**（MY-013797，
  native 插件默认 OFF）——正确姿势 `--mysql-native-password=ON` + 建库后
  `ALTER USER 'root'@'%'` 为 native（docker-mysql.sh 已按实测改）；
  ③ mysql 28（TLS off）对 8.4 caching_sha2 full-auth RSA 握手开箱可用
  （探针 PASS，无需 TLS）。
- 挂账销账/新增：NOTE ALW-56-JSON 兑现（数据级排除
  tools/gen-data-5.6.sql，登记 matrix.md，比较器零改动）；
  V0 rows 事件确认 5.6+ 无开关可产出（矩阵排除维持，路由层硬错误立场不变）。
- 审阅两轮后控制器补刀（520a40b）：PROBE_ONLY=1 在全量 RESULTS 变量赋值行
  即截断 tsv 的 bug——截断移至 PROBE_ONLY 分支 exit 之后；被清空的
  out/compat-results.tsv 从 compat-full-run2.log 逐字恢复（8 行，tab 校验）。
- 对后续影响：`make compat` = P1 收尾验收门之一；T16（性能/收尾）若改
  run-difftest 须保持 7 步契约；gen-data.sql 改动必须同步 gen-data-5.6.sql
  （文件头已钉注释）；8.4/5.6 镜像已在本机（后续无需再拉）。

### P2 Task 1: sqlopen 语义反转（WorkKind + 逆向 UPDATE + 完整性硬规则）

- 做了什么：`src/sqlopen/dml.rs` 新增 `pub enum WorkKind { ToSql, Flashback }`
  （`Debug, Clone, Copy, Default(#[default]=ToSql), PartialEq, Eq`）、
  `DmlBuilder` 增 `kind` 字段与构造器 `DmlBuilder::flashback(opts)` /
  访问器 `kind()`、统一分派入口 `dml_for(RowsKind, tm, s, rows)`
  （Flashback 下 Write↔Delete 互换、Update 透传）；`updates()` 按 kind
  镜像取位（SET=before/WHERE=after，签名不变，正向逐字节不变）；两条
  完整性硬规则——a) `plan()` 中 Flashback 遇 `Align::Padded` 由 warn 升级
  为 `SqlError::Value(BinlogError::InvalidData)`（消息含 `flashback`，
  对齐上游 events.go:87 fail-hard）；b) `cell()` 中 Flashback 命中
  `ColumnValue::Missing` → InvalidData 并提示 `binlog_row_image=FULL`。
  模块头补「P2 反转口径」文档段。
- **实现钉死口径（T2/T3/T4 消费）**：`WorkKind`、`DmlBuilder::{new,flashback,
  kind}`、`dml_for` 签名与分派表（简报原文）；`opts()/inserts/deletes/updates`
  不变；`DmlBuilder::default()` 仍 = ToSql（worker.rs 既有测试零改动）。
- **简报对账（Step 6 测试 vs Step 7 代码片段）**：Step 7 的 `cell()` 闸门只在
  被访问列触发，而 Flashback `deletes()` 的 WHERE 仅触键列——测试
  `flashback_missing_value_error_hints_row_image_full` 要求非键位 Missing 也
  报错，故补 `deletes()` 入口的 Flashback 整行预检（对 `p.cols` 逐列过
  `cell()`），`inserts/updates` 天然逐列访问无需预检。行为以钉死测试为准。
- **门禁偏差登记**：Step 7 手写 `impl Default for WorkKind` 触
  `clippy::derivable_impls`（-D warnings 硬闸），改为
  `#[derive(Default)] + #[default] ToSql`——语义逐字等价（Default=ToSql）。
- 上游对照：`-work-type` rollback 分派 `base/context.go:186` +
  `base/events.go:62`、出货面 `base/rollback_process.go`；SQL 生成面对照
  `base/sqlgen.go` `ifRollback` 形参（:137/:237/:288）与包装 :233/:284。
  不继承上游「JSON 恒进 SET」quirk（spec §3.1，正反向着皆然，本侧按实际
  diff；ALW-JSON-IN-SET 白名单容忍裁判多出项），测试
  `flashback_update_unchanged_json_not_in_set` 钉死。
- 测试：TDD 三轮 RED→GREEN（dml_for 分派 / updates 反转细节 / 两硬规则），
  dml.rs 22→27 测试；全量 `cargo test` 绿、clippy -D warnings 净、fmt 净。
- 遗留/对后续影响：`dml_for` 是 T3 worker 唯一入口（现 worker 仍直调
  inserts/deletes/updates，T3 切换）；Padded 硬规则使 flashback 对「中途
  DROP COLUMN」矩阵场景整事件报错（宁缺毋漏口径，T6/T7 差分需按此归入
  错误计数而非输出差集）。

### P2 Task 2: output 块索引模式 + flashback/reverse 并行逆序读取器

- 做了什么：① `src/output.rs`——`path_for` 前缀参数化（5 参版删除 → 6 参
  `{prefix}.{schema.table.}<N>.sql`；to-sql 传 `"to_sql"`、flashback tmp 传
  `".flashback.tmp"`、final 传 `"flashback"`）；`Writer::new` 尾增
  `prefix: String, index: bool`；`Sink::File` 增 `written: u64` 字节计数
  （初值 = FILE_HEADER 长度，Screen 不计数）；`write_group` 先组装本批完整
  字节串再单次写入，`index=true` 时每 rows-event 批登记
  `(offset, len, trx_id)`（**逐事件一块，非逐事务**），`Writer::blocks()`
  只读视图供逆序回读。② 新建 `src/flashback/{mod.rs,reverse.rs}`——
  `reverse_block`（记录原子化：extra-info 注释行保头、块内 SQL 行逆序）、
  `reverse_file`（按块索引从尾回读；keep_trx=true 逐字节复刻上游注入：
  lastTrxIdx 初值 0 → 首个写出块必 `commit;\nbegin;\n`、trx 变化处注入、
  尾补 `commit;\n`）、`run_files`（文件级任务队列，threads 只影响文件间
  并发；成功即删 tmp=上游 :20；空块表判空跳过不落 final）、
  `final_for_tmp`（tmp 名 → final 名前缀替换，只回 file_name 段）。
  ③ 唯一外部调用点 `pipeline/mod.rs` 同步
  `Writer::new(…, "to_sql".into(), false)`。
- **简报对账（代码片段 bug，按契约文本修正）**：`final_for_tmp` 片段
  `strip_prefix('.') + replacen(".flashback.tmp","flashback",1)` 自相矛盾——
  剥掉首点后串内已无 `.flashback.tmp`，替换恒不命中，与片段自带 doc 例
  （`.flashback.tmp.3.sql → flashback.3.sql`）冲突；实采**不剥点**的单次
  `replacen`，doc 两形态（plain + file-per-table `.flashback.tmp.d.t.3.sql`）
  以测试钉死。另简报测试 `b1.contains(b"…")` 作用于 `&[u8]` 是类型错
  （slice::contains 收单元素），改 `String::from_utf8_lossy(b1).contains(...)`，
  断言语义不变。
- 上游对照：`rollback_process.go:31`（lastTrxIdx=0 初值）、:76-155（尾块
  先出 + trx 变化注入 + 尾 commit）、:20（tmp 必删）。有意分歧（计划已裁，
  不修）：记录原子化（上游逐行整体逆序，注释行会漂到组尾）；空块表本侧
  判空跳过 vs 上游落仅 `commit;\n` 空文件——后者入差异清单（T7/T8 对账时
  登记）。
- 测试：TDD 两轮 RED→GREEN——output 签名/缺方法编译错 14 条 → 8/8 绿
  （块偏移用文件字节切片逐块核验：首块紧跟 SET NAMES 头、末块止于 EOF）；
  flashback 未定义符号编译错 17 条 → 13/13 绿（keep-trx 字节 golden、
  记录原子、无脚手架纯逆序、threads 1/8 逐文件字节全等、warn 行插Header后、
  final_for_tmp×2）。全量 `cargo test` 258 绿、clippy -D warnings 净
  （`reverse.rs` 增 `pub type Block=(u64,u64,u64)` 别名消 type_complexity，
  与钉死签名同型不改语义）、fmt 净。
- 遗留/对后续影响：T3 消费接口=本节点钉死面：`Writer::new(dir, false, fpt,
  extra, tz, ".flashback.tmp".into(), true)` + `finish()` 后 `blocks()`；
  `final_for_tmp` 只回文件名片段，T3 须 `tmp.parent().join(final_for_tmp(tmp))`；
  `run_files` 任一文件失败即返 Err 且**不清理**其余已建 tmp/final（调用方
  清场是既定分工）；`Writer::created()` 未加（简报未要求，blocks() 键集即可
  枚举 tmp）。

### P2 Task 3: 流水线接通 flashback（装配 / on-error 策略 / DDL 排除）

- 做了什么：① `config.rs`——`WorkType {ToSql, Flashback, Stats}`、
  `OnError {Stop, SkipBadEvent}`（`Clone,Copy,Debug,PartialEq,Eq`，简报钉死）+
  `Config` 三新字段 `work_type/keep_trx/on_error`，`validate(ToSqlArgs)` 恒填
  `{ToSql, true, SkipBadEvent}`（to-sql 默认 skip 不变）；CLI 面零改动（T5）。
  ② `worker.rs`——`build_groups` 三臂 match 改 `builder.dml_for(*kind, tm,
  &job.schema, &rows)?`（ToSql 下逐字节等价，e2e 全绿守卫）；`worker_loop`
  扩为 6 参（+`abort: Arc<AtomicBool>` + `stop_on_error: bool`）：Err/panic 两
  分支 `errors.fetch_add` 后**当且仅当** stop 形态 `abort.store(true, Relaxed)`。
  ③ `pipeline/mod.rs`——`Emitter { Sql(Writer) | Flash{tmp} }` 分派写出侧；
  `run_to_sql` 的 store 构造抽 `open_store` 与 `run_flashback` 共用；
  `run()` 拆 `run_pump`（两形态共用的文件泵）+ 形态收尾；新增
  `run_flash` = `flash_inner` + Err 时 `cleanup_flash_files`（**全部** created
  tmp + 对应 final 双清——T2 登记的调用方清场义务，spec §3.2 半成品不落盘）；
  `flash_inner`：DDL 排除汇总（`prepare` 非行分支登记
  非 begin/commit/rollback/空 的 QUERY → 收尾逐条 `tracing::warn!` +
  `eprintln!("flashback: {} DDL/query events excluded", n)`）→ `tmp.finish()`
  （**先 flush BufWriter 再回读**，句柄顺序=T2 块偏移契约）→ created 序装配
  `(tmp, parent().join(final_for_tmp(tmp)), blocks)` 作业 →
  `reverse::run_files(&jobs, keep_trx, threads, warn)`，`files=jobs.len()`；
  warn 行 = errors>0 且 Skip 形态时 `"-- WARNING: skipped {N} events,
  positions in stderr\n"`（T2 已测落位=FILE_HEADER 后）。stop 语义两形态：
  并行=收取循环+jion 后 `if stop && abort → Err("aborted: first error logged
  to stderr")`；threads=1 直通无 catch_unwind——`pump_direct` Err 分支直接
  返回 `Err(Config("event at {binlog}:{start_pos} aborted (--on-error stop):
  {e:#}"))`（**永不 unwrap/panic**，解码器不崩约束）。to-sql 侧 stop=false
  常量 + 独立哨兵，行为零变。④ `output.rs`——补 `Writer::created()` 访问器
  （T2 节点登记的缺口）。⑤ 测试基建：`Synth` 从 `tests/e2e.rs` 整段平移
  `tests/common/synth.rs`（pub(crate)+`#![allow(dead_code)]`，e2e 经
  `#[path=…] mod synth;` 引用、断言零改）；加性扩 `table_map_is/write_is/
  delete_is/update_is`（`d`.`t` int pk + VAR_STRING，meta 2B LE、串前缀
  max_len<256→1B）与 `IsPair` 别名（clippy type_complexity）。
- **简报对账（期望串手推，简报自授）**：用例 1 字面期望
  （`DELETE … id=3` 在首、`INSERT …(1,'a')` 在尾）= 正向语句装进逆序块位，
  与简报自述规则「块序=事件序逆序」+T1 Flashback 语义（W→DELETE、D→INSERT）
  矛盾；按规则手推钉死为
  `SET NAMES utf8mb4;\ncommit;\nbegin;\nINSERT INTO `d`.`t` (`id`,`b`) VALUES (3,'b');\ncommit;\nbegin;\nUPDATE `d`.`t` SET `id`=1 WHERE `id`=2;\nDELETE FROM `d`.`t` WHERE `id`=1;\ncommit;\n`
  （脚手架位置与简报串完全一致，仅块内语句取反转语义——head 注入=T2 上游
  口径原样消费，未重实现）。另 `Command` 单变体下简报 `let-else` 骨架触
  irrefutable/infallible-destructuring 双闸，改 validate 入 arm 的穷举 match
  （T5 补子命令时加 arm）。
- 上游对照：stop/skip 策略对应上游 rollback `log.Fatalf` 系（fail-hard）与
  本库 P1 robust-continue 的双形态开关；DDL 排除=file 模式 QUERY 不出 SQL
  （file.go:245-268）+ 本层告警面增强（上游静默）。
- 测试：TDD——`tests/flashback.rs` 先 RED（库级编译错 6 组：WorkType/OnError
  缺符号、run_flashback 缺函数、三字段缺 → 留档 `/tmp/p2t3-red.log`）后 GREEN
  5/5：多事务逐字节（含 threads=1/4 字节等价副断言+tmp 消失+目录零残留）、
  stop 清场（并行+直通双形态循环）、skip 头部 WARNING 行逐字节、DDL 排除+
  摘要不污染、to-sql 正向守卫（同款 fixture 经 run_to_sql 钉正向字节面）。
  worker 层新增哨兵单测（Err/panic 置位 + stop=false 永不置位）。全量
  `cargo test` 275 绿（lib 259/e2e 5/cli 3/flashback 6/fuzz_seed 2）、
  clippy --all-targets -D 净、fmt 净。
- 遗留/对后续影响：T5 消费 `run_flashback` + `Config{work_type,keep_trx,
  on_error}` 覆写面；`WorkType::Stats` 枚举位已立（T4 消费）。**登记边界**：
  a) flashback 形态忽略 `--to-stdout`（Writer 恒 stdout=false 文件 sink——
  reverse 需要磁盘文件；T5 CLI 层应拒收 flashback+to-stdout 组合或文档化）；
  b) ~~dispatcher 侧 schema 获取失败在 stop 形态仍计数跳过~~ **修复轮 1 已收口**：
  `prepare` 改 `Result<Option<Job>,_>`，stop 形态下 prepare 侧错误（缺表
  MetaError / 无 tm / strict-align）经 `prepare_fail` 直接升整跑 Err（spec §3.2
  完整性优先——**登记对简报 Step 1 只接 build/decode 哨兵的 scoped 偏离**，
  spec 优先级高于简报；to-sql 恒 skip 语义字节不变，新增
  `flashback_on_error_stop_aborts_missing_table` 用例钉死 Err+全清场）；
  c) file-per-table 的
  `.flashback.tmp.d.t.N.sql → flashback.d.t.N.sql` 装配按 parent().join 泛化
  接线（final_for_tmp 单测已钉名，端到端覆盖归 T6 真件）；d) 并行 stop 不
  提前中断投递/收取（哨兵后仍走完整收束再 Err——只损失败路径时延，不损
  正确性，tmp/final 全清）。

### P2 Task 4: stats 子系统（Out 泛型通道 / Aggregator 上游字节面 / run_stats）

- 做了什么：① `order.rs`——`Reorder<T = SqlGroup>` 泛型化（手写
  `impl<T> Default`，derive 会强加 T:Default——SqlGroup 不满足），
  `push/drain_remaining` 签名对 T 开放；to-sql/flashback 走默认参数字节零变。
  ② `worker.rs`——`Out { Sql(SqlGroup) | Fact(StatFact) | Status{binlog,pos,
  ts,status} }` + `OutMode { Sql, Stats }`（worker_loop 第 7 参）；
  `Job.status: TrxStatus`；`build_out_stats`：Rows→单 Fact（update 行对
  rows=len/2，其余 len；db/table 取 tm、start/end/ts 取 RawEvent 位点三件），
  非行→空批（标记不过 worker——控制者裁定）；`build_out(job,builder,mode)`
  统一分派（Sql 臂 = 既有 build_groups 包装，等价改写）。
  ③ `pipeline/mod.rs`——`Emitter::Stats(Aggregator)`；prepare 非行分支 stats
  标记派发（query 三关键字 begin→start_pos / commit|rollback→end_pos、
  Xid→Commit(end_pos)；DDL/空 QUERY/Gtid/Rotate 不派发=上游 :193-206 口径），
  与 rows 同源编号直推 reorder；emit Stats 臂 Fact→`statements += rows` +
  feed(Row)、Status→feed(Begin/Commit/Rollback)；`stop_on_error()` 扩为
  flash||stats 且 Stop（stats 默认 skip 归 T5 validate_stats）；
  `run_stats(&Config)->Result<StatsRun>`（output-dir 硬前置、报表 Err 路径
  不 finish=宁缺毋漏、重跑 O_TRUNC 覆盖）+ `StatsRun` Display
  `"stats done: events=…, statements rows=…, windows flushed=…, big/long
  trx=…, skipped=…"`（skipped=summary.errors）。
  ④ `src/stats/mod.rs` 新建——`StatFact/FactKind/StreamEvent/StatsSummary`
  （简报钉死接口）+ `Aggregator`（feed/finish）：上游 stats_process.go:150-268
  控制流逐支路移植（binlog 切换落盘+清窗+last_print=ts+interval；
  lastPrintTime 零值 init=ts+interval；begin 重置累加器含 Binlog/StartPos
  取标记位点、不判；commit/rollback 仅 StartTime>0 才判 `rows>=big ||
  dur>=long`；行事件双更新窗口 map+累加器，key=`db.tb`（KEY_DB_TABLE_SEP）；
  tick flush `ts>=last_print` 落盘序=**首现序**（超越项登记）、biglong `[...]`
  明细 **db.tb 升序**（超越项 3）、accumulator 收尾不清=上游只认 BEGIN）；
  两 txt 宽度模板逐字节（`{:<17} {:<19} … %-Ns` 全套 + 头行建文件即写）、
  `# skipped events: {N}` 尾注仅两 txt；`--stats-json` → binlog_status.jsonl
  /biglong_trx.jsonl 双件（serde 字段声明序=输出序）。⑤ `config.rs`——
  `print_interval/big_trx_rows/long_trx_seconds/stats_json` 四字段，
  validate 填默认 30/10/1/false（to-sql 零影响）。⑥ e2e `tests/stats.rs`
  双文件 Synth 真实布局 5 用例（golden 跨层同一字符串拷贝钉死）。
- 上游对照：stats_process.go（表头 :272/:280、内容宽同款 `%s` 版=context.go:
  530/540 O_TRUNC=File::create；datetime 下划线形复用 P1 `datetime_str`；
  XID=commit、`update` 行数=对数、begin 语义与本侧 keep_trx 无关——标记只看
  关键字）。三处有意超越登记于计划文档（窗口行序=首现序、statements 升序、
  jsonl 面）。
- 测试：TDD——Step 1 泛型重构免录 RED（编译期等价的机械改造）；Step 2 单元
  golden 先 RED（`/tmp/p2t4-red-step2.log`：Aggregator/StreamEvent/
  StatsSummary 缺符号 17 错）后 GREEN；Step 5 e2e 首跑 RED
  （`/tmp/p2t4-step5-run1.log`：fixture 位点自证 assert 抓到 f4 起点误用
  rows 自身起始 378≠tm 340——该「跨层对账」正是 Step 5 的核心价值，改
  `(tsp, ep)` 口径后 5/5 GREEN）。单元 golden 位点在 Step 5 依**真实 Synth
  尺寸探针**重推导（原手推 216/419/446/… 与实测 tm=38/write=31+5r/
  query=33+db+sql/xid=27/is 族 41/52-55/46 不符 → 全套改为 214/414/441/
  253/159/254/296/335/483/510，单元与 e2e 两侧同串同步钉死，控制流断言
  ts/rows 面零变）。`stats_e2e_on_error_stop_escalates` 钉 stats 显式 Stop
  复用 T3 prepare_fail 升格链（Err 且报表无尾注半成品）。全量 `cargo test`
  283 绿（lib 262 + e2e/cli/flashback/fuzz_seed/stats 5 目标 21）、
  clippy --all-targets -D 净、fmt 净。to-sql/flashback 字节面由既有
  261+15 项守卫全绿。
- 遗留/对后续影响：T5 消费 `run_stats` + `validate_stats`（stats 默认
  on_error=skip 在此落地；`--print-interval/--big-trx-rows/
  --long-trx-seconds/--stats-json` CLI 面 + 子命令 work_type 接线）；
  T6 真件 stats 对账（本层 golden=合成件，真机 datetime/时区面复核归 T6）；
  T7 比较器需容忍/核对尾注行（本侧特有，上游无——比较器跳 `#` 注释行）。
  **登记裁定/边界**：a) 标记 pos 角色压缩——`StreamEvent::{Begin,Commit,
  Rollback}` 单 `pos` 字段按角色承载 start/stop（上游 StartPos/StopPos 双
  字段在标记事件上各取其一），字节面与上游一致，接口从简报钉死版；
  b) `finish(skipped: u64)` 签名以简报 Step 3 正文为准（Interfaces 速记块
  无参版视为缩写），e2d Display 依赖此参；c) 尾注只进两 txt（jsonl 只冲刷，
  JSONL 混注释破格式）；d) windows 计数 = **非空**落盘次数（空窗口 tick 不
  计数）；e) stats 模式 `summary.events` = rows+标记派发数（缺表事件不派发
  不计数、只进 errors——与 to-sql 的 events 语义有差，Display 文案已按简报
  钉死）；f) dml/表过滤在 stats 形态同样先于派发生效（计数面与 to-sql 共
  用一条 prepare 通道，超越简报未提但零成本一致）。

### P2 Task 5: CLI 三子命令（CommonArgs/SqlTextArgs flatten 重构 + 三 validate + main dispatch）

- 做了什么：① `config.rs` 主体重构——`ToSqlArgs` 26 旗标拆 `CommonArgs`
  （binlog_dir…threads 19 项，含 `--output-dir`，**不含** `--to-stdout`）+
  `SqlTextArgs`（add_extra_info/no_db_prefix/full_columns/unique_key_first/
  ignore_primary_key_for_insert/strict_schema/insert_batch 7 项），二者
  flatten 进 `ToSqlArgs`（+to_stdout、+`--on-error` 默认 `skip-bad-event`、
  **零旗标名/默认/help 文案改动**）与 `FlashbackArgs`（+`--keep-trx`/
  `--no-keep-trx` 双 bool + `--on-error` 默认 `stop`）；`StatsArgs` =
  Common + `--print-interval/--big-trx-rows/--long-trx-seconds`（clap 只做
  u32 类型面）+ `--stats-json`。`Command::{ToSql,Flashback,Stats}` 三变体。
  ② 校验：共享内核抽 `build_common(&CommonArgs)->Result<Config,String>`
  （threads/schema 源/时区/时间对/位点逻辑原样搬入，SQL 文本与 work_type/
  on_error/keep_trx/stats 阈值落中性默认）；`validate`→`validate_to_sql`
  直重命名（无 deprecated 别名），新 `validate_flashback`（keep-trx 双旗标
  互斥在此判 Err——clap 面可同 parse，简报钉死；默认 stop/keep_trx=true）、
  `validate_stats`（范围 1..=600 / 1..=30000 / 0..=3600，越界 Err 带实值；
  on_error 恒 SkipBadEvent 无旗标）。`OnError` 加 `ValueEnum`（kebab-case
  `stop`/`skip-bad-event`）。`from_args` 三臂分派，Err → `error: …` +
  exit(2)（P1 出口唯一性不变）。③ `pipeline/mod.rs`——`RunSummary::
  display_with(prefix)`（Display 仍恒 `to-sql done:` 逐字节不变，e2e 断言
  核查无此串、无回归）；main 按 `cfg.work_type` 三臂 dispatch
  （flashback 传 `"flashback done"` 前缀；stats 走 `StatsRun` 自带
  `stats done:` Display），Err → exit(1)。④ 穷尽 match 迁移：`filter.rs`
  测试 3 处 `let Command::ToSql` 改 let-else+panic，`e2e::config_from`/
  `flashback::cfg_for`/`stats.rs cfg`/`stats/mod.rs cfg` 四 helper 改
  let-else/match-else+panic 并迁 `validate_to_sql`。⑤ `tests/cli.rs`+3
  冒烟（三子命令 help 旗标面 + flashback 拒绝 --to-stdout + stats 拒绝
  --full-columns + help_lists_subcommands 扩三串）。
- 真件冒烟（capture_8.0_minimal）：to-sql skip 默认 errors=1 照常 Ok；
  flashback 默认 stop 同件 → `error: event at …aborted (--on-error stop)`
  exit 1（差异化默认生效）；`--on-error skip-bad-event` 后
  `flashback done: events=0, …, errors=1` exit 0；stats 报表双件落盘。
- 测试：TDD——Step 1 RED `/tmp/p2t5-red-step1.log`（18 编译错：
  FlashbackArgs/StatsArgs/validate_* 缺符号），Step 2-4 GREEN
  `/tmp/p2t5-green.log`。全量 288 绿（lib 265 + cli 5 + e2e 5 + flashback
  6 + fuzz_seed 2 + stats 5，净增 5：config 3 + cli 2）、
  clippy --all-targets -D 净、fmt 净。
- 遗留/对后续影响：**to-sql 面 `--on-error stop` 是已暴露但未接线的
  旗标**——`stop_on_error()` 仍限 flash||stats（T3 合同「to-sql 恒
  robust-continue」未动），且 skipped-WARNING 头行只存在于 flash 收尾，
  故该值在 to-sql 路径当前完全无行为分叉（默认 Skip 面 P1 字节零变）。
  T6-T8 若需真 stop 语义须扩 `stop_on_error` 并补 e2e；若维持现状建议
  T9 文档标注。**Fix round-1 裁定（review）：上句作废——`validate_to_sql`
  对 `--on-error stop` 直接 Err 拒绝（to-sql best-effort by design，spec
  §3.5；stop 语义 flashback/stats 专属），静默无效暴露面收口，cementing
  测试翻转 + 新增 subprocess 冒烟。** stats 参数范围校验自 clap 移入 `validate_stats` 后，
  difftest 包装层（T7）传参越界会走 exit 2 而非 clap usage——文案差异
  不涉行为。`fargs/sargs` helper 与 `args()` 同放 config.rs tests，
  真件 e2e（T6）可直接复用子命令串形态。

### P2 Task 6: 真件 e2e（capture_8.0_rows）+ 一次性正逆活库对账脚本（P2 语义总闸）

- 做了什么：① 手抄 `tests/fixtures/capture_8.0_rows/schema.json`
  （version-1，表 `t10`.`u` 9 列无 PK——README 的 8.0.46 CREATE 无主键，
  WHERE 全列 = P1 语义；`t10`.`j` id PK + doc json + tag varchar）；正确性
  由真件 decode 全 Ok（列数/类型不符必报）+ to-sql 语句人工目核双向证实。
  ② `tests/e2e.rs` +4 真件用例（常驻跑，无需 docker，fixture 在库）：
  - `real_capture_flashback_full_image_and_forward_reconcile`（000002）：
    run_flashback 默认 stop Ok，摘要 (2,2,0,1)；结构断言首行 SET NAMES、
    尾行 commit;、`begin;` 计数=事务段数=2、`commit;`=begin+1、无
    .flashback.tmp* 残留；正逆对账 = 最小语句解析器（INSERT/UPDATE/DELETE →
    有序 (col,lit) 表，`col IS NULL` 与 `=NULL` 同型归一）+ 镜像重建
    （ins↔del 互换；upd：逆向 SET[c]=正向 WHERE[c]，逆向 WHERE[c]=正向
    SET 有则取之、无则 WHERE 值——依赖本 fixture 无 PK → WHERE 恒全列前提，
    测试注释已钉）→ 逆序产物 == 正序逐条镜像且序反转（有序全等 ⊇ multiset
    相等）；
  - `real_capture_flashback_minimal_image_hard_errors`（000003）：**真件版
    §3.2b 硬规则证明**——MINIMAL UPDATE（after 仅 f=123 其余 Missing）在
    Stop 下整跑 Err（threads=1 消息含 "MINIMAL row image"+"binlog_row_image
    =FULL"；threads=2 哨兵串同 Err），两形态半成品全清场；SkipBadEvent →
    Ok、坏事件计 1 跳、FULL UPDATE#2 照常逆产（逐字节含 WARNING 头行：
    `UPDATE `t10`.`u` SET `c`=NULL WHERE …i2`=-5;`）；对照 to-sql 同件
    skip 默认 Ok（Missing 是事件级编码错误非源级）；
  - `real_capture_partial_event_is_source_level_error`（000004）：**实测
    勘正简报**——event 39 在 FileReader::next 源层即 Err(PartialNotSupported)，
    **不经逐事件通道 → stop/skip 两策略、flashback/to-sql/stats 三形态全部
    整跑 Err**（简报预期「skip 可绕」为不成立；以现实为准钉死）。stats 错误
    路径产物 = binlog_status.txt 仅头行（finish 未达、无 skipped 尾注，
    重跑 O_TRUNC 覆盖）——现状记录，非本任务修复项；
  - `real_capture_stats_totals_match_fixture_rows`（000002→000003，
    --stop-file 跨文件）：报表行总和 inserts/updates/deletes = (1,3,0) ==
    README 独立计数（000002 1ins+1upd；000003 2upd **含 MINIMAL 件**——
    实测 stats 计数不挑镜像（decode 成功、len/2 折算照算），000003 行
    updates=2 硬证）；`summary.statements`=4 互证、windows=2（binlog 切换
    落盘）、biglong 仅头+尾注（零命中）、skipped=0。
  ③ 新建 `tools/flashback-reconcile.sh`（spec §5.5 活库总闸，人工触发、
  不入 CI/Makefile）：容器 mysql:8.0 无宿主 bind mount（datadir 全在容器
  可写层，破坏性操作零外溢）→ rec_db.rec_t（id PK + val + **doc JSON**）
  100 行 → 基线 CHECKSUM TABLE + 全表 dump → FLUSH + 记起点 → 混合 DML
  7 ins/5 upd（**含 1 条 JSON 列 update** 正靶）/3 del + 1 多行事务 +
  「UPDATE 打空=假绿」行数闸（104）→ docker cp 出 binlog → 宿主 flashback
  （离线 --schema-file——**登记**：mysqldump --no-data 是 DDL SQL、离线
  loader 只认 version-1 JSON，故脚本自写 JSON；不给 stop-pos——终点点出
  的事件按「等号也停」会被排除，单文件边界用 --start-file 即足）→ 产物
  原样灌回活库 → CHECKSUM TABLE == 基线 且 剔噪逐行 dump diff 双闸。
  trap DROP DATABASE + docker rm 自清理（KEEP=1 失败保容器）。
- **对账实跑（本任务内一次性，GREEN）**：`flashback done: events=15,
  statements=15, files=1, errors=0`；`baseline checksum = 3944497573 ==
  after-apply checksum = 3944497573` + row-data identical；binlog=
  mysql-bin.000004（start_pos=157→end_pos=4547）；全程 19s。JSON 列
  UPDATE 往返实证：正向 `SET doc=JSON_SET(…'$.tag','p2')` → 逆向
  `UPDATE … SET `doc`='{"n":3,"tag":"init"}' WHERE `id`=3;`（diff-based
  SET，未变化 JSON 列不入 SET；回灌值 = before 镜像规范文本，checksum 终判
  通过——**上游「JSON 恒进 SET」quirk 未被继承**的活库证明）。首跑 RED 为
  脚本自身比对缺陷（mysqldump「Dump completed」时间戳尾行混入 diff），非
  产品 bug；数据行两跑均全等。
- 测试：RED/GREEN 台账 = 探针先行（CLI 三文件两策略实测钉死现实 → 测试
  断言按现实写）；e2e 5→9；全量 `cargo test` 293 绿（lib 265 + cli 6 +
  e2e 9 + flashback 6 + fuzz_seed 2 + stats 5）、clippy --all-targets -D
  净、fmt 净。真件脚本非 CI（一次性语义闸 + 可重跑）。
- 遗留/对后续影响：① 000004 partial 的「skip 也硬拒」现实进 T7 比较器/
  T9 文档口径（上游 my2sql-go 对 39 的处置是 nil 值继续——本侧选择源级
  拒收，差异登记 P1 T12 已有、T6 实测复核）；② stats Err 路径残留头行
  文件如需收口属 P3 打磨（现语义 = 头行无尾注即「未完成」标记；
  **P3 T8-debt `60a9019` 已收口 jsonl 面**：Err 路径两 jsonl 不存在
  （drop-on-error），txt 两件保持该口径——见「P3 DoD 对账」6）；③ 脚本
  的 JSON dump 逐行 diff 依赖「单库单表、无触发器」前提，扩展多表时须换
  按表 CHECKSUM 循环；④ T7 difftest WORK_TYPE=rollback 可直接复用本脚本
  的容器 idioms（无 bind mount + docker cp 出 binlog）。

### P2 Task 7: 差分 harness WORK_TYPE 维度 + 比较器 rollback 规则

- 做了什么：① `compare.py` rollback 模式（第三可选参 `rollback`）——
  `load(d, mode)` 逐文件自适应双模绑定：A 侧（Go tmp 纯行倒置 → 注释漂尾、
  KeepTrx 无 flag 注册恒 false 无 scaffold）语句缓冲 + 注释到达时绑定 key；
  B 侧（我方 flashback 记录原子注释先行 + keep-trx scaffold + `-- WARNING`
  头行）剥离裸 `commit;`/`begin;`/WARNING/SET NAMES 后沿用现绑定；
  `_rb_struct` 结构断言钉含 scaffold 文件（begin数==commit数-1==事务段数、
  每 begin 前一非空行为 commit;、末行 commit;——首部悬空 commit 为平价容忍，
  上游 rollback_process.go:38 lastTrxIdx=0）；违例经 main 计 STRUCT-RED，
  白名单不吞结构；值面零新逻辑（复用 veq/seteq/canon 全链）。
  2sql 路径逐字节不变（mode 缺省走原循环）。② `selftest.py` 组 9 正反例：
  scaffold 剥离不误吞真 DELETE / 结构红例三形态（缺尾 commit、begin 前非
  commit、计数不配）/ A 漂尾绑定 vs B 原子同 key 判绿 / 镜像对不跨组误配 /
  A 侧末行孤儿语句判红。③ `run-difftest.sh` `WORK_TYPE=2sql|rollback|stats`
  （产物目录后缀 无/`-rb`/`-stats`）：步骤 4 裁判旗标直传、rollback 产物
  存在性闸改 `rollback.*.sql`；步骤 5 子命令映射 to-sql/flashback；stats =
  冒烟（裁判 stats 留档 go-stats/ 不参与退出码 + 我方 to-sql 基线语料 +
  两报表存在 + Σinserts+updates+deletes（跳 `#` 尾注，列 5/6/7）== 同流
  to-sql DML 行数内联 python 断言）；步骤 6 传第三参 rollback（仅 rollback）；
  步骤 7 离线回放对 rollback 同跑（flashback --schema-file + diff -r）。
  ④ 白名单登记 4 条：ALW-RB-COMMENT-DRIFT / ALW-RB-SCAFFOLD /
  ALW-RB-WARN-HEADER / ALW-RB-TMP-NAME（含头文件 SET NAMES 沿用说明）。
- **实跑暴露并修复的 T3/T5 挂空缺口**：`flashback --schema-dump` 被 parse
  受理但 `run_flashback` 无消费点（步骤 7 离线回放因此缺 schema.json，
  ENOENT 实红）——修复取「补消费」（与 run_to_sql 同款收口，5 行），
  tests/flashback.rs 用例 7 `flashback_honors_schema_dump` 先行判红再转绿；
  与 c522c33 对 to-sql inert 旗标「拒旗标」裁定同类的取舍论证写在测试
  doc 注释（回滚与正向同为 SQL 文本产物，参数面同构 → 补消费非拒受理）。
  stats 的 `--schema-dump`（CommonArgs 天然带入、无 SQL 产物）仍为惰性
  旗标——未动，登记待 T8/T9 裁定。
- **实跑计数（本任务内，全 GREEN，逐条原样）**：
  `WORK_TYPE=rollback make difftest` → `comparator selftest: 9/9 groups ...
  OK` + `groups A=21 B=21 aligned=21 green=21 red=0` + `OK difftest(rollback)
  8.0: diff-green + replay-byte-identical`，exit 0；
  `WORK_TYPE=stats make difftest` → `stats smoke: report total=36 to-sql
  DML lines=36` + `OK difftest(stats-smoke) 8.0: reports present + DML
  totals reconcile`，exit 0；
  `make difftest`（2sql 回归）→ `groups A=21 B=21 aligned=21 green=21
  red=0` + `OK difftest 8.0: diff-green + replay-byte-identical`，exit 0。
  结构闸真实性突变自检（非 CI 路径）：对真实 `flashback.3.sql` 删尾行
  commit; → `STRUCT-RED ... scaffold count mismatch: begin=15 commit=15
  segments=15` + `STRUCT-RED ... missing tail commit`，red=2 exit 1。
  真实件 scaffold 形态证实：rollback.3.sql 首行=语句+漂尾注释、零 begin;
  行；flashback.3.sql 首行 SET NAMES、commit;=16/begin;=15（=事务段 15+1
  口径含首悬空尾 commit）。
- 全量闸：`cargo test` 294 绿（+1 = flashback_honors_schema_dump）、
  clippy --all-targets 净、fmt 净。
- 遗留/对后续影响：① stats 冒烟维度的列序断言（5/6/7）绑定现报表
  字面形，若 T8 改窗体列须同步；② `_rb_struct` 仅在含 scaffold 文件上
  触发——`--no-keep-trx` 产物（无 scaffold）结构面恒平凡通过，如 T8 要
  钉该形态需另立断言；③ worktree 内 `reference/` 为实体拷贝（软链会让
  go build `-o ../../tools/bin/...` 相对路径落到主库——实踩）。

### P2 Task 8: compat 矩阵扩展（flashback×4 + stats 冒烟×2 → 14 用例）

- 做了什么：`tools/compat-matrix.sh` 的 `run_case` 接 WORK_TYPE 维度——
  每版本 plain 默认捕获的同套 datadir 流程**续跑** `WORK_TYPE=rollback|stats`
  的裁判+我方+比较器（run-difftest 自含产数，与 to-sql 族同数据同源）；
  `docs/compat/matrix.md` 增「P2 Task 8 追加族」节（结果列 = tsv 第 3 列
  逐字抄录）。范围裁决：CKSUM=none 与 V1ROWS 特殊用例不扩 rollback/stats
  变体（理由登记于 matrix.md「范围裁决」条）；stats 5.7/8.4 有意不跑
  （spec §3.6 省时裁决，聚合与服务器版本无关，5.6+8.0 两点覆盖）。
- **单轮真实跑 14/14 PASS**（commit `a2cebca`，2026-09-21，日志
  `out/compat-p2-full.log` + `out/compat-results.tsv` 14 行，tsv/log/产物
  mtime 13:41:56–13:44:53 互证）。逐用例数字（T9 本轮对 artifact 复验）：
  flashback-5.6 `PASS groups A=19 B=19 aligned=19 green=19 red=0`、
  flashback-5.7/8.0/8.4 同式 21/21；stats-5.6 `report total=32 == to-sql
  DML lines=32`、stats-8.0 `36 == 36`。scaffold 计数实证（T9 现场 grep
  `out/difftest-*-rb/rs/`）：5.7/8.0/8.4 `flashback.3.sql` begin=15/
  commit=16、5.6 `flashback.4.sql` begin=14/commit=15；A 侧（Go 裁判）
  四版本 begin=commit=0（上游 KeepTrx 无旗标绑定恒 false 的实证，
  context.go:126/184-233）。证据位置：`out/compat-results.tsv`、
  `out/compat-flashback-*.log`、`out/compat-stats-*.log`、
  `out/difftest-{5.6,5.7,8.0,8.4}-rb/`、`out/difftest-{5.6,8.0}-stats/`。
- T7 移交风险处置：① `_rb_struct` 组合闸未触发（本矩阵 14 用例全默认
  keep-trx）；② stats `--schema-dump` 空转旗标 → T9 补消费收口；
  ③ type 39 未触发（矩阵件无 PARTIAL_JSON）。
- 评审：Approved（零 Critical；1 Important + 2 Minor 为文档债 → **本表
  T9 节点清偿**：matrix.md「与 Go KeepTrx 缺省一致」措辞失实已在 T9 的
  matrix.md「口径勘误（T9 复审修正）」段勘误——我方默认开是有意超越、
  非上游缺省；stats 结果行非逐字 → T9 改为 tsv 第 3 列逐字抄录 +
  「守卫适用面登记（T9）」条）。Important 程序债：本节点即 T8 缺席
  节点的 T9 补记（用户常设要求：每任务一节点）。

### P2 Task 9: 文档收口 + DoD 对账 + 性能回归闸

- 做了什么（轮1 截断 + 轮2 收尾合并为一棵未提交树 → 本提交）：
  ① README：三形态特性矩阵行、吞吐行如实挂 P2 finding（R11 措辞，无
  「无回归」宣称）、快速上手补 flashback/stats 两子命令**实测例句**
  （`--uri` 在线形态，出自 tools/run-difftest.sh 第 5 步同构；本轮真跑：
  `flashback done: events=21, statements=36, files=1, errors=0` +
  `stats done: events=60, … skipped=0`，产物 out/quick-{flashback,stats}/）、
  上游差异清单追加 16–22 七项（P2 族）、修复悬空引用「差异 21」→ 22、
  `make difftest/compat` 注释与 selftest 组数（8→9）同步。
  ② matrix.md：P2 追加族结果列逐字化 + keep-trx 口径勘误（T8 评审
  Important 清偿）+ `_rb_struct` 守卫适用面登记。
  ③ docs/bench/p2.md 新建：DoD-4 回归闸全文（原始 −14.9% → 环境 −8.3% +
  代码 −3.2%、95% CI 跨 0；测量陷阱三条；复现脚本）——R11 裁决落文档。
  ④ 代码收口三项：`run_stats` 补消费 `--schema-dump`（b487a31 同构、
  TDD 用例 6 先红后绿，Err 路径不落半成品）；`--on-error stop` 拒绝措辞
  flashback-only（T5 挂账 nit：stats 无该旗标，tests/cli.rs + config.rs
  单测双钉 + 顺序钉桩）；tools/flashback-reconcile.sh 原子
  `SHOW MASTER STATUS` 单取 (File,Position)（rotate 竞态）+ after-dump
  `|| true` 摘除（吞失败→空文件假绿）+ 产物留档口径注释（T6 挂账清偿）。
  ⑤ **B.4 上游一致性四风险族裁决**（逐项实读 reference/my2sql-go，
  结论=一修三平/挂账，详见下节 DoD-附）：
  (a) GTID→begin 折叠：上游 MySQL GTID(33/34) 在 com.go:153-155 default
  分支 C_reContinue → **不喂** StatChan（file.go:220-221 continue 先于
  :274）；唯一折叠 = MARIADB_GTID_EVENT（stats_process.go:131-135
  sql="begin"），MariaDB 超范围（spec §1/D5）。我方 source.rs Gtid 状态
  透明 + prepare 不派发 = **一致，不改**（legal MySQL 事务恒有 BEGIN
  query 定界，两侧记账同构）。
  (b) 非派发事件与 interval tick：上游 tick 对**每个喂入事件**判定
  （stats_process.go:247-257），喂入集 = 过滤后 rows ∪ **任意 QUERY_EVENT**
  ∪ XID（com.go:144-151 query/xid 分支零过滤 + file.go:274-281）——
  被 --db/--table/--dml 过滤的 rows 不喂故不 tick（我方 prepare:504 过滤
  先于 Fact，一致）；**但 DDL/`use`/空文本 QUERY 上游喂入并冲刷窗口、
  重设锚点，我方历史实现完全不派发 = legal-input 真分歧**（binlog 中段
  DDL 跨 interval 边界时窗口行切分不同）→ **Rust 修复（TDD）**：
  `StreamEvent::Tick`（src/stats/mod.rs）+ prepare 非关键字 QUERY 派发
  Process + emit 映射 Tick（src/pipeline/mod.rs）；RED=`tests/stats.rs`
  用例 7 `stats_misc_query_ticks_window_like_upstream` 断言 windows
  (2,0)≠(3,0)（修复前实测），GREEN=7/7 stats 全绿、既有用例 golden
  零扰动（其 fixture 无杂 QUERY）。真 8.0 冒烟复跑：36==36 不变
  （捕获数据全部同秒、无跨界，tick 修复在该数据上不可见——纯加法
  保序派发，`stats done: events=` 51→60 = +9 条 DDL tick）。
  (c) duration `saturating_sub` vs Go uint 回绕：上游
  stats_process.go:200 `StopTime - StartTime`（uint32，commit 早于首行
  即回绕成 ~4.29e9 巨值 → :201 `>= longTrxSecs` 必命中垃圾「超长事务」）；
  锚点 `Timestamp + printInterval` 同型（:180,255）。我方 saturating 钳制
  （src/stats/mod.rs feed/write）→ 仅**非单调时间戳输入**（损坏/人为篡改）
  可见分歧；stats 威胁模型不含手工恶意 binlog（且 Go 侧回绕值本身即
  垃圾），legal-input（事件 ts 随位点非降）逐字节同 → **不改，挂账**。
  (d) 被过滤表的窗口行/markers：上游 rows 过滤即不进窗口
  （com.go:119-140 → file.go:220 先于 :274）；begin/commit markers 两侧
  **都不受** db/dml 过滤（query/xid 零过滤 vs 我方 prepare 标记派发先于
  :504 过滤检查）；整事务被过滤时上游 StartTime==0 守卫不发 biglong 行
  （stats_process.go:196 注释「the rows event may be skipped by
  --databases --tables」）vs 我方 `bl_start_time > 0` 同位守卫
  （src/stats/mod.rs feed Commit/Rollback 分支）= **一致，不改**。
  ⑥ 本 HANDOVER：T8/T9 节点（本节）+「P2 DoD 对账」节 + 挂账清单更新。
- 测试：全量 `cargo test` 296 通过 / 0 失败 / 1 ignored（lib 265 + cli 6 +
  e2e 9 + flashback 7 + fuzz_seed 2 + stats 7），
  `cargo clippy --all-targets -- -D warnings` 净、`cargo fmt --check` 净；
  三门均在本轮 Tick 修复后复跑。
- 遗留/对后续影响：p1.md `5.903 s` 笔误（真值 5.093 s，p2.md 注记为权威）
  移交集成方一行修；bench 判定工装（governor/taskset）与 --dml×stats
  裁判维度归 P3+；详见挂账清单。

## P2 DoD 对账（spec §6，Task 9 收尾）

> 取证纪律：每条 = 证据命令 + 结果摘要。**引用既往真实跑**时显式标注
> commit 与 artifact 位置；本轮（T9 工作树）能便宜复跑的均已复跑。

1. **差分与矩阵**（spec §6.1）——
   - `WORK_TYPE=rollback make difftest`：**T9 本轮真复跑 exit 0**（工作树
     含全部 T9 代码改动）：`comparator selftest: 9/9 groups ... OK` +
     `groups A=21 B=21 aligned=21 green=21 red=0` +
     `OK difftest(rollback) 8.0: diff-green + replay-byte-identical`；
     产物 `out/difftest-8.0-rb/` 重生成（B 侧 scaffold begin=15/commit=16
     复验不变）。
   - `WORK_TYPE=stats make difftest`：**T9 本轮真复跑 exit 0**
     （`stats smoke: report total=36 to-sql DML lines=36`）——B.4(b) tick
     修复后冒烟无回归的直接证据。
   - 变异红（结构闸非睡死）：引 T7 真跑（`51f79ae` 节点）：删真实
     `flashback.3.sql` 尾行 `commit;` → 2×STRUCT-RED、exit 1。
   - `make compat` 14 用例：**引 T8 真实跑（commit `a2cebca`，未在本轮
     复跑——整矩阵需 docker×4 版本产数 ~4 分钟 + 判定面无 T9 解码改动；
     显式引用）**：`out/compat-results.tsv` 14 行全 PASS（逐字抄录见
     docs/compat/matrix.md P2 追加族表），日志 `out/compat-p2-full.log`。
   - T6 活库正逆对账：引 `e5b8d30` 真跑（未复跑，人工触发非常驻）：
     `baseline checksum = 3944497573 == after-apply checksum = 3944497573`
     + 剔噪逐行 dump 全等；持久 artifact `out/flashback-reconcile/`
     （baseline.sql/after.sql/rows/flashback.log/binlog）。
2. **完整性立场落地**（spec §6.2）——三 hard 规则：
   `sqlopen::dml::tests::flashback_rejects_padded_dropped_columns_as_event_error`（a/Padded）、
   `flashback_missing_value_error_hints_row_image_full`（b/Missing，含
   `binlog_row_image=FULL` 提示语）、c=由 a 拦截（无键全列 WHERE 与 P1 一致）；
   真件证明 `tests/e2e.rs::real_capture_flashback_minimal_image_hard_errors`
   （000003 MINIMAL → stop 整跑 Err / skip 计 1 跳续产）。默认 stop：
   `config::tests::flashback_defaults_and_flags`。skip 告警链：
   `tests/flashback.rs::flashback_skip_marks_header`（`-- WARNING` 头行
   逐字节）+ e2e 真件事先核。DDL 排除告警：`flashback_ddl_excluded_with_summary`
   + T9 快速上手真跑实测（9 条 DDL/query excluded stderr 汇总）。
   证据命令：`cargo test`（本轮 296/296 绿）。
3. **keep-trx 逐字节 golden（差分独立）**（spec §6.3）——
   `src/flashback/reverse.rs` 单测 `reverse_bytes_byte_equal_upstream_keeptrx_quirk`
   （首部悬空 `commit;`、事务边界注入、文件尾 `commit;`——逐字节复刻
   rollback_process.go 语义）+ `no_keeptrx_emits_pure_reverse_without_scaffold`
   + `tests/flashback.rs::flashback_e2e_multi_trx_bytes`（threads 全等）；
   运行 = `cargo test` 同闸。差分侧独立证据 = 上条 compat scaffold 计数。
4. **stats 报表 + bench 闸**（spec §6.4）——双报表 + JSONL 逐字节 golden：
   `cargo test --test stats` 7/7（单元 golden = e2e 同串、T9 新增用例 7
   tick 语义）。事实流零延迟：to-sql bench 见 **docs/bench/p2.md**——
   DoD-3 绝对线 threads=8 = 88.352 MiB/s（92.6 MB/s）≥ 40 MB/s **PASS**；
   §6.4 回归闸 R11 裁决：原始 −14.9% 归因环境 −8.3% + 代码 −3.2%
   （配对 95% CI [−2.4%, +9.0%] 跨 0）→ **不 STOP；但如实挂「未判定
   finding」，不写「无回归」**；判定工装（governor/taskset）留 P3/P4。
5. **文档收口**（spec §6.5）——README 三形态矩阵行 + 快速上手实测例句 +
   上游差异清单 16–22（P2 七项：flashback 命名/记录原子化/keep-trx
   默认开+开关/WARNING 头与 SET NAMES/stats 表序确定化/DDL 排除策略/
   stats 不裁判差分）；docs/compat/matrix.md 14 用例 + 口径勘误 +
   守卫适用面；本文件 T1–T9 节点齐 + 本节 DoD + 挂账更新。
   P1→P2 行为差异入差异清单 = 差异 16–22（承接 P1 清单 1–15）。

## P3 Task 0: 协议 spike（mysql crate binlog feature 六问，闸）

- 提交：`826e2c9`（spike）+ 修复轮 `ba53aad`（三源一致），merge `84bf581`。
  产物 = `examples/repl_spike.rs`（throwaway 诊断样例，按 T8 裁决保留，
  `src/` 零引用）+ spec §2 六问结论勘误回填。
- 六问全实答（关键口径，均真机 8.0.46 实测）：① 入口实为
  `Conn::get_binlog_stream(self, BinlogRequest)`（**消耗 Conn** → repl 与
  元数据必须物理两连接）；`Event::write` 重建字节与磁盘件 `dd`+`od`
  **逐字节相同**（TABLE_MAP/WRITE_ROWS/QUERY 均验）→ 喂我方解码器零分叉，
  无需 §9 降级；② CRC32 由 crate 剥（`checksum()` 存原 4B），走 `Event::write`
  重建含 CRC 完整帧后 repl 路径不调 strip_checksum、与 file 含 CRC 校验直兼容；
  ③ 流首恒 fake-rotate+FDE（合成帧 header log_pos=**0**、payload=请求文件/
  起点；`is_fake()` 查 payload==0 故对合成首帧恒 false——判别只能靠
  ARTIFICIAL(ts=0)+header0+seq0+流首位置）；EOF 换件 ROTATE 亦合成
  （ts=0/pos=0/payload=4）→ **位置链禁经合成头、以请求 pos 种子**；
  ④ 心跳无 BinlogRequest 入口，退路 `SET @master_heartbeat_period=<ns>`
  预升级语句成立且优于预期：v1 心跳(0x1b) **header log_pos=活的主库写位**
  （573729 实证=当时 SHOW MASTER STATUS），checkpoint 推进不消费其位点；
  ⑤ caching_sha2 与 native 双认证全通；**URI query 参不透传**——`ssl-mode`
  直接 `Unknown URL parameter` 硬错（P3 不提供 TLS → README 差异 26）；
  ⑥ 断链两形态：优雅终止（docker restart）=迭代器**静默 None 无 Err**
  （与 stop-EOF 不可分 → 分类器 stop 前任何 None 一律按断链重连）；
  硬断（docker kill）=一次 `Err(IoError)` 后流中毒。
- 评审：Needs fixes → 轮1 `ba53aad` 全闭合（原 Critical：fake rotate 头
  log_pos 判 4 vs 真值 0，以捕获件裁定；原 Important：报告心跳位点口径
  自相矛盾，同源裁定）。挂账 T7：心跳线形 5.6/5.7 复核（T7 已收口，
  见该节；静默窗帧形仍 8.0-only → 新挂账）。

## P3 Task 1: repl CLI/Config 面（validate_repl + run_repl 壳）

- 提交：`c29b81b`，merge `366287d`。第四子命令 `Command::Repl(ReplArgs)`
  （CommonArgs+SqlTextArgs flatten + `--server-id` 必填无默认 +
  `--resume-file` + `--heartbeat-secs` 默认 30 + `--to-stdout`）；
  `validate_repl`：uri 必填（`--schema-file` 不可替代）、resume×显式位点
  「位点来源歧义」硬错、resume×to-stdout 互斥、heartbeat 0..=3600
  （datetime×非空 start-file 的第三对互斥在 T5 修复轮并入——validate
  终态见 src/config.rs 逐臂钉测）；
  dispatch 走 `WorkType::Repl`（与既有惯例一致，评审实核）；main.rs
  run_repl 壳。合并后 301 测试/clippy/fmt 绿、src/binlog 零 diff。
- 评审 Approved 零 Critical/Important。裁决记录：`--to-stdout` 字段新增
  采纳（spec 承诺逐语句 flush，拒绝组合测试需要该字段）；ReplSpec 不创建
  （Interfaces 约束面无此形状）；**start_pos 默认 4 → 裸默认=now 需以
  「start_file 空」为哨兵**（记为 T5 前置事实，T5 已带 live 钉）。
- Minor 挂账（不修，终审视野）：bare repl exit-2 断言不具判别力（同测试
  help 断言兜底）、heartbeat 错误文案缺 `repl:` 前缀、歧义案中英混排
  （为钉「歧义」子串）、validate_repl 重设 keep_trx/on_error 默认=防御性
  重复（与 validate_stats 同风格）。

## P3 Task 2: FrameStream transport + ReplSource（文件同构帧）

- 提交：`a7cebb5` → merge `3057324`（与 T3 的 mod.rs 并集按预定裁决解决，
  零意外）→ 评审 Needs fixes → 修复轮 `cd2d3fe` + scoped re-review：
  I5–I9 全 ADDRESSED、零断言删改。合并后全量 273+ 绿、binlog 冻结空。
- 交付：`src/repl/transport.rs`（FrameStream：mysql BinlogStream → 帧迭代，
  ReplError 面）+ `src/repl/source.rs`（ReplSource 实现
  `pipeline::source::EventSource`，喂 `Event::write` 重建帧）。两测互钉：
  重建帧 vs FileReader 逐字节同构 +
  `tests/repl.rs::synth_frame_export_is_byte_equal_through_repl_source`
  跨源钉；修复轮补两硬错分支负例——CRC
  负例仅翻尾 CRC 字节保长度、帧长自洽负例 Parity(false)+Crc::Off 隔离，
  均经去功能化双向验证。
- 接口裁决（入 T5 前置事实包）：`ReplError::Disconnect(String)` 新增
  （None-drop 语义无既有变体可表达）；trait Err 冻结为
  `BinlogError::InvalidData("repl: …")`、ReplError 经
  `ReplSource::transport_error()` 旁路供 T5 分类；双物理连接（heartbeat 预升级需独立
  setup 连接）。
- 挂账：1236 双义（purged vs server-id 冲突）split-by-message → T5 消费；
  对抗服务器形态（Minor3/4）在信任边界内不修；TDD 次序偏差（实现先于
  首跑红）评审明示 accept（编译级红+断链行为红存活）。

## P3 Task 3: Writer 流式刷盘/防覆盖 + checkpoint 原子档

- 提交：`cd9da06` → 修复轮 `dc1a9f4`（re-review clean：I1–I4 ADDRESSED）。
  merge `4bbfa3d`。全量 34+281 绿。
- 交付：Writer 流式模式开关（事务提交边界刷出，file 模式路径零扰动）+
  no-clobber 闸（`create_new` 原子归零 TOCTOU）+ `src/repl/checkpoint.rs`
  （serde_json 单对象、tmp+rename 原子替换、`write_atomic`/`read_verify`、
  written_files 对账）。修复轮：`read_verify` 豁免自家 `.{ckpt}.tmp` 崩溃
  残留（skip 不 unlink；异名 stray 当时硬 Stale，新测试钉——**终审 FIX B
  改判**：未登记多实物降为 warn 放行，缺档 Missing 仍硬错，见后文挂账）；written_files
  段名校验 `CpError::Malformed`（T4/T5 dispatch 需补 catch-all 臂）；
  crash≠power-fail 耐久注记。
- LOW drift 登记不修：目标为目录时 create_new 走 EISDIR 原始错而非钉文案
  （仍是硬错不覆盖）。

## P3 Task 4: Runner 泵泛化 + 提交边界 checkpoint 水位

- 提交：`313e9a1`。评审 Approved 零 Critical/Important-blocking。
- 交付：`run_pump` 经 `dyn EventSource` 泛化（原 pump_one_file 抽取重构，
  file 模式**字节面不变**——逐函数归一化比对核实、e2e 守卫）+ 水位数学
  （commit 边界才推 checkpoint：pop-before-flush 只在失效路径多滞后不说
  谎；flush→snapshot→write 序在码；水位对 Reorder pop 契约双向无洞）。
- 交后续注记（均已兑现/入档）：threads>1 水位用例 T6 必补（→ T6b mtw 件，
  并挖出 pump_parallel 真缺陷）；pump-Err 与 drain-Err 主次吞次=T5 运维面；
  opening_binlog 回退污染 file 模式=当前不可达（ckpt_out 永不上弦），T5
  禁把 arm 逻辑上提；run_live 返回后 ckpt_out 保持 Some（复用 Runner 换
  src 续泵是预期姿势）。

## P3 Task 5: run_repl 装配（三定位/resume/退避重连/SIGINT）

- 提交：轮2 `0247f92`（轮1 达 150 轮上限截断，WIP 全存活盘点入台账）→
  评审 Needs fixes → 修复轮 `7fea609` + scoped re-review 全 ADDRESSED
  零附带损伤（338 测试绿）。
- 交付：`run_repl`——位点三态定位（now 哨兵 live 弹版 / file+pos / datetime
  二分+逐事件过滤）、checkpoint resume（read_verify 四臂对账）、指数退避
  重连 1s→30s 封顶+抖动无限次、终止类硬错映射（1045 拆独立句、1227 家族、
  1236 按 msg 拆 Purged vs ServerId 冲突、server-id 冲突 3-strike 秒断门）、
  SIGINT drain（Ctrl-C → 完整事务落盘 → checkpoint → exit 130）、双物理
  连接、`ReplEnv` 测试缝（8 文件 ~1300 行装配 + list_binlogs）。
  live 两件（now/purge）真跑绿 15.02s；三门+冻结全过。
- 修复轮两项 Important：I1 resume 跑把更新写回被消费的旧 resume.json
  （毁上一轮 written_files 审计清单）→ **resume 落新 dir、旧档不可变**、
  文案对齐；I2 `probe_first_ts` 空闲当前件可永挂（缓解注记方向反了）→
  探针限界（PROBE_CAP=90s）。裁决入册：datetime+start-file 共存改 validate
  硬错（T1 面缺口）；3-strike 门按 spec 字面（登记 T7 矩阵 1236 敏感性风险，
  T7 实测未触发）。
- Minor 挂账（终审视野，其中三条转正入「遗留/挂账清单」）：live 测试失败
  路径容器/DB/temp 泄漏、ctrlc 进程单次安装、Purged 列恒 None（后随
  `SHOW BINARY LOGS` 第三列正名 = Encrypted，T6b P1' 修复）、
  pipeline/mod.rs 已 2600+ 行（repl 装配块 ~570 逻辑行可迁
  src/repl/assembly.rs——plan 钉了调用点故不迁）。前注 T6：N1 静默主库
  日期件 Ctrl-C 最长 (log2 files)×90s 后落地；N2 heartbeat>85s 退化保守
  （左移零损向）；N3 同路径词法守卫豁免面内。

## P3 Task 6: repl live e2e 套件（等价性总闸 + kill-9/restart/矩阵件）

- 拆单：6a=等价性总闸+`tools/repl-e2e-lib.sh`+Makefile `repl-test`
  （提交 `6a78347`，5 件对齐）；6b=kill-9 两跳+restart+位点/停止/心跳/
  threads>1 水位矩阵（轮1、轮2 先后触 150 轮闸，轮3 提交 `2941dfb`）；
  评审合并过一次门 → Needs fixes → 修复轮 `aba2753` → scoped re-review
  CLEAN 零附带损伤。
- **等价性总闸（P3 核心不变量）真机首过**：同窗混合 DML（2 表/JSON/BLOB/
  中文/10 行事务/回滚事务）repl==file `to_sql.3.sql` **11016B==11016B、
  sha256 同 `93937bc1…`**，窗口 `mysql-bin.000003:6617..17807`
  （ledger Task 6a 行；`tests/repl.rs::repl_stream_equals_file_mode_byte_for_byte`）。
- 轮1/2 挖出两个 live-only 真缺陷并 TDD 钉死：P1' `src/metadata/store.rs`
  `SHOW BINARY LOGS` 第三列 8.0 实为 Encrypted 'No' 串 → `Row::get::<u32>`
  panic 炸穿 datetime 定位（`col_u32` from_value_opt 非 panic 通道 + 单测）；
  P2' `src/repl/source.rs` rotate 链复位口径：真 rotate 后链留旧档尾位 →
  假干净收尾吞新档全部事件（改名即复位 payload 新档位点，红钉
  `real_rotate_renames_chain_so_eof_switch_cannot_falsely_stop`）。
  控制方首轮亲跑 8/2 的两红根因：F1=harness 只扫 stderr 而 tracing 默认
  写 stdout（生产无恙，测试面修）；F2=`pump_parallel` 真缺陷——源静默时
  阻塞 `next()` 不 reap → threads>1 水位停摆（relay thread + 20ms
  recv_timeout 修 + 红钉单测 `parallel_watermark_advances_while_source_idle`）。
  评审确认三块 live-only 生产改动全部正确（col_u32 非 panic、rotate 复位
  不可回归、pump relay 关停序+终帧语义等价）。
- 评审 Important：reconcile n>m OOB（重复容忍比对首次真调用即炸）→
  min(n,m) 界定 + 合成 dup 钉测 `reconcile_dup_shape_synthetic`（评审员
  独立复现红）。轮1 顺手清 5 Minor（vacuous 自检删、kill9 torn 路径可达
  且跨源真比对、Encrypted 正名、Bt 泄漏、lib 窗口契约注）。
- 最终全量：`make repl-test` **10 passed / 0 failed / 476.76s**（T6b 轮3
  控制方亲跑；restart 件 2×、mtw 件 3× 无翻转）；非 live 341 绿/clippy
  -D/fmt/binlog 冻结空（T8 本轮复跑 342 绿 = +fix 轮新测）。
- 挂账新增（fix 报告 + 评审注，转「遗留/挂账清单」）：live dup 路径无天然
  覆盖（restart 跑 n==m，由合成测独扛——勿把绿 restart 读作 dup 覆盖）；
  `parse_blocks` 对首行 SET NAMES 残缺静默丢而非报 torn。

## P3 Task 7: compat 矩阵 repl 族（5.6–8.4 × repl → 18 用例全绿）

- 交付：`tools/compat-matrix.sh` 扩 work=`repl`（`run_case` 第 5 参派发
  `repl_run`）+ repl 族 4 用例 + `REPL_ONLY=1` 调试入口；容器/灌流全复用
  T6 `tools/repl-e2e-lib.sh`（零第二套）。矩阵文档 = docs/compat/matrix.md
  「P3 Task 7 追加族」节：18 行 tsv 逐字 + 无 Go 裁判理由一句（spec §8）+
  逐版本注记。
- 用例形态 = Task 6 等价性总闸的矩阵化：钉 start 位点 `f0:p0` → **后台
  灌流器**（tag=A\<序号\>）→ 灌流中 `FLUSH LOGS`（窗口跨档，T6 lib 契约
  「跨档矩阵归 T7」兑现）→ `repl --stop-datetime D`（D=服务器钟+12s，
  repl/file 共用同一停止谓词）优雅收尾 → B 段补灌（必须不可见）→
  `docker cp` 取段 → file 模式同窗同旗标 to-sql → `diff -r -x resume.json`
  零放宽比较 + DML 指纹非空闸。
- 全量真跑：`make compat` 单次整跑 **18/18 PASS**（既有 14 + repl 4；
  `out/compat-results.tsv` 逐字入 matrix.md）。repl 行：
  `repl-5.6 equivalent=175335` / `repl-5.7 equivalent=146979` /
  `repl-8.0 equivalent=143582` / `repl-8.4 equivalent=164487` bytes，
  四版本 files=2（全部真跨档）、repl/file events 两侧逐例相等。
- 版本面实测收获：① 心跳 `SET @master_heartbeat_period` 在 **5.6.51/5.7.44
  均被接受**（降级告警 0 次/版本；spec §2 勘误-4 的 5.6/5.7 未证面补齐），
  8.4 位点经 `SHOW BINARY LOG STATUS` 改口（lib 既有分支）；② server-id
  逐例 7200+序号 递增，1236 同-id 踢线敏感性未触发（登记免疫方式）；
  ③ 灌流器 5.7/8.0 各遇一次 1213 死锁（单条 UPDATE 二级索引扫描 ×
  autocommit 插入；两侧消费同一份已落盘 binlog，等价性口径零影响，不重试
  不放宽）；④ 版本差零命中：无任何字节分歧需要解释。
- 生产代码改动：**无**（纯 harness/docs 增量；`src/binlog/` 零 diff 门禁
  保持空，repl 侧无修复需求——无 TDD 红绿事件发生在本任务）。
- 环境事实补记：新 worktree 首跑 difftest 族需 `reference/` 与
  `tools/bin/` 存在（两者 git-ignored；缺 reference 时 repl 族不受影响、
  14 旧族在步骤 [2/7] 即红——本轮实证过一次）。
- 三门：`cargo test` / `cargo clippy --all-targets -- -D warnings` /
  `cargo fmt --check` 收尾复跑（本轮结果见提交信息与 task-7 报告）。

## P3 DoD 对账（spec §10 + plan 验收清单，Task 8 收尾）

> 取证纪律同 P2 节：每条 = 证据命令 + 结果摘要；引用既往真实跑显式标注
> commit 与出处；本轮（T8 工作树 @ 基线 `9fe74b3`）能便宜复跑的均已复跑。
> docker 件不重跑（T6b 轮3 / T7 为控制方亲验真跑，台账逐字引用）。

1. **等价性总闸 + 矩阵**（spec §10-1 / §7-1）——
   - 8.0 主件：引 `6a78347` 真跑（ledger Task 6a 行，不重跑 docker）：
     窗口 `mysql-bin.000003:6617..17807` 混合 DML（2 表/JSON/BLOB/中文/
     10 行事务/回滚事务），repl==file `to_sql.3.sql` **11016B == 11016B**、
     sha256 同为 `93937bc1deaccf8b412f7359aea7dc648636e6cd9ca23a15f0b27062bd808052`。
   - `make repl-test` live 全量：引 T6b 轮3 控制方亲跑（/tmp/p3-t6b-live-run.log
     证据入 ledger Task 6b 行）：**10 passed / 0 failed / 476.76s**；
     restart 件 2×、threads>1 水位件 3× 复跑无翻转。
   - `make compat` 18 用例：引 `500b6cf` 单轮真跑 18/18 PASS（既有 14 同场
     复验；tsv 逐字已入 docs/compat/matrix.md）——repl 行 equivalent=
     **175335 / 146979 / 143582 / 164487 bytes**（5.6/5.7/8.0/8.4），
     四版本 files=2 全真跨档、repl/file events 两侧逐例相等。
2. **kill-9 resume + 容器重启重连**（spec §10-2）——
   `tests/repl.rs::repl_kill9_resume_zero_loss`（两跳：已提交事务零丢 +
   重复仅整事务、written_files 可界定）与 `repl_survives_server_restart`
   （docker restart → 退避重连从 checkpoint 续拉、终产物与基准等价）均在
   上条 repl-test 全绿轮内；checkpoint 单测族（write_atomic/read_verify/
   四臂对账/水位）随 `cargo test` 同闸（T8 本轮 342 绿）。
   注（勿误读）：live dup 路径无天然覆盖（restart 跑 n==m），由合成测
   `reconcile_dup_shape_synthetic` 独扛（新挂账 #6）。
3. **三门 + 全量回归 + 冻结门禁**（spec §10-3 / 验收清单 1、4）——
   T8 本轮真复跑（本工作树 @ 9fe74b3）：
   - `cargo test`：**342 passed / 0 failed / 11 ignored**（lib 306 +
     cli 8 + e2e 9 + flashback 7 + fuzz_seed 2 + repl 非 live 2 + stats 8；
     ignored = lib 1 + repl live 10，live 组实跑=上条 10/10）；P1/P2 全量
     回归含于其中 + compat 18/18 同场（既有 14 族 `500b6cf` 复验）。
   - `cargo clippy --all-targets -- -D warnings`：净（T8 本轮）。
   - `cargo fmt --check`：净（T8 本轮）。
   - **bench 免跑核验（plan T8 Step 2）**：`git diff main..HEAD -- src/binlog/`
     与 `git diff main..HEAD -- benches/` **双双空**（T8 本轮实测，`wc -l` =
     0/0）→ P3 无解码热路径与基准面改动，**吞吐数字按 P2 态引用**
     （docs/bench/p1.md 基线 + docs/bench/p2.md 回归闸 finding），P3 不重跑 bench。
4. **文档收口**（spec §10-4）——README：repl 矩阵行 ✅（18 用例口径）、
   快速上手 repl 例句 7)/8)（flags 逐一对 ReplArgs/validate_repl 实核；
   provenance=live 套件同款，不冒充本轮手跑）、差异登记 23–27（承接 P2
   清单 1–22；spec §8 五则 + TLS 件入册）、`make repl-test` 入差分测试节、
   examples/repl_spike.rs「诊断样例」注；本文件：T0–T6 节点补档（替换
   原「未逐条入档」注）、本节 DoD、挂账消费 + P3 新挂账 7 条；
   matrix.md 18 行（T7 已落，本轮仅链接）。
5. **卫生门禁**（spec §10-5 / 验收清单 6）——`reference/` 零改动
   （git-ignored 从未入库：`git log main..HEAD --name-only | grep
   ^reference/` 空，T8 本轮实测）；`grep -rn repl_spike src/` **空**
   （T8 本轮实测，exit 1；spike 件保留于 examples/）；spec §2 字节口径
   已经 Task 0 spike 实证回填（勘误随 `ba53aad` 同批提交）。
6. **P2 挂账两项消费完毕**（验收清单 5）——`60a9019`（merge `2e26bca`，
   评审 Approved 且实跑复核：26==26/0/0、36==36 磁盘独立重点数、mtime 序
   合法，台账原话）：① stats Err 路径 JSONL 收口（drop-on-error：Err 路径
   两 jsonl **不存在**、txt 两件保持既有口径；单元 + 集成双红→绿，集成件
   `tests/stats.rs::stats_err_path_leaves_no_partial_jsonl`，成功路径字节
   不变）；② difftest `--dml insert`×stats 冒烟维度（真跑 `[5.6/7]
   inserts=26 updates=0 deletes=0 to-sql(--dml insert) INSERT lines=26`，
   同轮回归 `[5.5/7] report total=36 == to-sql DML lines=36`）——台账两处
   挂账条已勾销（见下表 T8-debt 消费注）。
   注：①的失败运行会毁上一份好 JSONL（create 即 O_TRUNC + Drop unlink），
   合同合法「absent」——运维向一句话入新挂账 #1。
7. **全分支终审与合流复跑**（终审 = whole-branch review @ `590bac1`，
   修复轮 `8ef0279..2cfd916` + E 精化 `3ffac7e` + 措辞微修 `b683e57`）——
   终审六件 A(F)/B/E/C(文档随实现)/D/F：默认 threads>1 源错误冻结水位、
   written_files 滞后假 Stale、空闲 master Ctrl-C 无界、bare-Disconnect
   互踢永重连、裸 repl 写 CWD 无 checkpoint、no-clobber 文档过陈述；
   scoped re-review 判 A/B/C/D/F ADDRESSED、E PARTIAL（3-streak 60s 窗在
   封顶退避下数学性误杀 docker-restart 承诺）→ 轮2 改「仅 open-ok 零事件
   裸断计入 streak，refused/有进展复位」并红钉 `master_restart_timeline_not_fast_failed`。
   **合流轮控制方亲跑**（本工作树 @ `b683e57`，/tmp/p3-merge-repltest.log）：
   `make repl-test` **11 passed / 0 failed / 585.90s**（新增空闲-SIGINT 件
   真跑 exit130；restart 件 reconnect #1..#N refused 后恢复、基准 132 块
   前缀全等；threads>1 水位件 92 出样全真事务界；等价件 636224B repl≡file）；
   `cargo test` 非 live **349 passed / 0 failed**、clippy -D 净、fmt 净、
   `git diff main..HEAD -- src/binlog/ benches/ reference/` **空**、
   `grep -rn repl_spike src/` **空**。

## P3 挂账消费/新增一览

| 项 | 状态 | 证据 |
|---|---|---|
| P2 挂账：stats Err 路径 JSONL 头收口（P2 T6 节点遗留②/P2 T9 移交） | ✅ 消费（`60a9019`） | DoD-6；挂账清单原条勾销 |
| P2 挂账：`--dml`×stats 裁判维度（P2 T9 挂账） | ✅ 消费（`60a9019`） | DoD-6；挂账清单原条勾销 |
| P3 新挂账 7 条（T5/T6/T7 终审视野转正） | ⏳ 入册 | 「遗留/挂账清单」节 P3 块 |

## P4a 任务节点日志（T1–T5）

> spec `docs/superpowers/specs/2026-09-22-my2sql-rs-p4a-quality-lanes-design.md`，
> plan `docs/superpowers/plans/2026-09-22-my2sql-rs-p4a-quality-lanes.md`。四 lane
> 并行（T1–T4，各自 worktree + 独立 CARGO_TARGET_DIR，评审+修复轮后 ff 合入
> worktree-feat-p4a）+ T5 合流串行。合入序：T1 `52058c7` → T2 `1bee320` →
> T3 `7774558` → T4 `b4ba03c`。节点事实逐字引各 lane 报告
> （`.superpowers/sdd/2026-09-22-my2sql-rs-p4a-quality-lanes/task-<n>-report.md`）。

### P4a T1（Lane A）：cargo-fuzz 正式接入 + 解码器 3 处溢出闸（开闸条款首启用）

- 提交 `48e5134`（本体）+ 评审修复轮 1 `cae7319` + 轮 2 `df515d0`/`8dcdc7f`；
  合入 `52058c7`。
- 形态：`fuzz/` 独立 workspace（根 Cargo.toml `exclude = ["fuzz"]`，主构建/三门/
  矩阵零感知）；靶 `decode_event`（单事件字节→header→strip→type 路由）+
  `event_stream`（多事件流 + TrxStateMachine 语义面，钉 trx_id 不回退）；seedgen
  确定性产 **40 件语料**（7 畸形 + 3 合法基线 × crc0/crc1 × 两靶目录各 20，禁随机
  入仓、构建期 `self_check_legal` 断言 legal 件 crc1 腿可解）；`tools/fuzz-min.sh`
  闸 = 每靶 300s 并行 + 语料隔离（副本跑 `out/fuzz/<t>/corpus/`，仓库语料只读）+
  禁假绿（逐靶取退出码，非 0 且无 crash 件判 FAIL）。
- **2 发真 panic 实抓（本役核心发现）**：
  ① `panicked at src/binlog/table_map.rs:79:19: attempt to add with overflow`
  （parse_table_map `pos + n_cols`，0xFE LNE 声明 u64::MAX；tmin 51B）；
  ② `panicked at src/binlog/table_map.rs:299:31: attempt to add with overflow`
  （decode_optional_meta TLV `pos + l` 同源；tmin 562B）。TDD 红钉先行
  （`cargo test --test fuzz_seed` FAILED：`tm_ncols_overflow.bin: 解码层 panic
  （DoD-4 违例）`）→ `checked_add` 修复 → 绿；12 件历史 crash/minimized 输入逐件
  重跑不再崩。
- 第 3 处闸（`proto.rs::read_lns`）= **preventive**：评审轮 2 审计链实测——闸回退态
  下 seed6 磁盘件因表名声明长漂移（声明 8 实给 9）解析拐进 parse_charset
  （`InvalidData("... TLV type 254: truncated length prefix")`），**够不到 read_lns
  加法**。登记为已知事实（fuzz 语料面），seed6 不修；该闸红钉改由定向单测
  `read_lns_u64max_declared_len_is_too_short_not_panic` 独扛（回退态逐字红
  `panicked at src/binlog/proto.rs:57:27: attempt to add with overflow`，带闸绿）。
- 靶面勘误（评审轮 1）：简报「XID_EVENT (15,16)」笔误——15=FORMAT_DESC、16=Xid、
  33/34=Gtid，按 `src/binlog/event.rs` 权威表路由；event_stream 的 query_text
  `sv_len` 错读 err_code（body[9..11]→body[11..13]）同轮修复（此前 QUERY 喂状态机
  半面形同未跑）。
- 规模/计数：lane 修复轮 1 正式 300s×2 逐字 `Done 21569979 runs in 301 second(s)`
  （decode_event，exec/s 71661，cov 672）/ `Done 5190208 runs in 301 second(s)`
  （event_stream，exec/s 17243，cov 1805）；crash **0/2**。三门 350 passed/0 failed
  （=349 + read_lns 新钉）；`cargo +nightly fuzz build` Finished；动 src/binlog/
  强制的 difftest 8.0 `groups A=21 B=21 aligned=21 green=21 red=0` + replay
  byte-identical；compat 18 件移交本役 T5 亲跑（见 T5 节点）。

### P4a T2（Lane B）：tools/shadow-replay.sh 影子库三段闸

- 提交 `4edbea9` + 评审修复轮 1 `df76581`（唯一入库文件即脚本）；合入 `1bee320`。
- 闸形：前向（活库混合 DML→同窗 to-sql 产物灌影子库 == 主库后态 P1）/ 逆向
  （后态影子灌 flashback 产物 == 前态 P0）/ 往返（前向影子再逆灌 == P0），
  逐表 CHECKSUM TABLE 等值 + 行级 mysqldump diff **双腿独立**；另有 setup 起点
  自证闸（SETUP_* == P0/P1）、窗口边界钉（P1doc1/P1last 在场、P0 指纹缺席）、
  `assert_stmts` 硬闸（`statements=115 == DML 行=115` 升格为机闸）、静绿保险丝
  （NULL checksum/空表清单/空行集一律红）、`SHADOW_NEGCHECK=1` 验钞机（注入
  单行漂移，前向闸必须红）。容器 p3e2e-*-p4ash* 自建自清，trap 先于容器启动，
  INT/TERM→130 无泄漏；证据目录 `out/shadow-replay/<VER>/` 版本命名空间。
- 8.0 主件（anchor=live，spec 原形态，exempt-json=0）逐字：P0
  `t_doc=2877097027 t_ord=2830880655`、P1 `t_doc=2572327458 t_ord=2651036437`；
  FWD_SHADOW==P1、REV_SHADOW/RT_SHADOW==P0 与主库 live 值**逐位互换相等**，
  五闸全 `GATE GREEN ... checksum-equal=2/2 exempt-json=0 (rowdiff bytes=0)`，
  窗口 `mysql-bin.000003:12814..24356`。
- negcheck：漂移被双腿独立抓到（checksum 腿 `1/2` t_ord 2651036437→2408576875；
  行级腿唯一漂移行 `(1,'P0sku1',1,NULL)`→`(1,'P0sku1',2,NULL)`）→
  `NEGCHECK: expected mismatch observed` 反转 exit 0。
- **5.7 裁定（控制器背书，本役确认推广口径）**：5.7 `CHECKSUM TABLE` 对含 JSON 列
  的表**不可定值**（同表零写入连读四次 3642844648/3642844648/3876613648/3843993102；
  live/dump 回灌/ALTER 重建三态互异而逻辑内容逐字相同）——上游物理 JSON 未初始化
  填充进 checksum，等式无任何合法构造方式。处置：期望侧 = **REF clone**（快照回灌
  `p4ashadow_ref`）+ `gate_rows` 硬锚（REF↔live 行级逐字相等，锚不住整轮红）；
  豁免**仅限 checksum 腿的含 JSON 列表**（本 seed=t_doc），t_ord 与 live 直接互等
  （FWD 2651036437==P1、REV/RT 2830880655==P0）；**行级腿零豁免**。5.7 冒烟
  anchor=clone 全绿（19s）。8.0 面 a/b 均不启用，spec 原形态。
- T5 合流加固（`92a62ea`）：VER 白名单 `5.6|5.7|8.0|8.4` 在**任何 rm -rf / OUT
  路径插值之前**校验，非白名单直接 exit 2 报错退出（实测 `9.9` 与注入串
  `8.0; rm -rf /` 均拒于任何目录操作与容器启动之前，零残留）。

### P4a T3（Lane C）：difftest P4A 三列形真机捕获（测试债列形缺口销账）

- 提交 `3bf7fc1`（+in-lane 修复轮）；合入 `7774558`。文件面：新增
  `tools/gen-data-p4a.sql`、`tools/p4a-roundtrip.sh`、`docs/p4a-findings.md`；
  `tools/run-difftest.sh` 仅 +P4A env 门（默认关；P4A=1 且 VER≠8.0 → 拒 exit 2）。
  **src/ 零改动**。
- `P4A=1 VER=8.0 make difftest` 逐字：`groups A=14 B=14 aligned=14 green=14 red=0`、
  `to-sql done: events=14, statements=20, files=1, errors=0`、步骤 7 离线回放
  `diff -r` 空。三形**全部 Go 支持、无违例形**：ENUM 300 成员（2B packlen，
  meta 直证 `f702 f702`，序号 255/256/300 在场）、GEOMETRY POINT/LINESTRING/
  POLYGON + SRID 4326（`E610` LE 前缀字节保真首实抓，裁决 7 路径）、LONGBLOB
  70,000/280,000 B（`WRITE_ROWSv2 event len=280044` 4B prefix 跨页）。
- 机械核验（比较器之外）：两侧 20 条 DML 在既有三类打印差异归一后 md5 相同
  （`7be7a1a149b8ace7285ed8c1bd22739f` 双方）；19 个 hex payload token 序列全等。
  自 roundtrip（`tools/p4a-roundtrip.sh`）三表 CHECKSUM clone==main：
  2619814406 / 4084198807 / 1767911749 + 行级 diff 空（逐表真实数据行 4/2/1）。
- **ENUM 保真措辞（本役钉死）**：两侧输出均为 **1-based 序号**（既有裁决 D4 现状），
  不是成员名字符串保真——README/FINDINGS 引用口径以此为准。
- seed 实踩勘误：8.0 对 SRID 4326 按「纬,经」轴序解释首坐标（`POINT(179.9 -89.9)`
  报 ERROR 3617），改 `POINT(-89.9 179.9)` 保边界极值；简报「210,000B」为乘数
  笔误（实 280,000B）。首版 FINDINGS「7/5/4 数据行」系 mysqldump 裸 `--` 样板行
  计入的计数笔误，修复轮以双侧 `grep -ac '^INSERT'`>0 硬闸重生成 4/2/1
  （checksum 三值不变）。
- 回归：plain `make difftest` 21/21 不回归（P4A 默认关零字节变化）；compat 矩阵
  本件不触发重跑（独立于 compat 家族，spec §3；src/binlog 零改动）。

### P4a T4（Lane D）：5.6/5.7 idle 窗心跳 live 件（repl 家族 11→13）

- 提交 `24865ab`；合入 `b4ba03c`。`tests/repl.rs` +两 `#[ignore]` live 件
  （`repl_idle_heartbeat_5_7` / `_5_6`），`Bt::new_ver` 版本参数化为加性扩展；
  `tools/repl-e2e-lib.sh` **零改动**（所需版本门/降级探测/capture 均既有面，
  「仅加性」合同以改 0 处满足）。非 live 面 `cargo test --test repl` →
  2 passed / **13 ignored**（家族基数 11→13 就位）。
- 定向真跑（共享 8.0 门容器）：**2 passed / 0 failed / 228.89s**，跑后零泄漏容器；
  5.6 件追平对账 `repl 38 块 ≡ file 38 块（9847B 语句流）`、5.7 件
  `38 ≡ 38（9795B）`；两件 60s 静默零 `repl: reconnect #`、终档 ∈ 提交界。
- **帧形实测（spec §4 待证面钉死）**：5.6.51 与 5.7.44 idle 窗心跳帧形完全同型
  = **HEARTBEAT_LOG_EVENT v1（0x1b）**，ts=0、size=39（19B 头 + 16B 日志文件名 +
  4B 尾）、20s 节奏、header log_pos = 静默期主库**活写位点**（5.7=154、5.6=120，
  与连入时 SHOW MASTER STATUS 等值）；流首 0x04 fake-rotate 为合成帧、内部消化。
  **spec §4 的 fake-rotate 续命回退分支 = 死枝**（两版本均真发心跳帧，60s 静默
  过零假断链是帧在场的直接结果）。
- **SHOW 面事实**：`binlog_heartbeat%` 全局变量面在 5.6.51/5.7.44 均为**空集**
  （变量族属 8.0 面）；心跳周期实走会话级 `SET @master_heartbeat_period` 载荷
  （5.6.51 不止 SET 被接受，且真产帧——T7 事实的帧级补证）。T5 本轮已把
  件内 5.6 println 的 brief 冻结 fallback 措辞替换为该实测真相
  （`92a62ea`，仅字符串，断言零触碰）。

### P4a T5（合流 lane，本轮）：make 入口 + 接线修复 + 全量回归 + 文档收口

- 独占面履约：Makefile/README/HANDOVER 单写者；carry-over B 独立小 commit
  `92a62ea`（shadow-replay.sh VER 白名单 `5.6|5.7|8.0|8.4` 先于**任何** rm -rf /
  OUT 插值校验、非白名单 exit 2，实测 `9.9` 与注入串均零触碰拒入；
  repl.rs 5.6 println 换实测真相，仅字符串、断言零触碰）。
  Makefile：`.PHONY` + 两目标行 `fuzz-min`（`bash tools/fuzz-min.sh`，
  FUZZ_TIME 透传）/ `shadow-test`（`bash tools/shadow-replay.sh ${VER:-8.0}`）。
- **接线修复（回归唯一红，合流特权内）**：GATE 4 `make difftest` 首跑 rc=2——
  `tools/run-difftest.sh: 行 122: ./target/debug/my2sql-rs: 没有那个文件或目录`。
  归因 = **遗留接线 bug 而非任何 lane 回归**：spec §6 要求各 lane 独立
  `CARGO_TARGET_DIR=/tmp/p4a-*`，而 run-difftest.sh / compat-matrix.sh 硬编码
  `./target/debug/my2sql-rs`（四 lane 恰因本地默认 target/ 有产物而未踩）。
  修复：两脚本头部 `RSBIN="${CARGO_TARGET_DIR:-$ROOT/target}/debug/my2sql-rs"`，
  8 处引用全接 RSBIN（同一口径，判据零变化）；修后 difftest 双模 + compat 18 绿
  （接线修复独立 commit `edb2148`）。
  `gen-bench-binlog.sh` / `p4a-roundtrip.sh` / `flashback-reconcile.sh` 同类
  硬编码**不在回归路径**，挂小账不修（见挂账清单）。
- **全量回归六闸（串行，CARGO_TARGET_DIR=/tmp/p4a-merge，逐字日志
  `/tmp/p4a-merge-gate*.log` + 戳记 `/tmp/p4a-merge-stamps.txt`，stop-on-red；
  difftest/P4A/compat 因 my2sql-dt-8.0 共享全程零重叠）**：
  1. 三门（03:54:19Z→03:54:35Z）：`cargo test` = **350 passed / 0 failed**
     （lib 314 / cli 8 / e2e 9 / flashback 7 / fuzz_seed 2 / repl 2+13 ignored /
     stats 8 / doc 0）；`clippy --all-targets -D warnings` rc=0；`fmt --check` rc=0。
  2. `make fuzz-min` rc=0（→04:01:09Z）：decode_event
     `#22296408 DONE cov: 670 ft: 1747 corp: 439/56Kb exec/s: 74074` /
     `Done 22296408 runs in 301 second(s)`；event_stream
     `#6456918 DONE cov: 1842 ft: 9067 corp: 2023/1009Kb exec/s: 21451` /
     `Done 6456918 runs in 301 second(s)`；`[fuzz-min] OK 0 new crashes`；
     仓库语料 `fuzz/corpus/` 跑后 git 零变化（隔离合同成立）。
  3. `make shadow-test` 三态全绿：8.0（37s）五闸 `GATE GREEN ...
     checksum-equal=2/2 exempt-json=0 (rowdiff bytes=0)` +
     `ASSERT OK: to-sql statements=115 == DML lines=115`×2，ck 与 lane 真跑**逐位
     等**（P0 t_doc=2877097027 / t_ord=2830880655；P1 t_doc=2572327458 /
     t_ord=2651036437；窗口 12814..24356）；negcheck 注入漂移 t_ord
     2651036437→2408576875 → `GATE RED[FWD_SHADOW vs P1] ... checksum-equal=1/2`
     + `NEGCHECK: expected mismatch observed`（禁假绿闸成立）；5.7（19s）
     anchor=clone、exempt-json 仅 [t_doc] 且只在 checksum 腿、行级腿零豁免
     （REF rows=11 lines / rowdiff bytes=0），live 侧 t_doc ck 与 lane 轮互异
     ——**5.7 CHECKSUM TABLE 非定值事实再次实证**，t_ord 两值仍逐位稳。
  4. `make difftest`（修后重跑）：`groups A=21 B=21 aligned=21 green=21 red=0` +
     `OK difftest 8.0: diff-green + replay-byte-identical`；
     `P4A=1 make difftest`：`data script: tools/gen-data-p4a.sql`、
     `groups A=14 B=14 aligned=14 green=14 red=0` + 同 OK 行。
  5. `make compat`（04:06:42Z→04:11:34Z）：**18/18 PASS** `COMPAT MATRIX: ALL GREEN`。
     非 repl 14 件与 P3 完全同值（groups 19/21 体系、stats total=32/36、
     v1rows/caching_sha2 在场）。repl 族字节数对 P3 基线漂移：
     **173206/140559/140336/164489** vs 基线 175335/146979/143582/164487——
     **裁定为合法窗口差异、非回归**，证据链：①该 4 件 = stop-datetime（服务器钟
     +12s）墙钟窗口 × 后台灌流器（≤200 轮，内容纯 f(tag,round)），本轮捕获
     events=506/411/410 vs P3 的 510/429/419（少 1–3 轮）；②8.4 本轮 events=480
     **与 P3 完全同**，+2B 全部来自 `--add-extra-info` 每语句注记
     `# datetime=... startpos=... stoppos=...` 的位数漂移（实拆
     out/compat-repl-work-8.4/file/to_sql.3.sql 129 条注记行确认）；
     ③每 run 内 repl≡file 总闸 `diff -r` 逐字节等全 PASS（双侧 events/statements
     互等 506/1457、411/1190、410/1189、480/1392；files=2、跨档 span 在场、
     heartbeat_set_degrades=0）；④T1 三处 checked_add 闸在合法事件面不可达，
     非 repl 14 件零漂移即反证。
  6. `make repl-test`（04:11:35Z→04:25:36Z）：**13 passed / 0 failed /
     0 ignored / 823.02s**（P3 11 件轮 585.90s + 5.6/5.7 idle 两件 ≈ +237s，
     与预期 ≈+230s 相符）；跑后零泄漏容器。5.6 新措辞在本轮 --nocapture
     面完整实证打印（`92a62ea` 未单独重跑 5.6 件即由此覆盖）。
- 开闸条款台账（本轮复核）：src/binlog/ 全役改动 = T1 三处溢出闸
  （table_map.rs:79/:299 红钉真 panic 先行 + proto.rs::read_lns 预防闸独立单测），
  本轮 compat 18 + 350 非 live + difftest 双模 + repl 13 全绿 = 事后全量回归
  合同兑现；T2/T3/T4 src/ 零改动。
- **终审轮 FIX A–F**（全分支终审裁定 With-fixes → 单修复轮已全部落地，base tip
  `1d519ac`；范围 2149ce1..<新tip> 由 controller 记录）：A README 差异 9 种子计数改
  版本稳定措辞「全部 `tests/fuzz_seed/` 种子（现 7 件）逐字节钉死」（对齐
  `tests/fuzz_seed.rs` `SEEDS.len()` 全量断言）；B Makefile 加 `.NOTPARALLEL:` +
  一行注释（difftest/compat/repl-test 共享 my2sql-dt-8.0 固定名容器，`make -j`
  不得互踩；scripts 零改动）；C `tools/fuzz-min.sh` crash-* 归档后 prev/ 非空即显式
  `WARNING: N 件历史 crash 于 prev/（本轮计数不含上轮）`（防归档后重跑绿被读成
  从未 crash）；D `tests/repl.rs` idle 模块注释删「注记走 fallback 文本」旧措辞、
  同步 `92a62ea` 实测口径（SHOW 变量面为空；会话级 SET @master_heartbeat_period
  实生效），纯注释、断言零触碰；E `tools/p4a-roundtrip.sh` 硬编码
  `./target/debug/my2sql-rs` 换 `RSBIN="${CARGO_TARGET_DIR:-$ROOT/target}/debug/my2sql-rs"`
  （`edb2148` 同口径，lane 脚本补漏）；F `src/binlog/proto.rs::read_lns` 与
  `src/binlog/table_map.rs` TLV 循环登记 32 位 `as usize` 截断注记（目标支持面 =
  64 位，checked_add 闸已补全 64 位溢出表面），纯注释、零逻辑改动。本轮为
  注释/接线-only：免跑 difftest（无 Go oracle 需求），三门重跑 =
  `cargo test` **350 passed / 0 failed** + clippy `--all-targets -D warnings` rc=0 +
  `fmt --check` rc=0（`CARGO_TARGET_DIR=/tmp/p4a-fixr1`）；`bash -n` 两脚本 +
  `make -n fuzz-min` 通过。
- **合流亲跑节点（终审修复后，controller 亲跑于 tip `8130c6b`，
  `CARGO_TARGET_DIR=/tmp/p4a-merge`，DoD-7 同型；逐字日志
  `/tmp/p4a-e2e-{test,fuzzmin,difftest-p4a}.log`）**：
  1. `cargo test --no-fail-fast`：**350 passed / 0 failed / 14 ignored**
     （非 live 全量口径 = 349 + read_lns 常驻红钉 1 件）。
  2. `FUZZ_TIME=20 make fuzz-min`：rc=0，双靶（decode_event / event_stream）
     各自 `exit=0 crashes=0` + `OK 0 new crashes`，跑后 `git status
     --porcelain` 仅本 HANDOVER 编辑（scratch corpus 在 out/fuzz/，不污染
     仓内语料）。终审者已独立重放 seedgen：`src/bin/seedgen.rs` 产出
     2×20 件与仓内 `fuzz/corpus/*` **逐字节等**（语料确定性背书，DoD-4 的
     「P4 corpus 同源可再生」成立）。
  3. `P4A=1 make difftest`（VER=8.0）：三列形捕获
     `groups A=14 B=14 aligned=14 green=14 red=0` + 末行
     `OK difftest 8.0: diff-green + replay-byte-identical`。
  - **compat repl 字节基线口径裁定（本轮入账）**：repl 族绝对字节数
    `175335/146979/143582/164487 → 173206/140559/140336/164489` 漂移，
    归因 = 窗口轮次事件数差（506/411/410 vs 510/429/419；8.4 同 events 下
    +2B 为注记位数差），且每-run 内 repl≡file `diff -r` 全等恒成立。裁定：
    **绝对字节数不再作跨 run 基线**，等价性以每-run repl==file 逐字节为准。
  - 区间记录：`2149ce1..8130c6b` = spec/plan 2 件 + 四 lane `--no-ff` 合入
    （52058c7/1bee320/7774558/b4ba03c）+ T5 合流链（92a62ea/edb2148/1d519ac）
    + 终审修复轮 8130c6b。本节点后收口 = ff main → `cargo test` 快验 →
    tag `v0.4.0-p4a` → push（总授权内，P3 先例径）。

## P4a DoD 对账（spec §1–§5 + §6，T5 收尾）

- **§1 Lane A**：① `cargo +nightly fuzz build` 过 ✅；② 300s×2 真跑 0 新
  crash ✅（本轮 GATE 2 `Done 22296408` / `Done 6456918`、`OK 0 new crashes`）；
  `make fuzz-min` 常态闸 ✅（T5 落）；③ crash→红钉修复 ✅（2 真 panic 全收；
  read_lns 预防闸以定向单测独扛）；seed6 漂移入 known-not-fix 登记 ✅。
- **§2 Lane B**：三段（前向/逆向/往返）+ 逐表 CHECKSUM + 行级 diff 双腿 +
  SHADOW_NEGCHECK 禁假绿 ✅（本轮 GATE 3 三态）；主件 8.0 + 5.7 冒烟 ✅；
  证据逐字入档 ✅；`make shadow-test` 落 + VER 白名单加固（92a62ea）✅。
- **§3 Lane C**：三列形（ENUM>255 / GEOMETRY / LONGBLOB>64KB）真机捕获
  14/14 全绿 ✅；Go 裁判三形全支持、无违例形 → 零新增行为差异，差异清单
  续号 28 登记三形等口径（ENUM 保真 = 1-based 序号，非成员名）✅；
  测试债「列形缺口」销账 ✅。
- **§4 Lane D**：5.6/5.7 idle live 两件 ✅（本轮 GATE 6 全家 13/13 真跑）；
  帧形（HEARTBEAT v1 0x1b / ts=0 / size=39 / 20s / 活写位点）入注记 ✅；
  spec fake-rotate 回退分支判死枝登记 ✅；5.6 SHOW 面空 + 会话 SET 实通道
  入注记且件内措辞已换实测真相 ✅。
- **§5 T5**：Makefile 两目标行 ✅ / README 矩阵行 + 差异续号 ✅ /
  HANDOVER T1–T4 节点 + 本 T5 节点 ✅ / DoD 对账（本节）✅ /
  全量回归六闸全绿逐字入档 ✅。spec §5 书「repl-test 11」为规划时点数，
  实际家族 = **13**（T4 +2），按 §6「live 件增者如实计数」以 13 记。
- **§6 纪律**：解码器开闸条款台账见 T5 节点尾条 ✅；`reference/` 只读 ✅；
  禁虚账——上文全部计数/时长逐字出自 `/tmp/p4a-merge-gate*.log` ✅；
  每 lane 三门 ✅；独立 `CARGO_TARGET_DIR` ✅（本轮 /tmp/p4a-merge，且
  run-difftest.sh/compat-matrix.sh 接线已改按同口径）；测试基线滚动：
  非 live = **350**（≥349 达成），live repl = **13**。

## P4b 任务节点日志（T1–T5）

**P4b「性能面」= 主 spec §9 P4 行的性能侧子集**（质量侧已在 P4a 收官）：把
P1/P2/P3/P4a 全部在册性能挂账（spec §0 七条）统一消费。派发序 = T1/T2/T4 并行
→ T3（依赖 T1 工装 + T2 报告）→ T5 合流单写者。本役**唯一 src/ 性能改动 =
mimalloc 全局分配器**（机动项 O2 尝试后不显著回滚、不在历史）。逐字数字全部可在
`/tmp/p4b-t{1,2,3,4}-*` 源日志与 `docs/bench/p4b*.md` 找到（禁虚账，报告↔日志
冲突时以日志为准）。

### P4b T1（Lane 0）：`tools/bench-ab.sh` A/B 判定工装 + 挂账 #1/#4/#6/#7（commit `b6844fe` + fix `2c670b9`）

- **交付:** `tools/bench-ab.sh`（端到端 A/B 判定器：taskset 钉 P 核 0-11、
  governor 只记录、两侧各交替 N=5 轮 wall-clock、median+MAD 判显著，契约
  exit 0=不显著/1=显著/2=用法错）；`tools/gen-bench-binlog.sh` RSBIN 接线
  （`RSBIN="${CARGO_TARGET_DIR:-$ROOT/target}/debug/my2sql-rs"`，:24 定义/:116 消费）；
  `data/bench` 读侧 symlink 复用主仓缓存（零写入）；`/tmp/p4b-t1-ab7.md` 挂账#7
  全样本表。**零 src/ 改动**（仅 tools/ 两文件）。
- **Ruling（评审）:** b6844fe 判 FAIL(fixable) → fix 轮 `2c670b9` 逐项修 finding 1–5
  + bench-profile 同型 finding 6（崩溃 to-sql 被 `$(run_side)` 吞成有效样本 →
  run_side 显式 `|| rc=$?` + 调用点 `|| exit 1`；`--rounds 0/abc` 空样本 nan 假绿 →
  参数守护 `case` + verdict 空档/数值闸；悬空选项值 rc=1→rc=2；exit-0 零产物不计样本；
  `_median` `LC_ALL=C` 防呆 + `medianA<=0` 守卫）。**统计核数学与输出 printf 格式串逐字
  未动**（唯一 +/- = sort 前加 LC_ALL=C），入档判定无需重跑（当前数学核重投样本 100%
  复现）。selftest 扩至 8 例（+例5–8 为回归针）。
- **证据逐字（A/A 恒等冒烟 `/tmp/p4b-t1-aa.log` rc=0）:**
  `median A=7.6126s B=7.6771s … delta=+0.847% (0.0645s) thr=0.2902s` /
  `throughput MiB/s A=69.5 B=68.9` / `VERDICT: not-significant (B slower-than A)`。
- **证据逐字（挂账#7 P1 vs P2 N=5 `/tmp/p4b-t1-ab7-r5.log` rc=0）:**
  `median A=6.7459s B=6.9356s MAD A=0.2439 B=0.2857 delta=+2.812% (0.1897s) thr=0.5714s` /
  `throughput MiB/s A=78.4 B=76.3` / `VERDICT: not-significant (B slower-than A)`
  → 钉死不显著（<3.2% 弱信号落噪声带，N=5 不升级 N=9）。
- **挂账#4 复验（证伪）:** `grep -n '5\.903' docs/bench/p1.md` → 无输出 rc=1；
  `docs/bench/p1.md:35` 已是 **5.093 s**（笔误已被后手修正，本 lane 零改动，
  合同「不触 docs/」）。挂账虚假入账。

### P4b T2（Lane P）：`docs/bench/p4b-profile.md` profile 普查（只测不动，commit `5b6a363`）

- **交付:** `docs/bench/p4b-profile.md`（环境/曲线/perf 面/假设判定/排序表 五段）+
  `tools/bench-profile.sh`（busy 差值法 + comm 跨进程跟踪支撑 ≥30s）。**红线：src/ 与
  既有 tools/ 零改动**（构建/测试在 `git archive c59def0` 干净导出树，免疫脏共享 worktree）。
- **证据逐字（threads 曲线，每档 3 轮中位，taskset 0-11，`/tmp/p4b-t2-run.log`）:**
  `threads=1 median=12.288s MiB/s=43.0` / `threads=2 52.7` / `threads=4 74.8` /
  `threads=8 median=6.125s MiB/s=86.3` → **端到端并行效率 86.3/43.0 = 2.01×**
  （vs P1 挂账 2.5×；收缩全来自 t8 档绝对值 −17%，登记为 X3 待追加实验，未立项）。
- **假设判定（挂账 #2 逐条）:** A1 channel 交接 **证伪**（busy 8.84≈可忙线程数/R 89%，
  等待不主导）；A2 Reorder 等待窗 **证伪**（S 11%/D 0%/线程 10<12 钉核）；
  A3 行级分配上界 **证实（推断级）**（全负载 CPU 工作量 t1 12.2 → t8 54.2 core·s，
  膨胀 4.45× 换吞吐 2.01×，互证挂 T3 mimalloc A/B）；A4 文件读 syscall **证伪**
  （t1 busy 0.99/R 100%/D≈0，page-cache 命中）；A5 锁争用 **证据不足**（无 perf，
  futex 睡眠已排除，on-CPU 自旋与真实分配不可分）。
- **排序表:** O1 mimalloc（固定项）+ O2 worker 行级分配节食（A3 派生，须 O1 后复测）；
  A1/A2/A4/A5 证伪/证据不足项未强行入表（禁为动而动）。perf 面：`paranoid=4` →
  `perf record`/`perf stat` 双双 rc=255 拒绝，降级 = /proc 线程状态采样（函数级热点缺位为
  环境如实处置，`/tmp/p4b-t2-perfprobe.txt`）。
- **门禁:** `cargo test` = 350 passed/0 failed/14 ignored（`/tmp/p4b-t2-cargo-test.log`）。

### P4b T4（Lane R）：pipeline repl 装配块纯搬运 → `src/repl/assembly.rs`（commit `8ff5fe0`）

- **交付:** `src/pipeline/mod.rs` 3473 → **1540 行**；新建 `src/repl/assembly.rs` **1949 行**
  （29 项，按引用图划界：仅被 repl 装配路径引用者移动）；`src/repl/mod.rs` +1
  `pub mod assembly;`。`Runner`/`Emitter`/`open_store` 留守并 `pub(crate)` 化（file 模式
  三形态共用）。对外接口零变化（`pub use crate::repl::assembly::run_repl;`）。
- **Ruling（派发序）:** 本 lane 先行合入（T4 在 T3 前），使 T3 优化 diff 落在稳定结构上
  （spec §4 src/ 面互斥靠串行序保证）。注：实际合入序上 T4 `8ff5fe0` 早于 T1 fix/T2/T3，
  controller 已在派发时裁定。
- **证据逐字（纯 move 机械证明，task-4-report Step 2）:**
  证明 A（移动体逐字节）`diff <(sed -n '242,245p;252,1012p;2309,3473p' before) <(tail -n +20 assembly.rs)`
  → `rc=0` 无输出；证明 B（留守侧）仅 11 处 plumbing（删 6 类失效 import + `Duration` 收窄 +
  插 1 `pub use` + 7 处 `pub(crate)` 前缀），无改名/doc 改写/格式化 churn。
- **证据逐字（行为恒等产物逐字节）:** base(c59def0) vs new `to-sql`+`flashback` 同参
  `diff -r` rc=0，sha1 `49c81bcd…`（flashback）/`7c4c0f73…`（to-sql）base≡new。
- **证据逐字（bench A/A' 抽测，controller 裁定内联法）:** 中位 base 6438ms / new 6517ms
  → Δ **+1.2%**（≤3% 预期带内，to-sql 热路径不经 assembly）。**350 计数硬证**（repl_tests
  21 件全过）：`[lib] 314 + cli 8 + e2e 9 + flashback 7 + fuzz_seed 2 + repl(2/13ign) + stats 8 + doc 0`。

### P4b T3（Lane O）：mimalloc 全局分配器 + 机动项 O2（commit `281735d`，唯一 src/ 性能改动）

- **交付:** `Cargo.toml` `mimalloc = "0.1.52"` + `src/main.rs`
  `#[global_allocator] static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;`
  （3 files changed, 25 insertions）。**偏离 brief 示例（裁定按 crate 实况）：** brief 写
  `mimalloc::Mimalloc`，0.1.52 实际导出 `MiMalloc`（E0425 实证）。
- **glibc A/B（`/tmp/p4b-t3-ab.log`，两侧 sha256 DIFFER）:**
  `median A=5.9613s B=4.3779s MAD A=0.0293 B=0.0331 delta=-26.561% (1.5834s) thr=0.1192s` /
  `throughput MiB/s A=88.7 B=120.8` / `VERDICT: significant (B faster-than A)` rc=1 →
  **显著快 26.561%，保留接入**。
- **musl 复测（挂账 #3 消账，`/tmp/p4b-t3-musl-{base,tip}.txt`，musl-gcc 在册 ×3 中位）:**
  `base-musl median=158.0084s MiB/s=3.3`（≈精确复现 P1 在册 3.4 悬崖）/
  `mimalloc-musl median=8.2347s MiB/s=64.2`（≥50 门）→ **悬崖消账，挂账 #3 = 已解决**（≈19.2×）。
  std 目标 `--no-default-features` 不可用（mimalloc 硬依赖）→ 对照 = 无 mimalloc 的 BASE_T3
  worktree 同法 musl 构建。
- **语义恒等硬证（Step 5）:** base(9970ffb9) vs new(fda6465a) 同参 to-sql+flashback
  `diff -r` 全等（`TOSQL-IDENTICAL`/`FLASHBACK-IDENTICAL` rc=0），sha256
  `a54da851…`（to_sql）/`b4b2e69e…`（flashback）base==new → mimalloc 零语义触点。
- **机动项 O2（Step 6，`/tmp/p4b-t3-o2-ab.log`，base=mimalloc-tip 防双计）:**
  `delta=+0.282% (0.0128s) thr=0.1614s` / `VERDICT: not-significant (B slower-than A)` rc=0 →
  按 spec §3「零胜合法、禁为动而动」**回滚 O2**（含行为钉撤除），**未提交、不在历史**。
- **X3 免责:** mimalloc 把端到端 t8 推至 120.8 MiB/s 顺带越过 103.85 账本线，但**无专属
  第二 A/B**（P1 终审 tip vs 本 tip），**不作「17% 回退被收复」的归因**，X3 继续在册。
- **门禁（全在最终 tip）:** cargo test 350/0/14、clippy rc=0、fmt rc=0、compat 18/18、
  difftest 21/21 + P4A 14/14、语义恒等硬证。

### P4b T5（合流 lane，本轮）：make 入口 + 新基线 + 全量回归六闸 + 文档收口

- **独占面履约:** Makefile/README/HANDOVER/p4b.md 单写者。Makefile `.PHONY` 追加
  `bench-ab`/`bench-profile` + 两目标行（`bash tools/bench-ab.sh $(ARGS)` /
  `bash tools/bench-profile.sh`），`make -n` 双验通过；同 commit 附 `chmod +x
  tools/bench-ab.sh`（评审 nit：兄弟脚本除 gen-bench/p4a-roundtrip/repl-e2e-lib 外均带
  +x；git diff 仅 `old mode 100644 → new mode 100755`，内容零触碰）。
- **`docs/bench/p4b.md` 新权威基线:** 六段汇编（机器口径 / criterion 正式跑 / T1 ab7 /
  mimalloc A/B+musl / 机动项 verdict / T4 抽测）+ 裁定与免责节（mimalloc 硬依赖、X3
  registered-only、T2 2.01× 取代 P1 2.5×、threads 缩放 post-mimalloc 仍开放）+ traceability。
- **正式 criterion 基线（终态 tip，`/tmp/p4b-merge-bench.log`）:**
  `file_to_sql/threads=8/528 MiB time: [4.0550 s 4.1451 s 4.2409 s]
   thrpt: [124.71 MiB/s 127.59 MiB/s 130.42 MiB/s]`（median 4.1451 s → 127.59 MiB/s）；
  `threads=1 time: [10.084 s 10.162 s 10.273 s] thrpt: [51.480 MiB/s 52.042 MiB/s 52.445 MiB/s]`。
  **回归闸判定：127.59 vs 103.85 → +22.86%（更快）→ GREEN**（>5% 劣化 = 红未触发）。
- **全量回归六闸（串行 fail-fast，`CARGO_TARGET_DIR=/tmp/p4b-merge`，逐字日志
  `/tmp/p4b-merge-gate*.log` + 戳记 `/tmp/p4b-merge-stamps.txt`；docker 腿全程零重叠、
  仅自建容器、trap 清理、跑后零泄漏）:**
  1. **三门（07:55:08Z→）rc=0:** `cargo test --no-fail-fast` awk 汇总 **350 passed / 0 failed /
     14 ignored**（lib 314/1ign · cli 8 · e2e 9 · flashback 7 · fuzz_seed 2 · repl 2/13ign ·
     stats 8 · doc 0）；`clippy --all-targets -D warnings` rc=0；`fmt --check` rc=0。
  2. **`FUZZ_TIME=20 make fuzz-min` rc=0（08:03:00Z→）:** `decode_event: exit=0 crashes=0` /
     `event_stream: exit=0 crashes=0` / `[fuzz-min] OK 0 new crashes`。
     （副作用注记：跑后 `fuzz/Cargo.lock` 因 T3 mimalloc 硬依赖被 cargo 就地补入
     `mimalloc`/`libmimalloc-sys` 条目——非回归路径、T5 独占面外，已 `git checkout` 还原，
     移交 controller：fuzz 独立 workspace 锁未随 T3 传播，宜后续单独入账。）
  3. **`make difftest` rc=0（08:06:08Z）:** `groups A=21 B=21 aligned=21 green=21 red=0` +
     `OK difftest 8.0: diff-green + replay-byte-identical`；
     **`P4A=1 make difftest` rc=0（08:07:01Z）:** `data script: tools/gen-data-p4a.sql` +
     `groups A=14 B=14 aligned=14 green=14 red=0` + 同 OK 行。
  4. **`make compat` rc=0（08:07:37Z→08:13Z）:** **18/18 PASS `COMPAT MATRIX: ALL GREEN`**
     （非 repl 14 件与 P4a 完全同值：5.6 组 19/21 体系、stats total=32/36、v1rows/caching_sha2
     在场）；repl 族字节 `175276/144743/140124/162381`（跨 run 窗口漂移，每-run repl≡file
     `diff -r` 全 PASS 兜底，绝对字节不作跨 run 基线——P4a 裁定沿用）。
  5. **`make shadow-test`（8.0）rc=0（08:13:18Z）:** 五闸全 GREEN，逐表 checksum 与 P4a 收官
     **逐位等**（P0 t_doc=**2877097027**/t_ord=**2830880655**；P1 t_doc=**2572327458**/
     t_ord=**2651036437**）：SETUP_FWD vs P0 / FWD_SHADOW vs P1 / SETUP_REV vs P1 /
     REV_SHADOW vs P0 / RT_SHADOW vs P0 均 `checksum-equal=2/2 exempt-json=0 rowdiff bytes=0`；
     `ASSERT OK: to-sql statements=115 == DML lines=115`×2（flashback 同 115）——
     mimalloc(T3)+搬运(T4) 后影子库三段零语义漂移反证。
  6. **`make repl-test` rc=0（08:14:12Z→08:28:05Z）:** **13 passed / 0 failed / 0 ignored /
     2 filtered out / 815.30s**（src/ 动过 = 硬条件，13 件全真跑，逐字名册见 task-5-report），
     跑后零泄漏容器。
- **开闸条款台账:** src/ 全役性能改动 = mimalloc 一行 GlobalAlloc（T3）+ assembly move-only
  （T4）；本轮 compat 18 + 350 非 live + difftest 双模（21/14）+ shadow 五闸逐位 + repl live
  13 全绿 = 事后全量回归合同兑现（P4a 解码器开闸条款扩展至 pipeline 热路径）。
- **区间记录:** `aba8293..` = spec/plan 2 件（d9f16d7/c59def0）+ 四 lane（T4 `8ff5fe0`、
  T1 `b6844fe`、T2 `5b6a363`、T1fix `2c670b9`、T3 `281735d`）+ 本 T5 合流 commit。
  **本节点后收口（ff main → tag → push）= controller 特权，T5 lane 不执行（明确出界）。**

## P4b DoD 对账（spec §7 七条，T5 收尾）

1. **`tools/bench-ab.sh` 存在 + selftest + 恒等 A/A 冒烟逐字入档（挂账 #1/#4/#6/#7）** ✅
   （工装 + 8 例 selftest + A/A not-significant + ab7 复测 + gen-bench RSBIN + p1.md 无 5.903 证伪，见 T1 节点/p4b.md ①③）。
2. **`docs/bench/p4b-profile.md`：曲线 + 热点表 + 挂账 #2 假设逐条判定 + 优化候选排序表** ✅
   （threads 曲线 2.01×、perf 降级 /proc 采样、A1–A5 逐条判词、O1/O2 + X1–X4，见 T2 节点）。
3. **mimalloc 接入：A/B 工装对照 + musl 复测数字（挂账 #3 消账）** ✅
   （glibc A/B 显著快 26.561% + musl 3.3→64.2 MiB/s ≥50 悬崖消账 + 语义恒等逐字节，见 T3 节点/p4b.md ④）。
4. **机动项按各自 commit「行为恒等 + 显著」双证入账（或零机动裁定）** ✅
   （O2 尝试→not-significant（+0.282%<thr 0.1614s）→回滚，未入历史；本役唯一 src/ 性能改动 = mimalloc，见 p4b.md ⑤）。
5. **assembly 搬运完成且 diff 形态审为 move-only（挂账 #5）** ✅
   （pipeline 3473→1540 + assembly 1949，证明 A/B 机械 diff rc=0 + 产物逐字节 + 350 计数，见 T4 节点）。
6. **`docs/bench/p4b.md` 新基线 + 回归闸判定（vs 103.85 ±5%）+ HANDOVER 挂账处置全表** ✅
   （threads=8 median **127.59 MiB/s = +22.86% 更快 → GREEN**；§0 七条逐行销账见下）。
7. **T5 全量回归六闸全绿逐字入档；改动面含 src/ → repl live 13 件全跑** ✅
   （六闸 rc 全 0：350/0/14 · fuzz 0crash · difftest 21/21+P4A 14/14 · compat 18/18 ·
   shadow 五闸逐位 · repl live **13/13 真跑** 815.30s，见 T5 节点 + task-5-report）。

## P4b §0 挂账处置销账表（spec §0 七条逐行）

| # | 挂账（出处） | 处置 | 证据 |
|---|---|---|---|
| 1 | bench 判定工装（P2 挂账 :2040、p2.md 后续动作） | ✅ **落地 + 复评审**：`tools/bench-ab.sh` selftest 8 例 + A/A 冒烟 + FAIL(fixable)→fix 全绿 | `b6844fe`+`2c670b9`；`/tmp/p4b-t1-{selftest,aa}.log` |
| 2 | threads 1→8 并行效率 2.5× 上限「先 profile 再动」（P1 :823） | ✅ **profile done**；优化按 §2 裁定 = T2 归因（A3 证实推断级）→ T3 唯一动刀 = mimalloc；threads 缩放 post-mimalloc **仍开放**（无热路径结构改动，O2 不显著回滚） | `5b6a363`；p4b-profile 曲线/假设判定 |
| 3 | musl 吞吐悬崖 ~3.4 MiB/s（p1.md，P4 :2136） | ✅ **悬崖消账**：mimalloc musl release 3.3→**64.2 MiB/s**（≥50 门） | `281735d`；`/tmp/p4b-t3-musl-{base,tip}.txt` |
| 4 | `docs/bench/p1.md` 笔误 5.903→5.093（:2025 移交一行） | ✅ **证伪入账**：p1.md 无 5.903（grep 空），:35 已是 5.093——挂账虚假/已被后手修正，零改动 | `b6844fe`；`grep 5.903 docs/bench/p1.md`（rc=1 空） |
| 5 | pipeline/mod.rs 2600+ 行装配块迁 `src/repl/assembly.rs`（P3 T8 :2056） | ✅ **搬运**：move-only，逐字节 + 350 计数 + Δ+1.2% 抽测 | `8ff5fe0`；task-4-report Step 2/3/4 |
| 6 | gen-bench-binlog.sh 硬编码 target（P4a T5 :1845/:2122 同族小账） | ✅ **接线修**：RSBIN `${CARGO_TARGET_DIR:-$ROOT/target}`（edb2148 同口径），trace 实证 | `b6844fe`；`/tmp/p4b-t1-genbench-trace.log:5`；gen-bench :24/:116 |
| 7 | P2 回归闸未决：代码增量 −3.2%（CI 跨 0，p2.md） | ✅ **钉死不显著**：bench-ab P1 vs P2 N=5，delta **+2.812% < thr 0.5714s**，不升级 N=9（无「跨 0→显著」证据） | `b6844fe`+`2c670b9`；`/tmp/p4b-t1-ab7-r5.log` + p4b.md ③ |

## 校准记录

- **T9 后校准补丁**（review 驱动，fixture `tests/fixtures/capture_8.0_minimal/` 为
  第二权威）：① ALARM A——event.rs 事件码表勘误（V0=20/21/22、V1=23/24/25、
  V2=30/31/32、GTID=33、ANON=34、PREVIOUS_GTIDS=35、STOP=3；详见 Task 2 节点）；
  ② ALARM B——table_map.rs 接受 8.0 TLV optional metadata（无前导 total_length，
  镜像 fork `decodeOptionalMeta`；未知条目跳过、截断报错；详见 Task 4 节点）；
  ③ value.rs STRING 前奏 if 分支先转 u16 再 `<<4`（原 u8 域移位把 {0x10,0x20,0x30}
  全截成 0，Go row_event.go:1013 口径；该分支从零覆盖 → 新增合成穷举测试）；
  ④ V1 TIME hour 零位左补 `{:02}`（Go `%02d`）。TDD：每项先 RED 后 GREEN；
  test/clippy/fmt 三门全绿。

## 环境事实

- 本机：docker（镜像 mysql:5.6/5.7/8.0/8.4 全部就绪，T17 已拉 8.4）、Go 工具链 /opt/go/bin、cargo/rustc 最新 stable
- 工作区：`/home/cxd/Projects/aiediter/my2sql`
- SDD 台账：`.superpowers/sdd/2026-09-20-my2sql-rs-p1/progress.md`（git-ignored，恢复上下文先读它）
- SDD 台账（P2）：`.superpowers/sdd/2026-09-21-my2sql-rs-p2-flashback-stats/progress.md`
  （git-ignored；任务简报/评审 diff/各任务报告同目录）
- SDD 台账（P3）：`.superpowers/sdd/2026-09-21-my2sql-rs-p3-repl/progress.md`
  （git-ignored；任务简报/评审 diff/各任务报告同目录；live 套件证据
  `/tmp/p3-t6b-live-run.log`）
- SDD 台账（P4a）：`.superpowers/sdd/2026-09-22-my2sql-rs-p4a-quality-lanes/progress.md`
  （git-ignored；各 lane 简报/报告/评审同目录；T5 全量回归逐字日志
  `/tmp/p4a-merge-gate*.log` + 戳记 `/tmp/p4a-merge-stamps.txt`）
- SDD 台账（P4b）：`.superpowers/sdd/2026-09-22-my2sql-rs-p4b-performance/progress.md`
  （controller-private，git-ignored；各 lane 简报/报告/评审 + task-5-report.md 同目录；
  各 lane 逐字日志 `/tmp/p4b-t{1,2,3,4}-*`；T5 全量回归逐字日志
  `/tmp/p4b-merge-gate*.log` + `/tmp/p4b-merge-bench.log` + 戳记 `/tmp/p4b-merge-stamps.txt`）
- **运维注（P4a T1/T3 实踩，T5 入册）：worktree 跑 difftest/compat 需 `reference/`
  本地真实副本（`cp -a` 主仓 `reference/`），严禁 symlink** ——`go build -o
  ../../tools/bin/my2sql-go` 走**物理路径**解析，symlink 会把裁判二进制漏写进
  主仓 `tools/bin/`（产物同源 gitignored，无功能影响但污染主树、掩盖并行隔离）。
  各 lane 并行跑 8.0 差分件时容器名 `my2sql-dt-8.0` 全 worktree 共享，
  **difftest/compat/P4A 必须串行**（T5 回归轮即因此全程单线）。

## 遗留/挂账清单

- [x] ~~P2：flashback + stats（另出计划）~~——T1–T9 全部完成（本表上方
  节点 + 「P2 DoD 对账」节），待全分支终审合入。
- **P2 T9 新增挂账**：
  - [ ] 已知差异（stats，T9-B.4(c)，**不修**）：biglong duration =
    commit_ts − 事务内首 rows_ts，我方 `saturating_sub`（src/stats/mod.rs
    feed 的 Commit/Rollback 分支），上游 uint32 裸减回绕
    （stats_process.go:200——commit 早于 begin 的损坏输入会回绕成 ~4.29e9
    巨值使 `:201 >= longTrxSecs` 必命中垃圾行）；窗口锚点
    `ts+printInterval` 同理（:180,255 vs 我方 saturating_add）。
    仅非单调时间戳（损坏/人为篡改）可见，legal-input-only 论证入册
    （stats 威胁模型不含手工恶意 binlog，对齐 P1 frac_text 先例口径）。
  - [ ] 已修复入册（stats，T9-B.4(b)，**勿回退**）：非关键字 QUERY
    （DDL/`use`/空文本）上游会喂 StatChan 并参与窗口冲刷/锚点重设
    （file.go:274-281 + stats_process.go:247-257；rows 的 db/dml 过滤
    上游同样先于喂入 = 不 tick，两侧一致），我方以 `StreamEvent::Tick`
    对齐（tests/stats.rs 用例 7 逐字节钉 RED→GREEN）。P3 若改 stats
    派发面须保持该集合：喂入集 = 过滤后 rows ∪ 任意 QUERY ∪ XID；
    MySQL GTID(33/34)/TABLE_MAP/FDE 等永不喂入（com.go:153-155 default）。
  - [ ] `_rb_struct` 结构断言守卫适用面仅 **keep-trx（默认）产物**：
    无 scaffold 文件早退 → `--no-keep-trx` 输出结构面平凡通过、不获该
    保护；已登记 docs/compat/matrix.md「守卫适用面登记（T9）」条
    （T8 全 14 用例均为默认形态，范围裁决不扩跑该组合；如需钉该形态
    须另立断言——T7/T8 移交同源风险）。
  - [ ] `to-sql --on-error stop` 拒绝时序：validate_to_sql 的 stop 检查
    先于 build_common → 同时缺 schema 源时用户先见 on-error 条。可辩护
    （旗标语义优先级更高），已按 T5 挂账在 src/config.rs 注释 +
    tests/cli.rs 顺序钉桩收口为**文档化行为**，不再是测试债。
  - [x] ~~**docs/bench/p1.md 笔误待修（移交集成方，一行）**：记录表
    threads=8 用时 `5.903 s` 应为 **5.093 s**（其 criterion 摘录
    `[5.0501 s 5.0927 s 5.1408 s]` 与判定文字为准；p2.md 开头注记为
    权威说明）。不属本分支改动面，故未动。~~——**P4b T1 消费（`b6844fe`，挂账 #4）
    证伪入账**：`grep -n '5\.903' docs/bench/p1.md` 无输出（rc=1），p1.md:35 threads=8
    已是 **5.093 s**——挂账虚假/已被后手修正，零改动（详见「P4b §0 挂账处置销账表」#4）。
  - [ ] stats 报表尾注 `# skipped events: N` 为**自定口径**：上游两报表
    无尾注（stats_process.go:262-265 收尾仅冲刷窗口）——格式/落点由本
    计划简报裁定；比较器已约定跳 `#` 行（T7），该行不受裁判差分保护，
    改动须同步 run-difftest 冒烟内联脚本。
  - [x] ~~`--dml` 过滤与上游 stats 计数一致性**未做裁判差分**：T9 仅源码
    路径核对（上游 FilterSqlLen 于 com.go:75-101 拦 rows、query/xid
    直通 = 我方 prepare 过滤位形），差分维度 P3 再说（承 T7 口径）。~~——
    **P3 T8-debt 消费（`60a9019`）**：`tools/run-difftest.sh` 新增 [5.6/7]
    `--dml insert`×stats 双通道配平（真跑 inserts=26 updates=0 deletes=0
    == to-sql(--dml insert) INSERT lines=26；Σupdates/Σdeletes=0 即 dml
    过滤器跨通道语义一致钉），同轮回归 [5.5/7] 36==36 不变。
  - [x] ~~bench 判定工装（P3/P4）：若要判定 P2 未解析的 −3.2% 代码增量，
    需 `governor=performance` + `taskset` 钉 8 P 核重做 A/B（~30 分钟）；
    threads 1→8 并行效率 2.5× 的 P1 挂账保留。证据 docs/bench/p2.md
    「后续动作」节（T9 移入本条统一索引）。~~——**P4b T1/T2/T3 消费（挂账 #1/#2/#7）**：
    ① 工装 `tools/bench-ab.sh` 落地 + selftest 8 例 + 恒等 A/A 冒烟（`b6844fe`+fix
    `2c670b9`，governor 只记录/无 sudo 环境事实，taskset 钉 P 核 0-11）；② P2 −3.2%
    代码增量**复测钉死不显著**（P1 vs P2 N=5，delta +2.812% < thr 0.5714s，不升级
    N=9，`/tmp/p4b-t1-ab7-r5.log`）；③ threads 并行效率：T2 端到端实测 **2.01×**
    取代 P1 账本 2.5×（`5b6a363`），T3 mimalloc 收 allocator 膨胀（glibc A/B −26.561%，
    `281735d`），但 1→8 缩放 post-mimalloc **仍开放**（无热路径结构改动、O2 不显著回滚、
    X3 registered-only）——见「P4b §0 挂账处置销账表」#1/#2/#7。
- **P3 T8 新增挂账（repl 收尾登记，均不修、入终审/P4 视野）**：
  - [ ] **stats 失败运行会毁上一份好 JSONL**：`60a9019` 的 drop-on-error
    取 create 即 O_TRUNC + Drop unlink——本次 Err 运行不留半成品，但
    **上一轮成功的 JSONL 也一并销毁**（合同合法「absent」语义）。运维需
    知晓：报表以最后一次**成功** run 为准；temp+rename 原子替换留作
    范围决定未做（T8-debt 台账注记同源）。
  - [ ] **live 测试工装失败路径泄漏**（T5/T6 Minor 转正）：repl live 件
    失败时容器/DB/temp 目录不清理（成功路径有 EXIT trap）；ctrlc 处理器
    **进程级单次安装**（同进程二次装配回退默认直杀、130 语义失效——
    生产单 run 无碍，测试同进程多 run 注意）；`SHOW BINARY LOGS` 第三列
    8.0 实为 **Encrypted**（恒 None 的 Purged 语义误读已正名，`col_u32`
    非 panic 通道钉死，列面消费保持）。
  - [x] ~~**pipeline/mod.rs 已 2600+ 行**：repl 装配块 ~570 逻辑行可迁
    `src/repl/assembly.rs`——plan 钉死调用点在 pipeline/mod.rs 故本批
    未迁（纯搬运、无行为变更，留 P4 或终审裁决）。~~——**P4b T4 消费（`8ff5fe0`，挂账 #5）**：
    本役即做它——`pipeline/mod.rs` 3473→1540 行 + 新建 `src/repl/assembly.rs` 1949 行
    （29 项，按引用图划界）；move-only 机械证明（`diff` 逐字节 rc=0）+ 同输入产物逐字节
    + 350 计数 + bench A/A' Δ+1.2%（详见「P4b §0 挂账处置销账表」#5 与 T4 节点）。
  - [ ] **restart 件的 dup 路径无天然 live 覆盖**：重连续拉实测恒 n==m
    （无重复段），at-least-once 重复合并面仅由合成钉测
    `tests/repl.rs::reconcile_dup_shape_synthetic` 独扛——**终审勿把
    绿 restart 读作 dup 覆盖**（T6 评审注逐字）。
  - [ ] `parse_blocks` 对**首行 `SET NAMES` 残缺**静默丢弃而非报 torn
    （T6 fix 轮观察，backlog：语义上是「丢一个必然无 SQL 的头行」，
    与真 torn 帧的报错口径存在窄缝隙）。
  - [x] ~~**心跳帧线形 5.6/5.7 仅验过「SET 被接受」**（T7 收口），**静默
    窗（idle 真发心跳帧）形态仍 8.0-only**——矩阵窗口皆流量驱动、无
    idle 段（T0 挂账的残余半面，留 P4 多版本 idle 件）。~~——**P4a T4
    消费（`24865ab`）**：5.6/5.7 idle 心跳两件 live 真跑绿（228.89s），
    帧形实测 = 两版本均真发 HEARTBEAT v1（0x1b、ts=0、size=39、20s 节奏、
    活写位点），`binlog_heartbeat%` SHOW 面 5.6/5.7 空集、会话级
    `SET @master_heartbeat_period` 为实通道；spec §4 fake-rotate 回退分支
    判定为死枝（保留不删，见 P4a T4 节点与 DoD 对账）。
  - [ ] **compat repl 族两口径**（T7 评审 Minor）：跨档 ROTATE 产物差异
    仅 warn 不判红（files=2 计数闸兜底）；DML 指纹只钉 round-1 前置批
    （若 1213 类死锁杀在 round1 前置批，会**严格向误红**而非漏红——
    可接受方向，改口径须重跑矩阵）。
- **P3 终审修复轮（FIX A–F，2026-09-22，本分支六件全 TDD 先红后绿）**：
  全分支终审 6 findings 入册——A：pump_parallel 源错误早退跳收尾段 →
  Reorder 永久卡洞、同 Runner 复用下水位停表（fix 8ef0279）；B：resume
  对账「缺/多均硬错」overstate 死锁崩溃恢复 → 多实物降 warn、Missing
  仍硬错、Stale 变体删除（dde6515）；C：no_clobber 文档口径对齐实现
  （首个冲突文件创建时刻 O_EXCL 原子拒绝，非启动预扫，a0e0499）；D：
  空闲 master 心跳恒流上 Ctrl-C/stop 停摆 → ReplSource 帧顶中断门
  （d3b9970）；E：无 1236 特征的裸互踢断连无限循环 → Disconnect 同因
  3 连秒断纳入快速终止闸（864c4a9）；F：裸 repl 静默写 CWD 且永无
  checkpoint → 输出目标闸前移 validate_repl（d4c11a7）。
  - [ ] **心跳/中断联动（FIX D，运维注记）**：空闲 master 上 Ctrl-C/stop
    的停泵延迟以心跳周期为界（默认 30s，live 件
    `repl_sigint_idle_master_exits_130` 实测钉）；`--heartbeat-secs 0`
    **同时**禁用死链探测与空闲期即时停泵（中断要等下一个真事件，语义
    自负）；stop-datetime/stop-position 在空闲 master 上等下一**数据**
    事件生效（心跳不参与 stop 判定——登记行为，不修）。
  - [ ] **read_verify 对账契约改判（FIX B，运维向）**：resume 启动自检
    只对「written_files 承诺而盘上缺失」（Missing）与档损坏/畸形硬错；
    盘上多出的未登记实物（撕裂事务半块、rename 前崩溃残骸等 at-least-
    once 预期形态）降为 `tracing::warn!` 放行——旧契约「缺/多均硬错」在
    崩溃恢复主场景会死锁续跑，属 overstate 纠正而非门弱化；`CpError::Stale`
    变体随之删除（不可达）。「多实物绝不静默」由告警日志 + §5 防覆盖闸
    （新目录永不与残骸同名冲突）双兜底，清场核验责任转向运维读告警。
- **P4a T5 新增登记（合流轮入账，均不修）**：
  - [ ] **fuzz 语料 seed6 漂移（known，评审轮 2 字节级证实）**：
    `seed6_tm_meta_len_overflow` 因表名声明长漂移（声明 8 实给 9）解析拐进
    parse_charset 报错面（`TLV type 254: truncated length prefix`），**够不到**
    `read_lns` 的加法闸——该闸红钉由定向单测
    `read_lns_u64max_declared_len_is_too_short_not_panic` 独扛（永久），seed6
    本体不改（修它 = 改畸形输入语义，超出裁决范围；Err-不-panic 性质不受影响）。
  - [ ] **event_stream 靶的 trx_id 不回退断言在当前实现下恒真**：防未来回归的
    钉，非主动发现器；两靶真实发现力在 panic/UB 面（本役已证 2 发）。
  - [ ] **溢出类 panic 残点**：rows/json 更深路径理论可能存在同族越界，历轮
    300s×2（lane 两轮 + T5 一轮，共 >55M exec/靶-轮）未命中——靠
    `make fuzz-min` 常态化观察，非挂死账。
  - [ ] **spec §4 fake-rotate 回退口径 = 死枝**：5.6.51/5.7.44 实测均真发
    HEARTBEAT v1 帧（P4a T4 节点），任何「5.6 不支持心跳需 fake-rotate 续命」
    的后续文本不应接此口径；SHOW `binlog_heartbeat%` 面 <8.0 为空集，
    实通道 = 会话级 `SET @master_heartbeat_period`。
  - [x] ~~difftest/compat 脚本硬编码 `./target/debug/my2sql-rs`，与 spec §6
    「独立 CARGO_TARGET_DIR」纪律冲突（仅在 worktree 恰好有本地 target 时碰巧
    成立——P4a 各 lane 即碰巧绿）~~——**T5 合流轮接线修复**：run-difftest.sh +
    compat-matrix.sh 改 `"${CARGO_TARGET_DIR:-$ROOT/target}/debug/my2sql-rs"`
    （与 shadow-replay.sh 同口径，判据零变化；T5 回归轮实测暴露后当场修 `edb2148`，
    见 T5 节点）；~~`tools/gen-bench-binlog.sh`~~、`tools/p4a-roundtrip.sh`、
    `tools/flashback-reconcile.sh` 同款硬编码留同族小账（非回归链路）。
    **P4b T1 消费 gen-bench 部分（`b6844fe`，挂账 #6）**：`gen-bench-binlog.sh` 已接
    RSBIN `${CARGO_TARGET_DIR:-$ROOT/target}/debug/my2sql-rs`（:24 定义/:116 消费，edb2148
    同口径，`/tmp/p4b-t1-genbench-trace.log` 实证）；p4a-roundtrip.sh / flashback-reconcile.sh
    仍留同族小账（非回归链路，未触）。
- [x] ~~P3：repl 模式（另出计划；认证含 caching_sha2）~~——T0–T8 全部
  完成（本表上方「P3 Task 0–7」节点 + 「P3 DoD 对账」节），caching_sha2
  与 native 双认证 spike 钉死、8.4 矩阵经 `SHOW BINARY LOG STATUS` 改口
  通过；待全分支终审合入。
- [x] ~~P4：fuzz 正式接入（起点语料已备：`tests/fuzz_seed/` 4 件——终审 #1
  补第 4 件 `decimal_full_group_overflow.bin`，DECIMAL 满组溢出 repro +
  `tests/fuzz_seed.rs` 构造器/再生通道 FUZZ_SEED_REGEN=1）、影子库端到端回放~~
  ——**P4a 落地（T1/T2）**：`make fuzz-min`（`fuzz/` workspace 两靶 300s 闸，
  接入首轮即实抓 2 发解码器 panic 走开闸条款，起点 4 件扩至 7 畸形 + 3 合法
  × 双 crc 态 = seedgen 确定性 40 件）；`make shadow-test`（三段 checksum
  等 + 行级 diff 零 + negcheck 验钞机，8.0 spec 原形态、5.7 REF-clone 锚裁定，
  见「P4a 任务节点日志」）。
- [x] ~~P4：**musl 吞吐**（构建已销账：x86_64-unknown-linux-musl debug+release 绿、
  static-pie 可运行；实测崩塌至 ~3.4 MiB/s——musl malloc arena 竞争，
  候选 mimalloc / glibc-static，证据 docs/bench/p1.md）~~——**P4b T3 悬崖消账
  （`281735d`，挂账 #3）**：接入 mimalloc 全局分配器（现**无条件硬依赖**，无
  `--no-default-features` 逃生，musl release 含其 C 核并在 musl-gcc 下正常交叉编译）；
  musl 端到端 threads=8 复测 base **3.3 MiB/s**（≈精确复现 P1 3.4 悬崖）→ mimalloc
  **64.2 MiB/s**（≥50 门，≈19.2×）→ **悬崖消账**（`/tmp/p4b-t3-musl-{base,tip}.txt`；
  详见「P4b §0 挂账处置销账表」#3 与 p4b.md ④）。
- [x] ~~测试债（P2 邻近，终审登记）——矩阵覆盖缺口：① ENUM >255 成员
  （2B packlen 形态仅 `value.rs::enum_set_ordinals_to_uint` 合成单测，
  真机捕获与差分矩阵均无该列）；② GEOMETRY 真机捕获（裁决 7 字节保真
  路径无实抓 fixture）；③ LONGBLOB >64K 前缀行（packlen 4B 档 + 跨页
  payload 未进矩阵）。补捕获即补差分用例，不改解码器。~~——**P4a T3 销账
  （`3bf7fc1`，spec §3）**：三形真机捕获全走（`P4A=1 make difftest` 14/14
  组绿 + 自 roundtrip checksum/行级双门），**三形全部 Go 裁判支持、无违例
  形**；逐字证据 docs/p4a-findings.md + README 差异 28。
- [ ] 已文档化行为（终审核对，**不修**）：`tests/e2e.rs::parse_stmt`
  （~:463-465）对字面量含 `,`/`(`/`)`/` AND `/` WHERE ` 的敌意输入会
  panic（`unwrap`/切片越界）——**设计内**：该 helper 仅解析本仓 fixture
  真件产出的 SQL（`ab`/`xyz` 等字面量不触上述字符，前提见其文档注释），
  非通用 SQL 解析器；可接受性论证已在测试注释内，此行仅把裁定落进追踪
  文档。
- [ ] 已文档化行为（终审核对，**不修**）：writer 写盘错误在 `pump_*` 内
  `emit`/`finish` 处早返回 Err，`pump_parallel` 该路径不 join 已 spawn 的
  worker——整跑已进入终止收敛，worker 阻塞在 `job_rx.recv()` 随通道丢弃
  自然退出，detached 线程随进程回收；登记为文档化行为而非缺陷。
- [ ] 已文档化行为（终审核对，**不修**）：`frac_text`（time.rs）与
  go-mysql `fracTimeFormat` 的截断口径在 usec > 999999 时输出分歧
  （本侧 `%06` 展开为 7+ 位后取前 fsp 位）——该形态仅敌意/非法位域可
  触发，真机合法 binlog usec 恒 < 1e6；legal-inputs-only 注记，P2 fuzz
  若发现真机可达再复审。
- [ ] 已文档化行为（终审核对，**不修**）：ROTATE 事件携带的新文件名
  （rotate url）不做字符白名单/路径净化即信任——input-side-only 信任面：
  url 仅更新 `FileReader.name`（裁定 7：只改名、不切文件；跨文件续读由
  装配层 `next_binlog_name` 在该名上推导），最坏后果 = 拼出的路径
  open 失败/非 binlog 解码报错即停（读侧越权顶多「读到不该读的文件而
  报错」），输出侧文件名字节净化已另闸（T14），无写逃逸面。注记备案。
- [ ] spec §4.6 ENUM/SET 名称注释 → 推迟至 P2
- [x] ~~T15 白名单：TIMESTAMP 秒=0 → 1970-01-01（T6 裁定，go-mysql formatZeroTime 输出 0000-00-00）~~——ALW-ZERO-TIMESTAMP 落地（eq 零日期对，fsp 后缀须一致；selftest 第 6 组正反例）
- [x] ~~T15 白名单：DOUBLE Display 恒十进制无科学计数（Go %v 输出 1e+10 类）；BIT(64) 高位置 1 时本侧 UInt 正数 vs go-mysql int64 负数~~——ALW-FLOAT-WIDTH（≤17 有效位闸口 + f64/f32 同 bits）+ ALW-INT-SIGN-WRAP（mod 2^64）落地；1e+10 类经 num 桥 Decimal 直判等
- [ ] T15 校准：8.0 TLV opt-meta 已随 T9 后校准补丁真实解析（fixture 回归钉死）；剩余 = 5.7 signedness bitmap、与更多真机捕获（FULL 形态等）的差分校准。~~T4 charset 形状拒绝的构造性误判~~（已修：真机 8.0 件现 Ok，5.6/5.7 legacy 严格形态测试全保留）。
- [x] ~~**T2 勘误（T9 真机证实）**：event.rs 事件码表错档（30/31/32 标 V1、ANON=119 等）~~——T9 后校准补丁已按 const.go:54-87 勘误并加 fixture 走读回归；**残余子项归 T10**：rows V2 事件 extra-info（固定公共段后、行体前的可选 4B）读取跳过。~~残余子项~~（T10 已完成：extra-info 按自含长度整段跳过 + 未知 typecode → PartialNotSupported，见 Task 10 节点）。
- [x] ~~**T4 勘误（T9 真机捕获）**：T4 严格 LNE 解析器拒绝真实 8.0 TLV optional-metadata~~——T9 后校准补丁实现 fork 同构 `decodeOptionalMeta` 镜像（无总长前缀、未知项跳过、截断报错），`tests/fixtures/capture_8.0_minimal/` 真机 TABLE_MAP 回归通过（捕获件 /tmp/t9probe 亦同源）。
- [x] ~~T15 白名单候选：VAR_STRING(varbinary) 合法 UTF-8 时本侧 `Str`（utf8_safe 过闸），裁判 events.go 对 varchar/varbinary 非 "blob" 字样亦文本化——varbinary 二进制语义差异待 T15 对账确认。~~——ALW-VARBINARY-STR 落地（canon text/bytes 打标 + eq 双向桥，非UTF8 经 surrogateescape；真机 c_vb/c_bin 全绿）
- [x] ~~T15 白名单（JSON 渲染三类，T8 审阅裁定，几乎每行都会触发）：① 对象键序 = 存储序(长度,memcmp)，go-mysql 经 map+Marshal 输出纯字典序；② double 文本 = MySQL 显示规则（12.0/1e21/-0.0），Go %v 为 12/1e+21/-0；③ 本侧 `<>&`、U+2028/9 原样输出，Go json.Marshal 会 HTML 转义~~——ALW-JSON-KEYORDER/DOUBLE/HTMLESC 落地（jload+jeq 深比较：键序无关、数值 Decimal(str) 判等、转义解码后比）；另 ALW-JSON-IN-SET（seteq）覆盖上游 UPDATE SET 恒含 JSON 列（GenUpdateSetPart []byte 断言失败）而本侧按 diff 省略的差集
- [x] ~~T15 白名单（T11）：key_indexes 键名指向缺失列时本侧整键降级（pk=[]/丢 uk），上游 GetColIndexFromKey 映射为序号 0（bug 兼容会产生错误 WHERE）；表达式索引/坏 JSON 场景输出必分歧~~——矩阵 EXCL 落地：ALW-MULTI-UK/ALW-EXPR-INDEX/ALW-DROP-COL-ALTER（gen-data.sql 头注释钉死每表 ≤1 uk、无表达式索引、无中途 ALTER）；等宽场景 20/20 绿未触发分歧路径
- [x] ~~T15 白名单（T13）：blob/非utf8-text 字面量本侧 `0xUPPERHEX`，上游 X'lowerhex' 或原样字节引号串（语义等价）~~——ALW-BLOB-HEX 落地（canon HEXP/XHQ 双形→bytes，eq text↔bytes 桥）
- [x] ~~T15 白名单（T13 审阅）：多条件 WHERE 上游带括号 `(a=1 AND b=2)`（expression.go conjunctExpression），本侧裸连 `a=1 AND b=2`；SET 分隔上游 ", "/VALUES 行上游 ", ("，本侧 ","；比较器须括号/空白不敏感~~——ALW-WHERE-PARENS 落地（find_kw noparen + cond 剥括号 + split_top/sorted 结构解析；idn 反引号/空白不敏感）
- [x] ~~T15 白名单（T13 审阅）：标识符含反引号时本侧加倍 ``a``b``，上游 table.go/column.go 原样包裹不加倍（上游产生坏 SQL）~~——ALW-IDENT-BACKTICK：矩阵库表列名不含反引号（夹具规避该病理形态）
- [x] ~~T15 白名单（T13）：UPDATE 行前后无变化时上游 Fatalf 整跑终止，本侧 skip+warn——该病理夹具不得进入差分对比~~——EXCL ALW-NOCHANGE-UPDATE（gen-data.sql 所有 UPDATE 必改值）
- [x] ~~T15 纪律（T11）：binlog 比 schema 宽的场景差分必须跑 strict=true（上游 events.go:87 无条件 fatal，pad 列永不出 SQL）~~——ALW-COLCOUNT-STRICT 登记；v1 矩阵无中途 ALTER → 天然等宽，strict 路径由 T13 单测覆盖（EXCL ALW-DROP-COL-ALTER，P2 再议）
- [x] ~~T15 白名单：blob 字面量形态 本侧 `0xUPPERHEX` vs 上游 `X'lowerhex'`（sqltypes.go:567-570）——语义等价 SQL，比较器须双解（T13 裁定 1 重设计，非 parity 缺陷）~~——并入 ALW-BLOB-HEX（上上条）
- [x] ~~T13 决策点（T11）：strict 默认值 = CLI 语义决定（静默补列 vs 硬停），定稿前不得静默 non-strict~~——T13 定稿：`SqlOpts::strict_schema` 默认 **false**（非 strict：dropped 位列清单/WHERE 省略+warn、Truncated 静默前缀），true 经 align_cols 逐事件 ColCountFatal；与上游有效行为等价的论证见 Task 13 节点「裁定 2 定稿」段
- [x] ~~T15 夹具约束（T11 审阅发现）：上游 UniqueKeys 为 Go map 序（mysqlFuncs.go:221-238），多 uk 表 GetOneUniqueKey 选择跨运行不稳定；本侧确定性序更优——差分夹具限 ≤1 候选 uk 或容忍键选择分歧~~——EXCL ALW-MULTI-UK 落地：矩阵 t_uk 仅 1 个 UNIQUE 键、无复合多候选
- [x] ~~T15 白名单（T14）：`SET NAMES utf8mb4;` 文件头为本项目计划约束，上游 Go 版无此行（python my2sql 有）——比较器须容忍首行~~——ALW-SETNAMES-HEADER 落地（load() 跳头行）
- [ ] T15 白名单（T14）：`--to-stdout` 模式本侧与文件模式统一字节面（SET NAMES 头+extra-info 一并入屏幕流），上游屏幕模式仅打语句（events.go OutputToScreen 分支）
- [x] ~~T15 纪律（T14）：extra-info datetime 本侧按 `--time-zone` 固定偏移渲染（下划线形与上游字节平价），上游走运行主机 TZ——差分双方须显式给同一 `--time-zone`/`TZ` 再比对~~——ALW-EXTRAINFO-DTZ 落地：容器 TZ=UTC + oracle 进程 TZ=UTC + 本侧 --time-zone +00:00；datetime 字段不进对齐键
- [x] ~~T15 白名单：文件名字节净化（仅 path，SQL 文本原样）vs 上游可越界写~~——差分按 SQL 文本面比对（glob *.sql + 对齐键），路径净化不进比对面；净化本身由 T14 单测钉死
