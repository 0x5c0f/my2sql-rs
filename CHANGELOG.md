# 更新日志

本文件记录 my2sql-rs 的里程碑级变更。权威细节（逐字回归台账、白名单全文、
测量口径与陷阱）在 [docs/HANDOVER.md](docs/HANDOVER.md) 与
`docs/bench/*.md`；本文件只收录带出处的结论级数字（spec D6「禁新造数」：
`docs/superpowers/specs/2026-09-22-my2sql-rs-p5-release-design.md` §3 D6）。

## v0.5.0 — 发布面（2026-09-22）

- 版本真身对齐：包版本 0.1.0 → **0.5.0**，git tag `v0.5.0`（无战役后缀）；
  历史里程碑 tag `v0.1.0-p1`…`v0.4.1-p4b`（共五枚，均在册）保留不动，
  此后 git tag 与包版本紧耦合（出处 docs/superpowers/specs/2026-09-22-my2sql-rs-p5-release-design.md
  §3 D1；`Cargo.toml` version 单源，spec §3 D2）
- CI 门禁上线：ci.yml 门 = fmt / clippy `-D warnings`（--all-targets）/
  `cargo test --no-fail-fast`（live 件 `#[ignore]` 自然跳过）/ musl release
  编译门（只编译不运行）/ fuzz 靶编译门（runner 无 nightly 则如实降级为不门）
  （出处 同上 spec §3 D3）。**有界决定**：difftest / compat / repl-test（live）/
  fuzz（真跑）/ shadow 全部**不进 CI**——本地六闸体系为权威门禁，
  「CI 绿 ≠ 六闸绿」，六闸逐字台账见 docs/HANDOVER.md「P4b 任务节点日志」T5
  节点与「P4a 任务节点日志」T5 节点（出处 spec §3 D3）
- 发布面：GitHub Release 双目标产物
  `my2sql-rs-0.5.0-x86_64-unknown-linux-gnu`（glibc 动态）/
  `my2sql-rs-0.5.0-x86_64-unknown-linux-musl`（musl 静态单二进制）+
  `SHA256SUMS`（本次随 v0.5.0 落地；产物命名与 Release 链见 spec §3 D4/D5，
  发布与下载回验在 P5 T4 收口兑现）
- 已知限制（口径同 docs/HANDOVER.md「遗留/挂账清单」）：
  - repl `--uri` 不提供 TLS（README 行为差异 26；spike 实测 `ssl-mode`
    直接 `Unknown URL parameter` 硬错，出处 docs/HANDOVER.md「P3 Task 0」节点）
  - stats 失败 run 会清上一份好 JSONL（drop-on-error：create 即 O_TRUNC +
    Drop unlink；报表以最后一次**成功** run 为准，出处 docs/HANDOVER.md
    「P3 T8 新增挂账」）
  - DDL 回滚 / `--apply` 直写库 / MariaDB / 8.0.1 default_metadata 明确不做
    （关键决策 D5，出处 docs/HANDOVER.md「关键决策记录」表）

## v0.4.1-p4b — 性能面（2026-09-22）

- mimalloc 全局分配器（本役唯一合入的 src/ 性能改动）：glibc 端到端 A/B
  显著快 **−26.561%**（median 5.9613 s → 4.3779 s；出处 docs/bench/p4b.md ④）
- musl 静态吞吐悬崖消账（挂账 #3）：base-musl **3.3 MiB/s** →
  mimalloc-musl **64.2 MiB/s**（threads=8，≥50 门通过；3.4 MiB/s 悬崖原账见
  docs/bench/p1.md musl 表；出处 docs/bench/p4b.md ④）
- criterion 权威新基线：threads=8 median 4.1451 s → **127.59 MiB/s**；
  回归闸 vs P1 真值 **103.85 MiB/s** → **+22.86%（更快）→ GREEN**；
  DoD-3 绝对门槛（threads=8 ≥40 MB/s）以 **3.3×** 余量通过
  （出处 docs/bench/p4b.md ②，P1 基线 docs/bench/p1.md 结果表）
- bench 判定工装上线：`tools/bench-ab.sh`（median+MAD 显著性判定）+
  `tools/bench-profile.sh`（threads 曲线普查），`make bench-ab` /
  `make bench-profile` 直通（出处 docs/bench/p4b.md ①，
  docs/HANDOVER.md「P4b DoD 对账」1/2）
- 挂账 #7 钉死：P1→P2 代码增量端到端 A/B delta **+2.812%**（0.1897 s）
  < 阈值 0.5714 s → not-significant，N=5 不升级（出处 docs/bench/p4b.md ③）
- assembly 搬运（挂账 #5）：`src/pipeline/mod.rs` **3473 → 1540 行** +
  新建 `src/repl/assembly.rs` **1949 行**，move-only 逐字节硬证 +
  A/A' 抽测 Δ**+1.2%** 无退化（出处 docs/bench/p4b.md ⑥，
  docs/HANDOVER.md「P4b DoD 对账」5）

## v0.4.0-p4a — 质量并行面（2026-09-22）

- cargo-fuzz 正式闸（`make fuzz-min`）：两靶 decode_event / event_stream
  各 300 s 真跑 **0 新 crash**（合流轮逐字 `Done 22296408` / `Done 6456918`）；
  起点语料 = seedgen 确定性 **40 件**（禁随机入仓，与仓内 `fuzz/corpus/`
  逐字节等）（出处 docs/HANDOVER.md「P4a 任务节点日志」T5 节点 GATE 2 +
  「P4a DoD 对账」§1）
- 实抓 **2 发**解码器真 panic（table_map.rs:79 / :299 add-with-overflow）→
  TDD 红钉先行 → `checked_add` 修复；第 3 处闸 read_lns 为预防闸定向单测
  独扛——「解码器开闸条款」首启用（出处 docs/HANDOVER.md「P4a T1」节点）
- 影子库三段闸（`make shadow-test`）：前向/逆向/往返五闸全绿 +
  逐表 CHECKSUM + 行级 diff 双腿 + `SHADOW_NEGCHECK=1` 验钞机禁假绿；
  8.0 spec 原形态锚 + 5.7 REF-clone 锚裁定入册（JSON checksum 上游非定值，
  豁免仅 checksum 腿）（出处 docs/HANDOVER.md「P4a DoD 对账」§2 + T5 节点 GATE 3）
- difftest P4A 三列形真机捕获（`P4A=1 make difftest`，仅 8.0）：
  ENUM>255 / GEOMETRY / LONGBLOB>64K 三形全 Go 裁判支持，
  `groups A=14 B=14 aligned=14 green=14 red=0`；测试债「三列形覆盖缺口」
  销账，无新增行为差异（出处 docs/p4a-findings.md 裁判比较器行，
  docs/HANDOVER.md「P4a DoD 对账」§3）
- 5.6/5.7 idle 心跳 live 件两件：repl live 件家族 **11 → 13**；
  心跳帧形实测钉死（HEARTBEAT v1 0x1b / ts=0 / size=39 / 20s 节奏）
  （出处 docs/HANDOVER.md「P4a T4」节点）
- 合流全量回归六闸全绿：三门 `cargo test` **350 passed / 0 failed** +
  `make repl-test` **13 passed / 0 failed / 823.02s**
  （出处 docs/HANDOVER.md「P4a T5」节点 GATE 1/GATE 6）

## v0.3.0-p3 — repl 模式（2026-09-21）

- repl（伪装 replica 拉流，to-sql 流式形态）交付：功能超集四件 =
  事务边界 checkpoint + `--resume-file` 断点接续 / 指数退避自动重连 /
  心跳探活 / resume 防覆盖闸（出处 docs/HANDOVER.md「当前进度」P3 条目；
  语义细节 = README 行为差异 23–27）
- **等价性总闸**：repl 与 file 模式同 binlog 段产出逐字节一致
  （8.0 主件 `to_sql.3.sql` 11016 B == 11016 B、sha256 同值；
  出处 docs/HANDOVER.md「P3 DoD 对账」1）
- live 套件：`make repl-test` T6b 轮 3 **10 passed / 0 failed / 476.76s**；
  终审合流轮 **11 passed / 0 failed / 585.90s**（家族 P4a 起扩为 13 件，
  见 v0.4.0 节；出处 docs/HANDOVER.md「P3 DoD 对账」1、7）
- compat 矩阵 14 → **18** 用例（+repl 族 ×4 版本），单轮 **18/18 PASS**
  （出处 docs/HANDOVER.md「P3 DoD 对账」1，docs/compat/matrix.md
  「P3 Task 7 追加族」）
- P2 挂账两项消费：stats Err 路径 JSONL 收口（drop-on-error）+
  `--dml`×stats 裁判维度配平（出处 docs/HANDOVER.md「P3 DoD 对账」6）

## v0.2.0-p2 — flashback + stats（2026-09-21）

- flashback（回滚 SQL）：记录原子逆序（注释绑定为原子单元，超越上游裸行
  逆序）+ keep-trx 事务脚手架默认开（逐字节对齐上游注入位置，含悬空
  `commit;` quirk）+ DDL 排除与三条完整性硬规则（出处 docs/HANDOVER.md
  「P2 DoD 对账」2/3）
- stats：窗口×表 DML 行数 + 大/长事务两报表 + JSONL，golden 逐字节断言 +
  冒烟配平（报表 DML 总和 == 同流 to-sql 行数）；不做裁判差分（spec §3.6
  裁定）（出处 docs/HANDOVER.md「P2 DoD 对账」4）
- 判定证据：Go 裁判差分（rollback）8.0 `groups A=21 B=21 aligned=21
  green=21 red=0` 真复跑 exit 0 + compat **14 用例**全 PASS + 活库正逆
  对账 checksum `3944497573 == 3944497573`（出处 docs/HANDOVER.md
  「P2 DoD 对账」1）
- 吞吐回归闸：threads=8 **88.352 MiB/s**（= 92.6 MB/s）DoD-3 PASS；
  vs P1 基线原始读数 **−14.9%** → 同机 A/B 归因 = 环境漂移 −8.3% +
  代码增量 −3.2%（95% CI 跨 0）→ 不 STOP、如实挂「未判定 finding」
  （出处 docs/bench/p2.md 判定表 + docs/HANDOVER.md「P2 DoD 对账」4）

## v0.1.0-p1 — to-sql file 模式（2026-09-20）

- 全自研 binlog 解码器（决策 D1；无 async = D2；值保真全链路 `Vec<u8>` = D3；
  并行解码 + reorder 保序 + 反压 = D7）：to-sql file 模式全链路交付
  （读文件→解码→并行 worker→保序→写盘）（出处 docs/HANDOVER.md
  「关键决策记录」表）
- 差分底座：Task 15 golden 差分基建 vs Go 裁判 8.0 矩阵 **21/21** 绿
  （比较器 + 白名单基建）（出处 docs/HANDOVER.md「当前进度」P1 前序行）
- 全版本兼容矩阵（Task 17）：5.6.51 / 5.7.44 / 8.0.46 / 8.4.11 真机
  **8 用例**全绿（含 5.6 V1 rows 探针与 8.4 caching_sha2 专项；唯一真解码器
  bug RED→GREEN 修复 `b8f401c`）（出处 docs/compat/matrix.md 结果表，
  docs/HANDOVER.md「当前进度」P1 前序行）
- 吞吐基线（DoD-3）：528 MiB 合成 binlog、threads=8 median 5.093 s →
  **103.85 MiB/s ≈ 108.9 MB/s**（≥40 MB/s，**PASS 2.7×**）
  （出处 docs/bench/p1.md 结果表）
- 同轮入账：`tests/fuzz_seed/` 坏事件语料起步、musl 静态构建性能悬崖
  （threads=8 实测 **≈3.4 MiB/s**）如实登记挂 P4（出处 docs/bench/p1.md
  musl 表 + docs/HANDOVER.md「当前进度」P1 DoD 对账 ④）
