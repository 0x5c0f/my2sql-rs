# my2sql-rs P6「数据恢复面」设计（Data Recovery Frontier）

> **日期**: 2026-09-22  
> **前置战役**: P1 (v0.1.0-p1) → P2 (v0.2.0-p2 flashback/stats) → P3 (v0.3.0-p3 repl) → P4a (v0.4.0-p4a) → P4b (v0.4.1-p4b) → P5 (v0.5.0 发布面)**全部收官并推送**  
> **权威上位 spec**: `docs/superpowers/specs/2026-09-20-my2sql-rust-design.md` §7「P6：数据恢复能力增强」——本计划消费其中"让 flashback 更可靠、更可解释"的增量需求  
> **交接权威**: `docs/HANDOVER.md`（状态行、挂账清单、环境事实）；`docs/AUDIT_REPORT.md`（P6 前最后审计结果，健康度🟢）

---

## 0. 现状勘察（本 spec 的事实基座，全部实测）

| # | 事实 | 证据 |
|---|------|------|
| F1 | Flashback 已交付但报告简陋：DDL/Query事件仅 stderr 输出一行汇总计数 | `src/pipeline/mod.rs:512` "flashback: {N} DDL/query events excluded" |
| F2 | 无 dry-run 模式：无法预览可恢复比例直接生成 SQL 文件 | `--dry-run` flag 不存在（spec §3.5 CLI 中未定义） |
| F3 | `--on-error` 已有 stop/skip 两态但仅限 P2 T5 内部使用，用户不可感知 | `tests/flashback.rs` + `HANDOVER`「P2 DoD 对账」T9 裁定 |
| F4 | 差分测试白名单无 DDL mid-binlog 场景覆盖 | `docs/difftest-allowlist.txt` 全为 DML 差异项，无 Query 事件相关条目 |
| F5 | 用户真实需求：DBA 误删数据库后通过 binlog 恢复间隔期数据（mysqldump 每日备份场景） | 本次 brainstorming §Clarifying Questions 确认 |
| F6 | Schema dump 机制已存在（`--schema-dump` / `--schema-file`）支持离线解码，但无 schema version history 管理 | `src/metadata/store.rs:235+` + `HANDOVER`「当前进度」P2 条目 |
| F7 | Stress test 容器组（my2sql-dt-5.6/5.7/8.0）持续写入中，可提供真实 DML+ 潜在 DDL混合 binlog 样本 | `CronCreate` 30 分钟轮询监控中（PID 400156，截至 2026-09-22 21:54 UTC 启动） |
| F8 | Aud it 发现两个 Medium 问题（C1 unwrap, C9 debug output）暂不影响核心功能但需追踪 | `docs/AUDIT_REPORT.md` 建议 #1 (tracing migration), #2 (unwrap doc) |

**关键洞察**: 当前 flashback 能正确逆向 DML，但对 DDL 的处理是"静默跳过"——用户不知道哪些数据被跳过了、为什么跳过。**P6 的目标是增加"可解释性"而非"自动化恢复"**（后者危险且复杂）。

---

## 1. 目标与范围

### Goal
让 flashback 模式在生产场景中**更可解释、更可控**——即使遇到 DDL+DML 混合的复杂 binlog，也能明确告诉用户"我能恢复什么、跳过了什么、恢复率多少"。

### Hard Boundaries（明确不做）
- ❌ **No --apply 自动执行**: 工具只生成 SQL 文件，不连库执行（安全边界）
- ❌ **No DDL reverse engineering**: 不尝试将 DROP→CREATE 或 ALTER→inverse ALTER（MySQL DDL 不是总能逆反，风险高）
- ❌ **No schema version auto-switching**: 接受单份 schema dump，不维护历史版本快照（使用场景少，实现复杂度高）
- ❌ **No intelligent tolerance**: 跳过事件时不尝试用旧 schema 解码并生成带 warning 标记的 SQL（太激进，可能产生脏数据）

### In Scope（明确做）
- ✅ **Report-file mode**: 记录跳过事件的详细位置、类型、SQL 原文到独立 JSONL 文件
- ✅ **Dry-run preview**: 只统计不生成 SQL，输出 summary（可恢复事务数、跳过事件数、预计影响行数、恢复率%）
- ✅ **On-error 策略显式化**: 暴露用户开关 `--on-error {stop, skip}`，默认 stop（保守优先）
- ✅ **Automated e2e test**: drop-database → flashback → checksum compare 全流程回归测试

### Out of Scope（明确不做，续挂）
- 实时连接 DB 进行回放（repl × flashback = P3 职责）
- 多 schema 版本智能切换（未来扩展空间）
- DDL 反向 SQL 生成（超出"逆向 DML"核心目标）
- --apply 直写库能力（P6 不涉及，YAGNI）

---

## 2. 任务分解（4 任务串行推进）

```
T1: DDL 白名单化报告（~2–4 h）
  ↓
T2: Dry-run 统计预览（~1–2 h）
  ↓
T3: On-error 策略开关（~1 h，复用现有）
  ↓
T4: Drop-recovery e2e 测试（~2–4 h）
  → HANDOVER + CHANGELOG 更新
```

**并行纪律**: T1/T2/T3 修改同一模块（`src/pipeline/mod.rs`），需串行完成并逐 commit 验收；T4 在前三任务合入 main 后执行。

---

## 3. 关键决策（Ruling & Spec Compliance）

### D1 Report Format = Minimal JSONL Summary（极简 JSONL 报告）
**裁定**: 
```jsonl
{"timestamp": "2026-09-22T14:30:15Z", "binlog": "mysql-bin.000150", "position": 12345, "type": "Query", "sql": "ALTER TABLE t_users ADD COLUMN new_field VARCHAR(100)"}
{"timestamp": "2026-09-22T14:31:22Z", "binlog": "mysql-bin.000150", "position": 67890, "type": "Rows", "event": "column_count_mismatch", "table": "t_orders", "expected_cols": 5, "got_cols": 6}
```
**理由**:
- JSONL 流式格式，支持断点续读（tail -f 实时查看）
- 每条记录独立，便于 grep/awk/jq解析
- 不包含 preview SQL（避免报告膨胀至 50KB+），仅提供判定依据

**替代方案 rejected**:
- B) Full SQL preview → 报告会很大，不适合自动化解析
- C) Mixed mode → 增加复杂度，用户需求不明确

**Spec compliance**: 呼应 `CHANGELOG` §6 D6「禁新造数」原则 —— report 字段均为 runtime-fetched 实值，非 hardcoded 字面量。

---

### D2 Dry-run Mode = Summary Only（干跑只输出 summary）
**裁定**: 
```json
{
  "binlog_range": {"start_file": "mysql-bin.000100", "start_pos": 4, "end_file": "mysql-bin.000200", "end_pos": 50000},
  "summary": {
    "total_transactions": 1234,
    "recoverable_transactions": 1198,
    "skipped_events": 36,
    "estimated_rows_affected": 45678,
    "recovery_rate": "97.08%"
  },
  "warnings": [
    {"count": 15, "type": "Query_event"},
    {"count": 21, "type": "Column_count_mismatch"}
  ]
}
```
**理由**:
- 恢复率百分比让用户快速判断"是否值得继续执行"
- Warnings 按类型聚合，避免重复信息
- JSON 格式便于 CI/CD集成（exit code 基于 recovery_rate 阈值）

**Implementation note**: `--dry-run` flag 不触发 tmp 文件写入，仅遍历 binlog 一次计数（O(N) 时间，零 I/O）。

---

### D3 On-error Strategy = Stop Default（默认保守，可选激进）
**裁定**: 
- **默认行为** (`--on-error stop`): 遇 DML+DDL 混合时立即停止 + 打印完整错误堆栈 + 总结已恢复事务数
- **激进模式** (`--on-error skip`): 跳过单个坏事件但不中断流程 + 记录到 report-file + 最终生成部分恢复 SQL

**Rationale**:
- 符合 P2 完整性立场 (§3.2 flashback integrity): "坏输入即坏回滚，宁可中止不留半成品"
- Skip 作为可选项满足"尽可能恢复"的用户偏好，但需显式承诺风险
- CLI 参数沿用 P2 已有枚举类型 `OnError::Stop | SkipBadEvent`，零新增依赖

**Spec compliance**: 继承自 P2 T5 裁决（`HANDOVER`「P2 DoD 对账」T9），本役只做"暴露给用户"层面无行为变更。

---

### D4 E2E Test = Drop-Recovery Checksum Compare（DROP→Flashback→Checksum）
**裁定**: 
```bash
# Test steps
1. Create DB + tables + data → checksum A (pre-drop)
2. Run INSERT/UPDATE/DELETE for N hours → checksum B (pre-detect)
3. DROP DATABASE crash_db → simulate accident
4. Restore from mysqldump --no-data → empty db
5. my2sql-rs flashback --schema-file schema.json --binlog-dir ... --output-dir out/
6. Re-execute generated SQL → checksum C (post-restore)
7. Assert C == B (byte-level match on all tables)
```
**Pass criterion**: All tables' CHECKSUM MD5 == pre-drop values within recoverable range; skipped events logged to report with explicit count.

**Edge cases covered**:
- DDL mid-binlog (ALTER TABLE during recovery window)
- Column count mismatch (schema drift post-alter)
- Transaction boundaries (partial trx rollback safety)

**Integration**: Added to `make compat` matrix as new WORK_TYPE=drop-recovery case (auto-provisioned via docker-compose).

---

## 4. 全局约束（Global Constraints）

- **TDD first**: 每个任务先写 failing test（红证复现真实失败场景）→ implement → green → commit
- **Zero new dependencies**: 仅复用既有 crate（serde/derive for JSON parsing already present）
- **Backward compatibility**: 所有新增 flags 有合理 default（stop/skipped-to-stderr），不改现有 CLI 契约
- **CI gate discipline**: fmt/clippy/test/musl 编译门（spec §3 D3），drop-recovery 测试不进 CI（需 docker + binlog sample，本地六闸为主）
- **Documentation update**: HANDOVER §P6节点日志 + CHANGELOG v0.6.0 节 + README 差异清单续号
- **SDD ledger**: `.superpowers/sdd/2026-09-22-my2sql-rs-p6-data-recovery/progress.md` 逐任务闭环

---

## 5. 挂账处置表（P6 视角）

| 挂账（来自 HANDOVER/Audit） | P6 处置 | Status |
|-----------------------------|---------|--------|
| C1 — unwrap() ~30 处 | **续挂**（低优，P6 专注报告逻辑，不改现有 error path） | Track in AUDIT_LOG |
| C9 — debug output 2 处 | **消费**（report-file 实现时用 tracing::debug!() 替换 println!()） | Part of T1 |
| FLASHBACK-DDL-UNKNOWN | **关闭**（D3 明确 on-error skip 行为，不再未知） | Resolved |
| SCHEMA-VERSION-HISTORY | **拒绝**（不在 scope，文档化说明"单 schema dump 假设"） | Won't do |
| RECOVERY-DRYRUN-MODE | **消费**（D2 dry-run summary mode 交付） | Implemented in T2 |

**新挂账**（P6 引入的新开问题）:
- None（P6 边界清晰，无遗留债务）

---

## 6. DoD（P6 完成判据，逐条可验）

1. **T1 Report-file 交付**: `--report-file <path>` flag 工作，JSONL 格式含 binlog 位置 + event 类型 + SQL 原文；`cargo test --test ddl_skip_report` 绿 | ✓ Handover evidence + unit test diff
2. **T2 Dry-run 交付**: `--dry-run` flag 输出 JSON summary（含 recovery_rate%）; `cargo test --test dryrun_summary` 绿 | ✓ Output format validation
3. **T3 On-error 开关交付**: CLI help text 显示 `--on-error {stop, skip}`，默认 stop; `cargo test --test on_error_strategy` 绿 | ✓ Integration test coverage
4. **T4 E2E 交付**: `make compat-worktype=drop-recovery` run 全绿（checksum 比对通过）| ✓ Docker-provisioned test suite log
5. **Regression unchanged**: `make difftest` (WORK_TYPE=rollback) exit 0, byte-level identical outputs | ✓ No behavioral changes
6. **CI pass**: PR merge to main → GitHub Actions ci.yml 四门全绿（fmt/clippy/test/musl） | ✓ gh run conclusion=success
7. **Docs updated**: CHANGELOG v0.6.0 节 + HANDOVER P6 节点日志 + README 差异续号 | ✓ File existence + content verification

---

## 7. 规格引用溯源（Traceability）

- **§3 D1 (minimal JSONL)** → 对应 `docs/superpowers/plans/2026-09-22-my2sql-rs-p6-data-recovery.md` Task 1 Step 2
- **§3 D2 (dry-run summary)** → 对应 `docs/superpowers/plans/...` Task 2 Step 3
- **§3 D3 (on-error default)** → 继承自 `HANDOVER`「P2 DoD 对账」T9 R12 裁定
- **§3 D4 (e2e test)** → 对应 `docs/superpowers/plans/...` Task 4 Step 1–4
- **§6 挂账处置** → 映射 `docs/AUDIT_REPORT.md` Recommendation #1 (tracing migration)

---

## 8. 后续规划（Post-P6 Outlook）

**Phase 2 possibilities**（P6 完成后开放讨论）:
- **Schema history management**: Accept multiple schema snapshots with valid_from/valid_until ranges → automatic switch based on binlog position
- **Intelligent tolerance**: Try decoding with fallback schemas before skipping → generate warning-marked SQL
- **Partial apply mode**: Read generated SQL and execute selectively (transaction-by-transaction) → dangerous but useful for emergency recovery

**Not recommended yet** until:
- Field feedback from real production incidents validates demand
- Complexity ROI analysis shows >80% use case coverage
- Security review approves "partial execute" semantics

---

*Spec drafted by Qoder using brainstorming skill workflow.*
*Authoritative reference: users' explicit choices in Clarifying Questions #1–3.*
*Ready for writing-plans implementation planning.*
