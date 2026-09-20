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
- 状态：**Task 15 已完成并经审阅两轮修订（golden 差分基建，8.0 矩阵 21/21 组全绿）**
  （tools/docker-mysql.sh + gen-data.sql + run-difftest.sh + ~155 行
  stdlib 比较器 + 8 组自测 + 白名单运营化；`make difftest` 洁净态 exit 0；
  本轮未发现解码器 bug，三处红灯全部裁定为渲染/上游行为差异并入白名单。
  审阅 Important：ALW-FLOAT-WIDTH 类型盲 → 4613e0f 限 f32-canonical 侧 +
  矩阵补 JSON 值变更 UPDATE；残留低精度漏洞 → 01edac7 再要求 canonical 侧
  有效数字严格更多。见下 Task 15 节点）。前序：Task 14 端到端装配（275e1a6，
  task-14-report.md）、Task 13 sqlopen（ebf8a10，task-13-report.md）

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
