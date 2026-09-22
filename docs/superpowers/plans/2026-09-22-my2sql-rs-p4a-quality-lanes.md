# my2sql-rs P4a「质量并行面」Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 按 spec 落四条互不相交的质量 lane（fuzz 正式接入 / 影子库三段回放 / 三列形真机捕获 / 5.6-5.7 idle 心跳件），再经 T5 合流闸统一收口。

**Architecture:** T1–T4 文件面两两不相交、可 4 路并行派发（各自独立 worktree + 独立 `CARGO_TARGET_DIR=/tmp/p4a-<lane>`）；T5 是唯一串行合流者，独占 Makefile / README / docs/HANDOVER.md 写权。四 lane 交付的命令本体（`tools/fuzz-min.sh`、`tools/shadow-replay.sh`）由脚本自身直通，T5 只补 `make` 包装行。

**Tech Stack:** Rust 2024 edition（stable 1.80+ / nightly 1.100 + cargo-fuzz 在册）、bash + docker（mysql:5.6/5.7/8.0/8.4 全本地镜像）、python3（comparator 既有）、Go 裁判 `reference/my2sql-go/`（只读）。

**Spec:** `docs/superpowers/specs/2026-09-22-my2sql-rs-p4a-quality-lanes-design.md`（同 worktree，commit 1e68345）

## Global Constraints

- 基线 = main@2149ce1（v0.3.0-p3）；非 live 测试 **≥349 全绿**、live repl 家族 11 件、compat 矩阵 18/18 为滚动回归底线（§spec 6）。
- `reference/` 只读；禁虚账——计数/时长/crash 数/bytes 逐字来自真实 artifact；每任务收口前跑三门（`cargo test` / `cargo clippy --all-targets -- -D warnings` / `cargo fmt --check`）。
- **解码器开闸条款（替代 P3 冻结）**：`src/binlog/` 允许改动，但每处改动须红钉测试先行 + 事后全量差分回归（既有 18 件 + 349 非 live + P1 字节面 e2e）绿才可入；无改动则报告如实记「零改动」。
- 弱化比较器 / 加白名单消音差异 = 禁区；Go 裁判不支持的 shape → 如实登记 + 我方自 roundtrip，不改比较语义。
- 破坏性文件/容器操作仅限 `out/`、`data/<ver>/`、自建 `p3e2e-*` / `my2sql-*` 容器；live 脚本/件必带 trap 清场，KEEP 排障通道保留。
- 并行实现者用独立 `CARGO_TARGET_DIR`（lane 名后缀），禁共享主 target 目录。
- 文件互斥（违例即评审打回）：T1=`fuzz/**`+根`Cargo.toml`+`tools/fuzz-min.sh`+`tests/fuzz_seed.rs` 仅 pub 化；T2=`tools/shadow-replay.sh`；T3=`tools/run-difftest.sh`+`tools/gen-data-p4a.sql`+`tools/p4a-roundtrip.sh`；T4=`tests/repl.rs`+`tools/repl-e2e-lib.sh`（**只做加性改动，禁改既有函数签名**，T2 并发 source 它）；T5=Makefile/README.md/docs/HANDOVER.md/docs/matrix 注记行。

---

### Task T1 (Lane A): cargo-fuzz 正式接入 — fuzz/ 独立 workspace + 两靶 + tools/fuzz-min.sh

**Files:**
- Create: `fuzz/Cargo.toml`, `fuzz/fuzz_targets/decode_event.rs`, `fuzz/fuzz_targets/event_stream.rs`, `fuzz/src/bin/seedgen.rs`, `tools/fuzz-min.sh`
- Modify: 根 `Cargo.toml`（加 `[workspace]` 表）, `tests/fuzz_seed.rs`（**仅**给 4 个 seed builder + `event_bytes`/`tm_body`/`rows_head` 加 `pub`，零逻辑改动）
- Corpus: `fuzz/corpus/decode_event/*`, `fuzz/corpus/event_stream/*`（入库）

**Interfaces:**
- Consumes: `my2sql_rs::binlog::{event::{parse_header,strip_checksum,EVENT_HEADER_SIZE}, table_map::parse_table_map, rows::{decode_rows,RowsKind}}`；`my2sql_rs::pipeline::source::{TrxStateMachine, RawEvent, RawKind}`（pub 面，若路径私有先核 `src/lib.rs`/`src/pipeline/mod.rs` re-export，缺则加 `pub use`——属开闸例外面，报告单列）。
- Produces: `bash tools/fuzz-min.sh`（退出码 0=0 新 crash；证据落 `out/fuzz/<target>/`）；T5 的 `make fuzz-min` 直通本脚本。

- [ ] **Step 1: 依赖面核验（不写码）**

```bash
cargo +nightly fuzz --version          # 已装；失败 → 报告 BLOCKED 走 spec §1 兜底通道
grep -n 'pub mod\|pub use' src/pipeline/mod.rs src/pipeline/source.rs | head
```
确认 `TrxStateMachine`/`RawEvent`/`RawKind` 可从 crate 外路径可达；不可达则在本 crate `src/pipeline/mod.rs` 补 `pub use`（最小 re-export，无行为改动）。

- [ ] **Step 2: fuzz/ 包脚手架 + 根 Cargo.toml**

根 `Cargo.toml` 末尾（`[profile]` 表前亦可）加：

```toml
[workspace]
members = ["."]
exclude = ["fuzz"]
```

`fuzz/Cargo.toml`：

```toml
[package]
name = "fuzz-targets"
version = "0.0.0"
edition = "2024"
publish = false

[[bin]]
name = "decode_event"
path = "fuzz_targets/decode_event.rs"
test = false
doc = false

[[bin]]
name = "event_stream"
path = "fuzz_targets/event_stream.rs"
test = false
doc = false

[[bin]]
name = "seedgen"
path = "src/bin/seedgen.rs"
test = false
doc = false

[dependencies]
libfuzzer-sys = "0.4"
my2sql-rs = { path = ".." }

[workspace]
members = ["."]
```

- [ ] **Step 3: 靶 1 `decode_event.rs`（单事件）**

要点（完整实现由本任务自证）：
- `fuzz_target!(|data: &[u8]| { ... })`，**不 catch_unwind**（panic = crash 信号，正是靶目的）。
- 首字节 `data[0] & 1` 作 `with_crc` 选择器并剥掉首字节 → 余下当单事件流：`parse_header` → 尺寸自洽闸（`size < EVENT_HEADER_SIZE || size > buf.len()` → return）→ `strip_checksum` → 按 `h.event_type.0` 路由：19 → `parse_table_map`；20/21/22/23/24/25/30/31/32 → `decode_rows`（对固定合成 tm：先 `parse_table_map(合法 tm 字节)` 得 `TableMapEvent`，schema 固定 2×INT）；其余类型仅要求 header 解析不 panic。
- **陷阱钉死**：任何 schema 查找路径**禁止 panic**（`tests/fuzz_seed.rs` 的 `schema_for` 对未知表 `panic!` —— fuzz 语境是假阳性 crash 源；本靶用固定合成表，不走那张表）。

- [ ] **Step 4: 靶 2 `event_stream.rs`（多事件流 + 状态机）**

以 `tests/fuzz_seed.rs:run_decode_layer` 为口径在靶内重写走查器（fuzz 包无法 import 主 crate tests/；~40 行同构可接受，注释指回口径源）：逐事件 `parse_header`→`strip_checksum(body, with_crc)`→`parse_table_map`/`decode_rows`；`with_crc` 取首字节选择器（两态都跑）；遇 QUERY(2)/XID_EVENT(15,16)/BEGIN 语义体则构造 `RawEvent{kind: RawKind::Query(text)｜Xid｜…}` 喂 `TrxStateMachine::feed`，断言 `trx_id` 不回退（`assert!` 触发即 crash 报告）。表→schema 映射默认 = 2×INT 合成（同样禁 panic）。

- [ ] **Step 5: `seedgen.rs` + 种子语料入库**

`tests/fuzz_seed.rs` 只加 `pub`（4 个 `seed_*` + `event_bytes`/`tm_body`/`rows_head`/`jsonb_wrapped`）；`fuzz/src/bin/seedgen.rs` 以 `#[path = "../../../tests/fuzz_seed.rs"] mod seedsrc;` 复用 builder，产出两组语料写 argv 目录：
1. 4 枚畸形种子（原样）；
2. **合法基线事件** ≥3 枚：`event_bytes(TABLE_MAP_TYPE, tm_body(7,"fz","t_zero_bm",&[3,3],0,&[],&[0]))`、其后跟一条合法 WRITE_ROWS_V2（present=0 的空行区版即 seed2 的 tm 件 + 只含 tm 的事件）、seed1 的 tm 修全形（声明 4B 实给 4B）。
运行并入库：

```bash
cargo run --manifest-path fuzz/Cargo.toml --bin seedgen -- fuzz/corpus/decode_event fuzz/corpus/event_stream
```

- [ ] **Step 6: 构建闸（红→绿）**

```bash
cd fuzz && cargo +nightly fuzz build   # 预期 PASS；首 build 全量重编，耐心
```
主仓回归（确认根 [workspace] 化零破坏）：

```bash
cd .. && cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
```
预期 349+ 全绿。**若 fuzz build 失败**：按 spec §1 兜底 = 弃 cargo-fuzz 清单，手写 `[[bin]]` + `RUSTFLAGS="-Zsanitizer=fuzzer"` + `libfuzzer-sys` 直驱，脚本改调 `cargo +nightly run`；在报告登记。

- [ ] **Step 7: `tools/fuzz-min.sh`**

```bash
#!/usr/bin/env bash
# P4a T1：fuzz 最小闸 —— 每靶 300s×1 轮，crash 归零判据（spec §1）。
set -euo pipefail
cd "$(dirname "$0")/.."
TIME="${FUZZ_TIME:-300}"
for t in decode_event event_stream; do
  mkdir -p "out/fuzz/$t"
  (cd fuzz && timeout $((TIME + 240)) cargo +nightly fuzz run "$t" \
     "corpus/$t" -- "-max_total_time=$TIME" "-artifact_prefix=$(cd .. && pwd)/out/fuzz/$t/" \
     ) > "../out/fuzz/$t/run.log" 2>&1 &
done
wait
n=0
for t in decode_event event_stream; do
  c=$(find "out/fuzz/$t" -maxdepth 1 -type f -name 'crash-*' | wc -l)
  echo "[fuzz-min] $t: crashes=$c"; n=$((n + c))
done
[ "$n" = 0 ] && { echo "[fuzz-min] OK 0 new crashes"; exit 0; }
echo "[fuzz-min] FAIL $n crash artifact(s)"; exit 1
```
（子 shell 内 `../out` 相对路径以脚本实测为准修正——目标是 crash 必落 `out/fuzz/<t>/` 可数位。）
**crash 分支规程**：任一 crash → `cargo +nightly fuzz tmin` 最小化 → 新增为 `tests/fuzz_seed/` 语料（builder + SEEDS 注册 + 红钉 Err/panic 断言，先红后绿）→ 若需动 `src/binlog/` 按 Global 开闸条款走全量回归。0 crash 则如实记 transcript。

- [ ] **Step 8: 真跑 + 证据**

```bash
bash tools/fuzz-min.sh 2>&1 | tee out/fuzz/fuzz-min-run.log
```
记录：两靶 run.log 末行（execs/sec、time）、crash 计数 0/2、`out/fuzz/` 树。

- [ ] **Step 9: 提交**

```bash
git add -A fuzz Cargo.toml tests/fuzz_seed.rs tools/fuzz-min.sh
git commit -m "feat(p4a-fuzz): cargo-fuzz hookup — fuzz/ workspace, decode_event + event_stream targets, deterministic corpus, tools/fuzz-min.sh 300s gate"
```

### Task T2 (Lane B): tools/shadow-replay.sh — 影子库三段闸

**Files:**
- Create: `tools/shadow-replay.sh`
- Read-only consumes: `tools/repl-e2e-lib.sh`（source）、`tools/flashback-reconcile.sh`（P2 母本形态）、`target/debug/my2sql-rs`

**Interfaces:**
- Consumes: `p3e2e_container_start/wait_healthy/seed_schema/feed_mixed/master_pos/capture_binlogs/container_stop`、`p3e2e_sql`（既有签名，T4 保证加性稳定）。
- Produces: `bash tools/shadow-replay.sh [VER]` 退出码 0 = 三向全过；证据目录 `out/shadow-replay/`；T5 `make shadow-test` 直通。

- [ ] **Step 1: 骨架（沿 P2 母本纪律）**

```bash
#!/usr/bin/env bash
# P4a T2（spec §2）：影子库端到端三段闸 —— 前向(to-sql→影子==主后态) /
# 逆向(flashback→影子clone==前态) / 往返(前向影子逆灌==前态)。
# 人工/make 触发；容器 p3e2e-<ver>-p4ash<slug>-<pid> 自建自清(trap)，KEEP=1 失败保留。
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"; VER="${1:-8.0}"
source tools/repl-e2e-lib.sh
OUT="$ROOT/out/shadow-replay"; rm -rf "$OUT" && mkdir -p "$OUT"
SFX="p4ash$$"; p3e2e_container_start "$VER" "$SFX"
CTR="$P3E2E_CTR"; PORT="$P3E2E_PORT"; trap 'p3e2e_container_stop "$CTR"' EXIT INT TERM
DB=p4ashadow
p3e2e_seed_schema "$CTR" "$DB"
p3e2e_feed_mixed "$CTR" "$DB" 3 P0        # 基线前态
```

- [ ] **Step 2: 前态/后态快照件**

`snap <tag>` 函数：`docker exec "$CTR" mysqldump -uroot --no-tablespaces --skip-dump-date --default-character-set=utf8mb4 "$DB" > "$OUT/<tag>.sql"` + 逐表 `CHECKSUM TABLE` → `$OUT/<tag>.checksum`。表清单 = `SELECT TABLE_NAME FROM information_schema.TABLES WHERE TABLE_SCHEMA='<db>' ORDER BY 1`（钉序）。

- [ ] **Step 3: 前向闸**

记 `(f0,p0)=p3e2e_master_pos` → `p3e2e_feed_mixed "$CTR" "$DB" 3 P1` → `(f1,p1)` → `p3e2e_capture_binlogs "$CTR" "$f0" "$f1" "$OUT/binlog"` → `cargo build --quiet` → 在线 schema `to-sql --binlog-dir "$OUT/binlog" --start-file "$f0" --start-pos "$p0" --uri "mysql://root@127.0.0.1:$PORT" --time-zone +00:00 --output-dir "$OUT/fwd" --schema-dump "$OUT/schema.json"`（窗口尾界：产物含 'P1doc1'、不含后续）。
影子库 = `CREATE DATABASE <db>_fwd` + 前态 schema-only dump 重建 + **前态数据**灌入（`P0.sql` 改库名或用 `--databases` 形，自择实现但保证影子起点==前态）→ 灌 `$OUT/fwd/*.sql` → `snap FWD_SHADOW` → 硬断言：FWD_SHADOW.checksum 逐表 == 主库后态 `P1` snap；行级 `diff` 空（mysqldump 归一口径同上）。

- [ ] **Step 4: 逆向闸 + 往返闸**

- 逆向：主库后态 snap 后建 `<db>_rev`（= 后态数据起点）→ `flashback --binlog-dir "$OUT/binlog" --start-file "$f0" --start-pos "$p0" --schema-file "$OUT/schema.json" --time-zone +00:00 --output-dir "$OUT/rev"`（P2 通道原样）→ 灌 `<db>_rev` → snap REV_SHADOW == **前态** P0。
- 往返：对 `<db>_fwd`（前向影子）灌同一份 `$OUT/rev/*.sql` → snap RT_SHADOW == 前态 P0。
- 计数证据：三段各打 `tables=<n> checksum-equal=<n>` + 事务行数（产物 `grep -c '^INSERT \|^UPDATE \|^DELETE '` 逐字）。

- [ ] **Step 5: 负自检（闸的验钞机，SHADOW_NEGCHECK=1）**

正向三段全绿后，`SHADOW_NEGCHECK=1` 模式重走前向：灌 fwd 产物后、比对前，对影子库施一行 `UPDATE <db>_fwd.<t_ord 首表> SET qty = qty + 1 WHERE <pk 最小行>` → 脚本必须以非零退出 + `NEGCHECK: expected mismatch observed` 字样（即 checksum 闸能抓到漂移）。此模式**成功=FAIL、失败=OK**，由脚本内反转退出码，日志入 `$OUT/negcheck.log`。

- [ ] **Step 6: 真跑 8.0 主件 + 5.7 冒烟**

```bash
bash tools/shadow-replay.sh 8.0 2>&1 | tee out/shadow-replay-8.0.log
SHADOW_NEGCHECK=1 bash tools/shadow-replay.sh 8.0 2>&1 | tee out/shadow-replay-negcheck.log
bash tools/shadow-replay.sh 5.7 2>&1 | tee out/shadow-replay-5.7.log
```
5.7 面 seed 版本自适应（无 JSON 列降 LONGTEXT）由 lib 既有探测承担；若 5.7 首跑暴露 lib 缺陷 → 报告登记（**不改 lib**，归 T4/T5 仲裁）。

- [ ] **Step 7: 三门 + 提交**

```bash
cargo build --quiet && cargo test && git add tools/shadow-replay.sh && git commit -m "test(p4a-shadow): shadow-replay three-way gate — forward to-sql==master-after, reverse flashback==master-before, roundtrip self-consistency + negcheck self-test"
```

### Task T3 (Lane C): run-difftest P4A 表组 — ENUM>255 / GEOMETRY / LONGBLOB>64K 真机捕获

**Files:**
- Create: `tools/gen-data-p4a.sql`, `tools/p4a-roundtrip.sh`
- Modify: `tools/run-difftest.sh`（加 `P4A=1` env 分支：GEN 选择 + OUT 后缀 `-p4a`；不动既有路径）

**Interfaces:**
- Consumes: 既有 run-difftest 七步流程、Go 裁判、`tools/comparator/compare.py`（语义不变）。
- Produces: `P4A=1 bash tools/run-difftest.sh`（8.0 单点件）+ `out/difftest-8.0-p4a/FINDINGS.md`（T5 抄 README/差异登记唯一来源）。

- [ ] **Step 1: 三表 seed（tools/gen-data-p4a.sql）**

```sql
SET SESSION sql_mode='';  -- 与主 gen-data 同口径（以 tools/gen-data.sql 头部实际为准）
CREATE DATABASE IF NOT EXISTS p4a;
-- 形 1：ENUM 2B packlen（成员数 300 > 255）
CREATE TABLE p4a.t_enum_wide (
  id INT NOT NULL PRIMARY KEY,
  e ENUM('e0','e1', ... 'e299') NOT NULL,   -- 300 成员逐字展开为字面量：成员清单用 python3 一行生成
      -- （range(300) 拼 `'e%d'`）后粘进 SQL；SQL 文件内禁循环、禁省略号
  n ENUM('e0',...) NULL
);
-- 覆盖 255 边界：e254/e255/e299/e0 + NULL 行
INSERT ... ; UPDATE ...; DELETE ...;
-- 形 2：GEOMETRY 三型非空（8.0 允许 NOT NULL）+ SRID 探针
CREATE TABLE p4a.t_geom (
  id INT NOT NULL PRIMARY KEY,
  p POINT NOT NULL, l LINESTRING NOT NULL, g POLYGON NOT NULL,
  p4326 POINT NOT NULL SRID 4326
);
INSERT INTO p4a.t_geom VALUES
 (1, ST_GeomFromText('POINT(1.5 -2.25)'), ST_GeomFromText('LINESTRING(0 0,1 1,2 0)'),
      ST_GeomFromText('POLYGON((0 0,10 0,10 10,0 10,0 0))'), ST_GeomFromText('POINT(3 4)',4326)), ...;
-- 形 3：LONGBLOB >64KB（4B prefix + 跨页 payload，确定性 REPEAT 而非大文件）
CREATE TABLE p4a.t_blob (id INT NOT NULL PRIMARY KEY, data LONGBLOB NOT NULL);
INSERT INTO p4a.t_blob VALUES
 (1, REPEAT(CONVERT(x'41424344' USING binary), 17500)),          -- 70,000 B
 (2, REPEAT(CONVERT(x'00FF55AA' USING binary), 70000)),           -- 210,000 B 跨页
 (3, x'00');
```
（`REPEAT` 参数形态以真机执行为准微调；目标是行事件 payload >64KB 且值完全确定。5.6 无 `ST_*`? ——本件仅 8.0 主闸，5.6/5.7 矩阵面不在本 lane。）

- [ ] **Step 2: run-difftest.sh P4A 分支**

在 `GEN=` 选择处与 OUT 后缀处加最小分支：

```bash
P4A="${P4A:-}"
[ -n "$P4A" ] && { GEN="tools/gen-data-p4a.sql"; SFX="$SFX-p4a"; }
```
其余六步原样继承（含步骤 7 离线回放）。产物目录 `out/difftest-8.0-p4a`。

- [ ] **Step 3: 真跑差分并分流**

```bash
P4A=1 VER=8.0 bash tools/run-difftest.sh 2>&1 | tee out/difftest-8.0-p4a-run.log
```
- 全绿 → FINDINGS.md 记「三形 Go 裁判差分逐字节一致」+ 各表语句计数。
- 裁判 FAILED/panic/输出坏值 → **逐表二分**（临时 seed 只留单表复跑，登记 shape 归属），违例形改道：其余表照常差分，违例表用 Step 4 自 roundtrip 断言；FINDINGS.md 逐字登记 Go 侧行为（报错文本/坏产物片段），**比较器与解码器都不动**。
- 若**我方** panic/Err → 真缺陷，走开闸条款 TDD：从 `$OUT` binlog 里定位该事件的原始字节（`tools/event-census.py` / docker cp 后 python 走读），先写 `src/binlog` 红钉单测（复现字节内联为 fixture）→ 修绿 → 全量回归（18 件 + 349 + e2e）；同字节顺手登记 `tests/fuzz_seed/` 语料（builder + SEEDS，走其 regen 通道）。

- [ ] **Step 4: tools/p4a-roundtrip.sh（违例形自证通道，借 Lane B 思路独立成件）**

单库内：前态 = 空表（seed 后即 FLUSH，窗口 = 全 p4a 库）→ 我方 to-sql 产物灌 `_clone` 库 → `CHECKSUM TABLE` == 主库 + 行级 dump diff。输入 = Step 3 已 docker cp/或 `data/8.0` binlog（difftest 容器保留窗口内可复用：脚本自带起容器+灌 seed 的独立生命周期，**不依赖 run-difftest 容器**，形态照 T2 骨架）。只在 Step 3 出现裁判违例形时必跑；全绿时也跑一次作双保险（成本 ~2min）。

- [ ] **Step 5: FINDINGS.md 收口 + 三门 + 提交**

FINDINGS.md 必含：三形 × {Go 支持?, 差分, roundtrip} 矩阵 + 逐字计数 + 挂账勾销注（终审登记的三列形缺口 → 本件实抓）。既有 18 件矩阵**不因此重跑**（本件独立于 compat 家族，报告注明）。

```bash
cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
git add tools/gen-data-p4a.sql tools/run-difftest.sh tools/p4a-roundtrip.sh out/difftest-8.0-p4a/FINDINGS.md 2>/dev/null || git add tools/gen-data-p4a.sql tools/run-difftest.sh tools/p4a-roundtrip.sh
git commit -m "test(p4a-cols): difftest P4A table-group — ENUM>255 2B packlen / GEOMETRY byte-fidelity / LONGBLOB>64K cross-page capture + findings"
```
（FINDINGS.md 若 `.gitignore` 挡 out/ → 复制入 `docs/p4a-findings.md` 提交，T5 引用。）

### Task T4 (Lane D): 5.7/5.6 idle 窗心跳帧形 live 件

**Files:**
- Modify: `tests/repl.rs`（`Bt::new_pinned` 版本参数化 + 两新件）, `tools/repl-e2e-lib.sh`（**仅加性**）

**Interfaces:**
- Consumes: 既有 `repl_idle_60s_no_false_drop`（tests/repl.rs:2166）全形态、`Bt`（:800）、`libf/libf_try`、`live_gate`、`sid()`、`blocks_text/dir_blocks/raw_has/wait_bounded/read_cp`。
- Produces: `repl_idle_heartbeat_5_7`、`repl_idle_heartbeat_5_6`（#[ignore] live 件，T5 `make repl-test` 家族 11→13）；`Bt::new_ver(slug, ver)`（`Bt::new` 签名不变 = 8.0）。

- [ ] **Step 1: Bt 版本参数化（加性）**

`new_pinned(slug, hostport)` → 内部改走 `new_ver_pinned(slug, ver, hostport)`；容器名 `p3e2e-{ver_with_dashes}-{sfx}`、`p3e2e_container_start {ver} ...`。`new()`/`new_pinned()` 原签名委托 `ver="8.0"`。root 临时目录前缀带 slug 不变。

- [ ] **Step 2: 两新件 = idle 形状逐段移植 + 版本差异面**

以 2166 行为模板写 `repl_idle_heartbeat_5_7()`（容器 5.7，db 名 `p4aidl57`）：
- 60s 静默验活、F 段追平、`repl done errors=0`、零 `reconnect #`、终档 == Flast 提交界 —— 全保留。
- **追平判据升格**（spec §4）：静默窗后 `p3e2e_capture_binlogs` 取窗 binlog → file 模式同窗 `to-sql` 基准 → `blocks_text` 序列对账（T6a 切片段同款比较，非逐字节文件比较——版本 server 端注释/DDL 噪音容差按既有切片件的口径）。
- **帧形观测注记**：跑前 `bt.sql("SHOW GLOBAL VARIABLES LIKE 'binlog_heartbeat%'")`（5.6 无此变量 → `sql_soft` 容忍失败并注记「5.6 面：心跳周期纯客户端 COM_BINLOG_DUMP 载荷，SET 通道不存在」）；心跳帧在场证据 = 60s 静默零假断链（既有负断言）+ 若 ReplSource 有 unknown-type/heartbeat trace 面（grep `0x1b\|27.*heartbeat\|HEARTBEAT` src/repl/）则把观测行转录进测试 println，**不新增生产日志**。
- `_5_6` 同构（db `p4aidl56`，容器 5.6）：预期差异面 —— 5.6 seed 走 LONGTEXT 降级（lib 既有探测）、无 GTID 变量；若 5.6 心跳路径不支持（假断链爆）→ 按 T0 事实处理：件改「静默窗缩到读超时内 + 注记不支持」？ **不**——spec 裁定 60s 静默必须仍过：5.6 由 fake rotate 续命亦算帧形，断言只锁「零 reconnect + 追平」，帧形注记写实际观测。首跑红则报 BLOCKED 附 transcript。

- [ ] **Step 3: 定向真跑**

```bash
make repl-test MY2SQL_TEST_ARGS="-- repl_idle_heartbeat --exact --nocapture" ...
```
——无此透传口子则直接：
```bash
bash -c 'set -euo pipefail; source tools/repl-e2e-lib.sh; trap p3e2e_container_stop EXIT; p3e2e_container_start 8.0; export MY2SQL_TEST_URI=$P3E2E_URI MY2SQL_TEST_CTR=$P3E2E_CTR; cargo test --test repl -- --ignored --nocapture --test-threads=1 repl_idle_heartbeat'
```
（共享 8.0 容器只作 live 门；两件各自 Bt 专属容器。）两件全绿 + 注记抄入报告。

- [ ] **Step 4: 家族回归 + 三门 + 提交**

```bash
cargo test --test repl                      # 非 ignored 面全绿
cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
git add tests/repl.rs tools/repl-e2e-lib.sh
git commit -m "test(p4a-idle): 5.6/5.7 idle-window heartbeat live cases (Bt version-param, additive lib contract)"
```

### Task T5 (Merge lane, 串行): Makefile/README/HANDOVER 收口 + 全量回归

**Files:**
- Modify: `Makefile`, `README.md`, `docs/HANDOVER.md`（T5 独占写权）
- Consumes: 四 lane 合入后的 worktree-feat-p4a tip（由控制方 ff 合并后派发）

**Interfaces:**
- Produces: `make fuzz-min`、`make shadow-test [VER=…]`、`make difftest P4A=1` 文档行；P4a DoD 对账 + 全量回归台账。

- [ ] **Step 1: Makefile 包装行（直通脚本本体，零逻辑）**

```make
## P4a T1/T2 入口（脚本本体归各 lane；env 透传：FUZZ_TIME/SHADOW_NEGCHECK/VER）
fuzz-min:
	bash tools/fuzz-min.sh

shadow-test:
	bash tools/shadow-replay.sh $${VER:-8.0}
```
`.PHONY` 行追加两目标。

- [ ] **Step 2: README**：矩阵行 fuzz ✅（`make fuzz-min`，两靶 300s 0 crash + 日期）、影子库三段 ✅（`make shadow-test`）、三列形差分/差异登记（从 FINDINGS 逐字抄，续既有编号）、repl live 家族 13 件。

- [ ] **Step 3: HANDOVER**：T1–T4 节点（各 lane 报告逐字事实：crash 数、三向 checksum 值、帧形观测、FINDINGS 行）+ P4a DoD 对账（spec §1–§5 判据逐条 ✅/挂账）+ 挂账清单勾改（三列形缺口销账；5.6 心跳事实面若有注记新增）。

- [ ] **Step 4: 全量回归实跑（逐字入档）**

```bash
cargo test                                   # ≥349 + 本役新增非 live，如实计数
cargo clippy --all-targets -- -D warnings && cargo fmt --check
make fuzz-min
make shadow-test            # 8.0 复跑
make difftest                                 # 既有 8.0 主差分不回归
P4A=1 make difftest         # 或等效 env 行
make compat                 # 18/18 复跑
make repl-test              # 13 件（11 + 心跳两件）
```
任何红 → 定位 lane 归属，lane 面修复重跑，禁放宽判据。

- [ ] **Step 5: 提交**

```bash
git add Makefile README.md docs/HANDOVER.md
git commit -m "docs(p4a): merge lane — make fuzz-min/shadow-test, README matrix, HANDOVER nodes + P4a DoD + full regression transcript"
```

---

## 执行编排（控制方职责，非任务）

1. `sdd-workspace` 本计划 → 台账首行 plan 路径 → 冲突扫描表（四 lane 文件面互斥自证 + 根 Cargo.toml 为 T1 单写；repl-e2e-lib.sh T4 加性契约）。
2. **单条消息并行派发 T1–T4**（各 `isolation:"worktree"`，`CARGO_TARGET_DIR=/tmp/p4a-{fuzz,shadow,cols,idle}`）。
3. 各 lane DONE → 并行 task reviewer（brief/report/review-package 三输入，BASE=派发前记录 SHA）。
4. 修复轮 ≤3 恢复原 implementer，≥4 换强模型；终裁按 P3 先例。
5. 四 lane 全过后 ff 合并进 worktree-feat-p4a → 派 T5 → whole-branch 终审（requesting-code-review 模板）→ 一轮修复 → finishing（ff main + tag v0.4.0-p4a + push，总授权自决）。
