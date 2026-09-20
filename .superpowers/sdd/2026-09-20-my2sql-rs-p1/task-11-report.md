# Task 11 report — metadata 层（SchemaStore online/offline + 列数对账 + 键映射）

分支 `feat/p1`，基线 HEAD `51da768`。

## TDD 证据

- **RED**（tests + `todo!()` stubs 先行）：`cargo test` →
  `test result: FAILED. 138 passed; 14 failed; 1 ignored`，14 个失败全部
  `panicked at ... not yet implemented`，清单：
  `align_cols_{equal_ok,binlog_narrower_truncated,binlog_wider_padded,
  strict_fatal_both_directions}`、`key_indexes_{resolves_all_ordinals,
  pk_with_missing_col_degrades_to_empty,key_beyond_binlog_width_dropped,
  empty_key_lists}`、`offline_{dump_roundtrip,duplicate_last_wins,
  rejects_unknown_version,missing_table_not_found}`、
  `parse_keys_expression_index_null_column_becomes_empty_name`、
  `align_import_reachable`。（实现期间另有 2 轮编译错误：mysql 28 的
  `Row` 无 `get_ref`（改 `row.get::<Value,_>`）、`Value` 无 `Display`
  （兜底 Debug 文本）——非测试逻辑 RED，属 API 探测，记录备查。）
- **GREEN**（实现后）：`cargo test` → **152 passed + 3 passed（cli 套件）,
  0 failed, 1 ignored**（基线 133+3 → 新增 19 个单元测试）。
  `cargo clippy --all-targets -- -D warnings` 干净；`cargo fmt --check` 干净。
- 默认 `cargo test` 无需服务器即绿（live 测试 `#[ignore]`）。

## 真库在线路径证明（裁定 3）

- 容器：`docker run -d --rm --name my2sql-t11-mysql -p 33117:3306
  -e MYSQL_ROOT_PASSWORD=t11pass mysql:8.0 --skip-log-bin` →
  **8.0.46**；跑毕 `docker stop`（--rm 自动清除），无常驻残留。
- `MYSQL_TEST_URI="mysql://root:t11pass@127.0.0.1:33117" cargo test
  online_store_live -- --ignored --nocapture` → `1 passed`。
- 执行的查询（测试内）：`SELECT VERSION()`；DDL：CREATE DATABASE
  `my2sql_t11` + t1（10 列：`bigint unsigned` PK autoinc、`int`、
  `varchar(50)`、`decimal(10,2) unsigned`、`tinytext`、生成列
  `gen_col ... STORED`、`ALTER ... ADD COLUMN hid int INVISIBLE`）+
  键 `PRIMARY(id)`/`UNIQUE uq_mail(email)`/`UNIQUE uq_c(c1,c2)`/
  `KEY k_a(a)`；odd 表（无主键，`UNIQUE KEY fake_primary_idx(x)` +
  `KEY k_y(y)`）。store 侧每表懒查 2 条：`SHOW FULL COLUMNS FROM `db`.`tb``
  + `SHOW INDEX FROM `db`.`tb``；结束 `DROP DATABASE`。
- 样例：服务器输出 `id  bigint unsigned`（8.0 无显示宽度形态）→ 解析为
  `SchemaCol{name:"id", type_name:"bigint", unsigned:true}`（断言
  `type_name=="bigint" && unsigned`）；`amt decimal(10,2) unsigned` →
  `("decimal", true)`；`k_a`（Non_unique=1）不进任何键；
  `fake_primary_idx` → `pk=["x"]`（键名含 "primary" 判定实测成立）。
- 等价断言：本层 cols 名单与 `SHOW COLUMNS` 的 Field 列逐位相等（证明
  客户端零过滤）；dump→offline 回读与 online 结构全等。
- **第二真库（5.6 降级路径）**：throwaway `mysql:5.6`（端口 33118）同测
  实跑 `1 passed`（5.6.51）——`generated col added: false, invisible col
  added: false`，ALTER 失败即跳过、`find(..).is_some()==has_*` 断言成立，
  证实「任何可达实例」与 absent-safe（跑毕容器已删）。
- 附注（给 T15 harness）：容器刚 `mysqld is alive`（socket ping）时经 TCP 的
  `Conn::new` 仍可能 `IoError{server disconnected}`——需 TCP 就绪等待/短重试
  （本轮以 sleep+循环解决，产品码不加重试，裁定 2）。

## 简报 vs 上游裁定（三个 VERIFY 项 + 其余）

1. **「索引名含 primary 即 PK」**——属实，非 shorthand 杜撰：
   mysqlFuncs.go:194 `strings.Contains(strings.ToLower(kName),"primary")`。
   附带发现：多个 primary 名键时 Go 在 map 随机序下**最后生效、前者整体
   丢弃**（:221-238 `PrimaryKey` 覆写、非 pk 分支不入 uks）→ parse_keys
   确定序复刻该有效行为（测试 `parse_keys_name_containing_primary_is_pk`）。
2. **align_cols（context.go/sqlgen.go 实读）**——简报/controller 表述
   「pad + strict fatal」不精确：`GetAllFieldNamesWithDroppedFields`
   （sqlgen.go:23-33）确实补 `dropped_column_i`，但 events.go:87 在
   `len(colsTypeName) > len(tbInfo.Columns)` 时**无条件 log.Fatalf**——
   补位结果从不产出 SQL；收窄方向（binlog<schema）events.go:83 静默取
   schema 前 N 列（上游无 strict 开关，该方向永不 fatal）。实现：
   非 strict 三态齐出（Padded 保留简报绑定形状供 T13 决策），strict →
   两向皆 `ColCountFatal`（fatal 范围是简报绑定的 strict 语义，扩宽方向
   与上游一致；收窄方向 strict 下 fatal 属**超集扩展**，已注明）。
   **T15 对账纪律：列数扩宽场景上游恒 fatal，差分需 strict=true**。
3. **key_indexes 降级**——简报「找不到→pk=[]」非上游实码：
   GetColIndexFromKey（:337-348）缺失名**静默留 0**（零值 bug，生成 WHERE
   用错列）；仅 PK 名列表本身为空才 pk=[]（events.go:139-143）；序号≥行宽
   时上游后续 `row[idx]` 越界 panic。本层按简报绑定实现为**整键丢弃**
   （pk=[]/uk 剔除），系对上游 bug 的刻意偏离（错列 SQL 比无键 SQL 更坏），
   T15 白名单候选，已记录。
4. **SHOW FULL vs SHOW COLUMNS**（裁定 5 相关）：上游用 SHOW COLUMNS
   （:254），简报写 SHOW FULL COLUMNS——两者 Field/Type 前两列同序同值，
   本层按简报用 FULL、按列名取值，有效一致。**生成/不可见列：上游零
   过滤**（:282-299 逐行照收）；8.0.46 实测 SHOW FULL COLUMNS **包含**
   STORED GENERATED 与 INVISIBLE 列 → 本层同样收录（真库断言过）；
   5.6 无此类列，自然 absent-safe。
5. **type_name 归一化补充**：上游 GetFiledType 只切 `(`，8.0.19+ 无括号
   形态 `int unsigned` 会把 " unsigned" 留在 type_name（上游消费端靠
   Contains 侥幸无碍）；本层按 T9 契约剥尾部 unsigned/signed/zerofill 词，
   与 5.7 时代上游有效输出一致。
6. **错误类型**：align_cols 返回 `Result<Align, MetaError>` 而非简报的
   `BinlogError`（metadata 层语义独立、不向 binlog 层塞变体；简报裸
   `Result` 全为 shorthand 的授权细化）。依赖面未扩（mysql/serde/
   serde_json/thiserror/tracing 均既有白名单；已核对 Cargo.toml）。

## 文件

- `src/metadata/schema.rs`（扩展：serde derive、norm_type、Align/align_cols、
  key_indexes + 9 测试）
- `src/metadata/store.rs`（新建：MetaError、SchemaStore、parse_columns/
  parse_keys、fetch_online + 10 测试，含 1 ignored live）
- `src/metadata/mod.rs`（接线）
- `src/binlog/table_map.rs`（3 处测试断言限定 `Vec::<u64>::new()`，见下）

## 顾虑 / 遗留

- **serde_json 链接可见性副作用**：store.rs 首次 `use serde_json` 使
  `impl PartialEq<serde_json::Value> for u64` 进入 trait 选型，触发
  table_map.rs 三处既有 `Vec::new()` 推断歧义（E0282/0283 编译失败）。
  已最小限定修复；提示后续任何跨层新依赖首用都可能暴露同类潜伏歧义。
- align_cols 的 Padded 非 strict 分支**不对应上游任何可观测输出**（上游
  此处恒 fatal）——T13 接入时若默认非 strict，行为相对上游是「fatal→
  warn+继续」的放宽，默认值决策已挂 HANDOVER 遗留行。
- `SchemaStore::online` 仅存 URI 字符串语义交由 mysql crate Opts 解析；
  P1 无连接池/超时（上游亦无）；连接错误直冒 `MetaError::Db`，无重试。
- live 测试对 5.6/5.7 可跑性：生成/不可见列 ALTER 失败即跳过、断言随
  `is_some()==has_*` 联动——5.6.51 与 8.0.46 双真库实跑绿（见上节），
  「仅证明」风险已消除；剩余未实测形态 = 表达式索引（仅合成单测覆盖，
  5.6 无该语法，T15 矩阵 8.0 时可补真库断言）。
- 未遇到任何注入式 stop 指令（本轮仅有 harness 的任务清单提醒，非注入）。
