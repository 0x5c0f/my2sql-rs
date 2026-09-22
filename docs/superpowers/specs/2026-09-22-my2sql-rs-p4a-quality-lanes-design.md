# my2sql-rs P4a「质量并行面」设计（fuzz 接入 / 影子库回放 / 矩阵列缺口 / 多版本 idle 心跳）

**日期:** 2026-09-22 · **基线:** main@2149ce1（v0.3.0-p3）
**上游依据:** 主 spec `2026-09-20-my2sql-rust-design.md` §9 P4 行的**质量侧子集**；
性能调优/结构搬运/具名时区明确**不在本役**（P4b 或挂账续留）。

## 0. 裁决与授权

用户 2026-09-22 裁决：「按建议进行」+ **并行硬要求**——任务间无必要关联性时
必须以并行子代理派发。本役任务图按此设计：A/B/C/D 四 lane 文件面两两不相交，
自计划起即可 4 路同刻派发；仅 T5（合流）串行。

## 1. Lane A — cargo-fuzz 正式接入

- **形态:** 仓库根新增 `fuzz/`（cargo-fuzz 独立 workspace；根 `Cargo.toml`
  `exclude = ["fuzz"]`，主构建/三门/矩阵**零感知**）。
- **靶面（两枚）:**
  1. `decode_event`——单事件字节 → `parse_header`→`strip_checksum`→
     type 路由 `parse_table_map`/`decode_rows`（rows 靶配固定合成 schema 组）。
  2. `event_stream`——多事件拼接流：逐事件走靶 1 路径并喂 `TrxStateMachine`
     口径的头部循环（含 CRC 有无两态），钉「流级不 panic / Err-不-panic」。
- **语料:** 起点 = `tests/fuzz_seed/*.bin`（4 件，回归闸已在 `tests/fuzz_seed.rs`）
  + `fixtures/events.rs` 构造器确定产出的合法基线事件（禁随机入仓）。
- **接入判据:** ①`cargo +nightly fuzz build` 过；②300s×2 靶真跑（经本 lane 新建
  `tools/fuzz-min.sh` 直调 cargo-fuzz，`-artifact_prefix` 落 out/fuzz/）**0 新
  crash**——`make fuzz-min` 包装行由 T5 落（Makefile 单写者）；③任一 crash
  → 最小化入库为 `tests/fuzz_seed/` 新语料 + TDD 红钉修复（见 §6 解码器开闸条款）。
- **环境事实:** stable 1.80+/nightly 1.100 在册；cargo-fuzz 已安装核验（兜底通道
  = `libfuzzer-sys` + 手写 `RUSTFLAGS=-Zsanitizer=fuzzer`，仅核验失败时启用）。

## 2. Lane B — 影子库端到端回放（前向闸升格）

P2 `tools/flashback-reconcile.sh`（逆灌→CHECKSUM==基线，一次性人工件）升格为
`tools/shadow-replay.sh [VER] [KEEP=1]`，**三段全走**：

1. **前向:** 活库灌混合 DML（复用 `p3e2e_seed_schema`/`p3e2e_feed_mixed` 多表
   含 JSON/BLOB/中文）→ 同窗 to-sql 产物 → 灌**影子库**（同 schema 同基线快照
   恢复的独立库）→ `CHECKSUM TABLE` 逐表 + mysqldump 行级 diff == 主库后态。
2. **逆向:** 主库后态再建影子 clone → 灌 flashback 产物 → == 前态
   （吸收 P2 件语义，升为多表混合 DML）。
3. **往返:** 前向影子再逆灌 → 回前态（三角验证，钉产物自洽）。
- **判据:** 三向 checksum 等 + 行级 diff 空；证据（两 CHECKSUM 值、计数、
  产物快照 out/shadow-replay/）逐字入 HANDOVER。主件 8.0，`[5.7]` 冒烟一次。
- 纪律沿 P2：人工/make 触发、数据只进自建容器、trap 自清、KEEP 排障。

## 3. Lane C — 测试债三列形真机捕获（终审登记项）

`tools/run-difftest.sh` 捕获面扩 `p4a` 表组（seed SQL 追加三表）：
1. **ENUM >255 成员**（2B packlen；现仅合成单测面）；
2. **GEOMETRY**（POINT/LINESTRING/POLYGON 非空值；裁决 7 字节保真路径首次实抓）；
3. **LONGBLOB >64KB**（4B prefix + 跨页 payload）。

真机捕获 → Go 裁判差分（8.0 主件）；上游 Go 对该 shape 不支持/崩 → 差异**如实
登记** matrix.md/README 差异清单，我方改走「自 roundtrip」断言（to-sql→灌回→
CHECKSUM，借 Lane B 通道），**不改解码器、不弱化比较器**。解码器若真红 → §6。
矩阵既有 18 件不因表组变动而重跑（捕获差分件独立于 compat 家族）；登记挂账勾销。

## 4. Lane D — 5.6/5.7 idle 窗心跳帧形（T0 挂账残余半面）

`tests/repl.rs` 参数化既有 `repl_idle_60s_no_false_drop` 形状新增两 live 件
（`repl_idle_heartbeat_5_7` / `_5_6`，VER 门容器）：静默 60s 中心跳帧在场、
无假断链、随后灌流**追平**（产物 ≡ 事后 file 模式同窗）。帧形观测（type 0x1b
头位口径）逐版本入注记；5.6 若不支持 heartbeat 变量（SET 报错回退口径）→ 按
T0 事实处理并矩阵注记。lib 面改动**只许本 lane**（防碰撞）。

## 5. T5 合流 lane（串行收尾）

Makefile 新增 `fuzz-min` / `shadow-test` 目标行（分别包装 T1 的
`tools/fuzz-min.sh` 与 T2 的 `tools/shadow-replay.sh`；目标行集中在此落，
防 Makefile 三路并写）；README 矩阵行 fuzz ✅ / 影子库回放 ✅ +
差异登记续号；HANDOVER T1–T4 节点 + **P4a DoD 对账**；全量回归实跑入档：
三门 + `make compat` 18 + `make repl-test` 11 + `make fuzz-min` + `make shadow-test`。

## 6. 纪律与红线

- **解码器开闸条款（替代 P3 冻结）:** `src/binlog/` 允许改动，但**每处改动**
  须红钉测试先行 + 事后全量差分回归（既有 18 件 + 349 非 live 测试 + P1 字节面 e2e）
  绿才可入；无改动则 DoD 如实记「零改动」。
- 恒守：`reference/` 只读、禁虚账（计数/时长/crash 数逐字）、每任务三门、
  HANDOVER 节点、并行实现者独立 `CARGO_TARGET_DIR=/tmp/p4a-<lane>`。
- 测试基线滚动：合流前全量 = 非 live ≥349、live repl 11、live 件增者如实计数。

## 7. 任务图（执行编排）

```
T1(A fuzz)   ─┐
T2(B shadow) ─┼─▶ T5(合流: Makefile/README/HANDOVER/全量回归) ─▶ 终审
T3(C 列形)   ─┤      （四 lane 并行派发，评审随各 lane 完成即行）
T4(D idle)   ─┘
```

文件面互斥登记：T1=fuzz/ + 根 Cargo.toml(exclude)；T2=tools/shadow-replay.sh；
T3=tools/run-difftest.sh + tools/gen-data*.sql（捕获 seed 面）；T4=tests/repl.rs +
tools/repl-e2e-lib.sh。Makefile/README/HANDOVER 由 T5 独占。
