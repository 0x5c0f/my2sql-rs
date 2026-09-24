# my2sql-rs

[![CI Status](https://github.com/0x5c0f/my2sql-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/0x5c0f/my2sql-rs/actions)
[![Release](https://img.shields.io/github/v/release/0x5c0f/my2sql-rs)](https://github.com/0x5c0f/my2sql-rs/releases)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](LICENSE)

MySQL binlog 解析与 SQL 还原工具——用 Rust 重写的 Go my2sql 实现，提供离线 binlog 回放、数据恢复、统计报表等功能。

## 🎯 项目简介

**my2sql-rs** 是一个高性能的 MySQL binlog 解析工具，能够将 MySQL 的二进制日志文件（binlog）转换为可执行的 SQL 语句。主要适用于以下场景：

- **数据恢复**: 从 binlog 中恢复误删除或损坏的数据
- **审计分析**: 记录数据库变更历史，生成操作报告
- **架构演进**: 主从同步延迟检测、数据一致性校验
- **离线回放**: 无需连接数据库即可解码 binlog 文件

## ✨ 核心功能

| 功能 | 说明 |
|------|------|
| `to-sql` 模式 | 离线读取 binlog 文件，还原 INSERT/UPDATE/DELETE 语句 |
| `flashback` 模式 | 生成反向 SQL（回滚脚本），支持 dry-run 预览恢复率 |
| `stats` 报表 | 统计窗口内 DML 行数，识别大事务和长事务 |
| `repl` 模式 | 伪装 MySQL replica，实时拉流 binlog 持续解析 |

### 🔥 P6「数据恢复面」新增功能

- ✅ **Report-file**: JSONL 格式记录被跳过的 DDL 事件（位置 + 类型 + SQL 原文）
- ✅ **Dry-run preview**: 输出 recovery_rate% 统计摘要，DBA 预演决策依据
- ✅ **On-error 策略**: `--on-error {stop,skip-bad-event}` 显式暴露错误处理开关

## 📦 快速开始

### 安装方式

#### 方式一：下载预编译二进制

预编译产物随每个版本发布在 [GitHub Releases](https://github.com/0x5c0f/my2sql-rs/releases) 页面。
每个版本包含两个 Linux x86_64 目标，以及一份 sha256 校验清单：

- **glibc 动态链接版**（文件名形如 `my2sql-rs-<版本>-x86_64-unknown-linux-gnu`）——适配常规现代发行版，推荐优先选择
- **musl 静态链接版**（文件名形如 `my2sql-rs-<版本>-x86_64-unknown-linux-musl`）——适配 Alpine、精简 Docker 镜像等无 glibc 环境
- **`SHA256SUMS`** ——上述两个目标的 sha256 清单，用于校验完整性

在 Releases 页面按需选择版本：标有 **Latest** 的为最新正式版，标有 **Pre-release** 的为预发布版本（对应 tag 以 `-pre` 结尾）。
下载对应目标的二进制后，赋予可执行权限即可直接运行，加 `--help` 查看用法；
如需校验完整性，下载同一版本附带的 `SHA256SUMS` 一并做 sha256 校验即可。

#### 方式二：源码构建

```bash
# 安装依赖（Ubuntu/Debian）
apt-get update && apt-get install -y build-essential musl-gcc

# glibc 版本
cargo build --release

# musl 版本（需要 musl-tools）
cargo build --release --target x86_64-unknown-linux-musl
```

### 基本用法

```bash
# 1. 示例：解析 binlog 文件为 SQL
my2sql-rs to-sql \
  --binlog-dir data/8.0 \
  --start-file mysql-bin.000100 \
  --uri "mysql://root@127.0.0.1:3306" \
  --threads 8 \
  --to-stdout | head

# 2. 生成回滚 SQL（Flashback）
my2sql-rs flashback \
  --binlog-dir data/8.0 \
  --start-file mysql-bin.000100 \
  --uri "mysql://root@127.0.0.1:3306" \
  --output-dir out/flashback

# 3. 先看预览（dry-run 模式）
my2sql-rs flashback \
  --binlog-dir data/8.0 \
  --start-file mysql-bin.000100 \
  --uri "mysql://root@127.0.0.1:3306" \
  --dry-run

# 4. 统计 DML 操作（Stats）
my2sql-rs stats \
  --binlog-dir data/8.0 \
  --start-file mysql-bin.000100 \
  --uri "mysql://root@127.0.0.1:3306" \
  --output-dir out/stats

# 5. 实时拉流（Repl 模式）
my2sql-rs repl \
  --binlog-dir /nonused \
  --start-file "" \
  --uri "mysql://root@127.0.0.1:3306" \
  --server-id 9527 \
  --output-dir out/repl
```

## 🚀 高级特性

### 离线 schema 回放

无需连接数据库，先导出表结构后离线解码：

```bash
# Step 1: 在线跑一次，导出 schema dump
./my2sql-rs to-sql \
  --binlog-dir data/8.0 \
  --uri "mysql://root@localhost:3306" \
  --schema-dump schema.json

# Step 2: 断网后用 schema.json 离线回放
./my2sql-rs to-sql \
  --binlog-dir data/8.0 \
  --schema-file schema.json \
  --to-stdout
```

### 并行解码

```bash
# threads=8 时吞吐量达 127.59 MiB/s
./my2sql-rs to-sql \
  --binlog-dir data/8.0 \
  --threads 8 \
  --to-stdout
```

### 精确过滤

```bash
# 只解析特定库表的 DML
./my2sql-rs to-sql \
  --binlog-dir data/8.0 \
  --db users \
  --table orders \
  --dml insert,update \
  --to-stdout
```

## 📊 性能指标

| 指标 | 数值 |
|------|------|
| **吞吐量** | 127.59 MiB/s @ threads=8 (criterion benchmark) |
| **musl 性能** | 64.2 MiB/s @ threads=8 (经 mimalloc 优化) |
| **兼容性** | MySQL 5.6/5.7/8.0/8.4 |
| **测试覆盖** | 949 单元测试 + E2E 测试通过 |

详细性能数据见 [docs/bench/p4b.md](docs/bench/p4b.md)。

---

## 🔧 核心概念说明

### `--binlog-dir` - Binlog 文件目录

指定 MySQL binlog 二进制日志所在的**本地文件系统目录**，而非数据库连接。

**为什么需要本地目录？**  
MySQL 的 binlog 是文件形式的日志（类似手机相册），存储在服务器的数据目录下：
- MySQL 5.6: `/var/lib/mysql/binlog.*`
- MySQL 5.7+: `/var/lib/mysql/mysql-bin.*`

工具需要从磁盘读取这些二进制文件才能解析出 SQL 语句。

**获取 binlog 路径的方法：**
```bash
# 方法 1: 查看 MySQL 配置（所有环境通用）
mysql -uroot -p -e "SHOW VARIABLES LIKE 'datadir';"

# 方法 2: 直接在宿主机查看（物理机或虚拟机）
ls -la /var/lib/mysql/mysql-bin.*

# 方法 3: 通过 SHOW BINARY LOGS 查看所有可用文件
mysql -uroot -p -e "SHOW BINARY LOGS;"
```

**如何复制 binlog 到本地进行分析？**

**Docker 容器环境：**
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

**物理机/虚拟机环境：**
```bash
# 直接复制到本地分析目录
sudo cp /var/lib/mysql/mysql-bin.* ~/analysis/binlogs/

# 或直接使用（需确保工具对文件有读权限）
sudo chown -R your_user:your_group /var/lib/mysql/mysql-bin.*
```

**云数据库环境（如 RDS、PolarDB）：**
```bash
# 通常需要通过控制台下载或使用内网 IP 连接
# 阿里云 RDS: 登录控制台 → 实例详情 → 备份恢复 → 下载 Binlog
# PolarDB: 可通过 DataWorks 或其他工具导出
```

### `--uri` - 数据库连接字符串

用于**在线拉取 schema 信息**（表结构定义），格式为 `mysql://[user]:[password]@[host]:[port]`。

**为什么需要数据库连接？**  
Binlog 中只记录数据的变动（INSERT/UPDATE/DELETE 的值），不包含表的定义。工具需要连接数据库查询：
- 表有多少列？
- 每列的数据类型是什么？
- 主键/索引结构？

**常用格式示例：**
```bash
# 基础格式
--uri "mysql://root:@127.0.0.1:3306"           # root 用户，无密码
--uri "mysql://root:password@127.0.0.1:3306"   # root 用户，带密码
--uri "mysql://app_user:secret@db.example.com:3306/mydb"  # 指定库

# URI 编码（密码含特殊字符时）
--uri "mysql://root:P%40ssw0rd@127.0.0.1:3306"  # P@ssw0rd 需要编码
```

**权限要求：**
- 最小权限：`REPLICATION SLAVE, REPLICATION CLIENT`
- 推荐：新建专用账号
  ```sql
  CREATE USER 'repl'@'%' IDENTIFIED BY 'password';
  GRANT REPLICATION SLAVE, REPLICATION CLIENT ON *.* TO 'repl'@'%';
  FLUSH PRIVILEGES;
  ```

### `--schema-dump` / `--schema-file` - 离线模式神器

**无需连接数据库即可解码 binlog！**

**工作流程：**
```bash
# Step 1: 首次运行时导出表结构（需 --uri 连接数据库）
./my2sql-rs to-sql \
  --binlog-dir data/8.0 \
  --uri "mysql://root@127.0.0.1:3306" \
  --schema-dump schema.json  # ← 导出表结构到 JSON

# Step 2: 断网/远程环境仅用 schema 文件解码
./my2sql-rs to-sql \
  --binlog-dir data/8.0 \
  --schema-file schema.json \
  --to-stdout
```

**适用场景：**
- ✅ 生产环境无法直连（网络隔离）
- ✅ 多环境复用（同一份 schema 可用于 dev/staging/prod）
- ✅ 安全审计（不暴露数据库连接）

---

## 💡 典型使用场景

### 场景 1：恢复误删除的数据 🚑

**问题：** 刚执行了 `DELETE FROM users WHERE id = 5;`，立刻发现删错了！

**解决方案：**
```bash
# 1. 找到删除操作发生的 binlog 文件
mysql -uroot -p -e "SHOW BINARY LOGS;"
# 输出：mysql-bin.000100  123456
#       mysql-bin.000101  789012  ← 删除发生在这个文件

# 2. 生成回滚 SQL（flashback 模式）
./my2sql-rs flashback \
  --binlog-dir ./binlogs \
  --start-file mysql-bin.000101 \
  --uri "mysql://root@127.0.0.1:3306" \
  --threads 8 \
  --output-dir ./recovery

# 3. 预览要恢复的数据（dry-run 模式）
./my2sql-rs flashback \
  --binlog-dir ./binlogs \
  --start-file mysql-bin.000101 \
  --uri "mysql://root@127.0.0.1:3306" \
  --dry-run

# 输出：{"summary":{"recovery_rate":95.5,"total_transactions":20,"skipped_events":1},...}

# 4. 检查恢复的 SQL 文件
cat ./recovery/flashback.1.sql
# 包含：INSERT INTO users (...) VALUES (...);  ← 被删的那条记录

# 5. 执行恢复
mysql -uroot -p mydb < ./recovery/flashback.1.sql
```

---

### 场景 2：审计某段时间的操作 📝

**问题：** 想知道昨天下午 2 点到 4 点之间对订单做了什么修改？

**解决方案：**
```bash
# 提取特定时间窗口的 DML
./my2sql-rs to-sql \
  --binlog-dir ./binlogs \
  --start-time "2026-09-22 14:00:00" \
  --end-time "2026-09-22 16:00:00" \
  --db orders \
  --table order_items \
  --dml update,delete \
  --to-stdout > audit_report.sql

# 统计报表（行数汇总）
./my2sql-rs stats \
  --binlog-dir ./binlogs \
  --start-time "2026-09-22 14:00:00" \
  --end-time "2026-09-22 16:00:00" \
  --output-dir ./stats_report

# 查看结果
cat ./stats_report/binlog_status.txt
# 格式：文件名  开始时间  结束时间  DML 行数  Update 数  Delete 数  Insert 数  大事务 长事务 库名  表名
```

---

### 场景 3：迁移数据到另一张表 🔄

**问题：** 需要将历史数据从旧表结构迁移到新表结构。

**解决方案：**
```bash
# 1. 导出所有历史数据的 INSERT 语句
./my2sql-rs to-sql \
  --binlog-dir ./old_logs \
  --db old_schema \
  --table products \
  --start-file mysql-bin.000001 \
  --stop-file mysql-bin.000500 \
  --dml insert \
  --to-stdout > product_inserts.sql

# 2. 在新库导入（可先调整字段顺序）
mysql -uroot -p new_schema < product_inserts.sql
```

---

### 场景 4：实时监控数据库变更 👀

**问题：** 想实时监控某个库的数据变更（替代部分 canal 功能）。

**解决方案：**
```bash
# repl 模式持续拉流（伪装成 MySQL replica）
./my2sql-rs repl \
  --binlog-dir /nonused \
  --start-file "" \
  --uri "mysql://root@127.0.0.1:3306" \
  --server-id 9527 \
  --time-zone +00:00 \
  --output-dir ./realtime \
  --heartbeat-secs 30

# 后台运行并定期清理
nohup ./my2sql-rs repl ... > /dev/null 2>&1 &

# 查看进度（checkpoint 自动记录）
cat ./realtime/resume.json
```

**特性：**
- ✅ 断线自动重连（指数退避策略）
- ✅ 心跳探活（检测死链）
- ✅ 崩溃后无缝接续（resume.json checkpoint）

---

### 场景 5：离线分析服务器上的 binlog 🧪

**问题：** 把生产环境的 binlog 拷到开发机做分析，但开发机没有数据库连接。

**解决方案：**
```bash
# Step 1: 在可连接数据库的环境导出表结构
./my2sql-rs to-sql \
  --binlog-dir /prod_binlogs \
  --uri "mysql://prod_user:password@prod_db:3306" \
  --schema-dump schema.json

# Step 2: 将 schema.json 拷贝到无库环境
scp schema.json dev@dev-server:/home/dev/project/

# Step 3: 仅用 schema 文件解码 binlog（无需数据库连接）
./my2sql-rs to-sql \
  --binlog-dir /dev_binlogs \
  --schema-file schema.json \
  --to-stdout | head
```

> 💡 **关于 Schema 来源的说明**  
> 
> Binlog 中只记录数据变更（如 `UPDATE users SET name='A' WHERE id=1`），但不包含表的定义（如 `CREATE TABLE users (id INT, name VARCHAR(100))`）。工具需要知道表的**列名、类型、主键结构**才能正确生成 SQL。因此必须提供 schema 信息，可通过以下方式之一获取：
> 
> - **直连数据库**：`--uri`参数自动拉取实时 schema（最简单）
> - **预先导出 schema.json**：`--schema-dump` 一次导出，后续可多次复用（推荐用于离线/测试环境）
> - **从 mysqldump 提取**：通过 grep 或临时 MySQL 实例恢复表结构后，再用`--schema-dump`导出为 JSON 格式
> 
> **参考上游**：[my2sql-go](https://github.com/liuhr/my2sql) 同样要求提供 schema 或直连数据库，这是 binlog 解析工具的通用设计模式。

---

## ⚠️ 常见问题 FAQ

### Q: `--start-file` 应该填什么？
A: 填写 binlog 文件名，如 `mysql-bin.000100`，不是完整路径。完整路径由 `--binlog-dir` 指定。

### Q: 如何知道删除操作在哪个 binlog 文件里？
A: 
```bash
# 方法 1: 查看 MySQL 的二进制日志列表
mysql -uroot -p -e "SHOW BINARY LOGS;"

# 方法 2: 查看每个文件的时间范围
./my2sql-rs to-sql --binlog-dir ./data --list-files
```

### Q: `--threads` 设置多少合适？
A: 默认 8 线程适合大多数场景。SSD 环境下可以尝试 16+ 线程提升吞吐；机械硬盘建议保持 8 以内避免 IO 瓶颈。

### Q: Flashback 能否恢复 DDL（如 DROP TABLE）？
A: ❌ **不支持**。DDL 反向 SQL 需要业务理解（如 DROP 后重建表需已知结构），目前只支持 DML（INSERT/UPDATE/DELETE）的反向恢复。

## ⚙️ 配置参数

### 常用命令行选项

```bash
通用选项:
  --binlog-dir <DIR>          binlog 目录路径（必需）
  --start-file <FILE>         起始文件名（默认：最新文件）
  --start-pos <POS>           起始位点（默认：4）
  --end-file <FILE>           结束文件名
  --end-pos <POS>             结束位点
  
输出控制:
  --threads <COUNT>           并行线程数（默认：8）
  --output-dir <DIR>          输出目录
  --to-stdout                 直接输出到 stdout
  --file-per-table            每个表独立一个文件
  --time-zone <TZ>            时区偏移（默认：+00:00）
  
Flashback 专项:
  --report-file <PATH>        DDL Skip 事件 JSONL 报告路径
  --dry-run                   仅统计不写盘，输出 summary JSON
  --on-error <STRATEGY>       stop\skip-bad-event（默认：stop）

Repl 专项:
  --server-id <ID>            服务器 ID（必需，无默认值）
  --resume-file <PATH>        checkpoint 续传文件
  --heartbeat-secs <SEC>      心跳间隔（默认：30）
  --stop-datetime <DATETIME>  停止时间边界
```

完整命令行参数说明请参考 [COMMAND_LINE_OPTIONS.md](docs/COMMAND_LINE_OPTIONS.md)。

常见使用场景和案例请参考 [USE_CASES.md](docs/USE_CASES.md)。

## 🔍 与 Go my2sql 的差异

本工具在保持行为等价的基础上进行了多项改进：

1. **CLI 全新设计**: 参数命名更清晰，结构更符合 Rust 习惯
2. **确定性输出**: 任意 threads 下输出字节完全一致（上游受 Go map 随机序影响）
3. **容错机制**: 遇错误事件 skip+b 计数而非直接终止整跑
4. **离线模式**: 支持 `--schema-dump` 导出表结构后断网回放
5. **增强报告**: report-file + dry-run 提升可解释性和可控性

详细差异清单见 [CHANGELOG.md](CHANGELOG.md) 第 7 节。

## 🧪 测试

项目采用 TDD 开发模式，包含丰富的单元测试和 E2E 测试：

```bash
# 运行所有测试
cargo test

# 差分测试（需本地 Go my2sql 裁判）
make difftest

# 全版本兼容测试
make compat

# fuzz 模糊测试（短期模式）
make fuzz-min

# 影子库端到端验证
make shadow-test
```

测试结果：
- **单元测试**: 949 passed / 0 failed
- **Clippy**: 零警告
- **rustfmt**: 格式化合规
- **Fuzzing**: 300s×2 真跑 0 crash

## 📖 文档资源

- **[CHANGELOG.md](CHANGELOG.md)**: 完整发布历史和功能里程碑
- **[docs/HANDOVER.md](docs/HANDOVER.md)**: 交接文档、白名单台账、DoD 对账
- **[docs/compat/matrix.md](docs/compat/matrix.md)**: 全版本兼容矩阵测试报告
- **[docs/AUDIT_LOG.md](docs/AUDIT_LOG.md)**: 代码质量审计报告
- **[docs/bench/p4b.md](docs/bench/p4b.md)**: 性能基线详解

## 🛠️ 技术栈

- **Rust**: 主语言，1.96+ 稳定版
- **serde**: JSON序列化（P6 report-file/dry-run）
- **clap**: CLI参数解析
- **crossbeam-channel**: 高并发管道通信
- **mysql crate**: MySQL协议解码器
- **mimalloc**: 内存分配器（musl 性能优化关键）

## 🤝 参与贡献

欢迎提交 Issue 和 PR！提交前请确保：

1. 通过所有测试：`cargo test && cargo clippy --all-targets`
2. 代码格式化：`cargo fmt`
3. 更新测试用例（新增功能必须配套测试）
4. 更新文档（API 变更需同步 README 和手册）

## 📄 许可证

本项目采用 **Apache License 2.0** 开源协议。详见 [LICENSE](LICENSE) 文件。

```
Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

    http://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.
```

## 🙏 致谢

- **Go my2sql**: [liuhr/my2sql](https://github.com/liuhr/my2sql) — 原始参考实现
- **mysql-rs**: [blackfin/node_mysql2](https://github.com/blackfin/node_mysql2) — Rust MySQL 生态

---

**版本**: v0.5.1-p6  |  **发布日期**: 2026-09-23  |  **状态**: Stable ✅

[![](https://img.shields.io/badge/-View%20on-GitHub-blue.svg)](https://github.com/0x5c0f/my2sql-rs)
