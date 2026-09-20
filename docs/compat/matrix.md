# MySQL 5.6–8.4 兼容矩阵（Task 17）

执行：`make compat`（`tools/compat-matrix.sh`，逐用例日志 `out/compat-*.log`，
结果表 `out/compat-results.tsv`）。裁判 = Go my2sql（reference 未改动）；
比较器/白名单 = Task 15 基建，本任务**零放宽、零新增白名单规则**。

- 测试日期：2026-09-21
- 被测源码 commit：`b8f401c`（fix(task-12) FDE checksum 探针修正；其前为 a7c88eb）
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
产物与 native-auth 差分跑**逐字节一致**。日志 `out/compat-8.4-sha2.log`。

## 加测用例（超出版本轴）

| 用例 | 内容 | 结果 |
|---|---|---|
| 5.6-v1rows | `--log-bin-use-v1-row-events=1`：5.6 真机产 **V1 rows 事件（23/24/25）**，V1 解码路径（decode_rows v2=false）首次真机全类型走读 | PASS 19/19 组（事件普查确认 10×23/6×24/3×25，零 V2） |

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
make compat                      # 全矩阵（约 10 分钟，需 docker + /opt/go/bin）
VERSIONS="5.7" make compat       # 单版本调试（用例集仍含 checksum 双态）
KEEP=1 VER=5.7 CKSUM=none bash tools/run-difftest.sh   # 失败保容器
```
