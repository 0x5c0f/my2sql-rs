# Task 13 report — sqlopen（值编码 + DML 构建器）

分支 `feat/p1`，基线 HEAD `63c6d03`（T12 后）。功能提交 `ebf8a10`
feat: sql encoding and dml builders（`src/sqlopen/{mod,encode,dml}.rs`，
+969 行）。HANDOVER Task 13 节点 + checklist 销账 + 报告在本 docs 提交。

## TDD 证据

- **RED**：三模块接口块 + 28 个测试先落盘，`inserts/deletes/updates/plan/
  cell/cond/where_part` 全 `todo!()`。首轮 `cargo test sqlopen` →
  **19 个 dml 测试全部 FAILED（`not yet implemented` panic）**/ 9 pass
  （encode 侧纯函数先绿属预期——其桩即实现）。
- **GREEN 迭代中的非逻辑修正**（测试自身错误，钉死备查）：
  - raw-string `r"'...\"` 编译错 2 处 → 改 `r#"..."#`；
  - `"{got[0]}"` 非法 format → `"{}", got[0]`；
  - `it's` 期望曾误写 SQL 标准 `''` 双引号——权威 encodeRef 是反斜杠 `\'`，
    按代码改测试（authority-wins 又一实例）；
  - 反引号名测试期望多数了一个 `` ` ``（`quote_ident("a`")` 正确产出
    `` `a``` `` 共 5 字符），实现无错、测试改正确；
  - 非法 UTF-8 兜底首版把两侧引号也 hex 化（`0x27FFFE27`）——改为转义段
    先验 UTF-8、违约时输出不含引号的 `0xHEX`（对照 Bytes 分支同形态）。
- **最终三门**：`cargo test` **222 passed / 0 failed / 1 ignored**
  （219 单测 + 3 集成；基线 194 → 新增 28，任务书「191+1」预估按实测重计）；
  `clippy --all-targets -- -D warnings` 净；`fmt --check` 净。

## 权威发现（行号均实读 reference/my2sql-go）

1. **简报函数名失真（第 7 例）**：`GenerateInsertSql/GenerateUpdateSql/
   GenerateDeleteSql` 不存在，实为 `sqlgen.go` 的
   `GenInsertSqlsForOneRowsEvent`(:137)/`GenDeleteSqlsForOneRowsEvent`(:237)/
   `GenUpdateSqlsForOneRowsEvent`(:288)；「com.go/funcs.go 里的 Escape」也
   不存在——真转义链在 `sqltypes/sqltypes.go`。
2. **转义集 = encodeRef 9 项**（sqltypes.go:611-621）：`\0 ' " \b \n \r \t
   \Z \\`；简报疑问项 `%, ctrl-Z`：`%`/`_` **不**转义、0x1A 是 **ctrl-Z
   →`\Z`**（非 \S）。LIKE 例外（String.encodeSql :556-561）：`\` 后随
   `%`/`_` 不双写——逐字节复刻（测试钉死 `a\%b` 原样、`a\\%b` 前斜杠双写）。
3. **WHERE NULL**：`sqlbuilder.Eq`（expression.go:441-447）NULL 右值时算子
   `=`→` IS `，右值渲染 `null`（:27 nullstr）→ 上游产 `col IS null`，
   **从不** `col = null`。本层 `col IS NULL`（大写、语义同）；无键全列
   WHERE 逐列套用，NULL 位同样 IS（测试双断言：含 IS NULL 且不含 "= NULL"）。
4. **SET 差异比较**：上游比的是**解码值**（字节族 `CompareEquelByteSlice`、
   其余 Go `==`，sqlgen.go:343-370），不是编码串——本层 ColumnValue
   PartialEq（T10 已 derive，裁定 6 核实 int.rs:30 **无需新增** derive）
   镜像该效果；因 T5-T8 渲染确定性，字节比较与文本比较在本链路等价。
5. **ignore_pk 仅 INSERT**（sqlgen.go:159-164 + ConvertRowToExpressRow
   :204-217 列清单/VALUES 双位置剔除；UPDATE 函数签名无该参数——裁定 7
   「SET 是否剔 pk」核实结论：**不剔**，测试钉死 full_columns+ignore 组合
   下 pk 仍入 SET）。
6. **批量**：上游 events.go:150 恒 rowsPerSql=1（无 CLI batch flag），
   `--insert-batch` 是本工具重设计；None→1 行/句。
7. **上游 blob 面**：非 utf8 String→`X'lowerhex'`（:567-570），且 text 列
   在 events.go:102-117 被转 string 走引号文本——本侧 Bytes→`0xUPPERHEX`
   （裁定 1）双重分歧（前缀+大小写），已挂 T15 白名单。

## 裁定执行

- 裁定 1：encode_value 各分支按裁定；非法 UTF-8 Str 兜底 hex（测试含
  utf8_safe 链路实证 + 构造违约输入双路）。
- 裁定 2：`strict_schema` 默认 false；Padded→列清单/WHERE 省略+每事件
  warn（行为断言，未上 tracing mock——简单且钉死），Truncated→静默前缀；
  true→align_cols ColCountFatal 逐事件上抛。checklist「T13 决策点」已销账。
- 裁定 3：Missing 值位置 → `SqlError::Value(InvalidData)`（encode_value 单点
  + doc 注释「非出货路径」）。
- 裁定 4/5：`SqlOpts` 六字段全部有 T1 CLI 对应（无「字段有 flag 无」遗留），
  `from_config` 就位待 T14。
- 裁定 6-8：见权威发现 4/5/引号通道；DmlBuilder 无跨事件状态，简报 `..,`
  参数简写文档化 = (tm, schema, rows) 三参镜像。

## 偏差登记（T15 白名单候选）

- blob `0xHEX` vs `X'hex'`（已入挂账清单正式行）；
- 无变化行对：上游空 SET→Fatalf（进程死），本层跳语句+warn；
- NULL/IS 大小写、` AND ` 连接（上游同形）；
- strict non-default=false：等宽场景逐字节对等，宽出场景上游恒 fatal——
  T15 该情形仍按既有纪律跑 strict=true。

## 接缝（T14）

`DmlBuilder::new(SqlOpts::from_config(&cfg))` + 每事件
`decode_rows→(inserts|deletes|updates)`；表名取 `tm.schema/tm.table`
（binlog 面真名，上游 rEv.Table 同源）；`SqlError` 逐事件计错续跑；
add_extra_info 注释包装在 T14 输出层。

> 审阅更正（T13 review）：` AND ` 连接并非"上游同形"——上游多条件 WHERE 带括号 (expression.go conjunctExpression)，SET/VALUES 分隔符亦含空格；已入 HANDOVER 挂账清单 T15 白名单。
