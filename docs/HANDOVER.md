# my2sql-rs 交接文档（HANDOVER）

> 每完成一个节点统一更新本文件。目标读者：接手项目的任何人（含未来子代理/人类协作者）。
> 设计权威：`docs/superpowers/specs/2026-09-20-my2sql-rust-design.md`；P1 执行计划：`docs/superpowers/plans/2026-09-20-my2sql-rs-p1.md`。

## 项目一句话

Rust 独立重写 MySQL binlog 解析工具（to-sql / flashback / stats），能力对齐 Go 版 my2sql 但 CLI 全新设计；`reference/my2sql-go/` 为行为参考与差分测试裁判（不入库、勿改动）。

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

- 分支：`feat/p1`（main 只有文档）
- 里程碑：P1 计划 17 任务（执行序 1..15, 17, 16）
- 状态：**Task 11 已完成**（metadata 层 SchemaStore online/offline +
  align_cols 列数对账 + key_indexes 键名→序号；`cargo test` 152+3/155 绿
  （1 ignored = 真库 live 测试，已对 docker mysql:8.0.46 与 5.6.51 双实例
  实跑通过）、
  clippy -D warnings、fmt 干净；详见 task-11-report.md）

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

- 本机：docker（镜像 mysql:5.6/5.7/8.0 已就绪，8.4 需拉取）、Go 工具链 /opt/go/bin、cargo/rustc 最新 stable
- 工作区：`/home/cxd/Projects/aiediter/my2sql`
- SDD 台账：`.superpowers/sdd/2026-09-20-my2sql-rs-p1/progress.md`（git-ignored，恢复上下文先读它）

## 遗留/挂账清单

- [ ] P2：flashback + stats（另出计划）
- [ ] P3：repl 模式（另出计划；认证含 caching_sha2）
- [ ] P4：fuzz 正式接入、影子库端到端回放、musl 静态构建
- [ ] spec §4.6 ENUM/SET 名称注释 → 推迟至 P2
- [ ] T15 白名单：TIMESTAMP 秒=0 → 1970-01-01（T6 裁定，go-mysql formatZeroTime 输出 0000-00-00）
- [ ] T15 白名单：DOUBLE Display 恒十进制无科学计数（Go %v 输出 1e+10 类）；BIT(64) 高位置 1 时本侧 UInt 正数 vs go-mysql int64 负数
- [ ] T15 校准：8.0 TLV opt-meta 已随 T9 后校准补丁真实解析（fixture 回归钉死）；剩余 = 5.7 signedness bitmap、与更多真机捕获（FULL 形态等）的差分校准。~~T4 charset 形状拒绝的构造性误判~~（已修：真机 8.0 件现 Ok，5.6/5.7 legacy 严格形态测试全保留）。
- [x] ~~**T2 勘误（T9 真机证实）**：event.rs 事件码表错档（30/31/32 标 V1、ANON=119 等）~~——T9 后校准补丁已按 const.go:54-87 勘误并加 fixture 走读回归；**残余子项归 T10**：rows V2 事件 extra-info（固定公共段后、行体前的可选 4B）读取跳过。~~残余子项~~（T10 已完成：extra-info 按自含长度整段跳过 + 未知 typecode → PartialNotSupported，见 Task 10 节点）。
- [x] ~~**T4 勘误（T9 真机捕获）**：T4 严格 LNE 解析器拒绝真实 8.0 TLV optional-metadata~~——T9 后校准补丁实现 fork 同构 `decodeOptionalMeta` 镜像（无总长前缀、未知项跳过、截断报错），`tests/fixtures/capture_8.0_minimal/` 真机 TABLE_MAP 回归通过（捕获件 /tmp/t9probe 亦同源）。
- [ ] T15 白名单候选：VAR_STRING(varbinary) 合法 UTF-8 时本侧 `Str`（utf8_safe 过闸），裁判 events.go 对 varchar/varbinary 非 "blob" 字样亦文本化——varbinary 二进制语义差异待 T15 对账确认。
- [ ] T15 白名单（JSON 渲染三类，T8 审阅裁定，几乎每行都会触发）：① 对象键序 = 存储序(长度,memcmp)，go-mysql 经 map+Marshal 输出纯字典序；② double 文本 = MySQL 显示规则（12.0/1e21/-0.0），Go %v 为 12/1e+21/-0；③ 本侧 `<>&`、U+2028/9 原样输出，Go json.Marshal 会 HTML 转义
- [ ] T15 白名单（T11）：key_indexes 键名指向缺失列时本侧整键降级（pk=[]/丢 uk），上游 GetColIndexFromKey 映射为序号 0（bug 兼容会产生错误 WHERE）；表达式索引/坏 JSON 场景输出必分歧
- [ ] T15 纪律（T11）：binlog 比 schema 宽的场景差分必须跑 strict=true（上游 events.go:87 无条件 fatal，pad 列永不出 SQL）
- [ ] T13 决策点（T11）：strict 默认值 = CLI 语义决定（静默补列 vs 硬停），定稿前不得静默 non-strict
- [ ] T15 夹具约束（T11 审阅发现）：上游 UniqueKeys 为 Go map 序（mysqlFuncs.go:221-238），多 uk 表 GetOneUniqueKey 选择跨运行不稳定；本侧确定性序更优——差分夹具限 ≤1 候选 uk 或容忍键选择分歧
