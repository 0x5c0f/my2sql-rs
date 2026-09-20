# my2sql-rs 设计文档

日期：2026-09-20
状态：已确认（方案 A：全自研解码层）

## 1. 目标与定位

用 Rust 独立实现一个 MySQL binlog 解析工具，能力对齐但不兼容 my2sql（Go）：生成原始 SQL、回滚（闪回）SQL、DML/事务统计。本项目为独立项目，仅借鉴上游思路，不追求 CLI 兼容；`reference/my2sql-go/` 保留作行为参考与差分测试的裁判（oracle）。

### 功能范围（一期全做）
- work 能力：`to-sql`（正向 SQL）、`flashback`（回滚 SQL）、`stats`（DML 统计 + 大/长事务）
- 取数模式：本地 binlog 文件解析（file）、伪装从库实时拉取（repl）
- 目标服务端：仅 MySQL 5.7 / 8.0 / 8.4（含 9.x 尽力而为）；认证支持 mysql_native_password 与 caching_sha2_password
- 明确不做：DDL 回滚、回滚结果直接执行（`--apply`）、MariaDB 专用事件、8.0.1 partial rows image 的 default_metadata 解析（检测到即显式报错，不猜）

### 超越上游项（本设计的增量价值）
1. DECIMAL 精确解码（上游按 Double 近似）
2. 表结构导出/导入（`--schema-dump/--schema-file`），使 file 模式可完全离线运行
3. 单二进制静态发布、无 mysql_native_password 强制要求
4. 保序器从"微秒自旋锁"改为 reorder buffer，反压有界
5. 输出文件头 `SET NAMES utf8mb4;`；多行 INSERT 合并参数做实
6. minimal/partial image 下 WHERE 降级策略更安全（见 §4.5）

## 2. 技术选型

- 单 Cargo 包（bin crate），二进制名 `my2sql-rs`
- 线程模型：std::thread + crossbeam-channel，**不引入 tokio**（链路为批处理/单连接拉流，async 无收益）
- 依赖：`clap`（CLI）、`crossbeam-channel`、`mysql`（yawart 同步驱动：元数据查询/SHOW BINARY LOGS，支持 caching_sha2；复制协议层为自研，见 repl 模块）、`mysql_common`（仅协议原语：握手包、length-encoded int/string）、`crc32fast`、`simdutf8`、`serde`+`serde_json`（schema 缓存文件）、`thiserror`
- 解码层为纯函数库模块，零网络/零 IO 依赖，可独立单测与 fuzz

## 3. 模块架构

```
src/
├── main.rs          # clap 入口，装配流水线
├── config.rs        # 参数定义、校验、默认值
├── binlog/          # ★ 自研解码层（核心资产）
│   ├── event.rs     #   event header、类型枚举、checksum 剥离
│   ├── table_map.rs #   TABLE_MAP：列类型/元数据/null-bitmap 宽度
│   ├── rows.rs      #   ROWS event：null/present bitmap 游标
│   ├── value.rs     #   decodeValue：全类型二进制 → ColumnValue
│   ├── json.rs      #   JSON 二进制 → 紧凑文本
│   ├── decimal.rs   #   NEWDECIMAL packed → 精确十进制字符串
│   ├── time.rs      #   DATE(TIME)2 族 → 字符串（不经 chrono）
│   └── file_reader.rs # 本地 binlog 读循环、跨文件续读
├── repl/            # 复制协议：注册从库/BINLOG_DUMP/包流 → 事件字节
│   └── stream.rs
├── pipeline/        # 调度层
│   ├── source.rs    #   EventStream trait（file 与 repl 共同实现）
│   ├── filter.rs    #   事件级过滤：库表/SQL 类型/位点/时间
│   ├── order.rs     #   保序器（reorder buffer + 反压）
│   └── worker.rs    #   N 线程 SQL 生成
├── metadata/        # 表结构获取/缓存/schema 文件读写/列数对账
├── sqlopen/         # SQL 拼装与值编码（quote/hex/数值/时间）
├── rollback/        # 临时正序文件 + 磁盘逆序生成
├── stats/           # DML 统计、大/长事务识别与落盘
└── output.rs        # 文件命名、缓冲写、stdout 模式
```

数据流（to-sql / flashback）：

```
EventStream(file|repl) → filter → 事务状态机(trx_id/trx_status)
  → dispatcher(编号) → [worker×N: 解码行+生成SQL]
  → reorder(按序) → writer（to-sql: 直接落盘；flashback: 落临时文件+索引，结束后并行逆序）
  → 同时 stats 通道始终开启，stats 线程独立消费
```

## 4. 解码层正确性决策（核心）

**总原则：binlog 里是什么字节，SQL 里就是什么字面量。禁止任何"类型转换式解析"。**

### 4.1 值模型
`ColumnValue = Null | Int(i64) | UInt(u64) | Double(f64文本保真) | Decimal(String) | Str(Vec<u8>) | Bytes(Vec<u8>) | Json(String) | Missing`
- 类型信息保留到输出层，不设 Go `interface{}` 式弱类型层。
- 全链路字节优先（`Vec<u8>`/`&[u8]`），仅在确认 UTF-8 合法（simdutf8 校验）后才作为字符串输出，避免 Rust String 的 UTF-8 陷阱造成静默损坏。

### 4.2 时间族
DATETIME2/TIMESTAMP2/TIME2/DATE 解码为字符串（大端位拆分 + 按 fsp 补小数位），不引入 chrono 参与值语义；`0000-00-00`、负 TIME、`838:59:59` 原样输出。`--time-zone` 仅影响：event header 时间戳 → 注释 datetime；`--start/stop-datetime` 的解析。

### 4.3 整型
按 TABLE_MAP flag + 元数据列定义交叉判定 signed/unsigned；signed 符号位异或还原，unsigned 位宽零扩展至 u64；mediumint-unsigned 边界专项测试。

### 4.4 DECIMAL
实现官方 packed 编码（符号 XOR 0x80、4 字节 9 位数字、digit-per-byte 余数组），输出精确十进制字符串。差分测试白名单（Go 输出浮点近似文本）。

### 4.5 TEXT/BLOB 区分与 bitmap
- blob 族按元数据真实列类型区分 text/json（字符串输出）与 binary/geometry（`0x` hex 输出）；字符列含非法 UTF-8 → hex + 注释标记。
- null bitmap 游标按"已消耗 bit 数"连续推进，行间不重置，update 事件 before/after 共享游标。
- present bitmap（minimal image）缺失列 → `Missing`：UPDATE 的 SET 跳过该列；WHERE 降级全列且无全列可用时告警退出（不静默产错）。
- default_metadata 段存在且行不完整 → fatal 报错（一期不支持 partial）。

### 4.6 ENUM/SET
输出序号整数（与上游一致，保证差分可比；严格 sql_mode 下可回放）。`--add-extraInfo` 时注释头附带映射名。

### 4.7 checksum
按 format 版本与复制 flag 判定 CRC32 剥离；文件模式 checksum 错误=截尾（停止本文件并记录位点）；repl 模式=协议错误（可重连）。

### 4.8 JSON
自研二进制 → 紧凑文本（小端 dict/offsets 递归重组）。键序为 MySQL 存储序（长度优先+字典序），不承诺还原原始文本——差分测试按解析树语义比较。

### 4.9 DDL 容错三态
binlog 列数 < 表结构 → 按 binlog 截断；> → `dropped_column_N` 占位（对齐上游以便差分）；`--strict-schema` 时任一不一致即 fatal。

### 4.10 SQL 值编码（sqlopen 层）
`'`/`\` 反斜杠转义、`\0`→`0x` 或 `\0`、非法 UTF-8 与二进制列 → `0x...`；标识符反引号包裹并转义内嵌反引号；数值直出；表名可选 `db.` 前缀（`--no-db-prefix`）。

## 5. 流水线与回滚

### 5.1 保序器
dispatcher 顺序编号入队；worker 完成后发 (seq, sqls) 到 reorder 阶段（单线程持有 `next_seq` + HashMap，连续弹出写出）。缓冲上限 = 2×threads，超限阻塞入队形成反压。

### 5.2 事务状态机
QUERY_EVENT(begin)/XID_EVENT(commit)/GTID 事件维护 trx_id 自增与状态，随事件下发，供 keep-trx 与 stats 使用。

### 5.3 flashback 磁盘逆序
1. 回滚 SQL 正序写隐藏临时文件，内存索引记 `(sql_len, trx_id)`；
2. 全部完成后按文件并行逆序：从尾部按索引分块 seek+read，块内行逆序写出；事务边界注入 `commit;\nbegin;`（`--keep-trx` 默认开），文件尾补 `commit;`；
3. 删除临时文件；产物 `rollback.<binlog序号>.sql`；结束打印多 binlog 执行顺序指引（从大到小）。

## 6. CLI 设计（clap 子命令）

共享参数组：
- 连接：`--uri mysql://user:pw@host:port`（repl 与元数据共用；env 兜底密码）
- 范围：`--start-file/--start-pos/--start-datetime/--stop-file/--stop-pos/--stop-datetime`、`--binlog-dir`（file）、`--server-id`、`--time-zone`
- 过滤：`--db/--table/--ignore-db/--ignore-table/--dml insert,update,delete`
- 元数据：`--schema-file FILE`（离线模式）或连库；`--schema-dump FILE`
- 输出：`--output-dir`、`--to-stdout`、`--file-per-table`、`--add-extra-info`、`--no-db-prefix`
- SQL 形态：`--full-columns`、`--unique-key-first`、`--ignore-primary-key-for-insert`、`--insert-batch N`、`--strict-schema`
- 通用：`--threads N`（默认 cpu 数）、`--on-error stop|skip-bad-event`（stats 可用）

子命令专属：
- `to-sql`：无额外
- `flashback`：`--keep-trx/--no-keep-trx`
- `stats`：`--print-interval S`、`--big-trx-rows N`、`--long-trx-seconds S`、`--stats-json`

## 7. stats 输出

- `binlog_status.txt`：窗口（print-interval）× 表 的 inserts/updates/deletes 行数与起止时间/位点；binlog 切换即落盘。
- `biglong_trx.txt`：begin~commit 聚合，行数 ≥ big-trx-rows 或时长 ≥ long-trx-seconds 落盘，含每表 DML 明细。
- `--stats-json`：同上两份的 JSONL 版本，供程序消费。

## 8. 测试策略

1. **解码单测**：从 docker MySQL（5.7/8.0/8.4）真实 binlog 抠 hex fixture；覆盖全部类型 × {NULL、零值、边界、unsigned、非法 UTF-8、emoji}；bitmap 跨界、CRC32。
2. **差分测试**：数据生成脚本（全类型矩阵 + DDL 穿插 + 混合事务）→ 同一 binlog 跑 Go 裁判与 Rust → **语义比较器**：解析 SQL 逐列比值（JSON 比解析树、DECIMAL 比数值、blob 比字节、enum 比序号）；分歧白名单：DECIMAL 精度、SET NAMES 头、多行 INSERT 合并。
3. **fuzz**：cargo-fuzz 对 event/row 解码入口，零 panic 准则。
4. **端到端**：docker-compose 起 MySQL → 产生负载 → repl 与 file 双模式跑通 → 生成 SQL 回放到影子库验证数据一致（最终正确性证明）。
5. **性能门槛**：criterion；1GB binlog（row image=full）to-sql ≤ 60s、flashback ≤ 90s、stats ≤ 30s。

## 9. 阶段划分

- P1：binlog 解码层 + file 模式 + to-sql + 元数据（含 schema 文件）+ 差分测试基建 ← 命门先立
- P2：flashback（逆序+keep-trx）+ stats
- P3：repl 模式（复制协议、心跳/rotate、自动定位起始文件、实时 stdout）
- P4：影子库端到端回放测试、fuzz 补齐、性能调优、文档/发布

## 10. 风险登记

| 风险 | 等级 | 缓解 |
|---|---|---|
| 解码静默错误 | 高 | fixture 抠自真实 binlog + Go 裁判差分 + 影子库回放三重验证 |
| mysql_common 原语与自研层耦合出错 | 中 | 只用其无 IO 编解码小函数，接口面窄 |
| repl 协议细节（注册/心跳/大包） | 中 | P3 才做；官方协议文档逐条核对；docker 端到端兜底 |
| JSON 键序归一引发误报 | 低 | 语义比较器天然消化 |
| 工期失控 | 中 | 阶段独立可交付；P1 结束即有可用雏形 |
