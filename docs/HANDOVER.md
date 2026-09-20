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
- 状态：**Task 7 已完成**（DECIMAL 解码 decimal.rs；`cargo test` 76+3/79 绿、clippy -D warnings、fmt 干净）

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
  - `EventType` 常量：QUERY=2 CREATE_DB=3 ROTATE=4 FORMAT_DESC=15 XID=16 TABLE_MAP=19
    HEARTBEAT=27 WRITE/UPDATE/DELETE_ROWS_V1=30/31/32 GTID_LOG=33 V2=34/35/36
    PREVIOUS_GTIDS=37 ANONYMOUS_GTID_LOG=119（brief 内 "XID=15?" 注释为历史噪声，以本表为准）。
- 依赖调整（Task 1 遗留债务）：移除了直接依赖 `mysql_common 0.38.2`（src 内零引用，
  与 mysql 28 传递依赖的 0.37.3 双版本共存）。Task 3 若需 LNE 等原语，按 mysql 28
  对齐补 `mysql_common 0.37` 或直接手写，勿再引入 0.38。
- 与 brief 的偏差/细化：`tests/fixtures/events.rs` 仅存测试常量占位（bin-only crate 的
  tests/ 子目录不会被 cargo 编译为测试目标；单测常量按 brief 要求在 event.rs 的
  `#[cfg(test)] mod tests` 内自足）；Task 15 引入 lib 目标后可 `mod fixtures;` 复用。
- 遗留：event.rs / error.rs 顶部有临时 `#![allow(dead_code)]`（骨架阶段无生产消费者，
  沿 Task 1 惯例），Task 12+ 接入管道后移除。

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
    section: …"))`（D5：不支持的元数据必须报错、不得猜测）。MySQL 8.0
    `binlog_row_metadata=FULL` 的 TLV optional metadata（2B LE total_length +
    type/LNE长/值 条目）会被明确拒绝，完整 TLV 解析留 Task 15。
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
- [ ] T15 校准：8.0 TLV opt-meta 真实解析、5.7 signedness bitmap、T4 charset 形状拒绝的构造性误判
