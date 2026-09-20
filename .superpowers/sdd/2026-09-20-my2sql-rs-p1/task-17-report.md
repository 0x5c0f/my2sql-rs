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
