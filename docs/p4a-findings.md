# P4a T3 (Lane C) FINDINGS — difftest P4A 表组真机捕获

- 运行：`P4A=1 VER=8.0 bash tools/run-difftest.sh`（mysql 8.0.46 容器，
  binlog_checksum=CRC32、v1_row_events=0，data binlog=`mysql-bin.000003`）
- tee 全录：`out/difftest-8.0-p4a-run.log`；roundtrip：`out/p4a-roundtrip-run.log`
- 裁判比较器输出（逐字）：`groups A=14 B=14 aligned=14 green=14 red=0`
- 我方 to-sql（逐字）：`to-sql done: events=14, statements=20, files=1, errors=0`
- 步骤 7 离线回放：`rs` vs `rs-offline` `diff -r` 空（逐字节一致）
- src/ 与 tools/comparator/**：零改动（三形我方均无 panic/Err，裁判无违例）。

## 三形矩阵（挂账勾销：终审登记的三列形缺口 → 本件实抓）

| 形 | Go 支持? | Go 裁判差分 | 自 roundtrip（灌 `_clone`） |
|---|---|---|---|
| ENUM 300 成员（2B packlen） | 支持（值输出为 1-based 序号，与我方一致） | green（14/14 组内，无 ONLY-IN/无 DIFF） | CHECKSUM `2619814406` == `2619814406`，行级 diff 空 |
| GEOMETRY POINT/LINESTRING/POLYGON + SRID 4326（非空） | 支持（SRID+WKB 原样 hex，裁定 7 字节保真首次实抓） | green | CHECKSUM `4084198807` == `4084198807`，行级 diff 空 |
| LONGBLOB 70,000/280,000 B（4B prefix，payload >64KB） | 支持 | green | CHECKSUM `1767911749` == `1767911749`，行级 diff 空 |

三形 Go 裁判差分逐字节一致的严格口径：比较器语义 green 之外，另做机械核验——
两侧 20 条 DML 语句行，在既有三类已知打印差异（Go `null` vs 我方 `NULL`、
Go `X'小写'` vs 我方 `0x大写`、Go UPDATE 多列 `, ` vs 我方 `,`）归一后
md5 相同（`7be7a1a149b8ace7285ed8c1bd22739f` 双方）；19 个 hex payload
逐 token 序列全等（尺寸逐字 `[25,61,97,25, 25,77,181,25, 25,45,97,25,25,97,25, 70000,280000,1,70000]`）。

## 逐表语句计数（Go binlog_status.txt 与我方产物双向核对，逐字）

| table | inserts | updates | deletes | 语句合计 | 窗口 startpos-stoppos |
|---|---|---|---|---|---|
| t_enum_wide | 5 | 2 | 1 | 8 | 4984–6030 |
| t_geom | 3 | 2 | 1 | 6 | 6571–9643 |
| t_blob | 3 | 1 | 2 | 6 | 10104–781167 |

我方产物按表语句计数与上表逐格相同（python 逐行正则统计 go/rs 两侧一致）。

## 形 1：ENUM>255（2B packlen）

- 真机 binlog TABLE_MAP 元数据（data/8.0/mysql-bin.000003 走读，逐字）：
  列类型序列 `03 f7 f7`（INT, ENUM, ENUM），meta `…f702 f702…` → 两 ENUM 列
  packlen=2（300>255 生效）。
- 边界捕获（INSERT 语句逐字，两侧一致）：
  `(1,255,1)`（e254→255 / e0→1）、`(2,256,2)`（e255→256）、`(3,300,256)`（e299→300）、
  `(4,255,NULL)`、`(5,1,300)`；UPDATE `SET \`e\`=256 WHERE \`id\`=1`、
  `SET \`n\`=1 WHERE \`id\`=4`；DELETE `WHERE \`id\`=5`。
  序号 256/300 > 255 即 2B packlen 读路的行级实证。
- 备注（沿既有裁决，非本件差异）：ENUM 两侧均输出 1-based 序号而非成员名
  （裁定 7「名称映射留 P2」后现状即序号形，主矩阵 `c_enum` 同款，两家一致）。

## 形 2：GEOMETRY（字节保真实抓）

- 三型非空 + SRID 4326 全绿。样例（rs 侧逐字，Go 侧同 payload 仅记号差异）：
  - `INSERT … VALUES (1,0x000000000101000000000000000000F83F00000000000002C0,…)`
    （POINT(1.5 -2.25)：SRID 0 + WKB，25B）
  - 带孔 POLYGON 行 id=2：g payload 181B；
  - `UPDATE … SET \`p4326\`=0xE6100000010100000000000000000018400000000000001440 WHERE \`id\`=2`
    （`E610`=SRID 4326 LE）——SRID 前缀保真实抓。
- seed 期真机勘误（登记）：简报探针 `POINT(179.9 -89.9)` SRID 4326 在 8.0 报
  `ERROR 3617 (22S03) at line 42: Latitude 179.900000 is out of range in function st_geomfromtext. It must be within [-90.000000, 90.000000].`
  ——8.0 对 SRID 4326 按纬/经轴序解释首坐标。属 seed 构造笔误（非形状缺口、非裁判违例），
  改 `POINT(-89.9 179.9)` 保边界极值语义后执行为 NULL/无告警。

## 形 3：LONGBLOB>64K（4B prefix + 跨页 payload）

- 真机单事件（binlog 走读逐字）：`WRITE_ROWSv2 event len=70044` 与
  `WRITE_ROWSv2 event len=280044` —— 280,000B payload 单 rows 事件，远超 64KB
  界（4B 长前缀路径），并必然跨 binlog 页写出。
- 产物 hex payload 尺寸逐字：70000 / 280000 / 1（x'00' 行）/ 70000（UPDATE 改后镜像）；
  语句行长度 140,056B / 560,056B（id=2 INSERT）。DELETE `WHERE id=` 仅主键
  （无全镜像洪泛，两家同形）。
- 简报注「210,000 B」为乘数笔误（`x'00FF55AA'`=4B × 70000 = 280,000B）；
  SQL 字面量逐字照简报，实际尺寸以上述真机产物为准。

## 自 roundtrip（tools/p4a-roundtrip.sh，双保险亦跑）

- 独立容器生命周期（my2sql-p4a-rt，瞬态 datadir，seed 整灌 gen-data-p4a.sql →
  FLUSH 封口 → 窗口=全 p4a 库）→ 我方 to-sql → 产物灌 `p4a_clone`（前态=
  mysqldump --no-data 复制的空表）→ 三表 CHECKSUM TABLE clone==main（值见矩阵）
  + 行级 mysqldump diff 空（`.rows` 非空核验入脚本硬闸）。用时 24s。
- 本件实踩并已修复的脚本缺陷：mysqldump 对 GEOMETRY/LONGBLOB 输出含未转义的
  非 NUL 控制字节（0x01/0xC0…），GNU grep 二进制探测将 dump_rows 置空 →
  行级 diff 假绿；修复 = `grep -a` + 空 `.rows` 即红。
- 修复轮 1（外审 Important#1/#2）：`^--$` 样板行从 dump_rows 剔除（mysqldump
  恒发裸 `--` 行，旧「非空」闸对 0 数据行也假绿）→ `.rows` 为纯 INSERT 数据线；
  硬闸改为 main/clone 双侧 `grep -ac '^INSERT'` > 0（合法空结果亦必须红）。
  本节数字为修复后真跑重生成（全录 `out/p4a-roundtrip-fix1.log`，21s）：
  逐表真实数据行 4/2/1（初版报告「7/5/4」系含 3 行 `--` 样板的计数笔误，
  按修复后 grep 重跑核正；CHECKSUM 三值与初版逐字相同）。

## 回归与独立性登记

- `bash tools/run-difftest.sh`（plain，8.0，P4A 不设）复跑绿：
  `groups A=21 B=21 aligned=21 green=21 red=0` + 步骤 7 diff 空 →
  P4A 分支默认关闭不影响既有路径；`tools/compat-matrix.sh` 零改动。
- 既有 18 件 compat 矩阵不因本件重跑（本件独立于 compat 家族，spec §3 登记口径）。
- src/binlog 零改动 → 解码器开闸条款本 lane 记「零改动」；无 fuzz 语料登记
  （我方无 panic/Err，无新病理字节；FORBIDDEN 面未触碰）。
