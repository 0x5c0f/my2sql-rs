# my2sql-rs P2 设计：flashback + stats

> 前置：本设计是 `2026-09-20-my2sql-rust-design.md`（总 spec）§5.3/§6/§7 的
> 落地细化，只写 P2 增量；P1 已交付件（解码层、file 事件源、trx 状态机、
> 并行流水线、sqlopen、差分基建、兼容矩阵）全部复用，不再重述。
> 执行序：flashback 先行、stats 随后，同一计划、互不阻塞。

**Goal:** 为 my2sql-rs 增加两个 work 能力：`flashback`（回滚 SQL，逆向恢复）
与 `stats`(DML 统计 + 大/长事务识别)，并在 Go 裁判差分与 5.6–8.4 兼容矩阵
下验证。

## 1. 范围

做：
- `my2sql-rs flashback`（file 模式，对应上游 `-work-type rollback`）
- `my2sql-rs stats`（对应上游 `-work-type stats`）
- 二者进入 `make difftest`（新增 WORK_TYPE 维度）与 `make compat` 矩阵

不做（明确出界）：
- DDL 回滚 / `--apply` 直接执行（总 spec 既定）
- repl 模式下的 flashback/stats（P3）
- statement 格式 binlog、MariaDB

## 2. 上游事实（reference/my2sql-go，实读为证）

1. 逆向 DML（base/sqlgen.go 的 `ifRollback` 旗标）：
   INSERT 事件→`DELETE`（用 after-image）；DELETE 事件→`INSERT`（用
   before-image）；UPDATE 事件→UPDATE（SET=before 值，WHERE=after 值）。
2. 文件逆序（base/rollback_process.go）：正序生成写 tmp 文件 + **每事件
   块**（每 rows 事件组一条，非每事务）`(字节长, trx_id)` 索引
   （events.go:216,259）；收尾 N 线程从尾向前按块 seek+read，块内按行
   逆序写出；`keepTrx` 且事务 id 变化处注入 `commit;\nbegin;\n`——因
   `lastTrxIdx` 初值 0，**首个写出块必注入**（头部悬空 `commit;` 为上游
   原样行为），文件尾补 `commit;\n`；tmp 删除；产物 `rollback.<N>.sql`。
   注意上游按**裸行**逆序——extra-info 注释行与其 SQL 行在逆序后拆对。
3. 列数不匹配（binlog 列 > 表结构列）在 rollback 路径直接 `log.Fatalf`
   （base/events.go:87，"usually means DDL in the middle"）——上游对回滚
   完整性是硬失败立场。
4. stats（base/context.go + file.go）：`binlog_status.txt`（窗口×表）、
   `biglong_trx.txt`；默认值 PrintInterval=30、BigTrxRowLimit=10、
   LongTrxSeconds=1（范围 1..600 / 1..30000 / 0..3600）。
5. 上游 KeepTrx 无 CLI 旗标绑定（struct 字段 + 默认值），我们补上开关。

## 3. 设计决策

### 3.1 语义反转与顺序反转分离（核心架构）

- **语义反转**在 sqlopen 层：`DmlBuilder` 增 `WorkKind::{ToSql, Flashback}`。
  Flashback 下 insert→`DELETE ... WHERE <after-image>`、delete→
  `INSERT ... VALUES <before-image>`、update→`UPDATE SET <before> WHERE
  <after>`。SET 差异列规则、key→全列 WHERE 级联、quote/escape、
  insert_batch 全部复用 P1 路径（同宽同纪律）。
  **不继承**上游 JSON 恒进 SET 的 quirk——逆向同样按实际 diff。
- **顺序反转**在新独立组件 `src/flashback/reverse.rs`：正序产物（含
  extra-info 记录对）先落隐藏 tmp 文件，收尾按 `(块长, trx_id)` 索引并行
  从尾回读。**记录（注释行+SQL 行）为原子单元逆序**——修正上游裸行逆序
  的拆对缺陷（超越项，差分白名单登记）。
- 流水线其余零改动：dispatcher/reorder 仍正序，只是 Writer 侧新增
  「正序写 tmp」形态。这保证 P1 的线程模型、反压、panic  containment
  原样成立。

### 3.2 flashback 完整性立场（Ruling：终审建议 A 的延伸）

回滚脚本的每个洞都会静默丢数据，故：
- `--on-error` 默认 **stop**（任何解码/校验错误：中止、非零退出、删除
  tmp 不落半成品）。`skip-bad-event` 可显式使用：继续但产物头部注入
  `-- WARNING: skipped N events, positions in stderr`，结束再打印计数。
- **坏输入即坏回滚**三 hard 规则（对齐上游 fail-hard 立场并显式化）：
  a) `Align::Padded`（结构已删列）→ 该表事件报错（旧镜像不完整）；
  b) `ColumnValue::Missing`（MINIMAL row image）参与逆向 WHERE/VALUES →
  报错，提示需 `binlog_row_image=FULL`；
  c) 无 PK/UK 表 **允许** 全列 WHERE（与 P1 to-sql 一致），但 flashback
  下 dropped 列不可进 WHERE → 由 a) 已拦截。
- DDL/Query 事件：不进回滚脚本；stderr 汇总告警（datetime+位点+原文），
  由用户决策——工具绝不猜反向 DDL。

### 3.3 keep-trx

`--keep-trx` 默认开（上游无旗标但行为同此）：事务边界注入与文件尾
`commit;\n` 逐字节对齐上游 rollback_process.go；`--no-keep-trx` 则纯逆序
无脚手架。事务分组依据 = P1 trx 状态机既有 trx_id，零新机制。

### 3.4 stats 通道

- 独立子命令（**不**作为 to-sql/flashback 的旁路副作用——YAGNI，避免
  每模式双写路径）。数据源：worker 解码后只上送轻量事实
  `(binlog, start/stop_pos, ts, db, tb, kind, rows, trx_id, trx_status)`，
  专用 stats 线程（单线程即可，聚合非瓶颈）消费。
- 产物：`binlog_status.txt`（窗口×表：inserts/updates/deletes 行数 +
  起止时间/位点，binlog 切换即落盘）、`biglong_trx.txt`（begin~commit
  聚合，行数≥`--big-trx-rows`(默认10) 或时长≥`--long-trx-seconds`(默认1)
  落盘，含每表明细）、`--stats-json` 追加两份 JSONL。参数范围沿上游
  （1..600 / 1..30000 / 0..3600），`--print-interval` 默认 30。
- stats 的 `--on-error` 默认 **skip+count**（分析工具，坏事件计数进报
  表尾注）。

### 3.5 CLI（clap，沿用 P1 通用旗标组）

```
my2sql-rs flashback [通用: --dir/--file/--stop-file/--stop-pos/
  --start/stop-datetime, --db/--tbl 黑白名单, --uri|--schema-file,
  --threads, --output-dir, --file-per-table, --extra-info,
  --insert-batch, --ignore-pk-for-insert, --unique-first, --time-zone]
  [--keep-trx/--no-keep-trx] [--on-error stop|skip-bad-event(默认 stop)]

my2sql-rs stats [通用同上（除 SQL 文本类旗标：--insert-batch/
  --ignore-pk-for-insert/--unique-first/--extra-info 不适用）]
  [--print-interval 30] [--big-trx-rows 10] [--long-trx-seconds 1]
  [--stats-json] [--on-error stop|skip-bad-event(默认 skip)]
```
（`to-sql` 的 `--on-error` 维持 P1 默认 skip 不变——正向少一条仍可用。）

> 勘误（R12）：上列 stats 草图的 `[--on-error stop|skip-bad-event(默认 skip)]` **撤回**——发货 CLI 不在 stats 暴露 `--on-error` 旗标（`StatsArgs` 无此字段，`validate_stats` 恒 `SkipBadEvent`，见 HANDOVER §T5 裁定）；库级 `Stop` 语义保留且已测（tests/stats.rs 用例 5 `stats_e2e_on_error_stop_escalates`）。

### 3.6 差分与兼容矩阵扩展

- `run-difftest.sh` 增 `WORK_TYPE={2sql|rollback|stats}` 维度：裁判
  `-work-type $WORK_TYPE`，本侧对应子命令。rollback 差分对齐口径：
  比较器新增脚手架行剥离规则（`commit;`/`begin;` 行不进值比较，另设
  结构断言：keep-trx 下 `begin;` 行数 = 事务段数 K、`commit;` 行数 =
  K+1、每个 `begin;` 的前一行必为 `commit;`、文件末行为 `commit;`
  ——上游 `lastTrxIdx` 初值 0，首段亦注入，头部悬空 `commit;` 是上游
  原样行为，逐字节对齐；防注入逻辑被白名单吞掉）；extra-info 对齐键不变（startpos/stoppos 与
  正反序无关，组内多重集配对本已序不敏感）。
- stats 不做逐字节裁判差分（上游报表为自由文本 + 窗口聚合语义受
  print-interval/落盘时机影响）：以 8.0 矩阵数据 + 手算 golden 报表断言
  为准（每窗口行数/每表计数/大事务命中集均可从 gen-data.sql 静态推出），
  golden 进 tests/。裁判 stats 输出仅作人工对照参考留档 out/。
- `make compat`：flashback 用例全 4 版本跑（逆序件依赖解码层跨版本正确
  性）；stats 只跑 5.6 + 8.0（聚合逻辑与服务器版本无关，省一半时长）。

## 4. 模块与文件布局（增量）

```
src/sqlopen/dml.rs        + WorkKind，三 builder 的 image 反转分支
src/flashback/mod.rs      (新) tmp 文件写手 + (块长,trx_id) 索引
src/flashback/reverse.rs  (新) 并行逆序回读 + keep-trx 注入 + 原子记录对
src/stats/mod.rs          (新) 轻量事实流 → 窗口/事务聚合 → 两报表(+JSONL)
src/pipeline/mod.rs       Runner 装配按 work_type 接 flashback/stats 支路
src/config.rs             两子命令 + 旗标组（默认值按 §3.5）
tools/run-difftest.sh     WORK_TYPE 维度 + 比较器脚手架剥离/selftest 组
tools/comparator/         rollback 对齐规则 + 反例 selftest
docs/（README 矩阵更新、HANDOVER 节点制随执行推进）
```

## 5. 测试策略（TDD，红字为新增纪律）

1. sqlopen 反转单测：三事件 × {键表/无键/UK-only} × {NULL/边界值}，
   断言逆向 SQL 逐字节（fixture 复用 tests/fixtures/events.rs）。
2. reverse.rs 文件级单测：多事务 tmp → 期望最终字节（含 keep-trx 注入、
   注释+SQL 原子对、多块边界）；threads=1..8 输出逐字节相等。
3. 完整性三 hard 规则各有「必须报错」单测（Padded/Missing/DDL 告警）。
4. stats golden：从 gen-data.sql 静态推导窗口计数与大/长事务命中，
   断言两份报表 + JSONL；skip 事件计数进尾注。
5. e2e：真 8.0 矩阵 binlog 一次 flashback 全链路，产物逆序正确 + 用
   P1 的 to-sql 正向跑同一 binlog 再**人工核对一组逆向对账**（正逆各
   执行一次进临时表、diff 为空——进脚本，一次性验证语义正确性，非 CI
   常驻）。
6. 差分：`WORK_TYPE=rollback make difftest` 全绿；矩阵扩展见 §3.6。
7. 门禁不变：test/clippy -D/fmt + HANDOVER 每节点更新。

## 6. P2 完成定义（DoD）

1. `WORK_TYPE=rollback make difftest` exit 0（8.0 全矩阵语义对齐 +
   结构断言）；`make compat` 含 flashback 4 版本 + stats 2 版本全绿。
2. 完整性立场落地：三 hard 规则 + 默认 stop + skip 告警链路有测试。
3. keep-trx 注入逐字节对齐上游（差分外的独立 golden 测试）。
4. stats 双报表 + JSONL golden 绿；轻量事实流不加流水线延迟
   （bench 复跑 to-sql 数字无回归 >5%）。
5. README/兼容矩阵/HANDOVER 更新；P1→P2 行为差异入上游差异清单。
