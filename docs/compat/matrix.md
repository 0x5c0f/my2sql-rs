# MySQL 5.6–8.4 兼容矩阵（Task 17 + P2 Task 8）

执行：`make compat`（`tools/compat-matrix.sh`，逐用例日志 `out/compat-*.log`，
结果表 `out/compat-results.tsv`）。裁判 = Go my2sql（reference 未改动）；
比较器/白名单 = Task 15 基建，本任务**零放宽、零新增白名单规则**。

- 测试日期：2026-09-21（P1 修复轮 1 同日：5.6-v1rows 用例与 8.4 探针按新门重跑，
  其余 6 用例沿用全跑数据；P2 Task 8 全量 14 用例同日整矩阵重跑，下述 P1/P2
  全部结果行均出自该次 `out/compat-p2-full.log` + `out/compat-results.tsv`）
- 用例数：**14**（`out/compat-results.tsv` 14 行 = P1 to-sql 族 8 +
  P2 flashback 族 4 + P2 stats 冒烟 2；无排除性缺行——stats 5.7/8.4
  为 spec §3.6 有意不跑，见下）
- 被测源码 commit：P1 轮 = `b8f401c`（fix(task-12) FDE checksum 探针修正；
  其前为 a7c88eb）；P2 轮 = `51f79ae`（P2-T7 头；src/ 与 P1 判定相关代码未变，
  本轮仅扩 harness：run_case 增 WORK_TYPE 维 + 4+2 新用例行）
- 镜像实测版本：mysql:5.6.51 / 5.7.44 / 8.0.46 / 8.4.11（官方 docker 镜像）

## 结果表

| 版本 | to-sql 差分（Go 裁判） | 在线元数据 | 离线 schema 回放 | checksum 开关联跑 |
|---|---|---|---|---|
| 5.6.51 | PASS 19/19 组¹ | PASS（--uri 活库） | PASS（逐字节=在线） | 默认 CRC32 PASS；显式 NONE PASS |
| 5.7.44 | PASS 21/21 组 | PASS（--uri 活库） | PASS（逐字节=在线） | 默认 CRC32 PASS；显式 NONE PASS² |
| 8.0.46 | PASS 21/21 组 | PASS（--uri 活库） | PASS（逐字节=在线） | 默认 CRC32 PASS³ |
| 8.4.11 | PASS 21/21 组 | PASS（native，差分内）；**PASS（stock caching_sha2 探针）**⁴ | PASS（逐字节=在线） | 默认 CRC32 PASS³ |

¹ 5.6 组数 19 = 21 −（t_json 表 + t_all.c_json 相关），数据级排除见下。
² 该用例暴露唯一真机解码器 bug（5.7 FDE 恒带 CRC 尾 → 旧探针误判损坏），
已 RED→GREEN 修复并钉真机字节回归：`b8f401c`，测试
`fixture_5_7_checksum_none_fde_carries_crc_tail`。
³ 实测四个镜像默认 binlog_checksum **均为 CRC32**（brief「5.6 默认 NONE」系
5.6.1–5.6.5 早期行为，5.6.51 末版默认已是 CRC32，以如实测）。NONE 代码路径
由 5.6/5.7 显式 `--binlog-checksum=none` 两用例覆盖；CRC32 路径全版本默认覆盖。
⁴ 8.4 专项（brief 要求）：原版启动（不传任何 auth policy，默认插件
caching_sha2_password），显式建 `IDENTIFIED WITH caching_sha2_password` 探针
用户，`to-sql --uri` 走该用户读在线 schema（无 TLS，RSA full-auth 握手），
产物与 native-auth 差分跑**逐字节一致**。日志 `out/compat-8.4-sha2.log`；
修复轮 1 起该日志含插件实证行（逐行打印 `mysql.user` 的
`probe@% -> caching_sha2_password` 等），由 `PROBE_ONLY=1 bash
tools/compat-matrix.sh` 重跑生成（探针步骤全脚本化，声明自此有 artifact 背书）。

## 加测用例（超出版本轴）

| 用例 | 内容 | 结果 |
|---|---|---|
| 5.6-v1rows | `--log-bin-use-v1-row-events=1`：5.6 真机产 **V1 rows 事件（23/24/25）**，V1 解码路径（decode_rows v2=false）首次真机全类型走读 | PASS 19/19 组。事件普查为**实证硬门**（run-difftest 步骤 3.5，`tools/event-census.py` 走读事件头，无需 mysqlbinlog）：23(WRITE_V1)×10 / 24(UPDATE_V1)×6 / 25(DELETE_V1)×3，V2(30/31/32)=0、V0=0，产物 `out/difftest-5.6-v1rows/EVENT_CENSUS.txt`（修复轮 1 重跑生成）。普查勘误：修复前矩阵/报告把 6/3 记成 DELETE/UPDATE，实为 **UPDATE×6 / DELETE×3**（与 gen-data-5.6.sql 的 6 UPDATE + 3 DELETE 语句数吻合）；10/6/3 总数与"零 V2"结论不变 |

## P2 Task 8 追加族（flashback×4 + stats 冒烟×2，2026-09-21 @ `51f79ae`）

同一次 `make compat` 全量跑（14/14 PASS，`out/compat-p2-full.log`）。
每用例 = 该版本 plain 默认捕获的同套 datadir 流程续跑
`WORK_TYPE=rollback|stats` 的裁判+我方+比较器（run-difftest 自含产数，
非复用旧 binlog 文件——每次重新产数保证与 to-sql 族同数据同源）。

| 用例 | 版本 | 裁判 vs 我方 | 结果（groups/冒烟行如实抄自 tsv） |
|---|---|---|---|
| flashback-5.6 | 5.6.51 | Go `-work-type rollback` vs `flashback` | PASS 19/19 组 red=0（JSON 数据级排除沿用¹） |
| flashback-5.7 | 5.7.44 | 同上 | PASS 21/21 组 red=0 |
| flashback-8.0 | 8.0.46 | 同上 | PASS 21/21 组 red=0 |
| flashback-8.4 | 8.4.11 | 同上 | PASS 21/21 组 red=0 |
| stats-5.6 | 5.6.51 | 冒烟（不裁判比较；Go stats 输出留档 `out/difftest-5.6-stats/go-stats/`） | PASS report total=32 == to-sql DML lines=32 |
| stats-8.0 | 8.0.46 | 同上（留档 `out/difftest-8.0-stats/go-stats/`） | PASS report total=36 == to-sql DML lines=36 |

- **keep-trx scaffold 结构断言实证**（防 `_rb_struct` 无 scaffold 早退=守卫睡死）：
  四个版本的 B 侧产物均含 scaffold 且满足 begin 数 = commit 数 − 1 = 事务段数
  （我方默认 `--keep-trx true` 与 Go KeepTrx 缺省一致）。抽样计数（本轮产物）：
  5.7/8.0/8.4 `flashback.3.sql` begin=15/commit=16、5.6 `flashback.4.sql`
  begin=14/commit=15，违例会经 STRUCT-RED 计入 tsv 的 red=——red=0 即断言真跑过。
- **scaffold-free 对照**：A 侧（Go 裁判）本轮四版本产物实测均无 begin;/commit;
  行（begin=commit=0，与 compare.py 注释「A 侧 KeepTrx=false 天然无 scaffold」
  一致）——结构断言实际只经 B 侧触发，B 侧计数上条即为守卫非睡死的实证。
- **范围裁决（brief 12 行 scope，不外溢）**：CKSUM=none 与 V1ROWS=1 特殊用例
  不扩展 rollback/stats 变体（checksum 双态已在 to-sql 族 8 用例覆盖解码器，
  flashback 与其共用同一解码路径；V1 逆序已由 T6 e2e + 单测钉，矩阵不重复）。
- **stats 5.7/8.4 有意不跑**：spec §3.6 省时裁决——stats 聚合逻辑与服务器版本
  无关（输入已是解码后事件流），5.6（最低版本）+ 8.0（主力版本）两点覆盖。

## 每版本排除/裁剪清单（矩阵级，非白名单放宽）

- **5.6（tools/gen-data-5.6.sql，run-difftest 按 VER 自动选择）**：
  1. `t_all.c_json` 列 —— 5.6 无 JSON 类型（5.7.8 引入），服务器无法存储；
  2. `t_json` 专表及其实例；
  3. 事务内 `UPDATE t_json SET j=…`（JSON 值变更句）。
  其余矩阵单元格与 gen-data.sql 逐行同源（含 utf8mb3/GBK/unsigned/边界/无键表）。
  落点 = 数据级排除（NOTE ALW-56-JSON 的 T17 预案兑现），比较器规则未动。
- **全部版本**：V0 rows 事件（20/21/22）为 5.0 时代产物，5.6.51+ 无任何开关可
  产出（实测 5.6 默认即 V2），矩阵无法亦不应生成——路由层硬错误立场不变
  （T12），单测已钉；EXCL ALW-TS-V1-LEGACY 维持。
- **全部版本（承 T15）**：无变化 UPDATE、多 uk 表、表达式索引、中途 DROP
  COLUMN、JSON 内时间 opaque——沿用 Task 15 矩阵排除，本任务未扩大。

## 真机勘误（对 brief/docker-mysql.sh 旧假设）

1. **8.4 认证**：`--authentication-policy=mysql_native_password` 在 8.4.11
   **启动即失败**（MY-013797：native 插件默认 OFF，不算合法 policy 值）。
   正确姿势 = `--mysql-native-password=ON`（启插件）+ 建库后
   `ALTER USER 'root'@'%' IDENTIFIED WITH mysql_native_password BY ''`
   （MYSQL_ALLOW_EMPTY_PASSWORD 把 root@% 建成 caching_sha2，旧驱动裁判拒）。
   docker-mysql.sh 已按实测改写并真机通过。
2. **5.6 默认态**：如上³，5.6.51 默认 CRC32 + V2 事件（brief 两处历史假设
   由实测校正；两态仍都跑了，覆盖不降反升）。
3. **5.7 FDE CRC 尾**：见结果表注²（本矩阵唯一真 bug）。

## 复现

```bash
make compat                      # 全矩阵 14 用例（本轮实测约 4 分钟，需 docker + /opt/go/bin）
VERSIONS="5.7" make compat       # 单版本调试（该版本跑 to-sql 双态 + flashback；stats 两例固定 5.6/8.0 仍跑）
KEEP=1 VER=5.7 CKSUM=none bash tools/run-difftest.sh   # 失败保容器
VER=5.6 V1ROWS=1 bash tools/run-difftest.sh            # 单跑 v1rows（含事件普查门）
VER=8.4 WORK_TYPE=rollback bash tools/run-difftest.sh  # 单跑 flashback 差分
VER=8.0 WORK_TYPE=stats bash tools/run-difftest.sh     # 单跑 stats 冒烟
PROBE_ONLY=1 bash tools/compat-matrix.sh               # 单跑 8.4 探针（不碰全量 tsv）
```
