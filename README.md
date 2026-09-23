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

访问 [GitHub Releases](https://github.com/0x5c0f/my2sql-rs/releases/tag/v0.5.1-p6):

```bash
# Linux x86_64 (推荐现代发行版使用 glibc 版本)
wget https://github.com/0x5c0f/my2sql-rs/releases/download/v0.5.1-p6/my2sql-rs-0.5.1-x86_64-unknown-linux-gnu
chmod +x my2sql-rs-0.5.1-x86_64-unknown-linux-gnu
./my2sql-rs-0.5.1-x86_64-unknown-linux-gnu --help

# Docker/Alpine 用户可选 musl 静态链接版本
wget https://github.com/0x5c0f/my2sql-rs/releases/download/v0.5.1-p6/my2sql-rs-0.5.1-x86_64-unknown-linux-musl
chmod +x my2sql-rs-0.5.1-x86_64-unknown-linux-musl
./my2sql-rs-0.5.1-x86_64-unknown-linux-musl --help
```

验证哈希：
```bash
sha256sum -c SHA256SUMS
```

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

完整参数列表：
```bash
./my2sql-rs to-sql --help
./my2sql-rs flashback --help
./my2sql-rs stats --help
./my2sql-rs repl --help
```

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
- **社区贡献者**: 感谢所有提交 PR 和反馈 Issue 的用户

---

**版本**: v0.5.1-p6  |  **发布日期**: 2026-09-23  |  **状态**: Stable ✅

[![](https://img.shields.io/badge/-View%20on-GitHub-blue.svg)](https://github.com/0x5c0f/my2sql-rs)
