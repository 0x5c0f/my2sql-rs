# my2sql-rs 命令行参数详解

本文档提供 my2sql-rs 所有命令行参数的完整说明，包括用途、格式、取值范围和实际示例。

---

## 📖 快速导航

- [to-sql 模式](#to-sql 模式)
- [flashback 模式](#flashback 模式)
- [stats 模式](#stats 模式)
- [repl 模式](#repl 模式)
- [通用选项](#通用选项)

---

## 🔍 to-sql 模式

### 基本用法

```bash
my2sql-rs to-sql [OPTIONS] --binlog-dir <DIR> [SUBCOMMAND]
```

将 MySQL binlog 文件还原为原始 SQL 语句。

### 核心参数

#### `--binlog-dir <DIR>` **(必需)**

**作用**：指定 MySQL binlog 二进制日志所在的本地文件系统目录。

**为什么需要？**
MySQL binlog 是文件形式的日志（类似手机相册），存储在服务器的数据目录下：
- MySQL 5.6: `/var/lib/mysql/binlog.*`
- MySQL 5.7+: `/var/lib/mysql/mysql-bin.*`

工具需要从磁盘读取这些二进制文件才能解析出 SQL 语句。

**如何获取 binlog 路径？**
```bash
# 方法 1: 查看 MySQL 配置
docker exec my-mysql mysql -uroot -p -e "SHOW VARIABLES LIKE 'datadir';"

# 方法 2: 直接在宿主机查看（需容器挂载）
ls -la /path/to/mysql/data/
```

**如何复制 binlog 到本地？**
```bash
# 从容器复制到工作目录
docker cp my-mysql:/var/lib/mysql/mysql-bin.000100 ./data/
docker cp my-mysql:/var/lib/mysql/mysql-bin.index ./data/

# 或进入容器批量复制
docker exec -it my-mysql bash
root@container # chmod -R a+r /var/lib/mysql/mysql-bin.*
exit
cp /var/lib/mysql/mysql-bin.* ./data/
```

**类型**：目录路径（字符串）  
**必填**：是

---

#### `--uri <URI>` **(可选)**

**作用**：数据库连接字符串，用于在线拉取 schema 信息（表结构定义）。

**格式**：`mysql://[user]:[password]@[host]:[port]/[database]`

**常用示例**：
```bash
# root 用户，无密码
--uri "mysql://root:@127.0.0.1:3306"

# root 用户，带密码
--uri "mysql://root:password@127.0.0.1:3306"

# 指定库
--uri "mysql://app_user:secret@db.example.com:3306/mydb"

# URI 编码（密码含特殊字符时）
--uri "mysql://root:P%40ssw0rd@127.0.0.1:3306"  # P@ssw0rd 需要编码
```

**为什么需要？**
Binlog 中只记录数据的变动（INSERT/UPDATE/DELETE 的值），不包含表的定义。工具需要连接数据库查询：
- 表有多少列？
- 每列的数据类型是什么？
- 主键/索引结构？

**权限要求**：
- 最小权限：`REPLICATION SLAVE, REPLICATION CLIENT`
- 推荐：新建专用账号
  ```sql
  CREATE USER 'repl'@'%' IDENTIFIED BY 'password';
  GRANT REPLICATION SLAVE, REPLICATION CLIENT ON *.* TO 'repl'@'%';
  FLUSH PRIVILEGES;
  ```

**类型**：URI 字符串  
**必填**：否（可与 `--schema-file` 配合使用实现离线模式）

---

#### `--schema-dump <FILE>` **(可选)**

**作用**：导出当前连接的数据库表结构到 JSON 文件。

**使用场景**：
- 首次运行时导出表结构（需 `--uri` 连接数据库）
- 后续可在无数据库环境下复用（通过 `--schema-file`）

**示例**：
```bash
# 导出 schema 到文件
./my2sql-rs to-sql \
  --binlog-dir data/8.0 \
  --uri "mysql://root@localhost:3306" \
  --schema-dump schema.json

# 断网后用 schema.json 离线回放
./my2sql-rs to-sql \
  --binlog-dir data/8.0 \
  --schema-file schema.json \
  --to-stdout
```

**优点**：
- ✅ 无需连接数据库即可解码 binlog
- ✅ 多环境复用（dev/staging/prod）
- ✅ 安全审计（不暴露数据库连接）

**类型**：文件路径  
**必填**：否

---

#### `--schema-file <FILE>` **(可选)**

**作用**：使用预先导出的 schema JSON 文件进行解码（离线模式）。

**与 `--schema-dump` 的区别**：
- `--schema-dump`：从数据库**导出** schema
- `--schema-file`：使用已导出的 schema

**典型流程**：
```bash
# Step 1: 在可连接数据库的环境导出表结构
./my2sql-rs to-sql --uri "mysql://root@prod_db" --schema-dump schema.json

# Step 2: 将 schema.json 拷贝到无库环境
scp schema.json dev-server:/home/dev/project/

# Step 3: 仅用 schema 文件解码 binlog（无需数据库连接）
./my2sql-rs to-sql --binlog-dir ./binlogs --schema-file schema.json --to-stdout
```

**类型**：文件路径  
**必填**：否（可与 `--uri` 二选一）

---

### 位点控制参数

#### `--start-file <FILE>`

**作用**：指定解析的起始 binlog 文件名。

**默认值**：最新可用的 binlog 文件

**示例**：
```bash
# 指定具体文件
--start-file mysql-bin.000100

# 清空以使用 resume checkpoint（与 --resume-file 配合）
--start-file ""
```

**类型**：文件名（不含路径）  
**注意**：完整路径由 `--binlog-dir` 指定

---

#### `--start-pos <POS>`

**作用**：指定起始解析位置（字节偏移量）。

**默认值**：`4`（binlog 文件头之后的第一个事件）

**使用场景**：
- 从特定位置继续解析中断的任务
- 精确定位某个时间点的事件

**示例**：
```bash
# 从 pos 点开始
--start-file mysql-bin.000100 --start-pos 12345

# 与 --resume-file 配合清零
--start-file "" --start-pos 0 --resume-file checkpoint.json
```

**类型**：整数（≥ 4）  
**默认**：4

---

#### `--end-file <FILE>`

**作用**：指定解析的结束 binlog 文件名。

**默认值**：不限制（解析到最后一个文件）

**示例**：
```bash
# 只解析某个时间段的 binlog
--start-file mysql-bin.000100 --end-file mysql-bin.000105
```

**类型**：文件名  
**注意**：必须 ≥ start-file

---

#### `--end-pos <POS>`

**作用**：指定结束解析位置（字节偏移量）。

**默认值**：不限制（解析到文件末尾）

**类型**：整数  
**注意**：必须 ≥ start-pos

---

### 时间窗口过滤

#### `--start-time <DATETIME>`

**作用**：过滤开始时间，只解析该时间点之后的事件。

**格式**：`YYYY-MM-DD HH:MM:SS`

**示例**：
```bash
# 只解析 2026-09-22 14:00:00 之后的事件
--start-time "2026-09-22 14:00:00"
```

**类型**：日期时间字符串  
**与 `--start-file` 的关系**：两者结合使用时，取较晚的时间点

---

#### `--end-time <DATETIME>`

**作用**：过滤结束时间，只解析该时间点之前的事件。

**格式**：`YYYY-MM-DD HH:MM:SS`

**示例**：
```bash
# 只解析 2026-09-22 16:00:00 之前的事件
--end-time "2026-09-22 16:00:00"
```

**类型**：日期时间字符串

---

#### `--time-zone <TZ>`

**作用**：指定时区偏移，用于正确解析 binlog 中的 TIME/DATETIME 字段。

**默认值**：`+00:00` (UTC)

**常见值**：
```bash
--time-zone +08:00    # 中国标准时间
--time-zone -05:00    # 美国东部时间
--time-zone UTC       # UTC 时区
```

**为什么重要？**
如果时区设置错误，可能导致时间戳偏差。例如：
- Binlog 存储：`2026-09-22 14:00:00` (服务器时区 +08:00)
- 错误解读：显示为 `2026-09-22 06:00:00` (未设置时区，按 UTC 解释)

**类型**：时区偏移字符串

---

### 过滤参数

#### `--db <DB_NAME>`

**作用**：只解析指定数据库的 DML 操作。

**示例**：
```bash
# 只解析 orders 库
--db orders
```

**类型**：数据库名（字符串）

---

#### `--table <TABLE_NAME>`

**作用**：只解析指定表的 DML 操作。

**示例**：
```bash
# 只解析 users 表
--table users
```

**类型**：表名（字符串）

---

#### `--dml <TYPE_LIST>`

**作用**：只解析指定类型的 DML 操作。

**可选值**：`insert`, `update`, `delete`（逗号分隔）

**示例**：
```bash
# 只解析 INSERT 和 UPDATE
--dml insert,update

# 只解析 DELETE
--dml delete
```

**类型**：字符串列表

---

#### `--ignore-db <DB_NAME>`

**作用**：排除指定的数据库。

**示例**：
```bash
# 排除 performance_schema 和 sys
--ignore-db performance_schema --ignore-db sys
```

**类型**：数据库名列表

---

#### `--ignore-table <TABLE_PATTERN>`

**作用**：排除匹配的表。

**示例**：
```bash
# 排除所有临时表
--ignore-table %tmp%
```

**类型**：表名模式（支持通配符）

---

### 输出控制参数

#### `--threads <COUNT>`

**作用**：并行解码的线程数。

**默认值**：`8`

**性能影响**：
| threads | glibc 吞吐 | musl 吞吐 |
|---------|-----------|----------|
| 1       | ~30 MiB/s | ~10 MiB/s |
| 8       | **127.59 MiB/s** | **64.2 MiB/s** |
| 16      | ~140 MiB/s | ~80 MiB/s |

**选择建议**：
- SSD 环境：16+ 线程（充分利用 I/O）
- HDD 环境：8 以内（避免 IO 瓶颈）
- 内存受限：保持默认 8

**类型**：整数（≥ 1）  
**注意**：任意 threads 下输出字节完全一致

---

#### `--output-dir <DIR>`

**作用**：指定 SQL 文件输出目录。

**示例**：
```bash
# 输出到 ./out 目录
--output-dir ./out

# flashback 模式
--output-dir ./flashback_recovery
```

**类型**：目录路径  
**必填**：是（除非使用 `--to-stdout`）

---

#### `--to-stdout`

**作用**：直接将 SQL 输出到 stdout，而非文件。

**适用场景**：
- 管道处理（如 `| head` 预览前几行）
- 实时调试
- 快速验证

**示例**：
```bash
# 预览前 10 行
./my2sql-rs to-sql --binlog-dir ./data --to-stdout | head

# 直接应用到数据库
./my2sql-rs to-sql --binlog-dir ./data --to-stdout | mysql -uroot -p mydb
```

**注意**：不可与 `--output-dir` 同时使用

---

#### `--file-per-table`

**作用**：为每个表生成独立的 SQL 文件。

**示例**：
```bash
# 单个大 binlog → 多个小文件
./my2sql-rs to-sql \
  --binlog-dir ./large_binlog \
  --output-dir ./output \
  --file-per-table

# 结果：
# output/
# ├── dt.t_all.sql
# ├── dt.t_utf8.sql
# └── dt.t_gbk.sql
```

**优点**：
- ✅ 便于分发给不同团队
- ✅ 减小单个文件大小
- ✅ 支持增量处理

**缺点**：
- ❌ 事务可能被拆分到多个文件
- ❌ 文件数量过多

---

#### `--add-extra-info`

**作用**：在每条 SQL 前添加注释，包含元数据信息。

**输出格式**：
```sql
# datetime=2020-07-16_10:44:09 database=orchestrator table=cluster_domain_name binlog=mysql-bin.011519 startpos=15552 stoppos=15773
UPDATE `orchestrator`.`cluster_domain_name` SET `last_registered`='2020-07-16 10:44:09' WHERE `cluster_name`='192.168.1.1:3306'
```

**包含信息**：
- `datetime`：事件发生时间
- `database`：数据库名
- `table`：表名
- `binlog`：源文件
- `startpos/stoppos`：事件位置范围

**适用场景**：
- 调试追踪
- 审计日志
- 定位问题源头

---

#### `--no-db-prefix`

**作用**：SQL 语句中不包含数据库名前缀。

**对比**：
```sql
# 默认（带前缀）
INSERT INTO `dt`.`t_all` (...) VALUES (...);

# 不带前缀
INSERT INTO `t_all` (...) VALUES (...);
```

**使用场景**：
- 切换到目标库后执行
- 简化 SQL 长度

---

#### `--full-columns`

**作用**：UPDATE/DELETE 语句包含全部列信息（默认仅变化列）。

**对比**：
```sql
# 默认（省略等值列）
UPDATE `users` SET `name`='A' WHERE `id`=1 AND `email`='old@example.com';

# 全列
UPDATE `users` SET `id`=1, `name`='A', `email`='new@example.com' WHERE `id`=1;
```

**优缺点**：
- ✅ 全列：更清晰，易于理解
- ❌ 全列：语句更长，可能冗余

---

## 🔄 flashback 模式

### 基本用法

```bash
my2sql-rs flashback [OPTIONS] --binlog-dir <DIR> [SUBCOMMAND]
```

生成反向 SQL（回滚脚本），用于数据恢复。

### 核心新增参数（P6）

#### `--report-file <PATH>`

**作用**：记录被跳过的 DDL/Query 事件到 JSONL 报告文件。

**JSONL 格式**：
```json
{"timestamp": "2026-09-22T14:00:00Z", "binlog": "mysql-bin.000100", "position": 12345, "type_": "DDL", "sql": "DROP TABLE users"}
{"timestamp": "2026-09-22T14:05:00Z", "binlog": "mysql-bin.000100", "position": 12890, "type_": "BAD_EVENT", "sql": "UPDATE ..."}
```

**使用场景**：
- DBA 审计跳过事件的完整上下文
- 故障复盘
- 合规性检查

**示例**：
```bash
./my2sql-rs flashback \
  --binlog-dir ./binlogs \
  --uri "mysql://root@127.0.0.1:3306" \
  --report-file skip_events.jsonl

# 查看跳过事件
cat skip_events.jsonl
```

**类型**：文件路径（.jsonl 推荐）  
**注意**：自动追加，不覆盖已有内容

---

#### `--dry-run`

**作用**：仅统计不写盘，输出 recovery_rate% 预览摘要。

**输出格式**：
```json
{
  "summary": {
    "recovery_rate": 95.5,
    "total_transactions": 20,
    "skipped_events": 1
  },
  "binlog_range": [...],
  "warnings": [...]
}
```

**算法**：`recovery_rate = recoverable_trx / total_trx × 100%`

**价值**：DBA 预演决策——"我能恢复多少数据？"

**示例**：
```bash
# 先看能恢复多少
./my2sql-rs flashback --binlog-dir ./binlogs --dry-run

# 输出：Recovery rate: 95.5%
# 确认后再实际执行
./my2sql-rs flashback --binlog-dir ./binlogs --output-dir ./recovery
```

**注意**：不产生任何 SQL 文件

---

#### `--on-error <STRATEGY>`

**作用**：显式暴露错误处理策略。

**可选值**：
- `stop`（默认）：遇错误立即终止，不生成半成品
- `skip-bad-event`：跳过坏事件继续产出 + warning header injection

**对比**：
| 策略 | 优点 | 缺点 |
|------|------|------|
| stop | 完整性保证，无风险 | 整跑失败 |
| skip | 尽可能恢复 | 可能有遗漏 |

**示例**：
```bash
# 保守优先
./my2sql-rs flashback --binlog-dir ./binlogs --on-error stop

# 容错模式
./my2sql-rs flashback --binlog-dir ./binlogs --on-error skip-bad-event --report-file report.jsonl
```

**类型**：枚举值 `{stop, skip-bad-event}`

---

#### `--keep-trx` / `--no-keep-trx`

**作用**：是否包裹 BEGIN/COMMIT 事务脚手架。

**对比**：
```sql
# keep-trx（默认）
SET NAMES utf8mb4;
commit;
begin;
INSERT INTO users (...) VALUES (...);
...
commit;

# no-keep-trx
SET NAMES utf8mb4;
INSERT INTO users (...) VALUES (...);
...
```

**使用场景**：
- `keep-trx`：保持原子性，易于回滚
- `no-keep-trx`：纯逆序，兼容旧系统

**默认**：`keep-trx`（开）

---

## 📊 stats 模式

### 基本用法

```bash
my2sql-rs stats [OPTIONS] --binlog-dir <DIR> [SUBCOMMAND]
```

统计窗口内 DML 行数，识别大事务和长事务。

### 特有参数

#### `--big-trx-row-limit <N>`

**作用**：定义大事务的行数阈值。

**默认值**：`500`

**示例**：
```bash
# 找出影响行数≥1000 的大事务
--big-trx-row-limit 1000
```

**用途**：
- 性能优化（识别慢查询根源）
- 容量规划
- 主从延迟分析

**类型**：整数（≥ 0）

---

#### `--long-trx-seconds <SEC>`

**作用**：定义长事务的时间阈值（秒）。

**默认值**：`300`（5 分钟）

**示例**：
```bash
# 找出运行时间≥10 分钟的事务
--long-trx-seconds 600
```

**类型**：整数（≥ 0）

---

#### `--stats-json <PATH>`

**作用**：追加 JSONL 格式的统计详细记录。

**示例**：
```bash
./my2sql-rs stats \
  --binlog-dir ./binlogs \
  --output-dir ./stats \
  --stats-json events.jsonl

# 查看详情
tail -n 10 events.jsonl
```

**用途**：
- 程序化消费统计数据
- 与其他工具集成
- 自定义报表

---

## 🔄 repl 模式

### 基本用法

```bash
my2sql-rs repl [OPTIONS] --binlog-dir <DIR> [SUBCOMMAND]
```

伪装成 MySQL replica，实时拉流 binlog 持续解析。

### 特有参数

#### `--server-id <ID>` **(必需)**

**作用**：replica 服务器 ID，标识本工具身份。

**为什么必填？**
- MySQL 主从复制必需的唯一标识
- 防止 server-id 冲突导致对端强制断连

**取值范围**：整数（1 ~ 2^32-1）

**示例**：
```bash
--server-id 9527
--server-id 1001
```

**警告**：
- ❌ 不要与现有 replica ID 冲突
- ⚠️ 连续 3 次同因秒断即终止报错（防互踢）

---

#### `--resume-file <PATH>`

**作用**：checkpoint 续传文件路径，支持断点接续。

**语义**：每事务至少一次（at-least-once）
- 崩溃重放最多重复 checkpoint 后的完整事务
- 绝不半途切开事务

**使用示例**：
```bash
# 首次运行
./my2sql-rs repl --output-dir out/repl

# 崩溃后接续（必须换新的 output-dir！）
./my2sql-rs repl \
  --output-dir out/repl-2 \
  --resume-file out/repl/resume.json \
  --start-file "" --start-pos 0
```

**⚠️ 安全语义**：
- repl 永不 append 既有文件
- 接续产物永远进新 `--output-dir`
- 冲突在首个文件的创建时刻以 `create_new` 拒绝

---

#### `--heartbeat-secs <SEC>`

**作用**：心跳间隔（秒），用于探活死链。

**默认值**：`30`（0=禁用）

**工作原理**：
- 连续 2×间隔无事件 → 判死链走重连
- TCP 半开兜底

**示例**：
```bash
# 10 秒心跳（更敏感）
--heartbeat-secs 10

# 禁用心跳（不推荐）
--heartbeat-secs 0
```

**⚠️ 注意**：`0` 同时禁用死链探测与空闲期即时中断

---

#### `--stop-datetime <DATETIME>`

**作用**：停止时间边界条件。

**格式**：`YYYY-MM-DD HH:MM:SS`

**示例**：
```bash
# 运行 30 分钟后停止
./my2sql-rs repl \
  --uri "mysql://root@127.0.0.1:3306" \
  --server-id 9527 \
  --stop-datetime "$(date -u -d '+30 seconds' '+%Y-%m-%d %H:%M:%S')"
```

**触发时机**：等到下一个真实事件才判定

---

#### `--stop-pos <POS>`

**作用**：停止位点条件。

**示例**：
```bash
# 跑到 pos 点后停止
--stop-pos 1234567
```

**注意**：与 `--stop-datetime` 互斥，二选一

---

### 额外特性（超越上游 four pieces）

1. ✅ **事务边界 checkpoint** + `--resume-file` 断点接续
2. ✅ **指数退避自动重连**（1s→30s 封顶 + 抖动，无限次）
3. ✅ **心跳探活**（`--heartbeat-secs`）
4. ✅ **resume 防覆盖闸**（自设安全语义）

---

## 📚 参考链接

- [README 快速入门](../README.md)
- [CHANGELOG 完整历史](../CHANGELOG.md)
- [HANDOVER 交接文档](../HANDOVER.md)
- [上游 my2sql-go](https://github.com/liuhr/my2sql)
