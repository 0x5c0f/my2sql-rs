# my2sql-rs P6「数据恢复面」实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 flashback 模式在生产场景中更可解释、更可控——增加报告文件、dry-run 预览、on-error 显式化以及 e2e 测试验证

**Architecture:** 在现有 pipeline 架构基础上增加三个增量：1) DDL 跳过事件记录到 JSONL report-file；2) --dry-run 模式统计 recovery_rate% 不生成 SQL；3) 暴露 --on-error 开关给 CLI；4) Drop-Recovery E2E 测试校验恢复正确性

**Tech Stack:** Rust, serde/serde_derive(JSON), clap(CLI), crossbeam-channel(管道), mysql_common(解码)

**Spec:** `docs/superpowers/specs/2026-09-22-my2sql-rs-p6-data-recovery-design.md`

## Global Constraints

- **TDD first**: 每个任务先写 failing test → implement → green → commit
- **Zero new dependencies**: 仅复用既有 crate（serde/derive for JSON already present）
- **Backward compatibility**: 所有新增 flags 有合理 default（stop/skipped-to-stderr），不改现有 CLI 契约
- **CI gate discipline**: fmt/clippy/test/musl 编译门，drop-recovery 测试不进 CI
- **Documentation update**: HANDOVER §P6 节点日志 + CHANGELOG v0.6.0 节 + README 差异清单续号
- **SDD ledger**: `.superpowers/sdd/2026-09-22-my2sql-rs-p6-data-recovery/progress.md` 逐任务闭环

---

### Task 1: Report-file DDL Skip 记录 (对应 T1)

**Files:**
- Modify: `src/config.rs:CLIArgs` - 添加 `--report-file <path>` flag
- Create: `src/flashback/report.rs` (新模块) - JSONL 报告写入器
- Modify: `src/pipeline/mod.rs` - flashback 分支集成 report writer
- Test: `tests/flashback_report.rs` - DDL 跳过报告单测

**Interfaces:**
- Consumes: `pipeline::FlashbackEvent` (含 binlog position + event type + sql)
- Produces: JSONL file per DDL skip event

#### Step 1: Write the failing test

```rust
// tests/flashback_report.rs
#[test]
fn test_ddl_skip_report_format() {
    let events = vec![
        FlashbackEvent::DdlSkip {
            binlog: "mysql-bin.000150".to_string(),
            position: 12345,
            timestamp: "2026-09-22T14:30:15Z".to_string(),
            event_type: "Query".to_string(),
            sql: "ALTER TABLE t_users ADD COLUMN new_field VARCHAR(100)".to_string(),
        },
    ];

    let mut writer = JsonlReporter::new("test_output.jsonl").unwrap();
    for event in events {
        writer.write(&event).unwrap();
    }
    writer.close().unwrap();

    let content = fs::read_to_string("test_output.jsonl").unwrap();
    // Assert JSONL format
    assert!(content.contains("\"binlog\": \"mysql-bin.000150\""));
    assert!(content.contains("\"position\": 12345"));
}
```

- [ ] **Step 1: Write the failing test**

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test flashback_report test_ddl_skip_report_format -v`
Expected: FAIL with "module report not found" or compile error

- [ ] **Step 3: Write minimal implementation**

Create `src/flashback/report.rs`:
```rust
use serde::Serialize;
use std::fs::File;
use std::io::{BufWriter, Write};

#[derive(Serialize)]
pub struct SkipEvent {
    pub timestamp: String,
    pub binlog: String,
    pub position: u64,
    pub type_: String,
    pub sql: Option<String>,
}

pub struct JsonlReporter {
    writer: BufWriter<File>,
}

impl JsonlReporter {
    pub fn new(path: &str) -> Result<Self, std::io::Error> {
        let file = File::create(path)?;
        Ok(Self {
            writer: BufWriter::new(file),
        })
    }

    pub fn write(&mut self, event: &SkipEvent) -> Result<(), serde_json::Error> {
        writeln!(self.writer, "{}", serde_json::to_string(event)?)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
            .map_err(|e| anyhow::anyhow!(e))?;
        Ok(())
    }

    pub fn close(&mut self) -> Result<(), std::io::Error> {
        self.writer.flush()
    }
}
```

Update `src/pipeline/mod.rs`:
```rust
// Add import
mod flashback {
    pub mod report;
}

// In flashback mode dispatcher
if let Some(report_path) = args.report_file.as_ref() {
    let mut reporter = flashback::report::JsonlReporter::new(report_path)?;
    // Wrap event processing to call reporter.write() on DDL skip
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test flashback_report test_ddl_skip_report_format -v`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/flashback/report.rs src/pipeline/mod.rs tests/flashback_report.rs
git commit -m "feat(p6-T1): add report-file JSONL output for DDL skip events"
```

#### Step 2: Add CLI flag and integrate into pipeline

- [ ] **Step 1: Write the failing test**

```rust
// tests/cli_args_test.rs
#[test]
fn test_cli_report_file_flag() {
    let args = Cli::try_parse_from([
        "my2sql",
        "flashback",
        "--dir=/tmp/binlogs",
        "--output-dir=/tmp/out",
        "--report-file=/tmp/report.jsonl",
    ]).unwrap();

    assert_eq!(args.report_file, Some("/tmp/report.jsonl".into()));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test cli_args_test test_cli_report_file_flag -v`
Expected: FAIL with "no field named `report_file` on struct `CliArgs`"

- [ ] **Step 3: Write minimal implementation**

Modify `src/config.rs`:
```rust
#[derive(Parser)]
pub enum SubCommand {
    Flashback(FlashbackArgs),
}

#[derive(Parser)]
pub struct FlashbackArgs {
    #[arg(long)]
    pub report_file: Option<String>,
    
    // ... existing fields
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test cli_args_test test_cli_report_file_flag -v`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat(p6-T1): expose --report-file flag in CLI"
```

#### Step 3: Test with real binlog sample

- [ ] **Step 1: Write integration test**

```rust
// tests/flashback_e2e_report.rs
#[test]
fn test_flashback_with_ddl_generates_report() {
    // Use existing 8.0 matrix binlog with DDL events
    let tmp_dir = TempDir::new().unwrap();
    let report_path = tmp_dir.path().join("report.jsonl");

    // Run flashback on known binlog
    let output = Command::new("cargo")
        .args([
            "run", "--",
            "flashback",
            "--dir=tests/fixtures/binlogs/8.0/",
            "--output-dir", tmp_dir.path().to_str().unwrap(),
            "--report-file", report_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(report_path.exists());

    let content = fs::read_to_string(&report_path).unwrap();
    assert!(content.len() > 0); // At least some DDL skips recorded
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test flashback_e2e_report test_flashback_with_ddl_generates_report -v`
Expected: FAIL because integration not yet wired up

- [ ] **Step 3: Wire reporter into pipeline**

Modify `src/pipeline/mod.rs`:
```rust
// Find flashback branch where DDL skips happen
fn process_event(&mut self, event: FlashbackEvent) -> Result<()> {
    match event {
        FlashbackEvent::DdlSkip { .. } => {
            if let Some(ref mut reporter) = self.ddl_reporter {
                reporter.write(&skip_event)?;
            }
        }
        _ => {}
    }
    // ... existing logic
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test flashback_e2e_report test_flashback_with_ddl_generates_report -v`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/pipeline/mod.rs tests/flashback_e2e_report.rs
git commit -m "feat(p6-T1): wire report writer into flashback pipeline"
```

#### Step 4: Fix C9 debug output (concurrent cleanup)

- [ ] **Step 1: Audit for debug output**

Run: `grep -r "println!" src/ | grep -v "cfg(test)"`
Expected: Found 2 places in `src/metadata/store.rs`

- [ ] **Step 2: Replace println! with tracing::debug!**

Modify `src/metadata/store.rs`:
```rust
// Before:
println!("Schema dump at position {}: {:?}", pos, schema);

// After:
tracing::debug!("Schema dump at position {}: {:?}", pos, schema);
```

- [ ] **Step 3: Ensure tracing is configured**

Verify `src/lib.rs` has:
```rust
tracing_subscriber::fmt::init();
```

- [ ] **Step 4: Run tests**

Run: `cargo test`
Expected: All tests pass, no println! in non-test code

- [ ] **Step 5: Commit**

```bash
git add src/metadata/store.rs
git commit -m "fix(p6-T1-cleanup): replace println! with tracing::debug (C9 resolution)"
```

#### Step 6: Update documentation

- [ ] **Step 1: Update CHANGELOG**

Append to `CHANGELOG.md`:
```markdown
## v0.6.0 (unreleased)

### Added
- `--report-file <path>` flashback option to record DDL skip events as JSONL
- Dry-run preview mode (`--dry-run`) with recovery rate statistics
- Explicit `--on-error {stop, skip}` CLI flag (default: stop)
- Drop-recovery E2E test for verification

### Changed
- Flashback now outputs skipped events location/type/SQL to report file
- On-error strategy user-exposable instead of internal-only
```

- [ ] **Step 2: Update HANDOVER**

Append to `docs/HANDOVER.md`:
```markdown
- **P6「数据恢复面」**: 当前处于 Planning 阶段（spec 完成于 2026-09-22，实施中...）
  - T1 Report-file: 进行中（DDL 白名单化报告，JSONL 格式 D1）
  - T2 Dry-run: pending
  - T3 On-error: pending  
  - T4 E2E Test: pending
```

- [ ] **Step 3: Commit**

```bash
git add CHANGELOG.md docs/HANDOVER.md
git commit -m "docs(p6-T1): update changelog + handover for report-file delivery"
```

---

### Task 2: Dry-run Summary Preview (对应 T2)

**Files:**
- Modify: `src/config.rs` - 添加 `--dry-run` flag
- Modify: `src/pipeline/mod.rs` - Dry-run 路径（只计数不写 SQL）
- Create: `src/stats/summary.rs` (新模块) - Summary JSON 序列化器
- Test: `tests/dryrun_summary.rs` - 干跑统计验证

**Interfaces:**
- Consumes: `pipeline::BinlogIterator` (一次遍历收集计数)
- Produces: JSON summary (total_transactions, recovery_rate%, warnings)

#### Step 1: Write the failing test

```rust
// tests/dryrun_summary.rs
#[test]
fn test_dryrun_output_format() {
    let summary = Summary {
        binlog_range: BinlogRange { /* ... */ },
        stats: Stats {
            total_transactions: 1234,
            recoverable_transactions: 1198,
            skipped_events: 36,
            estimated_rows_affected: 45678,
            recovery_rate: "97.08%",
        },
        warnings: vec![Warning { count: 15, type_: "Query_event" }],
    };

    let json = serde_json::to_string_pretty(&summary).unwrap();
    
    // Assert structure
    assert!(json.contains("\"recovery_rate\": \"97.08%\""));
    assert!(json.contains("\"total_transactions\": 1234"));
}
```

- [ ] **Step 1: Write the failing test**

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test dryrun_summary test_dryrun_output_format -v`
Expected: FAIL with "cannot find struct `Summary`"

- [ ] **Step 3: Create summary module**

Create `src/stats/summary.rs`:
```rust
use serde::Serialize;

#[derive(Serialize)]
pub struct BinlogRange {
    pub start_file: String,
    pub start_pos: u64,
    pub end_file: String,
    pub end_pos: u64,
}

#[derive(Serialize)]
pub struct Stats {
    pub total_transactions: usize,
    pub recoverable_transactions: usize,
    pub skipped_events: usize,
    pub estimated_rows_affected: usize,
    pub recovery_rate: String,
}

#[derive(Serialize)]
pub struct Warning {
    pub count: usize,
    pub type_: String,
}

#[derive(Serialize)]
pub struct Summary {
    pub binlog_range: BinlogRange,
    pub summary: Stats, // Note: duplicate name intentional per spec
    pub warnings: Vec<Warning>,
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test dryrun_summary test_dryrun_output_format -v`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/stats/summary.rs tests/dryrun_summary.rs
git commit -m "feat(p6-T2): add summary JSON structures for dry-run output"
```

#### Step 2: Implement dry-run traversal logic

- [ ] **Step 1: Write integration test**

```rust
// tests/dryrun_e2e.rs
#[test]
fn test_dryrun_only_counts_not_generates_sql() {
    let tmp_dir = TempDir::new().unwrap();

    let output = Command::new("cargo")
        .args([
            "run", "--",
            "flashback",
            "--dir=tests/fixtures/binlogs/8.0/",
            "--dry-run",
            "--output-dir", tmp_dir.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();

    // Should succeed
    assert!(output.status.success());

    // But no SQL files generated
    let files: Vec<_> = fs::read_dir(tmp_dir.path())
        .unwrap()
        .filter_map(|e| e.ok().file_name().into_string().ok())
        .collect();
    
    assert!(!files.iter().any(|f| f.ends_with(".sql")));

    // stdout should contain JSON summary
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"recovery_rate\""));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test dryrun_e2e test_dryrun_only_counts_not_generates_sql -v`
Expected: FAIL with "--dry-run unknown flag"

- [ ] **Step 3: Add CLI flag**

Modify `src/config.rs`:
```rust
#[derive(Parser)]
pub struct FlashbackArgs {
    #[arg(long)]
    pub dry_run: bool,
    
    // ... existing fields
}
```

- [ ] **Step 4: Implement dry-run path in pipeline**

Modify `src/pipeline/mod.rs`:
```rust
fn run_flashback(args: &FlashbackArgs) -> Result<()> {
    let mut counter = TransactionCounter::new();
    
    // If dry-run, only iterate and count
    if args.dry_run {
        let events = binlog_iterator(args.dir)?;
        for event in events {
            counter.count(&event);
        }
        
        let summary = counter.build_summary();
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return Ok(());
    }
    
    // Normal path: generate SQL files
    generate_sql_files(args)?;
    Ok(())
}
```

- [ ] **Step 5: Implement TransactionCounter**

Create `src/pipeline/counter.rs`:
```rust
pub struct TransactionCounter {
    total_trx: usize,
    recoverable_trx: usize,
    skipped_events: usize,
    rows_affected: usize,
    warnings: HashMap<String, usize>,
}

impl TransactionCounter {
    pub fn count(&mut self, event: &FlashbackEvent) {
        match event {
            FlashbackEvent::Dml(dml) => {
                self.total_trx += 1;
                self.recoverable_trx += 1;
                self.rows_affected += dml.row_count();
            }
            FlashbackEvent::DdlSkip(_) => {
                self.skipped_events += 1;
                *self.warnings.entry("ddl_skip".to_string()).or_insert(0) += 1;
            }
        }
    }

    pub fn build_summary(self) -> Summary {
        let recovery_rate = if self.total_trx > 0 {
            format!("{:.2}%", 
                self.recoverable_trx as f64 / self.total_trx as f64 * 100.0)
        } else {
            "0.00%".to_string()
        };

        Summary {
            binlog_range: /* extract from args */,
            summary: Stats {
                total_transactions: self.total_trx,
                recoverable_transactions: self.recoverable_trx,
                skipped_events: self.skipped_events,
                estimated_rows_affected: self.rows_affected,
                recovery_rate,
            },
            warnings: /* convert map to vec */
        }
    }
}
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test --test dryrun_e2e test_dryrun_only_counts_not_generates_sql -v`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add src/config.rs src/pipeline/*.rs tests/dryrun_e2e.rs
git commit -m "feat(p6-T2): implement dry-run mode with recovery rate calculation"
```

#### Step 3: Validate JSON output format compliance

- [ ] **Step 1: Write golden test**

```rust
// tests/dryrun_golden.rs
#[test]
fn test_dryrun_matches_spec_format() {
    let tmp_dir = TempDir::new().unwrap();

    let _ = Command::new("cargo")
        .args([
            "run", "--", "flashback", "--dry-run",
            "--dir=tests/fixtures/binlogs/8.0/",
            "--output-dir", tmp_dir.path().to_str().unwrap(),
        ])
        .status()
        .unwrap();

    let stdout = /* capture from previous run */;
    let summary: Summary = serde_json::from_str(&stdout).unwrap();

    // Verify all required fields present
    assert!(summary.binlog_range.start_file.is_empty() == false);
    assert!(summary.summary.recovery_rate.ends_with('%'));
    assert!(!summary.warnings.is_empty() || summary.summary.skipped_events == 0);
}
```

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo test --test dryrun_golden test_dryrun_matches_spec_format -v`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add tests/dryrun_golden.rs
git commit -m "test(p6-T2): add golden format validation for dry-run output"
```

---

### Task 3: On-error Strategy Switch (对应 T3)

**Files:**
- Modify: `src/config.rs` - Make `--on-error` public flag
- Modify: `src/pipeline/mod.rs` - Use exposed strategy
- Test: `tests/on_error_strategy.rs` - Stop vs Skip behavior verification

**Interfaces:**
- Consumes: Existing `OnError` enum (Stop/SkipBadEvent)
- Produces: CLI argument parsing + behavioral difference

#### Step 1: Expose on-error flag to CLI

- [ ] **Step 1: Write the failing test**

```rust
// tests/cli_on_error.rs
#[test]
fn test_on_error_flag_parse_stop() {
    let args = Cli::try_parse_from([
        "my2sql", "flashback",
        "--on-error", "stop",
    ]).unwrap();

    assert_eq!(args.on_error, OnError::Stop);
}

#[test]
fn test_on_error_flag_parse_skip() {
    let args = Cli::try_parse_from([
        "my2sql", "flashback",
        "--on-error", "skip-bad-event",
    ]).unwrap();

    assert_eq!(args.on_error, OnError::SkipBadEvent);
}

#[test]
fn test_on_error_default_is_stop() {
    let args = Cli::try_parse_from([
        "my2sql", "flashback",
    ]).unwrap();

    assert_eq!(args.on_error, OnError::Stop); // Default
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test cli_on_error -v`
Expected: FAIL with "no field named `on_error` on `FlashbackArgs`"

- [ ] **Step 3: Modify config**

Modify `src/config.rs`:
```rust
use crate::error::OnError; // Import existing enum

#[derive(Parser)]
pub struct FlashbackArgs {
    #[arg(long, value_enum, default_value = "stop")]
    pub on_error: OnError,

    // ... existing fields
}

#[derive(clap::ValueEnum, Clone)]
pub enum OnError {
    Stop,
    SkipBadEvent,
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test cli_on_error -v`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/config.rs tests/cli_on_error.rs
git commit -m "feat(p6-T3): expose --on-error {stop,skip-bad-event} CLI flag (default: stop)"
```

#### Step 2: Verify behavioral difference

- [ ] **Step 1: Write integration test**

```rust
// tests/on_error_behavior.rs

/// Creates binlog with known bad event (column_count_mismatch)
fn create_binlog_with_bad_event(tmp_dir: &TempDir) -> PathBuf {
    // Use existing test fixture that triggers column mismatch
    tmp_dir.path().join("bad_event.binlog")
}

#[test]
fn test_on_error_stop_aborts() {
    let tmp_dir = TempDir::new().unwrap();
    let binlog_path = create_binlog_with_bad_event(&tmp_dir);

    let output = Command::new("cargo")
        .args([
            "run", "--", "flashback",
            "--dir", binlog_path.parent().unwrap().to_str().unwrap(),
            "--on-error", "stop",
        ])
        .output()
        .unwrap();

    // Should exit non-zero
    assert!(!output.status.success());

    // stderr should contain error message
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("bad event") || stderr.contains("error"));
}

#[test]
fn test_on_error_skip_continues_with_warning() {
    let tmp_dir = TempDir::new().unwrap();

    let output = Command::new("cargo")
        .args([
            "run", "--", "flashback",
            "--dir", "tests/fixtures/binlogs/8.0/",
            "--on-error", "skip-bad-event",
            "--output-dir", tmp_dir.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();

    // Should succeed despite bad events
    assert!(output.status.success());

    // Should generate partial SQL
    let files: Vec<_> = fs::read_dir(tmp_dir.path())
        .unwrap()
        .filter_map(|e| e.ok().file_name().into_string().ok())
        .collect();
    
    assert!(!files.is_empty()); // At least some SQL generated

    // Header should warn about skipped events
    let first_sql = fs::read_to_string(files[0]).unwrap();
    assert!(first_sql.contains("-- WARNING") || first_sql.contains("skipped"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test on_error_behavior -v`
Expected: May fail if on-error path not wired properly

- [ ] **Step 3: Wire on-error into pipeline**

Verify `src/pipeline/mod.rs` respects `args.on_error`:
```rust
match args.on_error {
    OnError::Stop => {
        // Strict path: panic on bad event
        process_events_stict(events)?;
    }
    OnError::SkipBadEvent => {
        // Lenient path: log warning, continue
        process_events_lenient(events)?;
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test on_error_behavior -v`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/pipeline/mod.rs tests/on_error_behavior.rs
git commit -m "feat(p6-T3): wire on-error strategy into flashback execution"
```

#### Step 3: Update help text and documentation

- [ ] **Step 1: Verify help text clarity**

Run: `cargo run -- flashback --help`
Expected: Contains `--on-error <ON_ERROR>` with choices (stop, skip-bad-event) and "(default: stop)"

- [ ] **Step 2: Update README**

Append to `README.md` usage section:
```markdown
## Flashback Mode

```bash
# Conservative (default): abort on any decode error
my2sql flashback --dir=./binlogs --on-error=stop

# Aggressive: skip bad events, generate partial recovery
my2sql flashback --dir=./binlogs --on-error=skip-bad-event
```

The `--on-error` flag controls behavior when encountering DDL+DML mixed events:
- `stop` (default): Immediate abort, zero risk of partial data loss
- `skip-bad-event`: Continue generating SQL for recoverable events, header warning injected
```

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs(p6-T3): document --on-error strategy in README"
```

---

### Task 4: Drop-Recovery E2E Test (对应 T4)

**Files:**
- Create: `tests/e2e_drop_recovery.rs` - End-to-end DROP→Flashback→Checksum 测试
- Create: `tests/fixtures/gen-drop-scenario.sh` - Docker 脚本生成 binlog + drop 场景
- Modify: `tools/comparator/checksum.rs` - Checksum 比对工具
- Test: Integration with docker-compose stress test containers

**Interfaces:**
- Consumes: Stress test container binlog logs (5.6/5.7/8.0)
- Produces: Checksum comparison result (pass if post-restore == pre-drop)

#### Step 1: Generate drop scenario test data

- [ ] **Step 1: Create scenario generation script**

Create `tests/fixtures/gen-drop-scenario.sh`:
```bash
#!/bin/bash
set -euo pipefail

# This script simulates DBA accident scenario:
# 1. Create DB + tables + initial data
# 2. Perform INSERT/UPDATE/DELETE for N hours
# 3. Record checksum A (pre-drop)
# 4. DROP DATABASE crash_db
# 5. Record checksum B (post-detect, empty db)
# Output: binlog snapshot + checksum metadata

CONTAINER_ID="my2sql-dt-8.0"
SCHEMA_DUMP="/tmp/schema.json"
BINLOG_SNAPSHOT="/tmp/binlog-snapshot"
CHECKSUM_A="/tmp/checksum-a.txt"

# Export schema dump
docker exec "$CONTAINER_ID" mysqldump --no-data test_db > "$SCHEMA_DUMP"

# Get current binlog position before "accident"
MYSQL_PWD="$PASSWORD_80"
mysql -h 127.0.0.1 -P 5308 -u root << EOF
FLUSH LOGS;
SELECT CURRENT_FILE, POSITION FROM mysql_BINLOG_INDEX;
EOF > "$BINLOG_SNAPSHOT"

# Calculate checksum of all tables
docker exec "$CONTAINER_ID" mysqlcheck --checksum test_db > "$CHECKSUM_A"

echo "Scenario prepared:"
echo "  Schema: $SCHEMA_DUMP"
echo "  Binlog: $BINLOG_SNAPSHOT"
echo "  Checksum A: $CHECKSUM_A"
```

- [ ] **Step 2: Make script executable**

```bash
chmod +x tests/fixtures/gen-drop-scenario.sh
```

- [ ] **Step 3: Run manually to verify**

```bash
./tests/fixtures/gen-drop-scenario.sh
```
Expected: Creates all artifacts, prints confirmation

- [ ] **Step 4: Commit**

```bash
git add tests/fixtures/gen-drop-scenario.sh
git commit -m "test(p6-T4): add drop scenario generation script"
```

#### Step 2: Implement e2e test workflow

- [ ] **Step 1: Write the failing test**

```rust
// tests/e2e_drop_recovery.rs
#[test]
#[ignore] // Requires docker + stress test containers
fn test_drop_recovery_checksum_match() {
    let tmp_dir = TempDir::new().unwrap();

    // 1. Simulate accident: get pre-drop checksum
    let checksum_a = read_checksum("/tmp/checksum-a.txt");

    // 2. Run flashback to generate recovery SQL
    let output = Command::new("cargo")
        .args([
            "run", "--", "flashback",
            "--schema-file", "/tmp/schema.json",
            "--dir", "/tmp/binlog-snapshot",
            "--output-dir", tmp_dir.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success());

    // 3. Re-execute generated SQL into clean DB
    let clean_db = spawn_clean_database().unwrap();
    execute_sql(&clean_db, tmp_dir.path()).unwrap();

    // 4. Calculate post-restore checksum
    let checksum_c = get_table_checksums(&clean_db);

    // 5. Assert byte-level match
    assert_eq!(checksum_a, checksum_c, "Post-restore checksums should match pre-drop values");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test e2e_drop_recovery test_drop_recovery_checksum_match -v`
Expected: FAIL (ignored by default, needs docker)

- [ ] **Step 3: Implement helper functions**

Create `tests/support/checksum.rs`:
```rust
pub fn read_checksum(path: &str) -> HashMap<String, String> {
    let content = fs::read_to_string(path).unwrap();
    content.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let parts: Vec<_> = line.split_whitespace().collect();
            (parts[1].to_string(), parts[2].to_string()) // table, checksum
        })
        .collect()
}

pub fn get_table_checksums(db_conn: &str) -> HashMap<String, String> {
    // Connect to DB, run CHECKSUM TABLE on all tables
    // Return map of table->MD5 checksum
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `env CONTAINER_ENABLED=1 cargo test --test e2e_drop_recovery test_drop_recovery_checksum_match -v`
Expected: PASS if stress test containers running

- [ ] **Step 5: Commit**

```bash
git add tests/e2e_drop_recovery.rs tests/support/checksum.rs
git commit -m "test(p6-T4): implement drop-recovery E2E workflow with checksum compare"
```

#### Step 3: Integrate with make compat

- [ ] **Step 1: Update Makefile**

Modify `Makefile`:
```makefile
compat-worktype:
	@if [ "$(WORK_TYPE)" = "" ]; then \
		echo "Usage: make compat-worktype=<type>"; \
		exit 1; \
	fi
	@case $(WORK_TYPE) in \
		rollback|stats) \
			go tool/run-difftest.sh $$WORK_TYPE; \
			;; \
		drop-recovery) \
			env CONTAINER_ENABLED=1 cargo test --test e2e_drop_recovery; \
			;; \
		*) \
			echo "Unknown WORK_TYPE: $$WORK_TYPE"; \
			exit 1; \
			;; \
	esac
```

- [ ] **Step 2: Document usage**

Append to `docs/AUDIT_REPORT.md` or create `docs/work-types.md`:
```markdown
## Work Types

- `make WORK_TYPE=rollback make difftest` — rollback against Go裁判
- `make WORK_TYPE=stats make difftest` — stats报表验证
- `make WORK_TYPE=drop-recovery make compat` — 完整恢复流程校验（需 docker）
```

- [ ] **Step 3: Commit**

```bash
git add Makefile docs/work-types.md
git commit -m "docs(p6-T4): add drop-recovery work type to compatibility matrix"
```

#### Step 4: Final验收 DoD 对账

- [ ] **Step 1: Run full六闸回归**

Run each in sequence:
```bash
make test          # Unit tests
make clippy        # Linting
make fmt           # Formatting
make fuzz-min      # Fuzzing (short)
make shadow-test   # Shadow lib对比
make difftest      # 差分测试
make compat-worktype=drop-recovery  # E2E
```

- [ ] **Step 2: Verify all gates pass**

Expected: All commands exit 0, no failures

- [ ] **Step 3: Verify spec §6 DoD 全部满足**

Checklist:
1. ✅ T1 Report-file交付 + unit test绿
2. ✅ T2 Dry-run交付 + summary格式验证绿
3. ✅ T3 On-error开关交付 + CLI帮助文本显示
4. ✅ T4 E2E交付 + `make compat-worktype=drop-recovery`全绿
5. ✅ Regression unchanged + `make difftest` exit 0
6. ⏳ CI pass (等待 PR merge 后 GitHub Actions 验证)
7. ✅ Docs updated + CHANGELOG/HANDOVER/README更新

- [ ] **Step 4: 撰写最终总结报告**

Append to `docs/superpowers/sdd/2026-09-22-my2sql-rs-p6-data-recovery/progress.md`:
```markdown
## P6 Completion Report

**Date**: 2026-09-22  
**Status**: Complete  

### Delivery Summary
- T1 Report-file: Delivered with JSONL format (D1)
- T2 Dry-run: Delivered with recovery_rate% summary (D2)
- T3 On-error: Delivered with stop default (D3)
- T4 E2E: Delivered with checksum compare (D4)

### Did Not Break
- `make difftest` (rollback mode): Still green
- Go裁判兼容性：Unchanged
- Existing API: Backward compatible

### Next Phase
Ready for PR merge to main branch. Post-P6 possibilities documented in spec §8.
```

- [ ] **Step 5: Final commit**

```bash
git add docs/superpowers/sdd/2026-09-22-my2sql-rs-p6-data-recovery/progress.md
git commit -m "feat(p6-complete): all four tasks delivered, DoD satisfied"
```

---

## Plan Execution Checklist

Before starting implementation:

- [ ] Read spec: `docs/superpowers/specs/2026-09-22-my2sql-rs-p6-data-recovery-design.md`
- [ ] Confirm Global Constraints understood (TDD, Zero deps, BC)
- [ ] Create worktree for isolation
- [ ] Initialize SDD ledger at `.superpowers/sdd/2026-09-22-my2sql-rs-p6-data-recovery/progress.md`
- [ ] Run pre-flight review: verify all tests currently pass

**Plan complete.** Two execution options:

**1. Subagent-Driven (recommended)** - I dispatch a fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** - Execute tasks in this session using executing-plans, batch execution with checkpoints

Which approach?
