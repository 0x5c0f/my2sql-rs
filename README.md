# my2sql-rs

MySQL binlog → SQL 还原工具的 Rust 独立实现（to-sql / flashback / stats），
能力对齐 Go 版 [my2sql](https://github.com/ultradb/my2sql)（本仓库内
`reference/my2sql-go/` 作为行为参考与差分裁判），但 CLI 全新设计、无 async
（std::thread + crossbeam-channel）。当前处于 **P1：file 模式 to-sql 已达发布
标准**；flashback / stats / 复制协议模式在后续阶段（P2/P3）。

## 功能矩阵

| 能力 | 状态 | 说明 |
|---|---|---|
| `to-sql` file 模式（binlog 目录离线读取） | ✅ | `make compat` 8 用例全绿 |
| MySQL 5.6 / 5.7 / 8.0 / 8.4 | ✅ | ROW 格式；full/minimal/noblob 镜像；CRC32/NONE 校验和；V1/V2 rows 事件（V0 硬拒，见下文差异 6） |
| 在线 schema（`--uri`） | ✅ | mysql://user@host:port；8.4 caching_sha2 与 native 双通过 |
| 离线 schema 回放（`--schema-file` / `--schema-dump`） | ✅ | 无 DB 可解码；导出→回放逐字节一致（difftest 第 7 步硬闸） |
| 并行 `--threads` | ✅ | 乱序并行解码 + reorder 保序刷出 + 反压；同输入任意 threads 输出字节一致 |
| 事件/库表过滤（`--db/--table/--ignore-*`、`--dml`、start/stop 窗口） | ✅ | |
| 输出形态（`--output-dir/--to-stdout/--file-per-table/--add-extra-info/--no-db-prefix/--full-columns` 等） | ✅ | |
| `flashback`（反向 SQL） | ❌ | P2 |
| `stats`（事件统计） | ❌ | P2 |
| repl 模式（伪装 replica 拉流） | ❌ | P3 |
| DDL 回滚 / `--apply` 直写库 / MariaDB / 8.0.1 default_metadata | ❌ | 明确不做（设计决策 D5） |
| fuzz 正式接入 / 影子库端到端回放 / musl 静态性能 | ❌ | P4（musl 构建本身已可用，见 docs/bench/p1.md） |

吞吐基线（DoD-3，发布态，528 MiB 合成 binlog）：**threads=8 ≈ 103.9 MiB/s**
（≥ 40 MB/s 通过），明细见 [docs/bench/p1.md](docs/bench/p1.md)。

## 快速上手

以下命令序列与输出均在本机（Linux, rustc 1.96, docker + mysql:8.0 镜像）实测：

```bash
# 0) 构建发布版
cargo build --release

# 1) 起一个开 ROW binlog 的一次性 MySQL 8.0（数据目录挂载 ./data/8.0；stdout 打印宿主映射端口）
PORT=$(bash tools/docker-mysql.sh 8.0)

# 2) 灌入全类型示例数据（换成你自己的业务 SQL 同理），并切一个新的 binlog 文件
docker exec -i my2sql-dt-8.0 mysql -uroot --default-character-set=utf8mb4 < tools/gen-data.sql
docker exec my2sql-dt-8.0 mysql -uroot -e "FLUSH BINARY LOGS"

# 3) 定位数据所在 binlog；datadir 属容器内 uid 999，借 root 助手容器放开宿主读取
BIN=$(docker exec my2sql-dt-8.0 mysql -uroot -N -e "SHOW BINARY LOGS" | tail -2 | head -1 | awk '{print $1}')
docker run --rm -u 0 -v "$PWD/data/8.0:/d" --entrypoint sh mysql:8.0 \
  -c 'chmod 755 /d && chmod a+r /d/mysql-bin.*'

# 4) 还原为 SQL（在线 schema 直连刚起的库）
./target/release/my2sql-rs to-sql \
  --binlog-dir data/8.0 --start-file "$BIN" \
  --uri "mysql://root@127.0.0.1:$PORT" --time-zone +00:00 \
  --threads 8 --to-stdout | head
```

输出（节选，含中文/emoji/GBK 十六进制字面量/JSON 保真渲染）：

```sql
INSERT INTO `dt`.`t_all` (`id`,`c_tiny`,...,`c_json`) VALUES (1,100,...,'2020-06-01 12:34:56.123456',...,'😀 emoji 😀',...,0x89504E470D0A1A0A,...,'{"b":1,"aa":[1,2,{"c":{"d":null,"e":"<&>"}}],"dec":9.99,"num":12.0}');
INSERT INTO `dt`.`t_utf8` (`id`,`s`,`t`) VALUES (1,'中文utf8','多字节文本测试');
INSERT INTO `dt`.`t_gbk` (`id`,`name`,`note`) VALUES (1,0xD6D0CEC447424BB2E2CAD4,0xD2FDBAC527D3EB5CB7B4D0B1B8DCD2D4BCB0BBBBD0D00ABFD8D6C6B7FB09);
UPDATE `dt`.`t_uk` SET `v`=11 WHERE `code`='A001';
UPDATE `dt`.`t_json` SET `j`='{\"n\":12.0,\"bb\":[1,2,{\"deep\":{\"中文\":\"😀\"}}],\"changed\":true}' WHERE `id`=1;
DELETE FROM `dt`.`t_nokey` WHERE `a`=2 AND `b` IS NULL AND `c` IS NULL;
```

不带 `--to-stdout` 时按默认写 `--output-dir`（可加 `--file-per-table` 分表落盘）。
断网/无 DB 场景：在线跑一次时加 `--schema-dump schema.json` 导出表结构，
之后用 `--schema-file schema.json` 替代 `--uri` 离线回放（输出逐字节一致）。
收尾清理：`docker rm -f my2sql-dt-8.0`。

## 差分测试（正确性底座）

本项目的正确性标准不是自说自话，而是与上游 Go my2sql 做**语义差分**：

```bash
make difftest   # 7 步：comparator 自检 → 构建 Go 裁判+Rust → mysql:8.0 产全类型矩阵
                # → 双方各自 to-sql → 语义比较（白名单闸口）→ 离线回放逐字节对差
make compat     # 全版本矩阵：5.6/5.7/8.0/8.4 × {差分, checksum 双态, V1 rows 探针,
                # 8.4 caching_sha2}，结果表 docs/compat/matrix.md
make test && make lint && make fmt   # 单元测试 / clippy -D warnings / rustfmt
```

- 裁判源码在 `reference/my2sql-go/`（只读，勿改动）；`make difftest` 每次经
  `go build` 现编到 `tools/bin/my2sql-go`（该目录不入库）——**差分测试需要本机
  Go 工具链**（脚本假定 `go` 在 PATH，含 `/opt/go/bin` 兜底）。
- 比较器 `tools/comparator/compare.py` 带自检（8 组正反例），语义等价判定 +
  显式白名单（每条差异都有编号与准入理由，见 `docs/HANDOVER.md` 挂账清单）。
- 需要 docker。除差分测试（`make difftest`/`make compat`）外不需要 Go 工具链。
- 吞吐基线：`bash tools/gen-bench-binlog.sh && cargo bench --bench decode`
  （输入缺失或 debug 编译档时 bench 自动跳过，不影响 `cargo test --all-targets`）。

## 与上游 Go my2sql 的行为差异（摘录）

以下差异为有意设计（多数登记在差分白名单中），迁移使用时请留意：

1. **CLI 全新设计**：子命令 `to-sql`，参数不再一一对应（如 `-local-binlog-file`
   → `--binlog-dir` + `--start-file`；过滤/窗口/输出参数全部重排）。
2. **离线 schema 回放**（上游无）：`--schema-dump` 导出 JSON 表结构，之后无 DB
   在线也可解码；导出格式有文档与单测钉死，可人工审阅修改。
3. **并行为「并行解码 + reorder buffer 保序单线程刷出 + 反压（阈值 2×threads）」**
   （D7，替代上游自旋锁方案）：任意 `--threads` 下输出字节确定一致（实测
   threads=1 与 8 产出 1,757,221 条语句完全相同）。上游多 uk 表还受 Go map
   随机序影响，本侧确定性选择。
4. **错误策略：skip + 计数，不终止整跑**。上游遇列数不对账、无变化 UPDATE 等
   直接 `Fatalf` 全局退出；本侧事件级计错（摘要行 `errors=N`）、其余事件照常
   产出。`--strict-schema` 可把 schema 列数不对齐从「补列 + warn」升级为硬错。
5. **输出文件名净化**：库/表名只做路径字节净化（防 `../` 越界与文件系统非法
   字符），SQL 文本面一字不改；上游按原名落文件，可越界写。
6. **V0（v1_row_events 之前的老行事件）与 PARTIAL image 硬拒**（D5）：给出明确
   InvalidData 错误而非猜测解码；上游会静默错解。V1/V2 事件均支持（真机矩阵钉死）。
7. **值保真链路全程 `Vec<u8>`**（D3）：文本列过 UTF-8 校验闸，非 UTF-8 数据以
   字节面保真（如 `0xHEX`），杜绝 Rust String 转换造成的静默损坏。
8. **blob / 非 UTF-8 文本字面量渲染为 `0xUPPERHEX`**，上游为 `X'lowerhex'` 或
   原样字节引号串——语义等价（ALW-BLOB-HEX）。
9. **JSON 渲染忠于 MySQL 显示规则**：对象键序 = 存储序（长度,memcmp）、数值按
   MySQL 文本规则（`12.0`/`1e21`/`-0.0`）、`<>&` 与 U+2028/9 不 HTML 转义；
   上游 = Go map 字典序 + `%v` + `json.Marshal` 转义（ALW-JSON-* 白名单，深比较
   判等）。防恶意 binlog 的实际口径：解码器对**已知**敌意输入做了 panic
   加固 + 回归闸（JSON 深度闸 100、DECIMAL 满组越界闸、截断/位图/charset
   畸形面，`tests/fuzz_seed/` 4 件种子逐字节钉死）；连续探索式 fuzz
   （cargo-fuzz 正式 campaign）归 P4，本工具不宣称穷尽防恶意 binlog。
10. **TIMESTAMP 零值渲染 `1970-01-01 00:00:00`**（MySQL 合法零值语义）；上游
    go-mysql 走 `formatZeroTime` 输出 `0000-00-00`（ALW-ZERO-TIMESTAMP）。
11. **UPDATE 仅输出变化列**（before/after 逐列对比后省略等值列）；上游 SET 段
    恒含全部列（ALW-JSON-IN-SET），且无变化 UPDATE 会让上游整跑终止。
12. **WHERE 多条件不带括号**（`a=1 AND b=2`，上游 `(a=1 AND b=2)`）、标识符中
    反引号加倍转义（上游原样包裹产生坏 SQL）（ALW-WHERE-PARENS / ALW-IDENT-BACKTICK）。
13. **SQL 文件头部含 `SET NAMES utf8mb4;`**（计划约束，Go 上游无此行）；
    `--to-stdout` 与文件模式统一字节面（上游屏幕模式只打裸语句）。
14. **CRC32 逐事件校验，且比上游更严**：FDE 的校验和也校验（含 mysqld
    「先算校验后置 flag 位」特例口径）；go-mysql 对 FDE 完全不校验。
15. **file 模式 DDL/Query 事件不产出 SQL**（与上游一致，仅喂事务状态机），
    flashback/DDL 处理明确不在一期范围（D5）。

## 文档

- 设计权威：`docs/superpowers/specs/2026-09-20-my2sql-rust-design.md`
- 进度/决策/白名单台账：[docs/HANDOVER.md](docs/HANDOVER.md)
- 吞吐基线明细：[docs/bench/p1.md](docs/bench/p1.md)
- 版本兼容矩阵：[docs/compat/matrix.md](docs/compat/matrix.md)
- 模糊测试种子语料：`tests/fuzz_seed/`（P4 fuzz 正式接入的起点语料）
