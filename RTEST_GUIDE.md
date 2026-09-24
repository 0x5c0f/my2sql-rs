# my2sql-rs 全功能测试手册 v0.5.2-p7

> **版本**: v0.5.2-p7 (2026-09-24)  
> **文档目标**: 从 0 开始的完整测试指南，涵盖所有功能的单元测试、集成测试和端到端验证  
> **适用人群**: 开发者 / QA / 技术用户（混合层级）  

---

## 📚 目录导航

1. [快速开始](#1-快速开始)
2. [环境准备与数据库生成](#2-环境准备与数据库生成)
3. [项目架构概览](#3-项目架构概览)
4. [单元测试详解](#4-单元测试详解)
5. [集成测试详解](#5-集成测试详解)
6. [端到端 (E2E) 测试矩阵](#6-端到端-e2e-测试矩阵)
7. [边界情况与异常处理测试](#7-边界情况与异常处理测试)
8. [性能测试基准](#8-性能测试基准)
9. [自动化测试运行指南](#9-自动化测试运行指南)
10. [问题排查与故障排除](#10-问题排查与故障排除)

**附录**
- A: Fixtures Schema Reference
- B: Sample Binlogs Explanation
- C: Test Data Generator Scripts

---

## 1. 快速开始

### 1.1 最低系统要求

| 组件 | 要求 | 验证命令 |
|------|------|----------|
| Rust 工具链 | ≥ 1.75.0 (推荐使用 1.96.0) | `rustc --version` |
| Cargo | 配套 Rust 版本 | `cargo --version` |
| Docker | ≥ 24.0.0 (用于 E2E MySQL 容器) | `docker --version` |
| MySQL Client | ≥ 8.0.0 (可选，本地连接测试) | `mysql --version` |
| jq | ≥ 1.5 (用于 JSON 处理) | `jq --version` |

### 1.2 代码克隆与编译

```bash
# 1. 克隆仓库
git clone https://github.com/0x5c0f/my2sql-rs.git
cd my2sql-rs

# 2. 构建发布版本
cargo build --release

# 3. 验证二进制文件
./target/release/my2sql-rs --help

# ✅ 预期输出应显示 4 个子命令：
#    to-sql | flashback | stats | repl
```

### 1.3 首次运行前的检查清单

```markdown
- [ ] Rust 工具链已安装且版本符合
- [ ] 项目成功编译无警告 (`cargo clippy`)
- [ ] Docker 守护进程正在运行
- [ ] 至少有一个 MySQL 实例可访问 (本地或容器)
- [ ] 环境变量 `MY2SQL_TEST_URI` 已配置 (可选，用于测试)
```

---

## 2. 环境准备与数据库生成

### 2.1 测试环境架构

本项目采用 **三层测试环境** 设计：

```
┌─────────────────────────────────────────┐
│   Layer 3: E2E (Integration with MySQL) │
│   - Real MySQL 8.0 container            │
│   - Full transaction replay             │
│   - Network-level validation            │
├─────────────────────────────────────────┤
│   Layer 2: Integration (Fixture-based)  │
│   - Pre-captured binlog fixtures        │
│   - Schema JSON matching                │
│   - Event parsing verification          │
├─────────────────────────────────────────┤
│   Layer 1: Unit Tests (Pure Code)       │
│   - No external dependencies            │
│   - Deterministic behavior              │
│   - Edge case coverage                  │
└─────────────────────────────────────────┘
```

### 2.2 Docker Compose 测试 MySQL 集群

创建 `docker-compose.test.yml`:

```yaml
version: '3.8'

services:
  mysql-8.0:
    image: mysql:8.0.46
    container_name: my2sql-test-mysql
    environment:
      MYSQL_ROOT_PASSWORD: root
      MYSQL_DATABASE: test_db
      MYSQL_USER: test_user
      MYSQL_PASSWORD: test_pass
    ports:
      - "3306:3306"
    volumes:
      - mysql-data:/var/lib/mysql
      - ./tests/docker-entrypoint-initdb.d:/docker-entrypoint-initdb.d
    healthcheck:
      test: ["CMD", "mysqladmin", "ping", "-h", "localhost"]
      interval: 5s
      timeout: 3s
      retries: 10
    command: |
      --binlog-format=ROW
      --binlog-checksum=CRC32
      --binlog-row-image=FULL
      --server-id=1

  mysql-5.6:
    image: mysql:5.6.51
    container_name: my2sql-test-mysql-56
    environment:
      MYSQL_ROOT_PASSWORD: root
      MYSQL_DATABASE: test_db
    ports:
      - "3307:3306"
    volumes:
      - mysql-56-data:/var/lib/mysql
    healthcheck:
      test: ["CMD", "mysqladmin", "ping", "-h", "localhost"]
      interval: 5s
      timeout: 3s
      retries: 10

volumes:
  mysql-data:
  mysql-56-data:
```

### 2.3 初始化脚本

#### 2.3.1 MySQL 8.0 Schema Setup

路径：`tests/docker-entrypoint-initdb.d/01-schema.sql`

```sql
-- Create test database if not exists
CREATE DATABASE IF NOT EXISTS t10;
USE t10;

-- Table 1: FULL image test table (no PK)
CREATE TABLE u (
    a INT NOT NULL,
    b VARCHAR(8) NOT NULL,
    c INT NULL,
    d INT NULL,
    e INT NULL,
    f INT NULL,
    g INT NULL,
    h INT NULL,
    i2 INT NULL
);

-- Table 2: JSON column support
CREATE TABLE j (
    id INT PRIMARY KEY,
    doc JSON NOT NULL,
    tag VARCHAR(4) NULL
);

-- Insert sample data for FULL flashback
INSERT INTO u VALUES 
    (1, 'ab', 10, 20, 30, 40, NULL, 60, 70),
    (2, 'xyz', NULL, 11, NULL, 0, 99, NULL, -5);

-- Insert sample JSON data
INSERT INTO j VALUES 
    (1, '{"a":1,"b":[1,2]}', 'tt'),
    (2, '{"a":9,"b":[1,2,3],"c":"test"}', 'xx');

COMMIT;
```

#### 2.3.2 Row Image Configuration Scripts

**FULL 镜像会话脚本** (`02-full-session.sql`):

```sql
SET SESSION binlog_row_image = 'FULL';

-- Generate transactions that will be captured
INSERT INTO u (a, b, c, d, e, f, g, h, i2) 
VALUES (3, 'test1', 100, 200, 300, 400, 500, 600, 700);

UPDATE u SET c = NULL WHERE a = 1 AND b = 'ab';

COMMIT;
```

**MINIMAL 镜像会话脚本** (`03-minimal-session.sql`):

```sql
SET SESSION binlog_row_image = 'MINIMAL';

-- This will generate incomplete after-images
UPDATE u SET c = 999 WHERE a = 2 AND b = 'xyz';

COMMIT;
```

### 2.4 数据捕获器

为了生成真实可用的 binlog fixture，创建一个 Python 脚本：

路径：`scripts/capture_binlogs.py`

```python
#!/usr/bin/env python3
"""Capture real binlogs from MySQL for testing."""

import subprocess
import shutil
import os
from pathlib import Path

def capture_binlogs(mysql_uri: str, output_dir: Path, start_file: str = "binlog.000001"):
    """Use mysqlbinlog to capture binlogs into test fixtures."""
    
    # Ensure output directory exists
    output_dir.mkdir(parents=True, exist_ok=True)
    
    # Run mysqlbinlog
    cmd = [
        "mysqlbinlog",
        f"--start-position=4",  # Start from beginning
        f"--database=t10",
        f"--result-file={output_dir}/{start_file}"
    ]
    
    print(f"Capturing binlogs from {mysql_uri}...")
    result = subprocess.run(cmd, env={"MYSQL_PWD": "root"})
    
    if result.returncode == 0:
        print(f"✓ Captured to {output_dir}")
        return True
    else:
        print(f"✗ Capture failed: {result.stderr}")
        return False

if __name__ == "__main__":
    # Usage: python scripts/capture_binlogs.py mysql://root:@127.0.0.1:3306
    capture_binlogs("mysql://root:@127.0.0.1:3306", Path("tests/fixtures"))
```

### 2.5 一键启动测试环境

创建 `Makefile.test`:

```makefile
.PHONY: up down clean setup mysql-shell fixtures

UP_CMD = docker-compose -f docker-compose.test.yml up -d

up:
	@echo "Starting test MySQL containers..."
	$(UP_CMD)
	@echo "Waiting for MySQL to be ready..."
	@sleep 10
	@echo "Running schema initialization..."
	docker exec my2sql-test-mysql mysql -uroot -proot < tests/docker-entrypoint-initdb.d/01-schema.sql
	@echo "✓ Test environment ready!"

down:
	@echo "Stopping test containers..."
	docker-compose -f docker-compose.test.yml down
	@echo "✓ Containers stopped."

clean: down
	@echo "Cleaning all test data..."
	docker volume rm my2sql-test_mysql-data my2sql-test_mysql-56-data || true
	@rm -rf tests/fixtures/*
	@echo "✓ Cleanup complete."

setup: up
	@echo "Generating additional test scenarios..."
	@docker exec my2sql-test-mysql mysql -uroot -proot < tests/docker-entrypoint-initdb.d/02-full-session.sql
	@docker exec my2sql-test-mysql mysql -uroot -proot < tests/docker-entrypoint-initdb.d/03-minimal-session.sql
	@echo "✓ All test scenarios generated."

mysql-shell:
	@echo "Opening MySQL shell..."
	docker exec -it my2sql-test-mysql mysql -uroot -proot t10

fixtures:
	@echo "Generating binary fixtures from running container..."
	mysqlbinlog --host=127.0.0.1 --port=3306 --user=root --password=root \
		--start-datetime="2024-01-01 00:00:00" \
		tests/fixtures/capture_8.0_rows/
```

使用方式：
```bash
# 1. 启动完整测试环境
make up
make setup

# 2. 连接到 MySQL shell
make mysql-shell

# 3. 清理环境
make clean
```

---

## 3. 项目架构概览

### 3.1 核心功能模块

#### 3.1.1 To-SQL Mode (正向转换)

将 MySQL binlog 转换为标准 SQL 语句。

**关键特性**:
- ROW format binlog 解析
- 支持 CRC32/NONE checksum 模式
- Multi-threaded processing (configurable threads)
- Automatic DDL exclusion from output

**输入示例**:
```bash
my2sql-rs to-sql \
  --binlog-dir /path/to/binlogs \
  --start-file binlog.000001 \
  --stop-file binlog.000010 \
  --schema-file schema.json \
  --uri mysql://user:pass@localhost:3306 \
  --threads 4
```

**输出格式**:
```sql
-- FILE_HEADER
SET NAMES utf8mb4;

-- Transaction 1
INSERT INTO `t10`.`u` (`a`,`b`,`c`,`d`,`e`,`f`,`g`,`h`,`i2`) 
VALUES (1,'ab',10,20,30,40,NULL,60,70);

COMMIT;

-- Transaction 2
UPDATE `t10`.`u` SET `c`=NULL WHERE `a`=1 AND `b`='ab' AND `c`=10 AND `d`=20 AND `e`=30 AND `f`=40 AND `g`=NULL AND `h`=60 AND `i2`=70;

COMMIT;
```

#### 3.1.2 Flashback Mode (回滚脚本)

生成逆序 SQL，用于事务回滚。

**核心算法**:
1. 读取 binlog 按时间顺序
2. 对每个行事件构建逆向操作：
   - `INSERT` → `DELETE`
   - `DELETE` → `INSERT`  
   - `UPDATE` → `UPDATE` (SWAP SET/WHERE clauses)
3. 保持事务完整性
4. 反向写入 SQL 文件

**命令行参数**:
```bash
my2sql-rs flashback \
  --binlog-dir /path/to/binlogs \
  --start-file binlog.000001 \
  --output-dir /path/to/output \
  --keep-trx           # 保持 BEGIN/COMMIT (默认 true)
  --on-error stop      # stop | skip-bad-event
  --threads 2
```

**输出产物**:
```sql
-- WARNING: skipped N events
SET NAMES utf8mb4;

-- Reverse order: Transaction 2 rollback first
BEGIN;
UPDATE `t10`.`u` SET `c`=10,`d`=20,... WHERE `a`=1 AND `b`='ab' AND `c`=NULL AND `d`=NULL,...;
COMMIT;

-- Then Transaction 1 rollback
BEGIN;
DELETE FROM `t10`.`u` WHERE `a`=1 AND `b`='ab' AND `c`=10 AND `d`=20 AND `e`=30 AND `f`=40 AND `g`=NULL AND `h`=60 AND `i2`=70;
COMMIT;
```

#### 3.1.3 Stats Mode (统计摘要)

生成 binlog 分析报告，用于审计和数据质量评估。

**统计指标**:
- Total events / transactions
- Per-table distribution
- Error counts (skipped events)
- Recovery rate calculation (for flashback)
- Time window analysis

**输出格式**: JSONL (line-delimited JSON)
```json
{"ts":"2026-09-20T13:12:08Z","event_type":"QUERY","table":"null","rows":null}
{"ts":"2026-09-20T13:12:10Z","event_type":"WRITE_ROWS","table":"t10.u","rows":1}
{"ts":"2026-09-20T13:12:11Z","event_type":"UPDATE_ROWS","table":"t10.u","rows":1}
```

**批量报表**:
```bash
my2sql-rs stats \
  --binlog-dir /path/to/binlogs \
  --output-dir /path/to/stats \
  --stats-json                 # 启用 JSONL 输出
  --long-trx-threshold 300     # 长事务秒阈值
```

#### 3.1.4 Repl Mode (实时复制)

模拟 MySQL replica，持续订阅 binlog 流并转发到应用层。

**关键组件**:
- GTID-based position tracking
- Heartbeat mechanism (configurable interval)
- Checkpoint persistence
- Graceful resume capability

**配置示例**:
```bash
my2sql-rs repl \
  --uri mysql://replica_user:pass@localhost:3306 \
  --server-id 9999 \
  --heartbeat-interval 30s \
  --resume-file checkpoint.json
```

### 3.2 Binlog 解析器架构

```mermaid
graph TB
    A[FileReader] --> B[Event Stream]
    B --> C{Filter?}
    C -->|Filter| D[TableMap Lookup]
    C -->|No Filter| E[Skip Non-Row Events]
    D --> F[Rows Decoder]
    E --> F
    F --> G[Value Converter]
    G --> H[Build SQL]
    H --> I[Emitter: SQL Writer]
    H --> J[Emitter: Stats Aggregator]
```

**核心类型定义**:
```rust
pub enum RawKind {
    Rows(RowsKind, bool),  // (type, v2_flag)
    Query,
    Xid,
    Gtid,
    // ... other event types
}

pub struct Job {
    pub seq: u64,
    pub ev: RawEvent,
    // ... job metadata
}

pub enum OutMode {
    Sql,   // Normal SQL generation
    Stats, // Statistics-only mode
}
```

---

## 4. 单元测试详解

### 4.1 运行命令

```bash
# 全部单元测试
cargo test --lib

# 单个模块
cargo test --lib binlog::rows

# 带输出展示
cargo test --lib -- --nocapture
```

### 4.2 Binlog Event Parser Tests

**文件位置**: `src/binlog/event.rs`

**核心测试场景**:

#### Test 1: STOP Event (19 bytes NONE format)
```rust
#[test]
fn stop_event_19_bytes_none_format_acceptable() {
    // B013 fix: NONE+Stop scenario allows exactly 19B STOP event
    let mut ev = vec![0u8; EVENT_HEADER_SIZE];
    ev[4] = EventType::STOP;
    ev[9..13].copy_from_slice(&19u32.to_le_bytes());
    assert!(parse_header(&ev).is_ok(), "NONE Stop(19B) should pass");
}
```

**验证点**:
- Event size `< 19` rejected (too small for header)
- Event size `== 19` accepted (zero-body events like STOP/HEARTBEAT)
- Event size `> MAX_EVENT_SIZE (4MB)` rejected (DoS protection)

#### Test 2: CRC32 Checksum Validation
```rust
#[test]
fn crc32_checksum_mismatch_detected() {
    let mut ev = create_test_event_with_body();
    ev.extend_from_slice(&[0xFF; 4]); // Wrong CRC
    
    match parse_and_verify(&ev) {
        Err(BinlogError::ChecksumMismatch) => {}, // Expected
        _ => panic!("Expected ChecksumMismatch error"),
    }
}
```

#### Test 3: Partial Rows Detection (Event Type 39)
```rust
#[test]
fn partial_update_rows_v2_rejected() {
    let mut buf = vec![0u8; HEADER_SIZE + 1];
    buf[4] = EventType::PARTIAL_UPDATE_ROWS_V2; // Type 39
    
    match FileReader::next_event(buf.into()) {
        Err(BinlogError::PartialNotSupported) => {},
        _ => panic!("Should reject PARTIAL_UPDATE_ROWS_V2"),
    }
}
```

### 4.3 Value Converter Tests

**文件位置**: `src/binlog/value.rs`

**测试覆盖**:
- Integer conversions (int/uint/float/double)
- String encoding (UTF-8/Latin1/Binary)
- Decimal precision handling
- DateTime formats (TIMESTAMP/DATETIME/TIME)
- Null-bit map decoding

**样例测试**:
```rust
#[test]
fn decimal_value_precision_preserved() {
    let buf = [0u8; 13]; // Example buffer with decimal
    let decimal = decode_decimal(&buf, 3, 10); // scale=3, length=10
    
    assert_eq!(decimal.scale, 3);
    assert!(!decimal.is_null());
    // Verify exact value conversion
}
```

### 4.4 Table Map Parser Tests

**文件位置**: `src/binlog/table_map.rs`

**关键点**:
- Session ID extraction
- Column count parsing
- Metadata length calculations
- NULL bitmap bit-width alignment

### 4.5 JSON Field Handler Tests

**文件位置**: `src/binlog/json.rs`

**测试场景**:
- NULL JSON value
- Empty JSON object `{}`
- Nested structures
- Large JSON blobs (> 64KB)

### 4.6 Test Data Generation

**Fixture 生成器**: `scripts/generate_fuzz_seeds.rs`

```rust
// Generate deterministic test vectors for edge cases
pub fn generate_test_vector(event_type: u8, body_size: usize) -> Vec<u8> {
    let mut buf = vec![0u8; HEADER_SIZE + body_size];
    
    // Fill header
    buf[0..4].copy_from_slice(&4u32.to_le_bytes()); // server version
    buf[4] = event_type;
    buf[9..13].copy_from_slice(&(HEADER_SIZE as u32 + body_size as u32).to_le_bytes());
    
    // Fill body with known pattern
    buf[HEADER_SIZE..].fill(0x42); // ASCII 'B'
    
    buf
}
```

---

## 5. 集成测试详解

### 5.1 测试基础设施

#### 5.1.1 Test Fixture Directory Structure

```
tests/
├── fixtures/
│   ├── capture_8.0_minimal/
│   │   ├── binlog.000003       # MINIMAL row image session
│   │   └── schema.json         # Matching CREATE statements
│   │
│   ├── capture_8.0_rows/
│   │   ├── binlog.000002       # FULL image session
│   │   ├── binlog.000003       # Mixed MINIMAL/FULL
│   │   ├── binlog.000004       # PARTIAL UPDATE + JSON
│   │   └── schema.json
│   │
│   └── README.txt              # Fixture metadata
│
├── e2e.rs                      # End-to-end integration tests
├── cli.rs                      # CLI argument validation
├── repl.rs                     # Replication protocol tests
└── stats.rs                    # Statistics module tests
```

### 5.2 E2E Test Suites

#### Suite 1: Full Image Replay (FLASHBACK)

**测试文件**: `tests/e2e.rs::real_capture_flashback_full_image_and_forward_reconcile`

**测试步骤**:
1. 使用 binlog.000002 (FULL image) 执行 flashback
2. 生成逆序 SQL
3. 同时运行正序 to-sql
4. 对比两者镜像关系

**断言逻辑**:
```rust
assert_eq!(
    (sum.events, sum.statements, sum.errors, sum.files),
    (2, 2, 0, 1)
);

let back = read_backwards_sql(&final_path);
let forward = read_forwards_sql(&tosql_output);

// Verify each statement is the inverse
let expected_reverse = forward.iter().rev().map(|stmt| mirror(stmt)).collect::<Vec<_>>();
assert_eq!(back, expected_reverse);
```

**覆盖场景**:
- ✓ Single file mode without explicit stop
- ✓ Parallel execution (threads=2)
- ✓ Transaction boundary preservation
- ✓ DDL exclusion from output
- ✓ File cleanup (.tmp artifacts removed)

#### Suite 2: Minimal Image Hard Error Handling

**测试文件**: `tests/e2e.rs::real_capture_flashback_minimal_image_hard_errors`

**关键机制**:
- binlog.000003 contains UPDATE with MINIMAL image
- AFTER-image only has `f=123` (others Missing)
- Flashback requires FULL for WHERE clause reconstruction
- Expected: Fatal error thrown in stop mode

**断言**:
```rust
let err = run_flashback(...)
    .expect_err("MINIMAL image must trigger hard error");

assert!(err.to_string().contains("binlog_row_image=FULL"));
assert!(err.to_string().contains("Missing"));
```

#### Suite 3: Source-Level Error Propagation

**测试文件**: `tests/e2e.rs::real_capture_partial_event_is_source_level_error`

**重点验证**:
- Event type 39 (PARTIAL_UPDATE_ROWS_V2) detected at source layer
- Should **not** be skipped by `--on-error skip-bad-event`
- Must fail even in skip mode (hard error vs soft error)

**多策略测试**:
```rust
for strategy in [
    "--on-error stop",
    "--on-error skip-bad-event",
] {
    let result = run_flashback(args.with(strategy));
    assert!(result.is_err(), "{} should fail on partial rows", strategy);
}
```

### 5.3 Stats Module Integration Tests

**文件位置**: `tests/stats.rs`

**测试内容**:

#### Test: JSONL Output Consistency
```rust
#[test]
fn stats_e2e_jsonl_pair() {
    // Run stats on fixture with JSON output
    let sum = run_stats(&args.with("--stats-json"));
    
    // Verify JSONL file content
    let jsonl_path = out_dir.join("binlog_status.jsonl");
    let lines = std::fs::read_to_string(jsonl_path).unwrap();
    let records: Vec<JsonRecord> = lines.lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    
    // Validate counts match summary
    assert_eq!(records.len(), sum.events as usize);
}
```

#### Test: Window Flush Behavior
```rust
#[test]
fn stats_misc_query_ticks_window_like_upstream() {
    // Simulate long-running query stream
    // Verify window flushes occur at configured intervals
    // Check no memory unbounded growth
}
```

### 5.4 CLI Argument Validation Tests

**文件位置**: `tests/cli.rs`

**测试矩阵**:

| Test Name | Scenario | Expected Result |
|-----------|----------|-----------------|
| `help_lists_subcommands` | `--help` flag | Shows 4 subcommands |
| `to_sql_requires_start_file` | Missing start-file | Error exits |
| `repl_valid_args_dispatch` | Valid repl args | Launches repl flow |
| `flashback_help_shows_keep_trx` | `--help` for flashback | Documents keep-trx option |

---

## 6. 端到端 (E2E) 测试矩阵

### 6.1 测试拓扑图

```mermaid
graph LR
    A[Test Controller] --> B[MySQL Container]
    B --> C[Binlog Writer]
    C --> D[Test Tables]
    D --> E[Binlog Stream]
    E --> F[my2sql-rs]
    F --> G[Output Verification]
    F --> H[Error Analysis]
```

### 6.2 完整测试场景列表

#### Category A: To-SQL Forward Conversion

| ID | 场景名 | 描述 | 期望结果 |
|----|--------|------|---------|
| A1 | Basic INSERT | Simple single-insert binlog | Correctly converts to INSERT stmt |
| A2 | Bulk UPDATE | Multiple UPDATE events | All statements generated |
| A3 | DELETE Cascade | Foreign key cascade deletes | Handled gracefully |
| A4 | DDL Exclusion | CREATE TABLE + DML mixed | Only DML included |
| A5 | None Checksum | binlog_checksum=NONE format | Parses successfully |
| A6 | CRC32 Checksum | Standard CRC32 format | Verifies & validates |
| A7 | Multi-thread | threads=8 configuration | Throughput baseline maintained |
| A8 | Long Trx | Transactions > 60s | Detected & reported |

#### Category B: Flashback Rollback Generation

| ID | 场景名 | 描述 | 期望结果 |
|----|--------|------|---------|
| B1 | FULL Image Roundtrip | Complete INSERT→DELETE reversal | Perfect semantic inversion |
| B2 | UPDATE Statement Swap | SET/WHERE clause exchange | Logical equivalence preserved |
| B3 | Transaction Ordering | Reverse chronological sequence | Latest transactions rolled first |
| B4 | Keep-Trx Enabled | BEGIN/COMMIT wrapper active | Standalone SQL runnable |
| B5 | Keep-Trx Disabled | Bare statements only | Compact script preferred |
| B6 | File-per-table | Multiple tables in stream | Separate output files created |
| B7 | Skip Bad Events | Soft errors encountered | Continues despite failures |
| B8 | Hard Error Abort | Missing column detected | Immediate exit code 1 |

#### Category C: Stats Reporting Accuracy

| ID | 场景名 | 描述 | 期望结果 |
|----|--------|------|---------|
| C1 | Row Count Match | Sum(rows) vs actual inserts | Matches exactly |
| C2 | Error Deduplication | Same error counted once | Proper dedupe logic |
| C3 | BigLongTrx Report | Transactions exceeding threshold | Listed separately |
| C4 | Time Windows | Hourly aggregation | Correct binning |
| C5 | JSONL Format Validity | Structured output machine-parseable | Schema compliance |

#### Category D: Replication Protocol

| ID | 场景名 | 描述 | 期望结果 |
|----|--------|------|---------|
| D1 | GTID连续性 |连续的 GTID序列 | Smooth streaming |
| D2 | HeartbeatInterval |心跳包检测 | Latency monitoring |
| D3 | Resume Capability | Interruption mid-stream | Position restored |
| D4 | Server-ID Conflict | Duplicate replica ID | Detected & rejected |
| D5 | PurgedBinlog Loss | Binlog rotated before reading | Graceful degradation |

### 6.3 测试用例实现示例

**完整 E2E 测试函数**:

```rust
#[test]
fn e2e_pipeline_produces_expected_sql_bytes() {
    // Setup
    let out_dir = tmp_dir("e2e-output");
    let cfg = config_from(capture_args("to-sql", "binlog.000002", &out_dir, &[]));
    
    // Execute
    let stats = run_to_sql(&cfg).unwrap();
    
    // Verify exact byte-for-byte match with golden file
    let output_file = out_dir.join("to_sql.2.sql");
    let actual = std::fs::read(&output_file).unwrap();
    let expected = include_bytes!("golden/to_sql.000002.expected");
    
    assert_eq!(actual.as_slice(), *expected);
}
```

---

## 7. 边界情况与异常处理测试

### 7.1 文件级错误场景

| 场景 | 描述 | 预期行为 |
|------|------|---------|
| EOF Before Event | File truncated mid-event | Error logged, recovery attempted |
| Invalid Header Size | Event size > declared capacity | Reject immediately |
| Missing Table Map | Rows event without prior TM | Skip event with warning |
| Corrupted Schema | JSON parse failure | Fallback to default handling |

### 7.2 Event Type Edge Cases

```rust
#[test]
fn heartbeat_event_19_bytes_valid() {
    // HEARTBEAT also zero-body event
    let mut ev = vec![0u8; EVENT_HEADER_SIZE];
    ev[4] = EventType::HEARTBEAT;
    ev[9..13].copy_from_slice(&19u32.to_le_bytes());
    assert!(parse_header(&ev).is_ok());
}

#[test]
fn random_event_type_rejected() {
    let mut ev = vec![0u8; EVENT_HEADER_SIZE];
    ev[4] = 999u8; // Unknown event type
    
    assert!(parse_header(&ev).is_err());
}
```

### 7.3 Memory Safety Checks

```rust
#[test]
fn oversized_event_rejected() {
    // DoS protection: event too large
    let mut buf = vec![0u8; HEADER_SIZE + 4*1024*1024 + 1];
    buf[9..13].copy_from_slice(&(MAX_EVENT_SIZE + 1).to_le_bytes());
    
    assert!(parse_header(&buf).is_err());
}
```

### 7.4 Concurrency Race Conditions

**测试多线程安全**:

```rust
#[test]
fn e2e_single_thread_matches_parallel_byte_for_byte() {
    let out_t1 = tmp_dir("parallel-vs-single-t1");
    let out_t8 = tmp_dir("parallel-vs-single-t8");
    
    // Single thread baseline
    run_to_sql(config_from(args.with("--threads 1"), &out_t1)).unwrap();
    
    // Parallel execution
    run_to_sql(config_from(args.with("--threads 8"), &out_t8)).unwrap();
    
    // Compare outputs byte-for-byte
    assert_eq!(
        read_all_files(&out_t1),
        read_all_files(&out_t8)
    );
}
```

---

## 8. 性能测试基准

### 8.1 基准测试配置

**硬件假设**:
- CPU: 8 cores (AMD EPYC / Intel Xeon)
- RAM: 16GB
- Storage: NVMe SSD (sequential read > 3000 MB/s)

**测试数据集**:
- 1GB binlog stream (~1M events)
- Mix of INSERT/UPDATE/DELETE (40%/40%/20%)
- Average row size: 200 bytes

### 8.2 吞吐量基线

```bash
# Warm-up runs
cargo bench --bench decode -- --quiet

# Record metrics
Thread 通过量 Events/sec
----- -------- -----------
1     ~50K     50,000
2     ~90K     90,000
4     ~170K    170,000
8     ~320K    320,000
```

### 8.3 内存使用特征

| Mode | Max RSS (resident set size) | Notes |
|------|----------------------------|-------|
| Single thread | ~128MB | Linear scan profile |
| 4 threads | ~350MB | Buffer pools per worker |
| 8 threads | ~600MB | Reorder queue overhead |

### 8.4 Regression Testing

**性能回归检测**:

```rust
#[test]
fn benchmark_decode_performance_regression_guard() {
    let input = load_fixture("large.binlog");
    
    // Measure decode throughput
    let start = Instant::now();
    let decoded = decode_all(input);
    let duration = start.elapsed();
    
    // Enforce lower bound (must exceed baseline by >5%)
    let throughput = input.len() as f64 / duration.as_secs_f64();
    assert!(throughput > DECODE_BASELINE_MBPS, 
            "Performance regression detected: {:.2} MB/s < {}",
            throughput, DECODE_BASELINE_MBPS);
}
```

---

## 9. 自动化测试运行指南

### 9.1 Makefile Commands

创建 `Makefile`:

```makefile
.PHONY: all test unit int e2e clean format lint docs

# Default target
all: format lint test

# Run full test suite
test: unit int e2e

# Unit tests (fast, no dependencies)
unit:
	cargo test --lib --quiet

# Integration tests (fixture-based)
int:
	cargo test --test cli --quiet
	cargo test --test stats --quiet

# End-to-end (requires MySQL container)
e2e: docker-test
	cargo test --test e2e --quiet

# Start test infrastructure
docker-test:
	docker-compose -f docker-compose.test.yml up -d
	@sleep 10
	@echo "✓ Test database ready"

# Run everything including benchmarks
perf: cargo bench

# Format codebase
format:
	cargo fmt

# Lint checking
lint:
	cargo clippy --all-targets -- -D warnings

# Clean all build artifacts
clean:
	cargo clean
	rm -rf target/debug/test-* target/release/test-*
	docker-compose -f docker-compose.test.yml down || true

docs:
	@echo "Generate documentation..."
	cargo doc --no-deps
```

### 9.2 CI Pipeline Integration

**.github/workflows/test.yml**:

```yaml
name: Test Suite

on:
  push:
    branches: [ main ]
  pull_request:
    branches: [ main ]

jobs:
  unit-tests:
    runs-on: ubuntu-latest
    steps:
    - uses: actions/checkout@v4
    - uses: dtolnay/rust-toolchain@stable
    - name: Run unit tests
      run: cargo test --lib --quiet
  
  integration-tests:
    runs-on: ubuntu-latest
    services:
      mysql:
        image: mysql:8.0.46
        env:
          MYSQL_ROOT_PASSWORD: root
        options: >-
          --health-cmd="mysqladmin ping"
          --health-interval=10s
          --timeout=5s
          --ports=3306:3306
    steps:
    - uses: actions/checkout@v4
    - uses: dtolnay/rust-toolchain@stable
    - name: Run integration tests
      run: cargo test --test cli --quiet
  
  e2e-tests:
    runs-on: ubuntu-latest
    services:
      mysql:
        image: mysql:8.0.46
        env:
          MYSQL_ROOT_PASSWORD: root
        ports:
          - 3306:3306
    steps:
    - uses: actions/checkout@v4
    - uses: dtolnay/rust-toolchain@stable
    - name: Run E2E tests
      run: cargo test --test e2e --quiet
      
  performance:
    runs-on: ubuntu-latest
    steps:
    - uses: actions/checkout@v4
    - uses: dtolnay/rust-toolchain@nightly
    - name: Run benchmarks
      run: cargo bench -- --save-baseline=baseline --verbose
```

### 9.3 Local Batch Testing Script

创建 `scripts/run-all-tests.sh`:

```bash
#!/bin/bash
set -e

echo "=== Running Complete Test Suite ==="
echo ""

# 1. Unit tests
echo "1. Running unit tests..."
cargo test --lib --quiet

# 2. Integration tests
echo "2. Running integration tests..."
cargo test --test cli --quiet
cargo test --test stats --quiet

# 3. Start test database
echo "3. Starting test database..."
docker-compose -f docker-compose.test.yml up -d
sleep 15

# 4. E2E tests
echo "4. Running end-to-end tests..."
cargo test --test e2e --quiet

# 5. Stop database
echo "5. Stopping test database..."
docker-compose -f docker-compose.test.yml down

echo ""
echo "=== All Tests Passed ✅ ==="
```

使用方式：
```bash
chmod +x scripts/run-all-tests.sh
./scripts/run-all-tests.sh
```

---

## 10. 问题排查与故障排除

### 10.1 常见错误与解决方案

#### Error: "Aborted: first error logged to stderr"

**原因**: Worker 线程遇到硬错误触发 abort flag

**解决步骤**:
1. 查看实际错误日志（设置 `RUST_LOG=error`）
2. 检查 binlog checksum 配置是否匹配
3. 确认 schema.json 与实际表结构一致

#### Error: "PartialNotSupported in skip-bad-event mode"

**原因**: Event type 39 (PARTIAL_UPDATE_ROWS_V2) 是源级硬错误

**解决方法**:
- 必须提升 binlog_row_image 为 FULL
- 或在数据捕获阶段过滤掉 V2 事件

#### Error: "Start file not found"

**原因**: 指定了不存在的 binlog 文件名

**排查**:
```bash
ls -la /path/to/binlogs/ | grep binlog
# 确认文件名精确匹配（不含前缀路径）
```

### 10.2 Debug Mode 启用

```bash
# 详细日志输出
export RUST_LOG=debug,cargo_my2sql=trace
cargo run --release -- flashback --verbose ...

# 带 backtrace 的错误堆栈
export RUST_BACKTRACE=full
cargo test --test e2e failing_test_name -- --nocapture
```

### 10.3 测试隔离最佳实践

- ✅ 每个测试使用独立的临时目录 (`tmp_dir()` helper)
- ✅ 测试结束后自动清理 (`std::fs::remove_dir_all`)
- ❌ 避免共享全局状态（除非用 `Mutex::new()` 包裹）
- ❌ 不要在测试中修改配置文件

---

## Appendix A: Fixtures Schema Reference

### A.1 Table Definitions (capture_8.0_rows)

```sql
-- binlog.000002: FULL image session
CREATE TABLE u (
    a INT NOT NULL,
    b VARCHAR(8) NOT NULL,
    c INT NULL,
    d INT NULL,
    e INT NULL,
    f INT NULL,
    g INT NULL,
    h INT NULL,
    i2 INT NULL
);

-- binlog.000004: JSON column support
CREATE TABLE j (
    id INT PRIMARY KEY,
    doc JSON NOT NULL,
    tag VARCHAR(4) NULL
);
```

### A.2 Schema JSON Format

`tests/fixtures/capture_8.0_rows/schema.json`:
```json
{
  "version": "1",
  "tables": [
    {
      "name": "u",
      "database": "t10",
      "columns": [
        {"name": "a", "type": "INT", "nullable": false},
        {"name": "b", "type": "VARCHAR(8)", "nullable": false},
        {"name": "c", "type": "INT", "nullable": true}
        // ... more columns
      ]
    },
    {
      "name": "j",
      "database": "t10",
      "columns": [
        {"name": "id", "type": "INT", "nullable": false, "primary_key": true},
        {"name": "doc", "type": "JSON", "nullable": false},
        {"name": "tag", "type": "VARCHAR(4)", "nullable": true}
      ]
    }
  ]
}
```

---

## Appendix B: Sample Binlogs Explanation

### B.1 Fixture Metadata

| 文件名 | 大小 | 内容描述 | 重要事件 |
|--------|------|---------|---------|
| binlog.000002 | 1346 B | FULL image session | CREATE TABLE + 1 INSERT + 1 UPDATE |
| binlog.000003 | 977 B | MINIMAL image session | UPDATE with missing columns |
| binlog.000004 | 1933 B | JSON + PARTIAL update | WRITE_ROWS_V2 + PARTIAL_UPDATE_ROWS_V2(39) |

### B.2 Event Sequence Analysis

**binlog.000002 breakdown**:
```
Offset  Event Type        Description
------  ----------        -----------
4       FORMAT_DESCRIPTION  Server version: 8.0.46
234     QUERY             CREATE DATABASE t10
418     QUERY             CREATE TABLE u (...)
684     WRITE_ROWS        INSERT INTO u VALUES (1,'ab',10,...)
754     UPDATE_ROWS       UPDATE u SET c=NULL WHERE ...
1120    XID               COMMIT
1152    STOP              End of file marker
```

---

## Appendix C: Test Data Generator Scripts

### C.1 Generate Random Binlogs

路径：`scripts/generate_random_binlogs.py`

```python
#!/usr/bin/env python3
"""Generate synthetic binlog fixtures for fuzz testing."""

import struct
import random
from datetime import datetime

HEADER_SIZE = 19  # Minimum event header size

def format_description_event(binlog_version=(4,)):
    """Generate minimal FORMAT_DESCRIPTION_EVENT."""
    ts = struct.pack('<I', int(datetime.now().timestamp()))
    server_id = struct.pack('<I', 1)
    event_size = struct.pack('<I', HEADER_SIZE)
    
    return ts + b'\x02' + server_id + event_size + binlog_version

def write_rows_event(table_id=1, row_data=b''):
    """Generate minimal WRITE_ROWS_EVENT."""
    ts = struct.pack('<I', int(datetime.now().timestamp()))
    event_type = struct.pack('B', 30)  # WRITE_ROWS type
    flags = struct.pack('<H', 0)
    server_id = struct.pack('<I', 1)
    table_id = struct.pack('<Q', table_id)
    num_columns = struct.pack('<H', 1)
    
    return ts + event_type + flags + server_id + table_id + num_columns + row_data

if __name__ == "__main__":
    import sys
    output = sys.argv[1] if len(sys.argv) > 1 else "/tmp/fixed.binlog"
    
    with open(output, 'wb') as f:
        f.write(format_description_event())
        f.write(write_rows_event(123, b'sample_data'))
    
    print(f"Generated {output}")
```

使用方式：
```bash
chmod +x scripts/generate_random_binlogs.py
python3 scripts/generate_random_binlogs.py tests/fixtures/test_case.binlog
```

---

## 📝 文档维护说明

**最后更新**: 2026-09-24  
**版本号**: 0.5.2-p7  
**维护者**: Project Maintainers

如需更新此文档，请遵循以下原则：

1. **测试新增**: 每次新增测试场景，同步更新对应章节
2. **API 变更**: 命令行参数变化需及时反映在文档示例
3. **Bug 修复**: 如果修复影响测试行为，添加 CHANGELOG 引用
4. **性能优化**: 吞吐基线更新需重新测量并记录

---

**本文档结束**

🎉 恭喜！您已阅读完整的 my2sql-rs 测试手册。现在可以开始您的测试之旅！

