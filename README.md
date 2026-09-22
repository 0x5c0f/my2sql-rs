# my2sql-rs

MySQL binlog → SQL 还原工具的 Rust 独立实现（to-sql / flashback / stats / repl），
能力对齐 Go 版 [my2sql](https://github.com/ultradb/my2sql)（本仓库内
`reference/my2sql-go/` 作为行为参考与差分裁判），但 CLI 全新设计、无 async
（std::thread + crossbeam-channel）。当前处于 **P3：file 模式 to-sql /
flashback / stats 与复制协议拉流模式 `repl`（× to-sql 流式形态）已达发布
标准**（flashback/stats × repl 明确不做，见 spec §0）。P3 之后 **P4a
质量并行面已合入**：cargo-fuzz 正式闸（`make fuzz-min`）、影子库三段回放
（`make shadow-test`）、difftest 三列形捕获（`P4A=1 make difftest`）、
5.6/5.7 idle 心跳 live 件（`make repl-test` 家族 13 件）——逐字回归台账见
[docs/HANDOVER.md](docs/HANDOVER.md)「P4a 任务节点日志」与「P4a DoD 对账」节。

## 功能矩阵

| 能力 | 状态 | 说明 |
|---|---|---|
| `to-sql` file 模式（binlog 目录离线读取） | ✅ | `make compat` 既有 14 用例全绿（= to-sql 族 8 + flashback 族 4 + stats 冒烟 2；P3 后 `make compat` 整跑为 18 用例 = +repl 族 4，逐字结果见 [docs/compat/matrix.md](docs/compat/matrix.md)） |
| MySQL 5.6 / 5.7 / 8.0 / 8.4 | ✅ | ROW 格式；full/minimal/noblob 镜像；CRC32/NONE 校验和；V1/V2 rows 事件（V0 硬拒，见下文差异 6） |
| 在线 schema（`--uri`） | ✅ | mysql://user@host:port；8.4 caching_sha2 与 native 双通过 |
| 离线 schema 回放（`--schema-file` / `--schema-dump`） | ✅ | 无 DB 可解码；导出→回放逐字节一致（difftest 第 7 步硬闸）；三形态（to-sql/flashback/stats）均消费 `--schema-dump` |
| 并行 `--threads` | ✅ | 乱序并行解码 + reorder 保序刷出 + 反压；同输入任意 threads 输出字节一致 |
| 事件/库表过滤（`--db/--table/--ignore-*`、`--dml`、start/stop 窗口） | ✅ | |
| 输出形态（`--output-dir/--to-stdout/--file-per-table/--add-extra-info/--no-db-prefix/--full-columns` 等） | ✅ | |
| `flashback`（反向/回滚 SQL，记录原子逆序 + keep-trx 事务脚手架） | ✅ | Go `-work-type rollback` 裁判差分 4 版本全绿（`flashback-{5.6,5.7,8.0,8.4}`）+ `WORK_TYPE=rollback make difftest` + 活库正逆对账（`tools/flashback-reconcile.sh`）；DDL 反向明确不做（D5） |
| `stats`（窗口×表 DML 行数 + 大/长事务识别，两报表 + JSONL） | ✅ | `stats-{5.6,8.0}` 冒烟绿（报表 DML 总和 == 同流 to-sql 行数）；上游报表字节面复刻，**不做**裁判差分（spec §3.6，理由见差异 22） |
| repl 模式（伪装 replica 拉流，to-sql 流式形态） | ✅ | 事务边界 checkpoint + `--resume-file` 接续 + 指数退避自动重连 + 心跳探活（超集四件，见差异 23）；**等价性总闸**：repl 与 file 模式同 binlog 段产出逐字节一致；`make repl-test` live 件 13 项全绿（P4a +5.6/5.7 idle 心跳两件）、`make compat` 18 用例（既有 14 + repl×4 版本，逐字见 matrix.md）；**不做**与 Go 裁判差分（上游 repl 不可作裁判，理由见差异 25）；TLS 不提供（差异 26） |
| DDL 回滚 / `--apply` 直写库 / MariaDB / 8.0.1 default_metadata | ❌ | 明确不做（设计决策 D5） |
| fuzz 正式接入（cargo-fuzz 两靶 + 确定性语料 + 300s 闸） | ✅ | `make fuzz-min`：`fuzz/` 独立 workspace，靶 decode_event/event_stream 各 300s（`FUZZ_TIME=<秒>` 缩窗），0 新 crash 判据；起点语料 = `tests/fuzz_seed/` 同源 seedgen 确定性 40 件（禁随机入仓）；两靶 300s×2 真跑 0 crash 证据（2026-09-22，T5 合流轮逐字）见 HANDOVER P4a 节 |
| 影子库端到端回放（前向/逆向/往返三段） | ✅ | `make shadow-test [VER=…]`：to-sql 产物灌影子库==主库后态、flashback 产物==前态、往返回前态，逐表 CHECKSUM + 行级 diff 双腿无假绿；8.0 spec 原形态（live 锚），5.7 REF-clone 锚（JSON checksum 上游不可定值，豁免仅 checksum 腿——裁定见 HANDOVER）；`SHADOW_NEGCHECK=1` 负自检验钞机 |
| P4A 列形捕获（ENUM>255 / GEOMETRY / LONGBLOB>64K） | ✅ | `P4A=1 make difftest`（仅 8.0 主闸）：三形 Go 裁判差分 14/14 组全绿 + 自 roundtrip checksum/行级双门；三形均裁判支持、无新增行为差异（登记见差异 28），明细 docs/p4a-findings.md |
| musl 静态性能 | ✅ | **P4b mimalloc 消账**：接入全局 mimalloc 后 musl release 端到端 threads=8 **64.2 MiB/s**（此前 3.3 MiB/s 的 musl malloc arena 悬崖，见 [docs/bench/p4b.md](docs/bench/p4b.md) ④）；mimalloc 现为**无条件硬依赖**（无 `--no-default-features` 逃生，musl 交叉编译含其 C 核） |

吞吐基线（DoD-3，发布态，528 MiB 合成 binlog，criterion
`cargo bench --bench decode`）：**当前权威基线 threads=8 median 127.59 MiB/s**
（P4b mimalloc 落地后终态跑，≥40 MB/s 通过，回归闸 vs P1 真值 103.85 MiB/s
**+22.86% 更快 → GREEN**）——逐字基线、三套口径（criterion 账本 / `make bench-ab`
端到端 A/B / `make bench-profile` 曲线）与挂账 #7「P1→P2 代码增量复测钉死不显著」
见 [docs/bench/p4b.md](docs/bench/p4b.md)。历史账本：P1 基线 **103.9 MiB/s**
（[docs/bench/p1.md](docs/bench/p1.md)）；P2 回归闸（spec §6.4）原始读数 −14.9%，
同机 A/B 归因为环境漂移 −8.3% + 代码增量 −3.2%（95% CI 跨 0，未达 5% 判定线）
——P4b 用 `tools/bench-ab.sh` 工装复测将该 −3.2% 弱信号**钉死为端到端不显著**
（delta +2.812%＝0.1897s < 阈值 0.5714s，N=5，不升级 N=9），证据与测量陷阱见
[docs/bench/p2.md](docs/bench/p2.md) + [docs/bench/p4b.md](docs/bench/p4b.md) ③。

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

同一区间做**回滚 SQL** 与**统计报表**（以下两条同样实测；形态出自
`tools/run-difftest.sh` 第 5 步与 `tools/flashback-reconcile.sh`）：

```bash
# 5) flashback：逆向 SQL 落文件（记录原子逆序 + keep-trx 事务脚手架；
#    DDL/非事务 QUERY 不进脚本、stderr 汇总告警——见差异 21）
./target/release/my2sql-rs flashback \
  --binlog-dir data/8.0 --start-file "$BIN" \
  --uri "mysql://root@127.0.0.1:$PORT" --time-zone +00:00 \
  --threads 8 --output-dir out/flashback
# flashback done: events=21, statements=36, files=1, errors=0
# （out/flashback/flashback.3.sql 头部：`SET NAMES utf8mb4;` / `commit;` /
#   `begin;` / `INSERT INTO \`dt\`.\`t_nokey\` …`——末事务最先反序）

# 6) stats：窗口×表 DML 行数 + 大/长事务两报表（--stats-json 追加 JSONL）
./target/release/my2sql-rs stats \
  --binlog-dir data/8.0 --start-file "$BIN" \
  --uri "mysql://root@127.0.0.1:$PORT" --time-zone +00:00 \
  --threads 8 --output-dir out/stats
# stats done: events=60, statements rows=36, windows flushed=1,
#             big/long trx=0, skipped=0
# binlog_status.txt 首条数据行（截尾空格；line 1 为列头）：
# mysql-bin.000003  2026-09-21_07:30:31 2026-09-21_07:30:31 1605       34706      7        1        0        dt              t_all
```

同一区间做**复制协议持续拉流**（repl；形态与 `tests/repl.rs` live 件及
`tools/compat-matrix.sh` repl 族同款，该套件实测全绿——逐字证据账见
`docs/HANDOVER.md`「P3 DoD 对账」节；需真实主库，可直接用上面 1)-2) 起的实例）：

```bash
# 7) repl 首跑：now 哨兵（--start-file ""，= 跑时 SHOW MASTER STATUS 取当前
#    位点，只看新流量）；checkpoint 自动落 {output-dir}/resume.json（事务边界
#    原子写）。不给 stop 条件即常驻拉流；Ctrl-C 优雅收尾（exit 130；
#    空闲期响应延迟 ≤ 一个心跳周期，默认 30s——--heartbeat-secs 0 则失此保障）。
./target/release/my2sql-rs repl \
  --binlog-dir /nonused --start-file "" \
  --uri "mysql://root@127.0.0.1:$PORT" --server-id 9527 \
  --time-zone +00:00 --output-dir out/repl \
  --stop-datetime "$(date -u -d '+30 seconds' '+%Y-%m-%d %H:%M:%S')"
# repl done: events=N, statements=N, files=N, errors=0
# （--binlog-dir 在 repl 下为 clap 占位——事件字节来自复制流，不读本地文件）

# 8) 中断（含 kill -9）后从 checkpoint 接续：**必须换新的 --output-dir**
#    （repl 永不 append 既有产物，防覆盖闸见差异 23）；--resume-file 指旧档，
#    并按位点三态哨兵显式清零（--start-file "" --start-pos 0）。
./target/release/my2sql-rs repl \
  --binlog-dir /nonused --start-file "" --start-pos 0 \
  --uri "mysql://root@127.0.0.1:$PORT" --server-id 9528 \
  --time-zone +00:00 --output-dir out/repl-2 \
  --resume-file out/repl/resume.json
```

- 位点三态互斥（validate 硬错「位点来源歧义」）：now 哨兵
  `--start-file ""` / `--start-file F [--start-pos P]`（pos 默认 4）/
  `--start-datetime T`（二分 `SHOW BINARY LOGS` 定位后逐事件过滤）；
  配 `--resume-file` 时须显式带清零哨兵 `--start-file "" --start-pos 0`
  （`--start-file` 是 clap 级必填，清零即「无独立 start」，不与 resume 互斥）。
- `--server-id` 必填无默认（差异 24）；`--heartbeat-secs` 默认 30（0=禁用，
  连续 2×间隔无事件判死链走重连；空闲 master 上 Ctrl-C 停泵延迟以心跳周期
  为界——**0 同时禁用死链探测与空闲期即时中断**；`--stop-datetime`/
  `--stop-pos` 仍需等下一个真实事件才判定）；
  `--resume-file` 与 `--to-stdout` 互斥。
- 输出目标必须显式（终审 FIX F）：`--output-dir` 与 `--to-stdout` 二选一，
  两者皆缺启动即拒——**repl 绝不写当前工作目录**；`--resume-file` 恒需
  `--output-dir`（checkpoint 与盘上产物对账）。
- 语义 = **每事务至少一次**：崩溃重放最多重复 checkpoint 之后的完整事务，
  绝不半途切开；重复段可由产物与 `written_files` 名单界定
  （kill-9 接续零丢失由 live 件 `repl_kill9_resume_zero_loss` 钉死）。
  resume 启动对账只对「名单承诺而盘上缺失」硬错；盘上多出的未登记残骸
  （撕裂事务半块等）告警放行，不死锁恢复。
- 一键回归：`make repl-test`（起一次性 mysql:8.0 容器跑 tests/repl.rs 全部
  live 件，`VER=5.7 make repl-test` 换版本；P4a 合流轮全家族实测
  13 passed / 0 failed / 823.02s，含 5.6/5.7 idle 心跳两件）。

收尾清理：`docker rm -f my2sql-dt-8.0`。

## 差分测试（正确性底座）

本项目的正确性标准不是自说自话，而是与上游 Go my2sql 做**语义差分**：

```bash
make difftest   # 7 步：comparator 自检 → 构建 Go 裁判+Rust → mysql:8.0 产全类型矩阵
                # → 双方各自 to-sql → 语义比较（白名单闸口）→ 离线回放逐字节对差
                # WORK_TYPE=rollback|stats make difftest → flashback 裁判差分 /
                #   stats 冒烟配平（同 7 步骨架，产物目录加 -rb/-stats 后缀）
                # P4A=1 make difftest → 追加 P4a 三列表组（ENUM>255 / GEOMETRY /
                #   LONGBLOB>64K；仅 8.0 主闸，默认关零影响）
make compat     # 全版本矩阵 18 用例：5.6/5.7/8.0/8.4 × {差分, checksum 双态,
                # V1 rows 探针, 8.4 caching_sha2} + flashback×4 + stats 冒烟×2
                # + repl 族×4（repl==file 逐字节等价，无 Go 裁判，见差异 25），
                # 结果表 docs/compat/matrix.md
make repl-test  # repl live e2e 套件（一次性 mysql:8.0 容器：等价性总闸、kill-9
                # 接续、容器重启自动重连、位点三态/stop/心跳、threads>1 水位、
                # P4a 5.6/5.7 idle 心跳两件；VER=<版本> 换镜像，
                # --test-threads=1 串行）
make fuzz-min   # P4a fuzz 正式闸：两靶各 300s 真跑（FUZZ_TIME=<秒> 缩窗），
                # crash artifact 落 out/fuzz/<靶>/，新 crash 或运行失败即红
make shadow-test  # P4a 影子库三段闸（VER=<版本> 换版本；SHADOW_NEGCHECK=1
                  # 负自检验钞机；KEEP=1 失败保留容器排障）
make test && make lint && make fmt   # 单元测试 / clippy -D warnings / rustfmt
```

- 裁判源码在 `reference/my2sql-go/`（只读，勿改动）；`make difftest` 每次经
  `go build` 现编到 `tools/bin/my2sql-go`（该目录不入库）——**差分测试需要本机
  Go 工具链**（脚本假定 `go` 在 PATH，含 `/opt/go/bin` 兜底）。
- 比较器 `tools/comparator/compare.py` 带自检（9 组正反例，含 rollback 模式
  结构断言组），语义等价判定 + 显式白名单（每条差异都有编号与准入理由，见
  `docs/HANDOVER.md` 挂账清单）。
- 需要 docker。除差分测试（`make difftest`/`make compat`）外不需要 Go 工具链。
- 吞吐基线：`bash tools/gen-bench-binlog.sh && cargo bench --bench decode`
  （输入缺失或 debug 编译档时 bench 自动跳过，不影响 `cargo test --all-targets`）。
  端到端 A/B 判定与 threads 曲线普查经 `make bench-ab ARGS="--a <binA> --b <binB>
  [--rounds N]"` / `make bench-profile` 直通（P4b 工装，taskset 钉 P 核、
  median+MAD 判显著，见 [docs/bench/p4b.md](docs/bench/p4b.md)）。
- `examples/repl_spike.rs` 为 P3 Task 0 协议 spike 的**诊断样例**（throwaway，
  按裁决保留供排障复跑；`src/` 对其零引用，不参与任何测试/发布链路）。

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
   畸形面，全部 `tests/fuzz_seed/` 种子（现 7 件）逐字节钉死）；连续探索式 fuzz
   （cargo-fuzz 正式 campaign）P4a 已接入为常态闸（`make fuzz-min` 两靶 300s
   0 新 crash + `tests/fuzz_seed/` 病理语料回归钉），但本工具不宣称穷尽防恶意
   binlog。
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
    flashback 对 DDL 的处置策略见差异 21（P2 已落地，D5 边界不变）。
16. **`rollback` 更名 `flashback` 且语义分层重做**（P2）：子命令
    `flashback` 对应上游 `-work-type rollback`；逆向 DML 语义等价
    （INSERT↔DELETE 互逆、UPDATE 的 SET=before 值 / WHERE=after 值），
    但**不继承**上游「JSON 列恒进 SET」quirk——逆向与正向同样按实际
    diff 省略未变化列（活库对账实证，tools/flashback-reconcile.sh）。
17. **记录原子化注释绑定**（超越项，P2）：extra-info 的「注释行+SQL 行」
    为一个原子单元整体参与逆序；上游
    `reference/my2sql-go/base/rollback_process.go` 按**裸行**逆序，注释
    与其 SQL 拆对、漂移到事务组尾部（比较器 ALW-RB-COMMENT-DRIFT 吸收
    该裁判面差异，我方产物为修正形态）。
18. **keep-trx 默认开 + 显式开关**（P2）：上游 `KeepTrx` 是纯 struct
    字段（context.go:126），`InitFlags`（context.go:184-233）**无任何
    旗标绑定** → 恒为 Go 零值 false，`-work-type rollback` 的裁判产物
    天然无事务脚手架。我方 `--keep-trx` 默认开（注入 `begin;`/`commit;`
    逐字节对齐上游注入位置，含首部悬空 `commit;` 原样 quirk 与文件尾
    `commit;`），`--no-keep-trx` 得纯逆序无脚手架形态（差异 矩阵级登记：
    docs/compat/matrix.md「口径勘误」条）。
19. **flashback 产物头部注入**（P2）：`--on-error skip-bad-event` 显式
    带病产出时，文件头注入 `-- WARNING: skipped N events, positions in
    stderr`（上游坏输入即 `Fatalf` 终止、无对应形态，ALW-RB-WARN-HEADER）；
    正向的 `SET NAMES utf8mb4;` 头行在 flashback 产物中沿用（上游
    rollback 文件无头行，ALW-SETNAMES-HEADER 同闸）。
20. **stats 报表表序确定化**（超越项，P2）：上游窗口 map 与 biglong
    明细 map 均为 Go map（stats_process.go:156,67），同输入跨运行行序
    随机；我方窗口行 = 表**首次出现序**、`[db.tb(...)]` 明细 = db.tb
    升序（逐字节可复现，golden/冒烟断言的前提）。
21. **flashback 的 DDL 排除策略与完整性硬规则**（P2，spec §3.2/D5）：
    DDL/非事务 QUERY 事件**绝不进**反向脚本且绝不猜测其反向——收尾在
    stderr 汇总告警（datetime+位点+原文）交用户决策；另有三条硬规则
    （Padded 结构删列 / MINIMAL Missing 值参与逆向 WHERE/VALUES 即报错，
    `--on-error` 默认 **stop** 整跑中止不留半成品）对齐并显式化上游
    events.go:87 的 fail-hard 立场。
22. **stats 不做裁判差分**（P2，spec §3.6）：上游 stats 报表为自由文本
    且窗口落盘时机受 print-interval/喂入时序影响，语义差分判据不成立——
    以 golden 逐字节断言（tests/stats.rs + src/stats 单测同源串）+
    冒烟配平（报表 DML 总和 == 同流 to-sql 行数）为准；裁判 stats 输出
    仅作人工对照留档 `out/difftest-*-stats/go-stats/`。

P3（repl）追加：

23. **repl = 对上游 repl 的功能超集（四件）**（P3，spec §8）：
    ① 事务边界 checkpoint + `--resume-file` 断点接续（at-least-once 语义，
    `written_files` 审计清单对账）；② 指数退避自动重连（1s→30s 封顶+抖动，
    无限次；认证/权限/purge/server-id 冲突为终止类硬错）；③ 心跳探活
    （`--heartbeat-secs`，连续 2×间隔无事件判死链，TCP 半开兜底）；
    ④ resume 防覆盖闸（自设安全语义：repl 永不 append 既有文件，接续产物
    永远进新 `--output-dir`；冲突在首个冲突文件的创建时刻以 `create_new`
    /O_EXCL 原子拒绝并报该冲突名——逐次一个、race 安全，非启动预扫）——上游
    `-mode` 断线即 `log.Fatalf` 终，四件全无对应物。
24. **repl 强制显式 `--server-id`（无默认）**：上游有默认值——同宿主
    server-id 冲突表现为对端强制断连的静默互踢，我方拒绝代答；另以
    「连续 3 次同因秒断即终止报错」防无限互踢。
25. **repl 不做裁判差分**（P3，spec §8）：上游 Go repl 无优雅停止、
    `log.Fatalf` 即崩、syncer 泄漏、无 checkpoint/重连/心跳，不具备裁判
    资格——正确性以**内部等价性总闸**替代：同段 binlog 上 repl 与 file
    模式产出逐字节一致（8.0 主件 + 5.6/5.7/8.0/8.4 矩阵 4/4，
    `make repl-test`/`make compat` 钉死，逐字见 docs/compat/matrix.md）。
26. **repl `--uri` 不提供 TLS**：mysql 28.0.2 的 URL 参白名单不含任何 ssl
    项且未知参数（如 `ssl-mode`）直接硬错（spike 实测）；TLS 需 crate
    feature + 程序化 SslOpts，P3 不启用，如实登记。
27. **上游 repl quirk 不继承清单**（P3，spec §8）：`repl.go:96` Fatalf
    吞错误参数、start-pos 无 table-map 时 `tbMapPos=0`、RawData 丢弃、
    charset 硬编码 utf8——均采我方 file 模式既有正确行为（与裁判差分
    同源的解码权威唯一性：repl 事件经 `Event::write` 重建为与磁盘文件
    逐字节同构的帧后喂同一解码器，零第二解码路径）。

P4a（质量面）追加：

28. **P4a 三列形真机捕获登记（差异清单续号；无新增行为差异）**（spec §3，
    逐字台账 docs/p4a-findings.md）：ENUM >255 成员（2B packlen，边界序号
    255/256/300 行级实证）、GEOMETRY POINT/LINESTRING/POLYGON（SRID 4326
    字节保真首次实抓）、LONGBLOB >64KB（4B 长前缀 + 跨页 280,000B 单事件）
    三形**全部 Go 裁判支持**：`P4A=1 make difftest` 14/14 组全绿（8.0），
    双方语句在既有三类打印差异（JSON `null`/`NULL`、hex 字面量
    `X'小写'`/`0x大写`、UPDATE 多列 `, `/`,`——均为既有白名单成员
    ALW-JSON-*/ALW-BLOB-HEX/ALW-WHERE-PARENS 同族口径，非新增差异）归一后
    md5 相同；两家 ENUM 均输出
    **1-based 序号**（裁决 D4 现状），非成员名字符串保真。挂账清单
    「测试债三列形覆盖缺口」由本件销账。

## 文档

- 设计权威：`docs/superpowers/specs/2026-09-20-my2sql-rust-design.md`
- P2 设计/计划：`docs/superpowers/specs/2026-09-21-my2sql-rs-p2-flashback-stats-design.md`、
  `docs/superpowers/plans/2026-09-21-my2sql-rs-p2-flashback-stats.md`
- P3 repl 设计/计划：`docs/superpowers/specs/2026-09-21-my2sql-rs-p3-repl-design.md`、
  `docs/superpowers/plans/2026-09-21-my2sql-rs-p3-repl.md`
- 进度/决策/白名单台账：[docs/HANDOVER.md](docs/HANDOVER.md)
- 吞吐基线明细：[docs/bench/p1.md](docs/bench/p1.md)（P1 基线）、
  [docs/bench/p2.md](docs/bench/p2.md)（P2 回归闸与未判定 finding）、
  [docs/bench/p4b.md](docs/bench/p4b.md)（**P4b 当前权威基线 + mimalloc A/B +
  挂账 #7 复测**）、[docs/bench/p4b-profile.md](docs/bench/p4b-profile.md)
  （P4b profile 普查：threads 曲线 / 假设判定 / 优化候选排序表）
- 版本兼容矩阵：[docs/compat/matrix.md](docs/compat/matrix.md)
- 模糊测试：种子语料 `tests/fuzz_seed/`（回归闸 `tests/fuzz_seed.rs`）+ P4a 起
  `fuzz/` cargo-fuzz workspace 正式接入（seedgen 单源确定性语料，
  `make fuzz-min` 直通），不再是「起点语料待接入」形态
