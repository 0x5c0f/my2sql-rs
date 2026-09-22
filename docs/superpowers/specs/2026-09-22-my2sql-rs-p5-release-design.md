# my2sql-rs P5「发布面」设计（Release Frontier）

> 日期：2026-09-22。前序战役：P1（v0.1.0-p1）/ P2（v0.2.0-p2）/ P3（v0.3.0-p3）/
> P4a（v0.4.0-p4a）/ P4b（v0.4.1-p4b）全部收官并推送。
> 权威上位 spec：`docs/superpowers/specs/2026-09-20-my2sql-rust-design.md` §9
> 「P4：影子库端到端回放测试、fuzz 补齐、性能调优、**文档/发布**」——本计划消费
> 其中唯一未动面「文档/发布」。
> 交接权威：`docs/HANDOVER.md`（状态行、挂账清单、环境事实）。

## 0. 现状勘察（本 spec 的事实基座，全部实测）

| # | 事实 | 证据 |
|---|---|---|
| F1 | 包版本与里程碑脱节：`Cargo.toml:3 version = "0.1.0"`，五轮战役后二进制自述仍是 0.1.0 | `grep -n version Cargo.toml` |
| F2 | 无任何 CI：仓库无 `.github/` 目录 | `test -d .github` → 缺失 |
| F3 | 无 CHANGELOG、无 GitHub Release | `ls` + `gh release list`（0 条） |
| F4 | 远端已推 5 枚里程碑 tag（v0.1.0-p1…v0.4.1-p4b），**不可移动**（已公开） | `git ls-remote origin` |
| F5 | `gh` CLI 已认证（account 0x5c0f，scopes 含 repo），repo 为 PRIVATE | `gh auth status` / `gh repo view` |
| F6 | `--version` 版本单源 = clap `#[command(name, version)]`（Cargo CARGO_PKG_VERSION），改 Cargo.toml 即全链路生效 | `src/config.rs:43-44` |
| F7 | README 状态行过期：仍写「当前处于 P3 …P4a 质量并行面已合入」，无 P4b 态、无发布/安装节 | `README.md:6-12` |
| F8 | 上位 spec §1/§3 目标「单二进制静态发布」在 P4b mimalloc 消账后已可双目标兑现（musl release 64.2 MiB/s ≥50 门） | docs/bench/p4b.md ④ |
| F9 | mimalloc 为无条件硬依赖，musl-gcc 交叉编译在本机实测可编 | Cargo.toml:12 + P4b T3（`281735d`） |
| F10 | `.qoder/`（harness 目录）在主仓 untracked，未入 .gitignore | `git status --porcelain` |
| F11 | 本地六闸回归体系齐备（test/clippy/fmt、fuzz-min、shadow、difftest×2、compat、repl-test），P4b 全绿 | HANDOVER「P4b DoD 对账」#7 |

## 1. 目标与范围

**目标**：把仓库变成「可对外发布」状态——版本真身对齐、门禁上云（CI）、
GitHub Release 双目标产物、发布文档收口；以 v0.5.0 对外。

**硬边界**：`src/` 零语义改动。本战役允许触碰的 Rust 代码面 = Cargo.toml
version 一行（+Cargo.lock 同步）。任何「顺手改行为」都是越界。

在范围内：
- Cargo.toml/Cargo.lock 版本 0.5.0（含 `--version` 输出核验）
- `.github/workflows/ci.yml`（push/PR 门禁）
- `.github/workflows/release.yml`（tag 触发双目标产物 → GitHub Release）
- `CHANGELOG.md`（新建，五轮里程碑倒序，数字全部逐字引既有 artifact）
- README 终审（状态行更新到 P4b 后态、新增安装/发布节、存量「实测」声明核对）
- `.gitignore` 补 `.qoder/`
- 同族小账：`tools/p4a-roundtrip.sh` / `tools/flashback-reconcile.sh` 硬编码
  target 改 `${CARGO_TARGET_DIR:-$ROOT/target}` 口径（edb2148/b6844fe 同型，
  脚本卫生非行为面）
- 收口：push → CI 实测绿 → tag v0.5.0 → Release 创建 → 产物下载回验

不在范围内（明确不做）：
- 把 difftest / compat / repl-test / fuzz-min / shadow-test 搬进 CI（见 §3 D3）
- crates.io 发布（未申请；GitHub Release 即本战役发布形态）
- 任何性能机动（X3 归因、threads 2.01× 结构面等——全部续挂，见 §6）
- TLS、DDL 回滚、MariaDB（D5 边界不变）
- 历史 tag 任何改动（F4）

## 2. 任务分解（4 任务；T1/T2/T3 并行 → T4 串行收口）

- **T1 版本+门禁面**：Cargo.toml 0.5.0 + Cargo.lock、`.gitignore`、
  `ci.yml`、两脚本 target 接线、版本单源核验（`--version` 输出逐字）。
- **T2 文档面**：`CHANGELOG.md` 新建 + README 终审（状态行、发布/安装节、
  虚账排查：凡「实测」声明对照现存证据）。
- **T3 发布工件面**：`release.yml` + 本机双目标构建试验（产物命名、
  SHA256SUMS 生成、下载-回验脚本形；**不**对远端创建任何 Release）。
- **T4 收口（串行独占远端面）**：合流全量回归 → push main → 轮询 CI run
  至绿（红 = 修复轮重推）→ annotated tag `v0.5.0` → push tag → 按定稿的
  R3 次序完成 Release（默认形：`gh release create --draft` 先建 → release.yml
  构建上传 → 回验后 undraft）→ 从 Release 下载回验 sha256 与本机试验一致 →
  HANDOVER/README 终态 + 台账。
  （R3 = 「Release 创建与产物上传的触发-次序链」，T3 试验定稿项，见 D5。）

并行纪律：T1/T2/T3 文件面互斥（T1=Cargo/.github/ci/tools 脚本，T2=docs，
T3=.github/release.yml+tools/release-*）；T1 与 T3 均触 `.github/workflows/`
但**不同文件**；远端（push/tag/Release）只允许 T4 触。

## 3. 关键决策（裁定入册）

- **D1 版本 = `0.5.0`，tag = `v0.5.0`（无战役后缀）**。
  理由：五枚历史 tag 已在远端不可动，包版本必须 > 0.4.1 历史；0.x 系列内
  次版本号跳动即里程碑语义；此后 git tag 与包版本紧耦合（`v$(cargo metadata)`
  一致性可校验），回归标准做法。旧后缀 tag 保留为战役里程碑，不迁移不删除。
- **D2 版本单源不动**：clap `version` 走 CARGO_PKG_VERSION（F6），CI/CD 内
  以 `cargo pkgid` / `cargo metadata` 读取，禁第二处写死版本字面量
  （release.yml 用 `grep '^version' Cargo.toml` 或等价物，workflow 内不出现
  `0.5.0` 字面量）。
- **D3 CI 有界面（明示不搬什么）**：ci.yml 门 = fmt-check + clippy
  `-D warnings`（--all-targets）+ `cargo test --no-fail-fast`（live 件
  #[ignore] 自然跳过）+ musl 编译门（`cargo build --release --target
  x86_64-unknown-linux-musl`，不运行）+ fuzz 靶编译门（`cargo check` on
  fuzz workspace，若 runner 无 nightly 则如实降级为不门——Step 实测裁定，
  禁虚账）。difftest/compat/repl-live/fuzz 真跑/shadow 全部**不进 CI**：
  需要 docker 多版本 MySQL + Go 裁判 + ~15 分钟 live 时窗，本地六闸体系
  （F11）是既有权威门禁；此决定逐字写入 HANDOVER 与本 spec，供未来读者
  知道「CI 绿 ≠ 六闸绿」。
- **D4 产物命名 machine-readable**：
  `my2sql-rs-<ver>-x86_64-unknown-linux-gnu` /
  `my2sql-rs-<ver>-x86_64-unknown-linux-musl` / `SHA256SUMS`（sha256sum 格式，
  ASCII 名，无压缩包——单二进制即发布物，musl 静态）。
- **D5 Release 创建走 `gh release create`**（控制器本机，T4）而非
  softtag+upload-artifact 全云端链：私有仓 Actions 产物下载需带 token 认证，
  `gh` 已具备；release.yml 只负责构建与上传到既有 Release（`gh workflow run`
  /tag 触发），body 取 CHANGELOG 对应节。两条路在 T3 试验时按 Actions 实际
  能力定稿（若 upload-artifact 链路全通则优先全自动，D5 降为兜底）——
  定稿记号 = **R3**（触发-次序链），T4 按 R3 执行。
  裁定原则：**产物可被匿名成员之外的人（即 repo 协作者）以最小步骤取到**。
- **D6 CHANGELOG 禁新造数**：每条性能/计数数字必须给出既有 artifact 路径
  （docs/bench/p4b.md、docs/compat/matrix.md、HANDOVER DoD 对账节），
  写「127.59 MiB/s（docs/bench/p4b.md ②）」类形；比较口径文字与出处一致。
- **D7 首次 CI 绿是 T4 的硬门**：Actions 矩阵对本仓**未经运行验证**（环境
  事实登记），首跑红是预期内事件，修复循环在 T4 内完成（tools 脚本 /
  workflow 面改动允许；src/ 语义面仍零豁免）。

## 4. 门禁与证据口径

- 全量本地回归沿用 P4b 六闸（T4 收口亲跑，逐字日志 + 时间戳入 HANDOVER）。
- 版本改动本身的行为恒等证明 = 六闸全绿 + `--version` 外一切输出字节面不变
  （抽 difftest 一站即可：产物 diff -r 空）。
- CI 绿证据 = `gh run list/view` 逐字摘录（conclusion=success，job 名逐条）。
- Release 证据 = `gh release view v0.5.0 --json assets` + 本机下载回验
  sha256 == release 构建机产物（若 T3 试验产物与云端构建产物字节级一致
  最好；不一致时以**云端产物自带 SHA256SUMS 内部自洽 + 功能 smoke
  （--version + 一次 to-sql 真跑）**为判，差异原因如实登记）。
- 所有「实测」字样声明的落档纪律不变：无逐字日志不入册（禁虚账）。

## 5. 全局约束（每任务隐含携带）

- TDD：门禁/脚本面先红（对故意破坏形失败）后绿；workflow yaml 以
  actionlint/yamllint（若可用）或至少 `ruby -tyml` 等可用解析器做形校验，
  无工具则手写解析测试 + 首跑实测兜底（D7）。
- `reference/my2sql-go/` 只读；比较器/白名单零改动。
- 远端写操作（push/tag/Release）只属 T4；T1-T3 全离线。
- mimalloc 硬依赖不动；依赖白名单不扩（本战役新增依赖 = 0）。
- docs 语言与既有风格一致（中文为主、逐字引用块）。
- SDD 台账 `.superpowers/sdd/2026-09-22-my2sql-rs-p5-release/progress.md`。

## 6. 挂账处置（P5 视角逐条）

| 挂账 | P5 处置 |
|---|---|
| threads 1→8 缩放 post-mimalloc 仍开放 / X3 归因 / X1/X2 追加实验 | **续挂**（性能面已闭合，发布战役不为动而动） |
| `--no-keep-trx` 结构守卫面（P2 T9） | 续挂 |
| stats 失败 run 毁上一份好 JSONL（temp+rename） | 续挂；CHANGELOG「已知限制」节如实披露 |
| live 工装失败路径泄漏 / ctrlc 进程级单次安装 | 续挂（运维注记已在册） |
| `tools/p4a-roundtrip.sh`/`flashback-reconcile.sh` 硬编码 target | **消费**（T1 顺手修，edb2148 同口径） |
| repl TLS 不提供（差异 26） | 不修；README 发布节明示「TLS 不支持」现状不变 |
| 溢出类 panic 残点靠 fuzz-min 常态观察 | 续挂（D3 下 CI 不含 fuzz 真跑，本地闸保留） |

## 7. DoD（P5 完成判据，逐条可验）

1. `Cargo.toml` = 0.5.0 且 `./target/release/my2sql-rs --version` 逐字
   `my2sql-rs 0.5.0`；Cargo.lock 同步；行为面六闸全绿逐字入档。
2. `.github/workflows/ci.yml` 在 tip 上 **实测绿**（gh run conclusion=success
   逐字入档；含 musl 编译门）。
3. GitHub Release `v0.5.0` 存在：双目标产物 + SHA256SUMS，本机下载回验通过
   （§4 口径逐字证据）。
4. `CHANGELOG.md` 存在，五轮里程碑全覆盖，全部数字带出处、抽查可溯源复算。
5. README 状态行 = P4b 后发布态（含安装/发布节），虚账排查记录（改了什么/
   核对结论）入 HANDOVER。
6. 历史 tag 五枚逐字未动（`git ls-remote` 前后对拍）；mimalloc 依赖面、
   src/ 语义零改动证明（`git diff v0.4.1-p4b..v0.5.0 -- src/` 空 +
   Cargo 版本行 diff 之外零命中）。
7. HANDOVER「P5 任务节点日志」+ DoD 对账 + 挂账处置表更新；台账闭合。
