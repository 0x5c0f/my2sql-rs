# Task 17 report — MySQL 5.6–8.4 全版本兼容矩阵

Status: **DONE_WITH_CONCERNS**（矩阵全绿；concerns 均为事实性勘误/环境记录，无未完项）

## 执行了什么

- `tools/compat-mysql 矩阵编排 = tools/compat-matrix.sh`：对 5.6/5.7/8.0/8.4 循环，
  全部复用 Task 15 入口 `VER=$VER bash tools/run-difftest.sh`（DRY，零步骤复制），
  外加 8.4 stock caching_sha2 在线元数据探针。`make compat` 直通。
- `tools/run-difftest.sh` 扩展（默认行为与 T15 完全一致，全 env 开关）：
  `CKSUM=none|crc32`（透传 docker-mysql，产物目录加后缀）、`V1ROWS=1`、
  `gen-data-$VER.sql` 存在则优先、步骤 5 加 `--schema-dump`、
  新步骤 7 离线 schema 回放（`--schema-file` 重跑 to-sql → 与在线输出
  `diff -r` 逐字节对差）、回显 `@@global.binlog_checksum` 等真机事实入日志、
  BIN 名落 `$OUT/BINLOG` 供探针复用。
- `tools/docker-mysql.sh`：CKSUM/V1ROWS/AUTH=stock/DT_NAME 开关；
  8.4 native 支持按真机重写（见勘误②）。
- `tools/gen-data-5.6.sql`：5.6 数据级裁剪版矩阵（剔 JSON 三处，其余与
  gen-data.sql 同源；头注释钉同步义务）。
- `docs/compat/matrix.md`：结果表（行=版本，列=to-sql 差分/在线元数据/
  离线回放/checksum 联跑）+ commit hash + 日期 + 每版本排除清单 + 真机勘误。
- `Makefile`：`compat` 目标；`.PHONY` 更新。
- 比较器/白名单：**零规则改动、零新增允许项**（唯一登记面变化 =
  allowlist NOTE ALW-56-JSON 从"预案"改为"兑现"文字）。reference/ 未触碰
  （只构建）。

## 每版本跑了什么（全绿，9/9 用例，commit b8f401c，2026-09-21）

| 用例 | 服务器事实 | 结果 |
|---|---|---|
| 5.6-default | 5.6.51 CRC32（默认）V2 | 19/19 green + 回放逐字节等 |
| 5.6-none | 5.6.51 显式 NONE | 19/19 green |
| 5.6-v1rows | 5.6.51 CRC32 + log_bin_use_v1_row_events=1；事件普查 10×WRITE_V1/6×DELETE_V1/3×UPDATE_V1，零 V2 | 19/19 green（V1 解码路径首次真机全类型走读） |
| 5.7-default | 5.7.44 CRC32 | 21/21 green |
| 5.7-none | 5.7.44 显式 NONE | 21/21 green（**先红后绿**，见下） |
| 8.0-default | 8.0.46 CRC32 | 21/21 green |
| 8.4-default | 8.4.11 CRC32，native policy 修 | 21/21 green |
| 8.4-caching_sha2-online | stock 8.4（无 policy 旗标），probe 用户 caching_sha2_password | PASS，输出与 native 跑 diff -r 全等 |

checksum 覆盖口径：实测四个镜像默认**均为 CRC32**，NONE 路径由 5.6+5.7
两条显式用例覆盖（两条代码路径在两个大版本上正反都过，满足 brief Step 2）。

## 发现的失败 + 修复（RED evidence）

**唯一真解码器 bug**（T12 属主，`fix(task-12)` = b8f401c）：
5.7+ mysqld 的 FDE 恒带 4B CRC 尾（binlog_checksum=NONE 时亦如此；alg 字节
在 len-5 位=0，vendored go-mysql event.go:186 `data[len(data)-5]` 同位读取）。
旧 `handle_fde` 的"声称 CRC32 但 FDE 验证失败→损坏"兜底把 `body[-1]`
（此时实为 CRC 尾字节，真机件恰为 0x01）当 alg 字节 → 5.7-none 整跑
`error: checksum mismatch`（out/compat-5.7-none.log 首跑留档）。
- RED：真机捕获件单测（mysql-bin.000003 首件 FDE 119B 逐字节）修复前输出
  `panicked ... called Result::unwrap() on an Err value: ChecksumMismatch`。
- GREEN：判定次序改为 FDE 校验通过→alg 取 len-5 决定 NONE/CRC32；不过→
  回退无尾形态解读。全量 cargo test 240+3+4 绿（1 ignored）、clippy -D、
  fmt 干净；修复后矩阵整体重跑（full run2）ALL GREEN。

## 排除清单（矩阵级，全部登记 matrix.md）

- 5.6：JSON 类型/表/事务内 JSON UPDATE（服务器无法存储）→ gen-data-5.6.sql。
- 全版本：V0 rows 事件不可产出（5.6.51 无此路径，实测默认 V2；路由层
  硬错误立场 + 单测维持）；T15 既有五 EXCL 不变。

## Concerns

1. brief 两处历史假设被实测否证（5.6 默认 NONE / 5.6 产 V0-V1 事件）——
   已按"实测为准"处理并加测 V1rows 用例补足 V1 覆盖，矩阵覆盖不降反升；
   若验收坚持 5.6.10 GA 行为，需换 5.6.10 镜像（本环境只有 latest 5.6 tag）。
2. 8.4 在线元数据探针验证的是 mysql crate 28 无 TLS 下 caching_sha2 RSA
   full-auth；repl 模式（P3）长连接/轮换场景未在本矩阵范围内。
3. 5.7-none 的 FDE bug 属"旧版本首次真机解锁"类——与 allowlist 无关，
   但说明 T12 的 FDE 判定当时只被 8.0 真机件钉过；本任务已补真机件回归。
4. 中途一次本任务外观察：5.6 首跑前 `docker pull mysql:8.4`（后台）正常，
   无网络阻塞项。

## 门禁自查

- bash -n：compat-matrix.sh / run-difftest.sh / docker-mysql.sh 全过。
- 全矩阵 `make compat` exit 0（full run2 日志 out/compat-full-run2.log，
  "COMPAT MATRIX: ALL GREEN"）。
- src 有改动 → cargo test（247 用例）/ clippy -D warnings / fmt 三门全绿。
- `docker ps -a` 无 my2sql-dt-*/my2sql-t17-* 残留。

---

## Fix round 1（2026-09-21，审阅者清零项；本节为追加，上文历史不改写）

审阅发现 6 项，全部处理。逐项结果：

### 1. 用例数勘误：9/9 → **8/8**

上文「全绿，9/9 用例」为误记：`out/compat-results.tsv` 恒 **8 行**
（5.6-default/none/v1rows、5.7-default/none、8.0-default、8.4-default、
8.4-caching_sha2-online = 7 差分用例 + 1 在线探针），且上表本身即 8 行。
已在 docs/compat/matrix.md（新增「用例数：8」行）、docs/HANDOVER.md
（当前进度 + Task 17 节点两处 9→8）与本报告（本节）三处钉正。

### 2. V1ROWS 事件普查：从口头声明 → 实证硬门 + artifact

- 新增 `tools/event-census.py`（纯 stdlib：fe'bin' 校验 → 19B 公共头走读，
  type 字节@4、event_size@9 → 类型计数 + rows V0/V1/V2 汇总；
  `--assert-v1-only` 硬断言 23/24/25 齐、30/31/32 零、无 V0，违例 rc=1）。
- `tools/run-difftest.sh` 新步骤 **3.5/7**（仅 V1ROWS=1 触发）：对数据
  binlog 跑普查，`tee $OUT/EVENT_CENSUS.txt`；set -e + pipefail 下断言失败
  = 用例 FAIL。事件编号对照 MySQL event_codes.h，与 file_reader.rs 路由表
  （23→Write、24→Update、25→Delete）一致。
- **单用例重跑**（VER=5.6 CKSUM= V1ROWS=1，复刻 run_case 全部命令）：
  rc=0，`out/compat-5.6-v1rows.log` +
  `out/difftest-5.6-v1rows/EVENT_CENSUS.txt` 重新生成。
- **普查结果（mysql-bin.000004，76 事件）**：23 WRITE_ROWS_V1 ×**10**、
  24 UPDATE_ROWS_V1 ×**6**、25 DELETE_ROWS_V1 ×**3**；30/31/32 全零；20/21/22
  全零。**结论 = 部分证实 + 部分勘误**：总数 10/6/3 与「零 V2、全 V1」
  证实；但原声明「6×DELETE_V1/3×UPDATE_V1」把 U/D **写反**——真机为
  UPDATE×6 / DELETE×3（与 gen-data-5.6.sql 的 6 条 UPDATE + 3 条 DELETE
  语句数独立吻合）。matrix.md 已改写为 artifact 引用式表述并记勘误；
  HANDOVER 的「10W/6D/3U」同步改「10W/6U/3D」。

### 3. docker-mysql.sh 8.0 臂 set -e 隐患

- 8.0 臂 `[ cond ] && EXTRA+=(...)` 已重写为显式 if；同型的
  `[ -n CKSUM ] && ...`、`[ -n V1ROWS ] && ...` 两行与 run-difftest.sh 的
  `ROWSMODE` 行一并转 if。
- **如实记录**：审阅声称的 abort（AUTH=stock VER=8.0 → 空端口）在本机
  bash 5.2.21 **不可复现**——git HEAD 原脚本 + stub docker 干跑，
  AUTH=stock VER=8.0 正常打印端口、rc=0（bash 对 `&&` 列表非末位命令的
  set -e 豁免）。但该模式依赖豁免规则（函数化/重构即成雷），属真隐患，
  按审阅要求清除。验证：`bash -n` 三脚本全过；stub-docker 干跑改后脚本
  AUTH=stock VER=8.0 → 端口输出、`docker run` 实参不含 native plugin 旗标；
  真机 AUTH=stock 路径由本轮 8.4 探针重跑（item 6）同型覆盖。

### 4. FDE 无尾兜底分支单测（src，TDD）

`handle_fde` else 臂（file_reader.rs:166）此前无任何命名测试钉死
（旁证：既有 with_fde(false) 合成用例顺路走过 body[-1]=0 分支，但语义
未声明、翻转不可见）。新增两测试，钉**现行有意语义**
（log_event.cc 立场：CRC32 流的 FDE 恒带尾，T17 真机四版本实证）：

- `no_tail_fde_with_none_alg_decodes_as_checksumless_stream`：5.6 式无尾
  FDE、体末 alg=0 → 按无校验流解码、with_crc=false、整流可用。
- `no_tail_fde_claiming_crc32_is_rejected_as_checksum_mismatch`：无尾但
  体末 alg=1 且 FDE 验证不过 → 只可能损坏/改写 → `ChecksumMismatch`
  硬拒，不得静默降级丢全文件校验。

RED 证据（pinned-semantics 测试无旧 bug 可红，用变异证明分支真实咬合）：
- `== 1` → `!= 1`：两测试**双双 RED**（none 被误拒、CRC32 声称不再拒）；
- `== 1` → `false`：reject 测试 RED（unwrap_err on Ok）；
- 还原后两测试 GREEN。全量 cargo test/clippy -D/fmt 见门禁节。

### 5. gen-data-5.6.sql:3 注释

「其余 20 组」→「其余 19 组」（比较器计数 A=19，与矩阵级剔除 2 处一致）。

### 6. 8.4 探针插件实证入日志

- `probe_84_caching_sha2` 在建 probe 用户后新增逐行打印
  `SELECT CONCAT('plugin proof: ',user,'@',host,' -> ',plugin) FROM
  mysql.user WHERE user IN ('root','probe')`（块重定向 →
  `out/compat-8.4-sha2.log`）；原 `grep -q` 硬断言保留。
- 新增 `PROBE_ONLY=1 bash tools/compat-matrix.sh` 门（只重跑探针、结果落
  独立 `out/compat-probe-only.tsv`，不碰全量 tsv/用例）。
- **已真机重跑**（非"待重跑"）：PROBE_ONLY=1 全绿，日志实证行 =
  `probe@% -> caching_sha2_password`（探针用户，to-sql --uri 即走它）、
  `root@% -> mysql_native_password`（datadir 承自 native 差分跑的持久化
  ALTER，如实记录）、`root@localhost -> caching_sha2_password`；
  to-sql 产物与 native 跑 `diff -r` 全等。矩阵 8.4 探针行判定不变。

### Fix round 1 门禁

- `bash -n`：compat-matrix.sh / run-difftest.sh / docker-mysql.sh 全过；
  event-census.py py_compile 过。
- src 改动：cargo test 全绿（lib 242（1 ignored）+ 集成 3 + 4，含新增两
  fallback 测试）/ clippy 语法见下 / fmt 干净
  `cargo clippy --all-targets -- -D warnings` 干净 / `cargo fmt --all --check` 干净。
- 重跑集：v1rows 用例 rc=0（含 3.5 普查门）、8.4 探针 rc=0；
  其余 6 用例沿用 8cda7e8 全跑数据（判定与代码均未变）。
- `docker ps -a`：无 my2sql-* 残留。
