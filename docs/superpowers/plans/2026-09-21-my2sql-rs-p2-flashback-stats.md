# my2sql-rs P2（flashback + stats）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 my2sql-rs 增加 `flashback`（回滚 SQL，逆向恢复）与 `stats`（DML 统计 + 大/长事务识别）两个子命令，经 Go 裁判差分（WORK_TYPE=rollback）与 5.6–8.4 兼容矩阵验证。

**Architecture:** 语义反转落在 sqlopen（`DmlBuilder` 增 `WorkKind`）；顺序反转落在新组件 `src/flashback/reverse.rs`（正序写隐藏 tmp + 块索引，收尾并行从尾回读）；stats 是独立子命令，复用 P1 流水线只改通道载荷为 `Out` 枚举（SQL 组 / 轻量事实），聚合器单线程消费保序流。P1 的线程模型、反压、panic 填洞契约原样成立。

**Tech Stack:** Rust 2024 edition（clap/chrono/serde_json/crossbeam-channel，全部已在 Cargo.toml，**不新增依赖**）、Python3 stdlib（比较器）、bash + docker（差分/矩阵）。

**Spec:** `docs/superpowers/specs/2026-09-21-my2sql-rs-p2-flashback-stats-design.md`（提交 c9fa852）。本计划随附一处 spec 勘误（见「Spec 勘误」节），以勘误后文本为准。

## Global Constraints

- **`reference/my2sql-go/` 只读**：行为参照 + 差分裁判源，任何任务不得修改其源文件（`go build -o tools/bin/...` 构建允许）。
- **TDD**：每段生产代码先有失败测试（红→绿→提交）；不许「先实现再补测试」。
- **不许虚报**：测试/矩阵/差分结果如实记录；比较器白名单规则必须值形状严格且逐条有上游依据。
- **门禁**：每任务收尾 `cargo test`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check` 三门全绿 + `docs/HANDOVER.md` 节点更新。
- **解码器不得 panic**（P1 终审事实：threads=1 直通无 catch_unwind）；新代码同守。
- **上游字节口径**（本计划实读为证，逐字节对齐对象）：
  - `base/events.go:321-331` 块内容 = extra 时 `"# datetime=%s database=%s table=%s binlog=%s startpos=%d stoppos=%d\n" + join(sqls,";\n") + ";\n"`；无 extra 时 `join(sqls,";\n") + ";\n"`。P1 `Writer::write_group`（src/output.rs:166）已产出同款字节。
  - `base/events.go:259` 索引 = **每 rows 事件组一条** `(len(块字节), trxIndex)`（非每事务一条）。
  - `base/rollback_process.go:31,76-155`：tmp 尾部 seek 打开、块逆序、块内**裸行**逆序（跳空行）；`lastTrxIdx` 初值 0 → **首个写出块必注入** `commit;\nbegin;\n`（块间 trx 变化处同）；文件尾补 `commit;\n`；tmp 删除。产物名 `rollback.{idx}.sql`（file-per-table 前插 `{schema}.{table}.`），tmp 为同名前置 `.` 隐藏文件。
  - `base/stats_process.go`：`binlog_status.txt` 头（context.go:530）与行格式 `%-17s %-19s %-19s %-10s %-10s %-8s %-8s %-8s %-15s %-20s\n`（列 binlog,starttime,stoptime,startpos,stoppos,inserts,updates,deletes,database,table）；`biglong_trx.txt` 头（context.go:540）与行 `%-17s %-19s %-19s %-10s %-10s %-8s %-10s %s\n`；datetime 用 `2006-01-02_15:04:05` 下划线形（= P1 `datetime_str`）；窗口 = lastPrintTime 初始 0→ts+interval、`ts>=lastPrintTime` 落盘重置、binlog 切换落盘；key=`db.tb`（funcs.go:123-125）；biglong：BEGIN 重置累加器、rows 累计（update 行计数=对数）、commit/rollback 且 StartTime>0 才判 `RowCnt>=bigLimit || Duration>=longSecs`；XID_EVENT 视同 commit；默认 30/10/1，范围 1..600 / 1..30000 / 0..3600（context.go:53-59）。
- **本计划对上游的三处有意超越**（差分白名单登记 + README 差异清单）：
  1. 记录原子逆序：extra-info 注释行与其 SQL 组不拆对（上游裸行逆序致注释漂到组尾）；块内 SQL 行仍逆序（语义所需）。
  2. keep-trx 补上 CLI 开关（上游 `KeepTrx` 无 flag 绑定，struct 默认 false；我们默认 **开**）。
  3. stats 明细 `[...]` 内表序：Go map 随机序，我们按 db.tb 升序（确定性输出）。

## Spec 勘误（已随本计划同一次提交落进 spec 文件本体）

`docs/superpowers/specs/2026-09-21-my2sql-rs-p2-flashback-stats-design.md` §2/§3.6 已修正：
「结构断言：keep-trx 注入次数=事务数-1」修正为——逐字节对齐上游时**注入次数=事务段数**（`lastTrxIdx` 初值 0，首个写出块必注入，rollback_process.go:31,131）。结构断言改为：`begin;` 行数 = 事务段数 K、`commit;` 行数 = K+1（尾 `commit;\n`）、每处 `begin;` 前一行必为 `commit;`。同时 §2 的 tmp 索引口径由「每事务块」修正为「每事件块」（events.go:216,259）。

## 模块文件布局（增量总览）

```
src/sqlopen/dml.rs         + WorkKind / DmlBuilder.kind / dml_for / updates 反转 / 两条 hard 规则   (T1)
src/output.rs              path_for 前缀参数化 + Writer 块索引模式(flashback tmp)                    (T2)
src/flashback/mod.rs       (新) tmp↔final 路径映射 + 收尾驱动                                        (T2)
src/flashback/reverse.rs   (新) 块索引并行逆序 + keep-trx 注入 + 原子记录                             (T2)
src/pipeline/worker.rs     build_groups 走 dml_for；+Out 枚举、build_out(stats)、Job.status          (T3,T4)
src/pipeline/mod.rs        Emitter 三形态 + on_error 策略 + DDL 汇总 + run_flashback / run_stats     (T3,T4)
src/pipeline/order.rs      Reorder<T> 泛化（默认 SqlGroup，既有测试零改动）                          (T4)
src/stats/mod.rs           (新) Aggregator：窗口/biglong/两报表+JSONL                                (T4)
src/config.rs              CommonArgs flatten + FlashbackArgs/StatsArgs + Config 新字段              (T5)
src/lib.rs                 pub mod flashback; pub mod stats;                                         (T2,T4)
src/main.rs                三子命令 dispatch                                                         (T5)
tests/common/synth.rs      (移) e2e Synth 构造器共享                                                 (T3)
tests/flashback.rs         (新) run_flashback 全链路库级测试                                         (T3)
tools/flashback-reconcile.sh (新) 一次性正逆对账（活库执行，非 CI）                                  (T6)
tools/run-difftest.sh      WORK_TYPE=2sql|rollback|stats 维度                                        (T7)
tools/comparator/compare.py +selftest.py   rollback 解析模式 + 结构断言 + 组9                        (T7)
tools/compat-matrix.sh     flashback×4 版本 + stats×2 版本                                           (T8)
docs/{compat/matrix.md, HANDOVER.md, README.md, difftest-allowlist.txt}  更新                        (T7,T8,T9)
```

任务执行序 = 编号序（T1→T9）。T1/T2 无相互依赖但 T3 依赖两者。

---

### Task 1: sqlopen 语义反转（WorkKind + 逆向 UPDATE + 完整性硬规则）

**Files:**
- Modify: `src/sqlopen/dml.rs`
- Test: `src/sqlopen/dml.rs`（同文件 `mod tests`）

**Interfaces:**
- Consumes: P1 `DmlBuilder::{inserts,deletes,updates}`、`SqlOpts::from_config`、`Align`、`ColumnValue::Missing`（现状见文件）。
- Produces（T2/T3 依赖，签名钉死）:
  - `pub enum WorkKind { ToSql, Flashback }`（`Debug, Clone, Copy, PartialEq, Eq`）
  - `DmlBuilder::new(opts: SqlOpts) -> Self`（kind=ToSql，语义不变）
  - `DmlBuilder::flashback(opts: SqlOpts) -> Self`
  - `DmlBuilder::kind(&self) -> WorkKind`
  - `pub fn dml_for(&self, kind: crate::binlog::rows::RowsKind, tm: &TableMapEvent, s: &TableSchema, rows: &[Row]) -> Result<Vec<String>, SqlError>`
  - `updates()` 在 kind=Flashback 时自动出 SET=before / WHERE=after（方法签名不变）

- [ ] **Step 1: 写失败测试（WorkKind 构造与 dml_for 分派）**

```rust
#[test]
fn flashback_dml_for_maps_events_to_reverse_statements() {
    let b = DmlBuilder::flashback(SqlOpts::default());
    let t = tm(3);
    let s = schema3(); // pk=a
    // INSERT 事件(after-image) → DELETE 键定位
    let w = [row(&[i(1), sv("x"), i(2)])];
    assert_eq!(
        b.dml_for(RowsKind::Write, &t, &s, &w).unwrap(),
        vec!["DELETE FROM `db`.`t` WHERE `a`=1;"]
    );
    // DELETE 事件(before-image) → INSERT 全列
    let d = [row(&[i(7), sv("z"), i(8)])];
    assert_eq!(
        b.dml_for(RowsKind::Delete, &t, &s, &d).unwrap(),
        vec![r#"INSERT INTO `db`.`t` (`a`,`b`,`c`) VALUES (7,'z',8);"#]
    );
    // UPDATE → SET=before, WHERE=after 键
    let u = [row(&[i(1), sv("x"), i(2)]), row(&[i(1), sv("y"), i(2)])];
    assert_eq!(
        b.dml_for(RowsKind::Update, &t, &s, &u).unwrap(),
        vec![r#"UPDATE `db`.`t` SET `b`='x' WHERE `a`=1;"#]
    );
    // 正向 kind 不受影响
    assert_eq!(
        DmlBuilder::new(SqlOpts::default())
            .dml_for(RowsKind::Write, &t, &s, &w)
            .unwrap()[0],
        r#"INSERT INTO `db`.`t` (`a`,`b`,`c`) VALUES (1,'x',2);"#
    );
}
```

测试模块头部补 `use crate::binlog::rows::RowsKind;`。

- [ ] **Step 2: 跑测试确认失败**（`cargo test --lib sqlopen::dml 2>&1 | head`，Expected: 编译错 `cannot find WorkKind`）

- [ ] **Step 3: 实现 WorkKind + dml_for + kind 字段**

`DmlBuilder` 结构体加字段 `kind: WorkKind`（`#[derive(Debug, Clone, Default)]` 保持——`WorkKind` 实现 `Default`（ToSql））。构造与分派：

```rust
impl Default for WorkKind {
    fn default() -> Self {
        WorkKind::ToSql
    }
}
impl DmlBuilder {
    /// 正向构建器（P1 行为，零改动）。
    pub fn new(opts: SqlOpts) -> Self {
        Self { opts, kind: WorkKind::ToSql }
    }
    /// P2 回滚构建器：语义反转（INSERT↔DELETE、UPDATE 的 SET/WHERE 镜像）。
    pub fn flashback(opts: SqlOpts) -> Self {
        Self { opts, kind: WorkKind::Flashback }
    }
    pub fn kind(&self) -> WorkKind {
        self.kind
    }
    /// 事件种类 → 逆向/正向 SQL 的统一分派（T3 worker 唯一入口）。
    pub fn dml_for(
        &self,
        kind: RowsKind,
        tm: &TableMapEvent,
        s: &TableSchema,
        rows: &[Row],
    ) -> Result<Vec<String>, SqlError> {
        use RowsKind::*;
        match (kind, self.kind) {
            (Write, WorkKind::ToSql) | (Delete, WorkKind::Flashback) => self.inserts(tm, s, rows),
            (Delete, WorkKind::ToSql) | (Write, WorkKind::Flashback) => self.deletes(tm, s, rows),
            (Update, _) => self.updates(tm, s, rows),
        }
    }
}
```

- [ ] **Step 4: 写失败测试（UPDATE 反转细节）**

```rust
#[test]
fn flashback_update_full_columns_and_unchanged_pair() {
    let bf = DmlBuilder::flashback(opts(|o| o.full_columns = true));
    let rows = [row(&[i(1), sv("x"), i(2)]), row(&[i(1), sv("y"), i(2)])];
    assert_eq!(
        bf.updates(&tm(3), &schema3(), &rows).unwrap()[0],
        r#"UPDATE `db`.`t` SET `a`=1,`b`='x',`c`=2 WHERE `a`=1 AND `b`='y' AND `c`=2;"#,
        "full: SET 全列取 before、WHERE 全列取 after"
    );
    // 无变化行对：正向跳语句（上游 abort）；逆向同样跳（空 SET 同构）
    let same = [row(&[i(1), sv("x"), i(2)]), row(&[i(1), sv("x"), i(2)])];
    assert!(DmlBuilder::flashback(SqlOpts::default())
        .updates(&tm(3), &schema3(), &same)
        .unwrap()
        .is_empty());
}

#[test]
fn flashback_update_unchanged_json_not_in_set() {
    // 上游逆向 UPDATE 仍带「JSON 恒进 SET」quirk；本层不继承（spec §3.1）——
    // 未变化的 JSON 列不得出现在 SET。比较器 ALW-JSON-IN-SET 容忍裁判多出的
    // JSON 项，本测试钉死我方严格。
    let mut s = schema3();
    s.cols[1].type_name = "json".into();
    let mut t = tm(3);
    t.column_type[1] = 0xf6; // MYSQL_TYPE_JSON
    let rows = [
        row(&[i(1), ColumnValue::Json(r#"{"a":1}"#.into()), i(2)]),
        row(&[i(1), ColumnValue::Json(r#"{"a":1}"#.into()), i(9)]),
    ];
    let got = DmlBuilder::flashback(SqlOpts::default())
        .updates(&t, &s, &rows)
        .unwrap();
    assert_eq!(got[0], r#"UPDATE `db`.`t` SET `c`=2 WHERE `a`=1;"#);
}
```

- [ ] **Step 5: 实现 updates() 反转** — 行对循环内镜像取值：`let (set_row, where_row) = match self.kind { WorkKind::ToSql => (after, before), WorkKind::Flashback => (before, after) };`；SET 差异比较 `b != a` 不变（对称）；assigns 值从 `set_row` 取；`where_part(&p, where_row, tm)`。空 assigns 跳过逻辑不变。

- [ ] **Step 6: 写失败测试（完整性硬规则 a/b）**

```rust
#[test]
fn flashback_rejects_padded_dropped_columns_as_event_error() {
    // 硬规则 a（对齐上游 events.go:87 fail-hard）：dropped 列 = 旧镜像不完整，
    // 回滚脚本宁缺毋漏 → Flashback 下 Padded 是错误而非告警。
    let b = DmlBuilder::flashback(SqlOpts::default());
    let rows = [row(&[i(1), sv("x"), i(2), i(99)])];
    let e = b.deletes(&tm(4), &schema3(), &rows).unwrap_err();
    assert!(matches!(e, SqlError::Value(BinlogError::InvalidData(_))), "{e:?}");
    assert!(e.to_string().contains("flashback"), "{e}");
    // 正向不受影响（P1 行为回归守卫）
    assert!(DmlBuilder::new(SqlOpts::default()).inserts(&tm(4), &schema3(), &rows).is_ok());
}

#[test]
fn flashback_missing_value_error_hints_row_image_full() {
    // 硬规则 b：Missing（MINIMAL row image / partial）进 WHERE/VALUES → 报错，
    // 提示需 binlog_row_image=FULL。
    let b = DmlBuilder::flashback(SqlOpts::default());
    let rows = [row(&[i(1), ColumnValue::Missing, i(2)])];
    let e = b.deletes(&tm(3), &schema3(), &rows).unwrap_err();
    assert!(e.to_string().contains("binlog_row_image=FULL"), "{e}");
    let e = b.inserts(&tm(3), &schema3(), &rows).unwrap_err();
    assert!(e.to_string().contains("binlog_row_image=FULL"), "{e}");
    // 正向维持 P1 既有 InvalidData（encode 路径），消息不要求 FULL 提示
    let e = DmlBuilder::new(SqlOpts::default())
        .inserts(&tm(3), &schema3(), &rows)
        .unwrap_err();
    assert!(matches!(e, SqlError::Value(BinlogError::InvalidData(_))), "{e:?}");
}
```

- [ ] **Step 7: 实现硬规则** — `plan()` 里 `Align::Padded` 分支：`if self.kind == WorkKind::Flashback { return Err(BinlogError::InvalidData(format!("flashback: dropped columns in `{}`.`{}` make old row images unreliable — refusing to emit partial rollback", s.db, s.table)).into()); }`，否则保持既有 warn。`cell()` 里：`if self.kind == WorkKind::Flashback && matches!(v, ColumnValue::Missing) { return Err(BinlogError::InvalidData(format!("flashback: column ordinal {ord} missing (MINIMAL row image); requires binlog_row_image=FULL"))).into(); }`（放在宽度检查之后）。

- [ ] **Step 8: 全绿 + 门禁** — `cargo test --lib && cargo clippy --all-targets -- -D warnings && cargo fmt`。dml.rs 模块注释补一段「P2 反转口径」（WorkKind 语义、两条 hard 规则、不继承 JSON-SET quirk、上游字节对照 file:line）。

- [ ] **Step 9: 提交** — `git add src/sqlopen/dml.rs && git commit -m "feat(sqlopen): WorkKind flashback reversal — dml_for mapping, reverse UPDATE, dropped/missing hard rules"`；HANDOVER 节点更新并入本提交（`docs/HANDOVER.md`）。

---

### Task 2: flashback 落盘组件（tmp 写手 + 并行逆序 reverse.rs）

**Files:**
- Create: `src/flashback/mod.rs`、`src/flashback/reverse.rs`
- Modify: `src/output.rs`（path_for 前缀参数化、Sink 偏移计数、Writer 块索引模式）、`src/lib.rs`（`pub mod flashback;`）
- Test: `src/output.rs`、`src/flashback/reverse.rs` 同文件 tests

**Interfaces:**
- Consumes: P1 `Writer::{new, write_group, finish}`、`SqlGroup`、`FILE_HEADER`、`path_for`、`sanitize_for_path`。
- Produces（T3 依赖）:
  - `output::path_for(dir: &Path, prefix: &str, binlog: &str, db: &str, table: &str, file_per_table: bool) -> PathBuf`（既有 5 参版删除，全部调用点改 6 参：to-sql 传 `"to_sql"`，flashback tmp 传 `".flashback.tmp"`，final 传 `"flashback"`）
  - `Writer::new(dir, stdout, file_per_table, extra_info, tz, prefix: String, index: bool)`（新末两参）
  - `Writer::blocks(&self) -> &HashMap<PathBuf, Vec<(u64, u64, u64)>>` — tmp 路径 → `Vec<(offset, len, trx_id)>`
  - `flashback::reverse::{reverse_block, reverse_file, run_files}`

- [ ] **Step 1: 失败测试 — output.rs 前缀参数化 + 块索引**

改既有 6 处 `path_for` 调用加 `"to_sql"` 前缀参（测试内断言字节不变），新增：

```rust
#[test]
fn writer_indexes_blocks_for_flashback_tmp() {
    let dir = std::env::temp_dir().join(format!("my2sql-p2t2-b-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let tmp = dir.join(".flashback.tmp.1.sql");
    {
        let mut w = Writer::new(
            dir.clone(), false, false, true,
            FixedOffset::east_opt(0).unwrap(),
            ".flashback.tmp".into(), true,
        );
        let mut g = grp("mysql-bin.000001", "d", "t"); // trx_id=1, sqls=["SELECT 1;"]
        w.write_group(&g).unwrap(); // 注释行 + 1 SQL
        g.trx_id = 2;
        g.sqls.push("SELECT 2;".into());
        w.write_group(&g).unwrap();
        assert_eq!(w.finish().unwrap(), 1);
        let idx = w.blocks().get(&tmp).expect("block index for tmp");
        let total = std::fs::metadata(&tmp).unwrap().len();
        assert_eq!(idx.len(), 2);
        assert_eq!(idx[0].0, FILE_HEADER.len() as u64, "首块紧跟 SET NAMES 头");
        assert_eq!(idx[1].0 + idx[1].1, total, "末块止于文件尾");
        assert_eq!((idx[0].2, idx[1].2), (1, 2), "trx_id 逐块透传");
        let bytes = std::fs::read(&tmp).unwrap();
        let b0 = &bytes[idx[0].0 as usize..][..idx[0].1 as usize];
        let b1 = &bytes[idx[1].0 as usize..][..idx[1].1 as usize];
        assert!(b0.starts_with(b"# datetime=") && b0.ends_with(b"SELECT 1;\n"), "{b0:?}");
        assert!(b1.starts_with(b"# datetime=") && b1.ends_with(b"SELECT 2;\n"), "{b1:?}");
        assert!(b1.contains(b"SELECT 1;\nSELECT 2;\n"), "块内语句保序（逆序属 reverse.rs）");
    }
    std::fs::remove_dir_all(&dir).ok();
}
```

- [ ] **Step 2: 跑红**（`cargo test --lib output:: 2>&1 | head`，Expected: 签名不符编译错）

- [ ] **Step 3: 实现 output.rs 改动** — `Sink` 增加 `written: u64`（文件型 sink 初值 = FILE_HEADER 字节数；Screen sink 不计数）；`Writer` 增字段 `prefix: String`、`index: bool`、`blocks: HashMap<PathBuf, Vec<(u64,u64,u64)>>`。`sink_for` 建文件时 `Sink::File { bw, written: FILE_HEADER.len() as u64 }`。`write_group` 组装完本批字节串 `batch`（注释行+逐句+`\n`，即现逻辑）后：`if self.index { 取 path 与 sink.written，push (off, batch.len() as u64, g.trx_id) }`，再写入并 `written += batch.len()`。`path_for` 加 `prefix` 参数：`format!("{prefix}.{n}.sql")` / `format!("{prefix}.{db}.{table}.{n}.sql")`（净化逻辑不动）。既有 `Writer::new` 调用点（pipeline/mod.rs:77）补 `"to_sql".into(), false`。

- [ ] **Step 4: 失败测试 — reverse.rs 字节 golden（keep-trx 上游对齐）**

`src/flashback/reverse.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn write_tmp(path: &Path, blocks: &[&str]) -> Vec<(u64, u64, u64)> {
        let mut f = std::fs::File::create(path).unwrap();
        std::io::Write::write_all(&mut f, crate::output::FILE_HEADER.as_bytes()).unwrap();
        let mut off = crate::output::FILE_HEADER.len() as u64;
        let mut idx = Vec::new();
        for (i, b) in blocks.iter().enumerate() {
            f.write_all(b.as_bytes()).unwrap();
            idx.push((off, b.len() as u64, (i as u64 / 2) + 1)); // 每两块一事务
            off += b.len() as u64;
        }
        idx
    }

    #[test]
    fn reverse_bytes_byte_equal_upstream_keeptrx_quirk() {
        // 上游口径（rollback_process.go:31,131-133,153-155）：lastTrxIdx 初值 0，
        // 首个写出块（tmp 尾块）必注入 commit;\nbegin;\n（头部悬空 commit 是其原样
        // 行为，「逐字节对齐」= 照抄）；块间 trx 变化处注入；尾补 commit;\n。
        let dir = std::env::temp_dir().join(format!("my2sql-p2t2-r-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tmp = dir.join(".flashback.tmp.1.sql");
        let outp = dir.join("flashback.1.sql");
        // 块 = 单行 SQL（无 extra-info 形态 = 与上游裸行逆序全等）
        let idx = write_tmp(&tmp, &["INSERT A;\n", "INSERT B;\n", "INSERT C;\n", "INSERT D;\n"]);
        run_files(&[(tmp.clone(), outp.clone(), idx)], true, 2, None).unwrap();
        assert_eq!(
            std::fs::read_to_string(&outp).unwrap(),
            "SET NAMES utf8mb4;\n\
             commit;\nbegin;\nINSERT D;\nINSERT C;\n\
             commit;\nbegin;\nINSERT B;\nINSERT A;\n\
             commit;\n"
        );
        assert!(!tmp.exists(), "tmp 必须删除（上游 rollback_process.go:20）");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reverse_block_is_record_atomic_with_comment_first() {
        // 超越项 1：注释行不漂到组尾；块内 SQL 行逆序（语义所需）
        let blk = "# datetime=X stoppos=99\nINSERT r1;\nINSERT r2;\n";
        assert_eq!(
            reverse_block(blk),
            "# datetime=X stoppos=99\nINSERT r2;\nINSERT r1;\n"
        );
        // 无注释块 = 纯行逆序（与上游一致）
        assert_eq!(reverse_block("A;\nB;\n"), "B;\nA;\n");
    }

    #[test]
    fn no_keeptrx_emits_pure_reverse_without_scaffold() {
        let dir = std::env::temp_dir().join(format!("my2sql-p2t2-nk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tmp = dir.join(".flashback.tmp.1.sql");
        let outp = dir.join("flashback.1.sql");
        // 两块同事务（write_tmp 的 i/2+1 规则：块0/1=trx1、块2/3=trx2）→
        // keep_trx=false：仅逆序、零脚手架。块含多行：块内行逆序照旧。
        let idx = write_tmp(&tmp, &["A1;\nA2;\n", "B1;\n", "C1;\n", "D1;\n"]);
        run_files(&[(tmp.clone(), outp.clone(), idx)], false, 1, None).unwrap();
        assert_eq!(
            std::fs::read_to_string(&outp).unwrap(),
            "SET NAMES utf8mb4;\nD1;\nC1;\nB1;\nA2;\nA1;\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn threads_1_and_8_output_identical_across_files() {
        // 文件级任务队列：threads 只改文件间并发 → 3 文件两跑（threads 1/8）
        // 逐文件字节全等（含 keep-trx 注入的 per-file lastTrxIdx 复位语义：
        // 每文件独立 0 初值，与上游 per-file 线程一致）。
        let dir = std::env::temp_dir().join(format!("my2sql-p2t2-th-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut jobs1 = Vec::new();
        for n in 1..=3 {
            let tmp = dir.join(format!(".flashback.tmp.{n}.sql"));
            let outp = dir.join(format!("flashback.{n}.sql"));
            let idx = write_tmp(&tmp, &["s1;\n", "s2;\n", "s3;\n"]);
            jobs1.push((tmp, outp, idx));
        }
        run_files(&jobs1, true, 1, None).unwrap();
        let first: Vec<String> = jobs1.iter().map(|j| std::fs::read_to_string(&j.1).unwrap()).collect();
        for j in &mut jobs1 {
            std::fs::remove_file(&j.1).unwrap();
            let idx = write_tmp(&j.0, &["s1;\n", "s2;\n", "s3;\n"]); // 重跑需重建 tmp
            j.2 = idx;
        }
        run_files(&jobs1, true, 8, None).unwrap();
        let second: Vec<String> = jobs1.iter().map(|j| std::fs::read_to_string(&j.1).unwrap()).collect();
        assert_eq!(first, second, "threads 不得影响单文件字节");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn warn_line_inserted_after_header_and_empty_index_skipped() {
        let dir = std::env::temp_dir().join(format!("my2sql-p2t2-wl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tmp = dir.join(".flashback.tmp.1.sql");
        let outp = dir.join("flashback.1.sql");
        let idx = write_tmp(&tmp, &["X;\n"]);
        run_files(&[(tmp.clone(), outp.clone(), idx)], false, 1,
            Some("-- WARNING: skipped 3 events, positions in stderr\n")).unwrap();
        assert_eq!(
            std::fs::read_to_string(&outp).unwrap(),
            "SET NAMES utf8mb4;\n-- WARNING: skipped 3 events, positions in stderr\nX;\n"
        );
        // 空块表：不落 final、tmp 删除、Ok（与上游产出仅 commit;\n 空文件的分歧
        // 登记入差异清单——判空跳过）
        let tmp2 = dir.join(".flashback.tmp.2.sql");
        std::fs::File::create(&tmp2).unwrap().write_all(crate::output::FILE_HEADER.as_bytes()).unwrap();
        let outp2 = dir.join("flashback.2.sql");
        run_files(&[(tmp2.clone(), outp2.clone(), Vec::new())], true, 1, None).unwrap();
        assert!(!outp2.exists() && !tmp2.exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
```

（`write_tmp` 辅助见首测；测试文件需 `use std::io::Write;` 于末测处——统一放 tests 模块头。）

- [ ] **Step 5: 实现 reverse.rs**

```rust
//! 顺序反转（spec §3.1）：正序 tmp + (offset,len,trx_id) 块索引 → 并行从尾
//! 回读。字节口径对照上游 rollback_process.go（模块级差异=记录原子化+前缀注入
//! 照抄，见 plan T2 测试注释）。

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, VecDeque as SharedQueue};

use crate::output::FILE_HEADER;

/// 块内逆序：extra-info 注释行与其 SQL 组为原子记录（注释保头），SQL 行逆序。
pub fn reverse_block(text: &str) -> String {
    let mut lines: Vec<&str> = text.split_terminator('\n').collect();
    let comment = if lines.first().is_some_and(|l| l.starts_with("# datetime=")) {
        Some(lines.remove(0))
    } else {
        None
    };
    lines.reverse();
    let mut out = String::with_capacity(text.len());
    if let Some(c) = comment {
        out.push_str(c);
        out.push('\n');
    }
    for l in lines {
        out.push_str(l);
        out.push('\n');
    }
    out
}

/// 单 tmp → final（调用方保证 blocks 非空）。keep_trx=true 时逐字节复刻上游
/// 注入：首块前注入（last=0 而 trx≥1）、trx 变化处注入、尾 `commit;\n`。
pub fn reverse_file(
    tmp: &Path,
    out: &Path,
    blocks: &[(u64, u64, u64)],
    keep_trx: bool,
    warn_line: Option<&str>,
) -> std::io::Result<()> {
    let mut src = File::open(tmp)?;
    let mut dst = BufWriter::new(
        OpenOptions::new().create(true).write(true).truncate(true).open(out)?,
    );
    dst.write_all(FILE_HEADER.as_bytes())?;
    if let Some(w) = warn_line {
        dst.write_all(w.as_bytes())?;
    }
    let mut last_trx: u64 = 0;
    for (off, len, trx) in blocks.iter().rev() {
        let mut buf = vec![0u8; *len as usize];
        src.seek(SeekFrom::Start(*off))?;
        src.read_exact(&mut buf)?;
        if keep_trx && last_trx != *trx {
            dst.write_all(b"commit;\nbegin;\n")?;
        }
        last_trx = *trx;
        dst.write_all(reverse_block(&String::from_utf8_lossy(&buf)).as_bytes())?;
    }
    if keep_trx {
        dst.write_all(b"commit;\n")?;
    }
    dst.flush()
}

/// 并行驱动：文件级任务队列（单文件内不再切分——上游同口径
/// events.go:272-282 每文件一线程），threads 只影响文件间并发。
/// 成功即删 tmp；任一文件失败 → 汇总返回 Err（调用方清场）。
pub fn run_files(
    files: &[(PathBuf, PathBuf, Vec<(u64, u64, u64)>)],
    keep_trx: bool,
    threads: usize,
    warn_line: Option<&str>,
) -> std::io::Result<()> {
    let queue = std::sync::Arc::new(Mutex::new(
        files.iter().cloned().collect::<VecDeque<_>>(),
    ));
    let err = std::sync::Arc::new(Mutex::new(None::<std::io::Error>));
    let mut hs = Vec::new();
    for _ in 0..threads.clamp(1, files.len().max(1)) {
        let (q, e, k, w) = (queue.clone(), err.clone(), keep_trx, warn_line.map(str::to_string));
        hs.push(std::thread::spawn(move || loop {
            let task = q.lock().unwrap().pop_front();
            let Some((tmp, out, blocks)) = task else { break };
            if blocks.is_empty() {
                let _ = std::fs::remove_file(&tmp);
                continue;
            }
            if let Err(ioe) = reverse_file(&tmp, &out, &blocks, k, w.as_deref()) {
                *e.lock().unwrap() = Some(ioe);
                return;
            }
            std::fs::remove_file(&tmp).ok();
        }));
    }
    let mut join_err = None;
    for h in hs {
        if h.join().is_err() && join_err.is_none() {
            join_err = Some(std::io::Error::other("reverse worker panicked"));
        }
    }
    let stored = err.lock().unwrap().take();
    match (stored, join_err) {
        (Some(e), _) => Err(e),
        (None, Some(e)) => Err(e),
        (None, None) => Ok(()),
    }
}
```

（`SharedQueue` 别行删掉——未用即不 import；实现时保持零警告。）

- [ ] **Step 6: `src/flashback/mod.rs`**

```rust
//! P2 flashback：正序 tmp → 逆序回滚脚本（顺序反转层）。
//! 语义反转在 sqlopen::dml（WorkKind），本模块只管文件舞蹈。
//! 命名族（有异于上游 rollback.*，CLI 重设计既定）：
//! final `flashback.{N}.sql` / file-per-table `flashback.{db}.{table}.{N}.sql`;
//! tmp 为隐藏文件 `.flashback.tmp.{N}[.db.table]` 收尾删除。

pub mod reverse;

/// tmp 路径 → final 路径：去掉文件名的 `.flashback.tmp` 前导点段
/// （`.flashback.tmp.3.sql` → `flashback.3.sql`；
///  `.db.tb..flashback.tmp.3.sql` 形态不存在——path_for 前缀恒为首段，
///  file-per-table 实为 `.flashback.tmp.db.tb.3.sql`，见 output.rs 6 参版）。
pub fn final_for_tmp(tmp: &Path) -> PathBuf {
    tmp.file_name()
        .map(|f| {
            let s = f.to_string_lossy();
            PathBuf::from(s.strip_prefix('.').unwrap_or(&s).replacen(".flashback.tmp", "flashback", 1))
        })
        .unwrap_or_else(|| tmp.to_path_buf())
}
```

**注意**：Step 3 的 `path_for` 6 参版把前缀放首段 → file-per-table 名 = `{prefix}.{db}.{table}.{n}.sql`，tmp 前缀取 `".flashback.tmp"` 得 `.flashback.tmp.d.t.3.sql`，final 替换后 `flashback.d.t.3.sql` ✓。`final_for_tmp` 用一次 `strip_prefix('.') + replacen` 即覆盖两形态；给 `final_for_tmp` 写两条单测钉死（含 file_per_table 与穿越名已被净化的假设）。

- [ ] **Step 7: lib.rs 挂模块 + 全绿 + 门禁 + 提交** — `pub mod flashback;`；`cargo test --lib`（Step 1/4 全部绿）、clippy、fmt。commit `feat(flashback): tmp block index + parallel record-atomic reverse with upstream keep-trx parity`。

---

### Task 3: 流水线接通 flashback（装配 / on-error 策略 / DDL 排除）

**Files:**
- Modify: `src/config.rs`（新字段+枚举+validate 默认值）、`src/pipeline/mod.rs`、`src/pipeline/worker.rs`（dml_for 一行改派 + abort 哨兵）、`src/lib.rs`
- Create: `tests/common/synth.rs`（从 `tests/e2e.rs` 平移）、`tests/flashback.rs`
- Modify: `tests/e2e.rs`（改为 `#[path = "common/synth.rs"] mod synth; use synth::*;`，行为零变）

**Interfaces:**
- Consumes: T1 `dml_for`/`DmlBuilder::flashback`；T2 `Writer` 7 参构造/`blocks()`/`reverse::run_files`/`flashback::final_for_tmp`。
- Produces（T5/T6 依赖）:
  - `config::{WorkType, OnError}`：`pub enum WorkType { ToSql, Flashback, Stats }`、`pub enum OnError { Stop, SkipBadEvent }`（均 `Clone, Copy, Debug, PartialEq, Eq`）
  - `Config` 新字段：`work_type: WorkType`、`keep_trx: bool`、`on_error: OnError`（`validate(ToSqlArgs)` 填 `{ToSql, true, Skip}`——to-sql 默认 skip 不变）
  - `pipeline::run_flashback(cfg: &Config) -> Result<RunSummary, PipelineError>`
  - `worker_loop(job_rx, res_tx, builder, errors, abort: Arc<AtomicBool>)`（新末参；Err/panic 且 abort 已置位请求时 `abort.store(true)`——由 Runner 传入 `stop` 模式的哨兵）

- [ ] **Step 1: worker.rs 改派 dml_for + 哨兵**

`build_groups` 的 `match kind {…}` 三臂替换为 `builder.dml_for(*kind, tm, &job.schema, &rows)?`。`worker_loop` 增参 `abort: Arc<AtomicBool>`：Err 与 panic 两分支在 `errors.fetch_add` 后加 `abort.store(true, Ordering::Relaxed);` **当且仅当** 新参 `stop_on_error: bool`（再加一参——共 6 参）为 true。`pump_direct` 的 Err 分支：`if stop { return Err(PipelineError::Config(format!("event at {}:{} aborted (--on-error stop): {e:#}", job.ev.binlog, job.ev.start_pos))); }`。

- [ ] **Step 2: Runner 装配改造（失败测试先行：库级）**

`tests/flashback.rs`（先把 `tests/e2e.rs` 的 `Synth` 整段平移到 `tests/common/synth.rs` 并 `pub(crate)`，e2e.rs 引用之——平移即改，测试断言零改）：

```rust
// tests/flashback.rs — 骨架，Step 4 补断言
use std::path::Path;

use clap::Parser;
use my2sql_rs::config::{Cli, Command, Config, OnError, WorkType};

/// 复用真实解析/校验路径构造 to-sql Config，再覆写 flashback 三字段。
/// `dir` = e2e 同款 fixture 根（内含 `binlog/` 与 `schema.json`）。
fn cfg_for(dir: &Path, out: &Path, keep: bool, oe: OnError) -> Config {
    let cli = Cli::try_parse_from([
        "my2sql-rs", "to-sql",
        "--binlog-dir", dir.join("binlog").to_str().unwrap(),
        "--start-file", "mysql-bin.000001",
        "--schema-file", dir.join("schema.json").to_str().unwrap(),
        "--output-dir", out.to_str().unwrap(),
    ])
    .expect("cli parse");
    let Command::ToSql(a) = cli.cmd else { panic!("expected to-sql subcommand") };
    let mut c = Config::validate(a).expect("config validate");
    c.work_type = WorkType::Flashback;
    c.keep_trx = keep;
    c.on_error = oe;
    c
}
```

三个用例：
1. `flashback_e2e_multi_trx_bytes`：Synth 产 `BEGIN, W(1), U(1→2), XID, BEGIN, D(3), XID`（表 d.t int pk + varchar）→ `run_flashback` → `flashback.1.sql` 逐字节 = `SET NAMES utf8mb4;\ncommit;\nbegin;\nDELETE FROM `d`.`t` WHERE `id`=3;\ncommit;\nbegin;\nUPDATE `d`.`t` SET … WHERE …;\nINSERT INTO `d`.`t` (`id`,`b`) VALUES (1,'a');\ncommit;\n`（UPDATE 块在前因逆序：trx1 内 U 在 W 后 → 逆序 W 后 U？——块序 = 事件序逆序：D(trx2) | U(trx1) | W(trx1)，trx 变化处注入 → 期望串执行者按此规则手推并钉死）。同时断言隐藏 tmp 已消失。
2. `flashback_on_error_stop_aborts_cleanly`：注入坏事件（fuzz_seed 风格 body 或 table_id 不符）→ `Err(_)`，非零码由 main 负责；断言 out 目录无 `flashback.*.sql` 且无 `.flashback.tmp*`。
3. `flashback_skip_marks_header`：`--on-error skip-bad-event` + 同坏事件 → Ok，summary.errors=1；产物第 2 行 = `-- WARNING: skipped 1 events, positions in stderr`，且该坏事件所在块不出现。
4. `flashback_ddl_excluded_with_summary`：流中夹 `CREATE TABLE` QUERY 事件 → 产物无该语句；`RunSummary` 不变，stderr 断言不做强（tracing），断言 run 成功 + DDL 块不存在即可。

- [ ] **Step 3: 跑红**（run_flashback 不存在）

- [ ] **Step 4: 实现 run_flashback**

`config.rs`：两枚举 + `Config` 三新字段 + `validate()` 构造处补默认（`work_type: WorkType::ToSql, keep_trx: true, on_error: OnError::Skip`）。

`pipeline/mod.rs`：

```rust
/// 写出侧三形态（Runner::emit 的分支点；SQL 两支复用 output::Writer）。
enum Emitter {
    Sql(Writer),
    Flash { tmp: Writer },
}

pub fn run_flashback(cfg: &Config) -> Result<RunSummary, PipelineError> {
    if cfg.output_dir.is_none() {
        return Err(PipelineError::Config(
            "flashback requires --output-dir (reverse pass needs files on disk)".into(),
        ));
    }
    let store = open_store(cfg)?; // 与 run_to_sql 共用抽出的小函数
    let writer = Writer::new(
        cfg.output_dir.clone().unwrap(), false, cfg.file_per_table,
        cfg.add_extra_info, cfg.time_zone, ".flashback.tmp".into(), true,
    );
    let mut st = Runner::new(cfg, Filters::from_config(cfg), store,
        DmlBuilder::flashback(SqlOpts::from_config(cfg)), Emitter::Flash { tmp: writer });
    match st.run_flash() {
        Ok((mut summary, files)) => {
            summary.files = files;
            Ok(summary)
        }
        Err(e) => Err(e), // run_flash 内部已清 tmp（见下）
    }
}
```

`Runner` 字段 `writer: Writer` → `emitter: Emitter`；`emit()` 按分支转发 `write_group`。`run()` 收尾分叉：flashback 形态在 `Emitter::Flash` 的 `finish()`（flush + 文件数）后取 `tmp.blocks().clone()`，`tmp.created()` 序保 `Vec<(tmp, final, blocks)>`（`Writer` 需 `pub fn created(&self) -> &[PathBuf]` 访问器 + blocks 按路径查），调 `reverse::run_files(&jobs, cfg.keep_trx, cfg.threads, warn)`，`files = jobs.len()`；**返回前删除 tmp 计数**（run_files 已删）。`warn = if summary.errors > 0 && cfg.on_error == OnError::SkipBadEvent { Some(format!("-- WARNING: skipped {} events, positions in stderr\n", summary.errors)) } else { None }`。

错误路径清场：`run_flash`（= 通用 `run()` 加 `cleanup_on_err: true`）在任何 `Err` 返回前 `for p in tmp.created() { let _ = fs::remove_file(p); }`（stop 与源级错误同路径；半成品不落盘原则 spec §3.2）。threads==1 直通的 stop 分支在 `pump_direct` 直接 `Err`（Step 1）；并行 stop：`abort` 哨兵在 `reap`/收取循环后 `if stop && abort.load(Relaxed) { drop(job_tx); 收集 join; 清 tmp; return Err(Config("aborted: first error logged to stderr")) }`。

DDL 排除汇总：`prepare()` 非 rows 事件分支（现 return None 前），flashback 形态下若 `RawKind::Query(sql)` 且 sql 关键词 ∉ {begin,commit,rollback,""} → `self.ddl.push((ev.timestamp, ev.binlog.clone(), ev.start_pos, sql.clone()))`；`run()` 收尾 `for (ts, f, pos, s) in &self.ddl { tracing::warn!(binlog=%f, pos, datetime=%datetime_str(*ts, cfg.time_zone), "DDL excluded from rollback script: {s}"); }` 并 `eprintln!("flashback: {} DDL/query events excluded", self.ddl.len())`。

`pump_parallel` 两处 worker 构造与 `pump_direct` 错误分支按 Step 1 新签名接线（to-sql 侧 `stop=false` 恒零改动行为，`abort` 传 `Arc::new(AtomicBool::new(false))` 即可）。

- [ ] **Step 5: 全绿 + 门禁 + 提交** — commit `feat(pipeline): flashback wiring — dml_for dispatch, on-error stop/skip policy, DDL exclusion summary`。

---

### Task 4: stats 模块 + 流水线 stats 通道

**Files:**
- Create: `src/stats/mod.rs`
- Modify: `src/lib.rs`、`src/pipeline/mod.rs`（Emitter::Stats + run_stats + markers 派发）、`src/pipeline/worker.rs`（Out 枚举 + build_out + Job.status）、`src/pipeline/order.rs`（Reorder 泛化）
- Test: 各文件同文件 tests + `tests/stats.rs`

**Interfaces:**
- Consumes: `TrxStatus`、`Filters::accept`、`decode_rows`、`datetime_str`、`Worker` 线程模型。
- Produces（T5/T6/T7 依赖）:
  - `stats::StatFact { binlog, start_pos, end_pos, timestamp, db, table, kind: FactKind(Insert|Update|Delete), rows: u64, trx_id: u64 }`（`Clone, Debug`）
  - `stats::StreamEvent<'a> { Row(&'a StatFact), Begin{binlog:&'a str,pos:u32,ts:u32}, Commit{…}, Rollback{…} }`
  - `stats::Aggregator::new(cfg:&Config, output_dir:&Path) -> Result<Self, std::io::Error>`、`feed(&mut self, ev: &StreamEvent) -> std::io::Result<()>`、`finish(&mut self) -> std::io::Result<StatsSummary { windows: u64, biglong: u64 }>`
  - `worker::Out { Sql(SqlGroup), Fact(StatFact), Status{ binlog: String, pos: u32, ts: u32, status: TrxStatus } }`（Status 仅由 dispatcher 在 stats 形态直推 reorder，不过 worker）；`Reorder<T=SqlGroup>`（默认参，既有测试零改）；通道 `(u64, Vec<Out>)`
  - `pipeline::run_stats(cfg: &Config) -> Result<StatsRun, PipelineError>`，`StatsRun` Display：`stats done: events=…, statements rows=…, windows flushed=…, big/long trx=…, skipped=N`

- [ ] **Step 1: Reorder 泛化 + 通道类型改（纯机械，既有测试全绿即红→绿不单独要求——TDD 豁免登记：类型重构无行为变化，行为由后续 stats 测试钉死）**

`Reorder<T = SqlGroup>`：`buf: HashMap<u64, Vec<T>>`，`push/drain_remaining` 泛型化。worker.rs：

```rust
#[derive(Debug, Clone)]
pub enum Out {
    Sql(SqlGroup),
    Fact(StatFact),
    Status { binlog: String, pos: u32, ts: u32, status: TrxStatus },
}
```

`Job` 增 `status: TrxStatus`（prepare 填 `self.trx.feed` 的第二返回元）。worker_loop/pump_* 的 `Vec<SqlGroup>` 全部换 `Vec<Out>`（build_groups 结果 `.map(|gs| gs.into_iter().map(Out::Sql).collect())`）。`worker_loop` 增 `mode: OutMode`（`enum OutMode { Sql, Stats }`）：Stats → `build_out_stats(job)`（见 Step 3），Sql → 现路径。

- [ ] **Step 2: 失败测试 — Aggregator 字节 golden（无 binlog，纯事件序列）**

`src/stats/mod.rs` tests：喂 12 个 StreamEvent（两窗口 interval=5s、binlog 切换、update 双行、trx 命中 big(rows>=3) 与 long(duration>=1)、rows-before-begin 不判、rollback 收尾、skip 尾注）→ 断言 `binlog_status.txt` / `biglong_trx.txt` **全文件逐字节**（含头行、列宽、下划线 datetime、`[d.t(inserts=2, updates=1, deletes=0)] `明细格式、表名字典序）。JSONL 开关联动断言两 `.jsonl` 行内容（`serde_json::to_string` 手推逐字段）。窗口落盘行序 = **首次出现序**（Go map 随机序的确定性替代，超越项 3）。

- [ ] **Step 3: 实现 Aggregator + build_out_stats**

上游算法逐行照抄（stats_process.go:150-268），要点：`last_print_time==0 → ts+interval`；`ts >= last_print_time` 落盘并重置为 `ts+interval`；binlog 变化落盘清空；`window: Vec<(String /*db.tb*/, BinEventStatsPrint)>` + `HashMap<String, usize>` 索引保首现序；biglong 累加器字段语义与上游一致（`start_time` 取事务内**首个 row 事件** ts；`duration = commit_ts - start_ts`；命中判 `rows >= big || duration >= long`，`>=` 边界照抄）；XID→Commit marker（prepare 已把 Xid 的 `TrxStatus::Commit` 传下）。`--dml` 过滤在 prepare 既有链上生效（Ruling：stats 计数随过滤器；上游一致性风险登记 HANDOVER——裁判对照仅在人工留档中比对，不参与判定）。报表尾行 `# skipped events: {N}`（finish 时写，N 来自 RunSummary.errors——Aggregator::finish 收 `skipped: u64` 参）。JSONL：`--stats-json` 时同数据流双写 `{binlog,starttime,stoptime,startpos,stoppos,inserts,updates,deletes,database,table}` 与 `{…,rows,duration,tables:[{table,inserts,updates,deletes}]}` 两份 `.jsonl`。

`build_out_stats(job: &Job) -> Result<Vec<Out>, SqlError>`：
```rust
match &job.ev.kind {
    RawKind::Rows(kind, v2) => {
        let tm = job.ev.tm.as_deref().ok_or(...)?;
        let rows = decode_rows(&job.ev.body, tm, &job.schema, *kind, *v2)?;
        Ok(vec![Out::Fact(StatFact { binlog: job.ev.binlog.clone(),
            start_pos: job.ev.start_pos, end_pos: job.ev.end_pos,
            timestamp: job.ev.timestamp, db: tm.schema.clone(), table: tm.table.clone(),
            kind: match kind { RowsKind::Write => FactKind::Insert, RowsKind::Update => FactKind::Update, RowsKind::Delete => FactKind::Delete },
            rows: match kind { RowsKind::Update => (rows.len()/2) as u64, _ => rows.len() as u64 },
            trx_id: job.trx_id })])
    }
    // Query/Xid：Out::Fact 空——marker 由 dispatcher 侧直接入 reorder 流（seq 与
    // rows 事件同源编号，保序不破坏），worker 对非 rows 投空批填洞。
    _ => Ok(Vec::new()),
}
```

dispatcher `prepare()`：stats 形态下 **所有** 事件都编号派发（现仅 rows）；非 rows 且 `status ∈ {Begin, Commit, Rollback}` 时不走 worker——dispatcher 直接 `reorder.push(seq, vec![Out::Status{ binlog, pos, ts, status }])`（seq 递增、计数入 summary.events）；worker 只收 rows。**Ruling：Status 事件由 dispatcher 顺序入队**（它是 dispatcher 已有信息，过一遍 worker 纯浪费）。

- [ ] **Step 4: 实现 run_stats + Emitter::Stats**

`run_stats(cfg)`：`Aggregator` 建报表文件（`binlog_status.txt`、`biglong_trx.txt` + 可选 JSONL，`O_TRUNC` 语义=File::create 天然）；Runner 装配 `Emitter::Stats(agg)`；threads==1/并行两路同一 `Out` 通道（pump 层 `OutMode::Stats`）；`emit`：`Out::Fact → feed(Row)`、`Out::Status{binlog,pos,ts,status} → feed(Begin/Commit/Rollback)`。finish 后 `StatsRun { summary, windows, biglong }`。**stop 语义**：stats 默认 skip（Config::validate_stats 定）；显式 `--on-error stop` 复用 T3 哨兵链。

- [ ] **Step 5: 失败测试 — run_stats 端到端**（`tests/stats.rs`）：Step 2 场景的 Synth binlog → `run_stats` → 两报表逐字节 = 与 Step 2 golden 同一字符串（跨层一致钉死）；`--stats-json` 双件产出；threads=1/4 输出等字节。

- [ ] **Step 6: 全绿 + 门禁 + 提交** — commit `feat(stats): lightweight fact stream + aggregator, upstream-format reports with jsonl` 与 `refactor(pipeline): generalize reorder/worker payload to Out for stats channel`（可两提交）。

---

### Task 5: CLI 三子命令（config 重构 + main dispatch）

**Files:**
- Modify: `src/config.rs`（主体重构）、`src/main.rs`、`tests/cli.rs`
- Test: `src/config.rs` 内 tests（迁移+新增）、`tests/cli.rs`

**Interfaces:**
- Consumes: T3/T4 的 `WorkType/OnError/run_flashback/run_stats` 与 Config 字段。
- Produces: `Command::{ToSql(ToSqlArgs), Flashback(FlashbackArgs), Stats(StatsArgs)}`；`Config::validate_to_sql/validate_flashback/validate_stats`（`validate` 保留为 `validate_to_sql` 的 `#[deprecated]`-free 直重命名，全部既有调用点改名）；`Cli::from_args() -> Config`（按子命令填 `work_type` 等）。

- [ ] **Step 1: 失败测试 — clap 解析面**（config.rs tests 重写既有 `fn args()` 走 `Command::ToSql`，新增：）

```rust
fn fargs(extra: &[&str]) -> FlashbackArgs { /* try_parse_from(["my2sql-rs","flashback","--binlog-dir","/d","--start-file","f.000001","--schema-file","/s", ...extra]) */ }

#[test]
fn flashback_defaults_and_flags() {
    let c = Config::validate_flashback(fargs(&[])).unwrap();
    assert_eq!((c.work_type, c.on_error, c.keep_trx), (WorkType::Flashback, OnError::Stop, true));
    let c = Config::validate_flashback(fargs(&["--no-keep-trx"])).unwrap();
    assert!(!c.keep_trx);
    let c = Config::validate_flashback(fargs(&["--on-error", "skip-bad-event"])).unwrap();
    assert_eq!(c.on_error, OnError::SkipBadEvent);
    // --to-stdout 不存在于 flashback：try_parse 必失败
    assert!(Cli::try_parse_from(["x", "flashback", "--binlog-dir", "/d", "--start-file", "f", "--to-stdout"]).is_err());
    assert!(Config::validate_flashback(fargs(&["--keep-trx", "--no-keep-trx"])).is_err());
}

#[test]
fn stats_flags_and_ranges() {
    let c = Config::validate_stats(sargs(&[])).unwrap();
    assert_eq!((c.work_type, c.on_error, c.print_interval, c.big_trx_rows, c.long_trx_seconds),
               (WorkType::Stats, OnError::SkipBadEvent, 30, 10, 1));
    assert!(Config::validate_stats(sargs(&["--print-interval", "601"])).is_err());
    assert!(Config::validate_stats(sargs(&["--big-trx-rows", "30001"])).is_err());
    assert!(Config::validate_stats(sargs(&["--long-trx-seconds", "3601"])).is_err());
    assert_eq!(Config::validate_stats(sargs(&["--long-trx-seconds", "0"])).unwrap().long_trx_seconds, 0);
    assert!(Cli::try_parse_from(["x", "stats", "--binlog-dir", "/d", "--start-file", "f", "--full-columns"]).is_err());
}

#[test]
fn to_sql_gains_default_on_error_skip() {
    let c = Config::validate_to_sql(args(&[])).unwrap(); // 既有 helper
    assert_eq!((c.work_type, c.on_error), (WorkType::ToSql, OnError::SkipBadEvent));
}
```

- [ ] **Step 2: 实现 config 重构** — `CommonArgs`（binlog_dir…threads 20 旗标，含 `--output-dir`；**不含** --to-stdout）、`SqlTextArgs`（add_extra_info/no_db_prefix/full_columns/unique_key_first/ignore_primary_key_for_insert/strict_schema/insert_batch）flatten 进 ToSql（+to_stdout、+`--on-error` 默认 `skip-bad-event`）与 Flashback（+`--keep-trx`/`--no-keep-trx` 双 bool 互斥校验、+`--on-error` 默认 `stop`）；StatsArgs = Common + `--print-interval/--big-trx-rows/--long-trx-seconds`（clap `value_parser=(Range 或自写 check fn)`，**范围校验在 validate_stats 里做**，clap 只做类型 u32）+ `--stats-json`。共享校验抽 `fn build_common(c: &CommonArgs) -> Result<Config, String>`（现 validate 的 threads/schema 源/时区/时间对/位点逻辑原样搬入），三 `validate_*` 各调它再叠自身。`Config` 新字段：`print_interval: u32, big_trx_rows: u32, long_trx_seconds: u32, stats_json: bool, work_type, keep_trx, on_error`（to-sql 路径给中性默认）。

- [ ] **Step 3: main.rs dispatch**

```rust
let cfg = Config::from_args();
tracing_subscriber::fmt::init();
let rc = match cfg.work_type {
    WorkType::ToSql => run_to_sql(&cfg).map(|s| println!("{s}")),
    WorkType::Flashback => run_flashback(&cfg).map(|s| println!("{s}")),
    WorkType::Stats => run_stats(&cfg).map(|s| println!("{s}")),
};
match rc { Ok(()) => {}, Err(e) => { eprintln!("error: {e}"); exit(1) } }
```

`from_args` 按 `Command` 分派三 validate。`RunSummary` Display 文案 `to-sql done:` 前缀按 work_type 打印（字段 `pub work: WorkType` 入 summary 或 Display 收参——采 Display 参：`summary.display_with("flashback done:")`，main 传串；**to-sql 输出文案不得变**，P1 e2e 有断言者查）。

- [ ] **Step 4: tests/cli.rs 冒烟** — 既有 `--help` 测试外补：`flashback --help`、`stats --help` exit 0 且 stdout 含 `--keep-trx` / `--print-interval`。

- [ ] **Step 5: 全绿 + 门禁 + 提交** — commit `feat(cli): flashback + stats subcommands with upstream-derived flag defaults`。

---

### Task 6: 真件 e2e + 一次性正逆对账脚本

**Files:**
- Modify: `tests/e2e.rs`（新增 flashback/stats 两节，输入 = `tests/fixtures/capture_8.0_rows`）
- Create: `tools/flashback-reconcile.sh`

**Interfaces:**
- Consumes: 三子命令全链（CLI 真二进制 `target/debug/my2sql-rs`）。
- Produces: 可重跑的活库对账脚本（人工触发，不入 CI/Makefile 默认目标）。

- [ ] **Step 1: 真件 flashback e2e** — 用 `capture_8.0_rows` fixture（P1 已有矩阵件）跑 `run_flashback`（schema 走 `tests/fixtures/*/schema.json`——无则 `--schema-dump` 先产）：断言 ① exit Ok；② 产物字节结构（首注 SET NAMES、`begin;` 计数 = 事务段数、尾 `commit;`、tmp 无残留）；③ 语句级对账：正序 to-sql 产物的每条 `INSERT/UPDATE/DELETE` 经镜像映射（ins↔del 互换、upd 的 SET/WHERE 互换后按比较器同款规范化）能在逆序产物中一一配对（集合相等，multiset）。
- [ ] **Step 2: 真件 stats e2e** — 同 fixture `run_stats`：`binlog_status.txt` 各行 inserts/updates/deletes 总和 == to-sql 产物语句计数（独立推导，防 stats 通道自身漏计）；biglong 报表存在且 ≥0 行。
- [ ] **Step 3: `tools/flashback-reconcile.sh`（spec §5.5 一次性语义验证）** — 容器 mysql:8.0：建 `rec_db.rec_t`（pk id, val varchar, 预置 100 行提交）→ `FLUSH BINARY LOGS` 记起点 → 灌混合 DML（insert 7/update 5/delete 3 + 多行事务 + **故意含一条 JSON 列 update**）→ 记终点 → `docker exec mysqldump --no-data` 出 schema → 宿主跑 `flashback`（在线 uri）→ 产物进容器执行 → `CHECKSUM TABLE` 与基线（DML 前 dump 出的校验值）比对，不等则红。脚本头注释明示「人工触发；数据只进本脚本自建的 rec_db，退出前 DROP DATABASE 自清理」。
- [ ] **Step 4: 跑一次对账 + 记录** — 执行脚本（本任务内一次性），输出贴 HANDOVER 节点（含耗时与 CHECKSUM 值）；失败即修到绿（这是 P2 语义正确性的总闸）。
- [ ] **Step 5: 全绿 + 门禁 + 提交** — commit `test(p2): real-capture flashback/stats e2e + live reverse-apply reconciliation script`。

---

### Task 7: 差分 harness WORK_TYPE 维度 + 比较器 rollback 规则

**Files:**
- Modify: `tools/run-difftest.sh`、`tools/comparator/compare.py`、`tools/comparator/selftest.py`、`tools/difftest-allowlist.txt`

**Interfaces:**
- Consumes: 双方 rollback 产物（裁判 `rollback.{N}.sql` 无 scaffold/注释漂尾；我方 `flashback.{N}.sql` 有 scaffold/记录原子）。
- Produces: `WORK_TYPE=rollback make difftest` 全绿链路 + `WORK_TYPE=stats` 冒烟链路。

- [ ] **Step 1: compare.py rollback 模式**（第三个可选参 `rollback`）：
  - `load(d, mode)`：rollback 下先按 `*.sql`（glob 天然跳过隐藏 `.flashback*`/tmp）读行；**跳过** `commit;`/`begin;`/`-- WARNING`/`SET NAMES` 行；A 侧注释行**后随**于其语句 → 解析改为「语句缓冲 + 注释行到达时绑定 key」；B 侧注释先行（现逻辑）。
  - 结构断言（B 侧，逐文件）：`begin;` 行数 == `commit;` 行数 - 1 == 事务段数；每 `begin;` 前一非空行为 `commit;`；文件末行 == `commit;`。违反 → 计红并打印文件名（白名单不吞结构）。
  - 值比较复用现 `veq`/`seteq` 全链（组内 multiset 配对本已序不敏感；ALW-JSON-IN-SET 已覆盖上游逆向 SET 多 JSON 项）。
- [ ] **Step 2: selftest.py 组 9** — 正反例：scaffold 剥离不误吞真 DELETE（`DELETE ... commit;` 不可能同值）、结构断言红例（缺尾 commit / begin 前非 commit / 计数不配）、A 侧注释漂尾绑定 vs B 侧原子记录同 key 判绿、rollback UPDATE 镜像对（正向 vs 逆向文本不同形但各自组内自洽）不跨组误配。
- [ ] **Step 3: run-difftest.sh** — `WORK_TYPE="${WORK_TYPE:-2sql}"`：步骤 4 裁判旗标 `-work-type` 映射 `2sql→2sql / rollback→rollback`；步骤 5 我方子命令映射 `to-sql→to-sql / rollback→flashback`（flashback 无 `--to-stdout`，其余参数同）；步骤 6 `python3 compare.py go rs $([ $WORK_TYPE = rollback ] && echo rollback)`；步骤 7 离线回放对 rollback 同样跑（`--schema-file` + `diff -r`）；产物目录后缀 `-rb`。`WORK_TYPE=stats`：步骤 4/6 跳过裁判比较，改为步骤 5'= `stats` 冒烟（两报表存在 + 总和 == to-sql 语句数断言，python -c 内联）+ 裁判 stats 输出留档 `$OUT/go-stats/`（人工对照，不参与退出码）。
- [ ] **Step 4: 白名单登记**（tools/difftest-allowlist.txt）：新增条目——记录原子化（注释位序差异）、scaffold 行剥离+结构断言、SET NAMES/头文件（沿用）、`-- WARNING` 头行、tmp 命名差异（产物名 `flashback.` vs `rollback.`——按名匹配非比较项）。
- [ ] **Step 5: 全矩阵实跑** — `WORK_TYPE=rollback make difftest`（8.0）exit 0；`make difftest`（2sql 回归）exit 0；输出计数（groups/green/red）如实贴 HANDOVER 节点。
- [ ] **Step 6: 提交** — commit `test(difftest): WORK_TYPE=rollback|stats dimension + comparator rollback parsing & structural asserts`。

---

### Task 8: 兼容矩阵扩展（flashback×4 + stats×2）

**Files:**
- Modify: `tools/compat-matrix.sh`、`docs/compat/matrix.md`、`Makefile`（如有 target 变量则透传）

- [ ] **Step 1: 矩阵脚本** — 现有 8 用例（to-sql 族）之后追加：`flashback-5.6 / flashback-5.7 / flashback-8.0 / flashback-8.4`（复用该版本已捕获 binlog：同 datadir 续跑 `WORK_TYPE=rollback` 的裁判+我方+比较器三件套——若脚本结构是调 run-difftest.sh 则直接 `WORK_TYPE=rollback VER=…` 追加行；若内联则抽函数复用）+ `stats-5.6 / stats-8.0`（我方 stats 跑通 + 总和一致性内联 python 断言；裁判 stats 输出仅留档）。TSV 增列/增行沿现格式，`results.tsv` 行数如实。
- [ ] **Step 2: 全量跑** — `make compat`（或等价入口）完整执行一次；任何失败红→修→整矩阵重跑（P1 纪律）。
- [ ] **Step 3: matrix.md 落表** — 新用例行（版本、日期、commit、结果、排除项说明：stats 5.7/8.4 有意不跑 = spec §3.6 省时裁决）；更新总计数文案，禁「9/9 式」虚账。
- [ ] **Step 4: 提交** — commit `test(compat): flashback on 5.6-8.4 + stats on 5.6/8.0 green`。

---

### Task 9: 文档收口 + DoD 对账 + 性能回归闸

**Files:**
- Modify: `README.md`、`docs/HANDOVER.md`、`docs/bench/p1.md`（或新增 p2 小节）

- [ ] **Step 1: bench 回归（DoD-4）** — 若 `out/bench-file-to-sql` 件在盘：`cargo bench --bench decode` 复跑 to-sql 基线（T4 通道泛化是唯一嫌疑改动），与 108.9 MB/s 对比，回归 >5% 即停、回报为 finding；产物不在 → 重跑 `tools/gen-bench-binlog.sh`（10 分钟级）或显式记录「未跑原因」。数字如实入 docs/bench。
- [ ] **Step 2: README** — 特性矩阵加 flashback/stats 两行；快速上手补两子命令实跑过例句；上游差异清单追加（rollback→flashback 命名、记录原子化、keep-trx 默认开+开关、stats 表序确定性、`-- WARNING` 头、DDL 排除策略、stats 不做裁判差分）。
- [ ] **Step 3: HANDOVER** — P2 各任务节点（含 T6 对账实录、T8 矩阵计数、终审前置状态）、挂账清单更新（musl 性能、ENUM>255 测试债等 P1 存量保留，新增 P2 挂账：stats 尾注格式为自定口径、`--dml` 过滤与上游 stats 计数的一致性未对裁判验证）。
- [ ] **Step 4: DoD 对账（spec §6）** — 逐条 1-5 附证据（命令 + 输出摘要），写入 HANDOVER「P2 DoD」节。
- [ ] **Step 5: 三门 + 提交 + 推送** — `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`；commit `docs(p2): readme + handover + bench regression, DoD accounting`；`git push origin main`（含挂起的 c9fa852 与本计划提交）。

---

## 验收清单（整分支终审前自查）

- [ ] `WORK_TYPE=rollback make difftest` exit 0（含结构断言全绿）
- [ ] `make compat` 全绿（10 to-sql/既有 + 4 flashback + 2 stats），matrix.md 计数如实
- [ ] flashback 三 hard 规则 + 默认 stop + skip 告警链各有测试（spec §6.2）
- [ ] keep-trx 注入逐字节 golden（含上游头部注入 quirk）独立于差分存在（spec §6.3）
- [ ] stats 双报表 + JSONL golden 绿；to-sql bench 无 >5% 回归（spec §6.4）
- [ ] reference/ 零改动（`git log -p -- reference/ | head` 为空）
- [ ] clippy -D / fmt / test 三门 + 每任务 HANDOVER 节点齐
