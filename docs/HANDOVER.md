# my2sql-rs 交接文档（HANDOVER）

> 每完成一个节点统一更新本文件。目标读者：接手项目的任何人（含未来子代理/人类协作者）。
> 设计权威：`docs/superpowers/specs/2026-09-20-my2sql-rust-design.md`；P1 执行计划：`docs/superpowers/plans/2026-09-20-my2sql-rs-p1.md`。

## 项目一句话

Rust 独立重写 MySQL binlog 解析工具（to-sql / flashback / stats），能力对齐 Go 版 my2sql 但 CLI 全新设计；`reference/my2sql-go/` 为行为参考与差分测试裁判（不入库、勿改动）。

## 关键决策记录（不可回退项）

| # | 决策 | 理由 |
|---|---|---|
| D1 | 解码层全自研（方案 A），仅复用 mysql_common 协议原语 | 市面 Rust 库解码覆盖参差，静默错 SQL 不可接受 |
| D2 | 无 async：std::thread + crossbeam-channel | 批处理+单连接拉流，tokio 无收益 |
| D3 | 值保真总原则：列值全链路 `Vec<u8>`，datetime 族解码为字符串，不经 chrono/sqltypes 式转换 | 防 UTF-8/时区静默损坏 |
| D4 | ENUM/SET 输出序号（与 Go 裁判一致）；DECIMAL 精确解码（超越项，差分白名单） | 可比性+正确性 |
| D5 | 一期不做：DDL 回滚、--apply、MariaDB、8.0.1 default_metadata（检测到报错不猜） | YAGNI/安全 |
| D6 | 兼容矩阵含 MySQL 5.6/5.7/8.0/8.4（5.6 需显式 row 格式，报错给指引） | 用户验收要求 |
| D7 | 保序=reorder buffer 单线程刷出，反压阈值 2×threads | 替代上游自旋锁 |

## 当前进度

- 分支：`feat/p1`（main 只有文档）
- 里程碑：P1 计划 17 任务（执行序 1..15, 17, 16）
- 状态：**未开始实施**（本文档创建于 Task 1 派发前）

## 任务节点日志

（每任务完成追加一节：做了什么/关键接口/遗留项/对后续任务的影响）

## 环境事实

- 本机：docker（镜像 mysql:5.6/5.7/8.0 已就绪，8.4 需拉取）、Go 工具链 /opt/go/bin、cargo/rustc 最新 stable
- 工作区：`/home/cxd/Projects/aiediter/my2sql`
- SDD 台账：`.superpowers/sdd/2026-09-20-my2sql-rs-p1/progress.md`（git-ignored，恢复上下文先读它）

## 遗留/挂账清单

- [ ] P2：flashback + stats（另出计划）
- [ ] P3：repl 模式（另出计划；认证含 caching_sha2）
- [ ] P4：fuzz 正式接入、影子库端到端回放、musl 静态构建
- [ ] spec §4.6 ENUM/SET 名称注释 → 推迟至 P2
