# my2sql-rs P5「发布面」实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把仓库变成可对外发布的 v0.5.0——版本真身对齐、CI 门禁上云、GitHub Release 双目标产物、发布文档（CHANGELOG/README）收口。

**Architecture:** 纯门面战役：`src/` 零语义改动（唯一 Rust 面改动 = Cargo.toml version 一行）。三条并行 lane（T1 版本+CI / T2 文档 / T3 发布工件）文件面互斥，T4 串行独占远端面（push/CI 实测/tag/Release）收口。

**Tech Stack:** Rust stable 1.96（edition 2024）、GitHub Actions（ubuntu-latest）、gh CLI、x86_64-unknown-linux-musl（musl-gcc）、sha256sum。

**Spec:** `docs/superpowers/specs/2026-09-22-my2sql-rs-p5-release-design.md`（本计划从其 §0–§7 逐条消费；spec 是裁定权威）

## Global Constraints

- `src/` 零语义改动；依赖白名单不扩（本战役新增依赖 = 0）；mimalloc 硬依赖不动（spec §1 硬边界）。
- 历史 tag（v0.1.0-p1…v0.4.1-p4b，远端已推）任何改动 = 越界（spec D 案 F4）。
- 远端写操作（push / tag / Release）只属 Task 4；T1–T3 全离线（spec §5）。
- 禁虚账：一切数字逐字对 artifact；CHANGELOG 每条数字带出处路径（spec D6）；「实测」声明无日志不入册。
- workflow yaml 内**不出现 `0.5.0` 字面量**（版本单源 Cargo.toml，spec D2）；release 产物名模板 `my2sql-rs-<ver>-x86_64-<target-triple>`（spec D4）。
- TDD：脚本面先红（对不存在/故意破坏形失败）后绿；yaml 形校验用可用解析器（rubyPsych / python-yaml / actionlint 任一，探测顺序见 T1 Step 1，全缺则登记降级 + T4 首跑实测兜底，spec D7）。
- `reference/my2sql-go/` 只读；比较器/白名单零改动；docs 中文风格与既有一致。
- CI 有界面按 spec D3 逐字（fmt+clippy+test+musl 编译门；difftest/compat/repl-live/fuzz 真跑/shadow 不进 CI），该决定须原文转述进 workflow 头注释与 CHANGELOG。
- SDD 台账：`.superpowers/sdd/2026-09-22-my2sql-rs-p5-release/progress.md`。

---

### Task 1: 版本真身 + CI 门禁面（T1）

**Files:**
- Modify: `Cargo.toml:3`（`version = "0.1.0"` → `"0.5.0"`）、`Cargo.lock`（同步根包一行）
- Modify: `tools/flashback-reconcile.sh:128`（硬编码 `./target/debug/my2sql-rs` → RSBIN 口径，edb2148/b6844fe 同型；勘察注：`tools/p4a-roundtrip.sh` 已被 FIX E 接线，**不在改动面**，spec §6 表该行按此修正）
- Create: `.github/workflows/ci.yml`
- Modify: `.gitignore`（`reference/` 行前追加 `### Qoder 会话工件（harness 目录，不入库）\n.qoder/`）

**Interfaces:**
- Consumes: 无（首棒）。
- Produces: T3 消费「版本单源在 Cargo.toml，读法 = `cargo metadata --format-version 1 --no-deps | jq -r .packages[0].version`」；T4 消费 ci.yml job 名 `ci` 与 step 名清单（`gh run view --log` 对照）。

- [ ] **Step 1: 形校验工具探测（结果入报告）**

```bash
which actionlint ruby python3 2>/dev/null; python3 -c 'import yaml' 2>&1
```
选定其一作 yaml 校验器（优先 actionlint > ruby `-ryaml` > python3 `yaml.safe_load`）；全缺 → 登记「降级：T4 首跑实测兜底」。

- [ ] **Step 2: 红 —— 版本断言与 yaml 校验先行失败**

```bash
grep -n 'version = "0.5.0"' Cargo.toml                    # 期望 rc=1（红）
grep -n 'target/debug' tools/flashback-reconcile.sh        # 期望 :128 命中（红）
test -f .github/workflows/ci.yml                           # 期望 rc=1（红）
```

- [ ] **Step 3: 版本 + 脚本接线落地**

`Cargo.toml` 仅 version 行；`cargo update --workspace-only`（或直接 `cargo check -q` 触发 lock 同步）后核验 `git diff Cargo.lock` = 根包版本一行。
`flashback-reconcile.sh`：`:30` 附近加 `RSBIN="${CARGO_TARGET_DIR:-$ROOT/target}/debug/my2sql-rs"`（注释沿 p4a-roundtrip.sh:16-18 同型），`:128` 改 `"$RSBIN" flashback \`。

- [ ] **Step 4: `.github/workflows/ci.yml`（全文）**

```yaml
# my2sql-rs CI 门禁（P5，spec §3 D3 有界决定）：
#   门 = fmt --check / clippy -D warnings（--all-targets）/ cargo test
#        --no-fail-fast（live/ignored 件自然跳过）/ musl release 编译门。
#   不进 CI（逐字沿 spec D3，本地六闸体系为权威门禁，「CI 绿 ≠ 六闸绿」）：
#   make difftest / compat / repl-test / fuzz-min 真跑 / shadow-test ——
#   需 docker 多版本 MySQL + Go 裁判 + 长 live 时窗，CI 不承载。
name: ci
on:
  push:
    branches: [main]
  pull_request:
jobs:
  ci:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: rust-lang/rust-actions@ubuntu-latest
        with:
          args: install x86_64-unknown-linux-musl
        continue-on-error: true   # target 多已随 toolchain 在；缺则下一步兜底
      - name: Ensure musl target + linker
        run: |
          sudo apt-get update && sudo apt-get install -y --no-install-recommends musl-tools
          rustup target add x86_64-unknown-linux-musl || true
      - name: fmt
        run: cargo fmt --check
      - name: clippy
        run: cargo clippy --all-targets -- -D warnings
      - name: test
        run: cargo test --no-fail-fast
      - name: musl release build gate
        run: cargo build --release --target x86_64-unknown-linux-musl
      - name: fuzz workspace check（nightly-only，非阻断）
        continue-on-error: true
        run: cargo check --manifest-path fuzz/Cargo.toml
```

**注意（实现时自验）**：上面第二个 step（rust-actions 兜底装 target）是冗余形，**实现时删掉它**，只留「Ensure musl target + linker」一步（apt-get + rustup 二连即全）——Step 5 校验的最终 yaml 以删冗余形为准（防 workflow 面手抖：两个入口装同一 target 属占位冗余）。

- [ ] **Step 5: yaml 形校验 + 本地镜像 CI 三门**

Step 1 选定校验器过形；然后本机逐 step 镜像跑一遍（= CI 的本地证据）：

```bash
CARGO_TARGET_DIR=/tmp/p5-t1-tgt bash -c 'set -e; cargo fmt --check; cargo clippy --all-targets -- -D warnings; cargo test --no-fail-fast 2>&1 | tail -3'
CARGO_TARGET_DIR=/tmp/p5-t1-musl cargo build --release --target x86_64-unknown-linux-musl -q
./  # 产物 smoke：
/tmp/p5-t1-tgt/release/my2sql-rs --version    # 期望逐字 "my2sql-rs 0.5.0"
```

- [ ] **Step 6: 行为恒等抽证（版本行不碰语义）**

`make difftest` 一站（或最小等价：`bash tools/run-difftest.sh` 全 7 步）绿 = 产物面逐字节不变首证。

- [ ] **Step 7: commit**

```bash
git add Cargo.toml Cargo.lock .gitignore .github/workflows/ci.yml tools/flashback-reconcile.sh
git commit -m "feat(p5-T1): version 0.5.0 + ci workflow gates + flashback-reconcile RSBIN wiring"
```

---

### Task 2: 发布文档面（CHANGELOG + README 终审）（T2）

**Files:**
- Create: `CHANGELOG.md`
- Modify: `README.md`（状态行 `:6-12`、新增「安装与发布」节、文档节 `:329+` 指针）

**Interfaces:**
- Consumes: 既有 artifact 路径全集（下表）。
- Produces: T4 消费 Release body 素材 = CHANGELOG「v0.5.0」节全文；T4 HANDOVER 节点引用本 lane 的虚账排查记录（报告内）。

- [ ] **Step 1: 红 —— CHANGELOG 不存在**

`test -f CHANGELOG.md` → rc=1。

- [ ] **Step 2: CHANGELOG.md 撰写（禁手改数字，逐字引源）**

结构（Keep-a-Changelog 形，中文）：

```markdown
# 更新日志
本文件记录 my2sql-rs 的里程碑级变更。权威细节（逐字回归台账、白名单全文、
测量口径与陷阱）在 docs/HANDOVER.md 与 docs/bench/*.md；本文件只收录带
出处的结论级数字（spec D6：禁新造数）。

## v0.5.0 — 发布面（2026-09-22）
- 版本真身对齐：包版本 0.1.0 → 0.5.0（历史里程碑 tag v0.1.0-p1…v0.4.1-p4b
  保留不动，此后 git tag 与包版本紧耦合）
- CI 门禁上线：fmt / clippy -D warnings / cargo test / musl release 编译门
  （有界决定：difftest/compat/repl-test/fuzz/shadow 不进 CI，本地六闸为
  权威门禁——「CI 绿 ≠ 六闸绿」，见 docs/superpowers/specs/…p5…design.md §3 D3）
- GitHub Release 双目标产物：x86_64-unknown-linux-gnu / -musl + SHA256SUMS
- 已知限制（口径同 HANDOVER 挂账清单）：repl 无 TLS（差异 26）；stats 失败
  run 会清上一份好 JSONL；DDL 回滚/--apply/MariaDB 明确不做（D5）

## v0.4.1-p4b — 性能面（2026-09-22）   ← 数字逐字取自 ↓
（mimalloc glibc A/B −26.561%、musl 悬崖 3.3→64.2 MiB/s、criterion 权威基线
 threads=8 median 127.59 MiB/s（+22.86% vs 103.85 更快 GREEN）、
 bench-ab/bench-profile 工装、assembly 搬运 —— 出处 docs/bench/p4b.md ①–⑥）

## v0.4.0-p4a — 质量并行面（2026-09-22）
（fuzz 正式闸两靶 300s + 实抓 2 panic 开闸条款、影子库三段闸、difftest P4A
 三列形 14/14、5.6/5.7 idle 心跳 live 件 —— 出处 HANDOVER「P4a DoD 对账」+
 docs/p4a-findings.md）

## v0.3.0-p3 — repl 模式（2026-09-21）
（伪装 replica 拉流 + checkpoint/resume + 退避重连 + 心跳 + 防覆盖闸；
 live 13 件家族、compat 18 用例 —— 出处 HANDOVER「P3 DoD 对账」）

## v0.2.0-p2 — flashback + stats（2026-09-21）
（记录原子逆序 keep-trx、两报表+JSONL、差分/矩阵/活库对账 ——
 出处 HANDOVER「P2 DoD 对账」+ docs/bench/p2.md）

## v0.1.0-p1 — to-sql file 模式（2026-09-20）
（全自研解码、5.6–8.4 矩阵、差分底座、108.9 MB/s@threads=8（2.7×）——
 出处 docs/bench/p1.md、docs/compat/matrix.md）
```

各节内每条数字**必须**回读源文件核对后誊抄（`docs/bench/p4b.md` / `docs/p4a-findings.md` / HANDOVER 三 DoD 对账节 / `docs/bench/p1.md:35`）；`13 件` = P4b 后家族数（P4a T4 扩 11→13，出处 HANDOVER P4a T4 节点）。

- [ ] **Step 3: README 终审**

① 状态行 `:6-12` 重写：P1–P4b 全五轮收官态 + 「P5 发布面：CI 门禁 + v0.5.0 Release」一句；删「当前处于 P3」过期形。② 「快速上手」前新增节：

```markdown
## 安装与发布

预编译二进制见 [GitHub Releases](https://github.com/0x5c0f/my2sql-rs/releases/tag/v0.5.0)：
`my2sql-rs-0.5.0-x86_64-unknown-linux-gnu`（glibc 动态）与
`my2sql-rs-0.5.0-x86_64-unknown-linux-musl`（musl 静态单二进制，
吞吐 64.2 MiB/s@threads=8，见 docs/bench/p4b.md ④）+ `SHA256SUMS`。

    sha256sum -c SHA256SUMS   # 下载后校验

自构建：`cargo build --release`（glibc）；
`cargo build --release --target x86_64-unknown-linux-musl`（需 musl-gcc）。
```

③ 「文档」节追加 `CHANGELOG.md` 行。④ **虚账排查**：全文扫「实测/逐字/全绿」声明逐条对照现存证据（重点 `:48` rustc 1.96 本机实测 = 事实、`:160` repl-test 13 件 823.02s = P4a T5 账、P4b 后 13/0@815.30s = HANDOVER P4b T5 节点——以最新一次真跑为准改引）；改动与「核对无误」清单全部写进 lane 报告（T4 汇编入 HANDOVER）。

- [ ] **Step 4: 自检 + commit**

`cargo build --release -q && /tmp 无关`（README 引用路径存在性 grep 核验：docs/bench/p4b.md、matrix.md 等被引路径逐一 `test -f`）；CHANGELOG 内每个数字与其标注出处行做 diff 级对照（摘录进报告）。

```bash
git add CHANGELOG.md README.md
git commit -m "docs(p5-T2): CHANGELOG (五轮里程碑，数字全带出处) + README 终审 (状态行/安装发布节/虚账排查)"
```

---

### Task 3: 发布工件面（release.yml + tools/release-build.sh）（T3）

**Files:**
- Create: `tools/release-build.sh`
- Create: `.github/workflows/release.yml`

**Interfaces:**
- Consumes: T1 的版本单源读法（`cargo metadata --format-version 1 --no-deps | jq -r .packages[0].version`）；产物名模板（spec D4）。
- Produces: T4 消费——R3 链定稿形（见 Step 4 头注）+ `tools/release-build.sh` 本机试验日志 + 产物 sha256 存档（`/tmp/p5-rel-local/`）。

- [ ] **Step 1: 红 —— 脚本不存在**

`test -f tools/release-build.sh` → rc=1。

- [ ] **Step 2: `tools/release-build.sh`（全文）**

```bash
#!/usr/bin/env bash
# P5-T3：本机双目标 release 构建（CI release 链的本机等价试验 + 兜底产物源）。
# 产物：$OUT/my2sql-rs-<ver>-x86_64-unknown-linux-{gnu,musl} + SHA256SUMS
# 纪律（spec D2/D4）：版本单源 cargo metadata，脚本内无版本字面量；
# 每 target 独立 CARGO_TARGET_DIR（跨历史构建串味条例，P4b T3 先例）。
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/out/release-local}"
VER=$(cargo metadata --format-version 1 --no-deps --manifest-path "$ROOT/Cargo.toml" \
      | sed -n 's/.*"my2sql-rs","version":"\([^"]*\)".*/\1/p')
[ -n "$VER" ] || { echo "FATAL: 版本读取失败（cargo metadata 形态变更？）" >&2; exit 1; }
mkdir -p "$OUT"
build_one() { # $1=target triple（空=glibc 本机）
  local tgt="$1" tdir="/tmp/p5-rel-tgt-${tgt:-host}"
  if [ -z "$tgt" ]; then
    CARGO_TARGET_DIR="$tdir" cargo build --release -q --manifest-path "$ROOT/Cargo.toml"
    cp "$tdir/release/my2sql-rs" "$OUT/my2sql-rs-$VER-x86_64-unknown-linux-gnu"
  else
    CARGO_TARGET_DIR="$tdir" cargo build --release -q --manifest-path "$ROOT/Cargo.toml" --target "$tgt"
    cp "$tdir/$tgt/release/my2sql-rs" "$OUT/my2sql-rs-$VER-x86_64-unknown-linux-musl"
  fi
}
build_one ""
build_one "x86_64-unknown-linux-musl"
( cd "$OUT" && sha256sum my2sql-rs-* > SHA256SUMS )
"$OUT/my2sql-rs-$VER-x86_64-unknown-linux-gnu" --version
"$OUT/my2sql-rs-$VER-x86_64-unknown-linux-musl" --version
ls -l "$OUT"
```

（`sed -n` 版本提取对 `cargo metadata` 单行 JSON 的 `"name":"my2sql-rs","source":null,"version":"X"` 实际形态**必须实跑验证**；不匹配 → 改 jq 形 `cargo metadata --format-version 1 --no-deps -q | jq -r '.packages[0].version'`，jq 在册与否 Step 3 实测登记。）

- [ ] **Step 3: 本机真跑试验**

```bash
bash tools/release-build.sh /tmp/p5-rel-local   # 期望：双产物 + SHA256SUMS + 两 --version 行
sha256sum /tmp/p5-rel-local/my2sql-rs-* ; cat /tmp/p5-rel-local/SHA256SUMS   # 逐字入报告
file /tmp/p5-rel-local/my2sql-rs-*-musl   # 期望 static-pie 形登记
```

红→绿全程逐字日志入报告。musl 构建失败 = 环境事实上报（不静默，P4b T3 先例）。

- [ ] **Step 4: `.github/workflows/release.yml`（全文；R3 = draft-Release 链）**

```yaml
# P5-T3（spec D5，R3 定稿形）：tag v* push → 本机收口链（T4）已先以
# `gh release create --draft` 建好 v<ver> draft Release；本 workflow 构建
# 双目标产物并 `gh release upload` 挂入；T4 回验后 undraft。
# （为何 draft 先行而非 workflow 自建 Release：私有仓 + 控制器需在
#  undraft 前有本机回验位——最小步骤可取原则，spec D5。）
name: release
on:
  push:
    tags: ['v[0-9]*']
permissions:
  contents: write
jobs:
  build:
    runs-on: ubuntu-latest
    strategy:
      matrix:
        include:
          - target: x86_64-unknown-linux-gnu
            suffix: gnu
          - target: x86_64-unknown-linux-musl
            suffix: musl
    steps:
      - uses: actions/checkout@v4
      - run: sudo apt-get update && sudo apt-get install -y --no-install-recommends musl-tools
      - run: rustup target add ${{ matrix.target }}
      - name: build
        run: |
          cargo build --release --target ${{ matrix.target }}
          cp "target/${{ matrix.target }}/release/my2sql-rs" \
             "my2sql-rs-$(cargo metadata --format-version 1 --no-deps | \
             sed -n 's/.*"version":"\([^"]*\)".*/\1/p' | head -1)-${{ matrix.suffix }}"
      - name: upload
        env:
          GH_TOKEN: ${{ github.token }}
        run: |
          gh release upload "${GITHUB_REF_NAME}" \
            "my2sql-rs-"*-${{ matrix.suffix }} --clobber
  sha:
    needs: build
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - env: { GH_TOKEN: '${{ github.token }}' }
        run: |
          gh release download "$GITHUB_REF_NAME" -p 'my2sql-rs-*' -D /tmp/rel
          ( cd /tmp/rel && sha256sum my2sql-rs-* > SHA256SUMS )
          gh release upload "$GITHUB_REF_NAME" /tmp/rel/SHA256SUMS --clobber
```

（workflow 内版本经 cargo metadata 动态取，无字面量——Global Constraints 复验 `grep -c '0\.5\.0' .github/workflows/*.yml` 必须 0。`sed` 提取形与 T3 Step 2 同源风险：多 package metadata 输出可能含依赖版本串扰，**实现时优先 jq（ubuntu-latest 预装）或 `cargo pkgid` 截取**，本机 Step 3 实证后定稿。）

- [ ] **Step 5: yaml 形校验（T1 Step 1 同型工具）+ commit**

```bash
git add tools/release-build.sh .github/workflows/release.yml
git commit -m "feat(p5-T3): release-build.sh 双目标本机试验 + release.yml（R3 draft 链）"
```

---

### Task 4: 合流收口 + 远端发布链（T4，串行独占远端面）

**Files:**
- Modify: `docs/HANDOVER.md`（P5 节点日志 + DoD 对账 + 状态行 + 挂账处置注）
- Modify: `README.md`（仅当 T2 虚账排查/T4 实测有口径差时微调）
- 远端：push origin main、annotated tag `v0.5.0`、draft→publish Release

**Interfaces:**
- Consumes: T1 ci.yml（job `ci` + step 名）、T2 CHANGELOG v0.5.0 节（Release body）、T3 R3 链 + `/tmp/p5-rel-local` sha256 存档。
- Produces: 对外发布态 v0.5.0；台账闭合。

- [ ] **Step 1: 合流全量回归六闸（P4b 同型，独立 target dir `/tmp/p5-merge`）**

`cargo test --no-fail-fast`（**350/0/14** 计数钉，版本行外 diff = T1/T2/T3 面，src/ 零动）+ clippy + fmt；`FUZZ_TIME=20 make fuzz-min`；`make shadow-test`；`make difftest` + `P4A=1 make difftest`；`make compat`；`make repl-test` 13 件。逐字日志 + 时间戳（禁 tail 截断采数，P4b 教训：全量重定向后求和）。

- [ ] **Step 2: ff main + push + CI 实测绿（DoD-2 硬门）**

```bash
# 主仓：git fetch && git merge --ff-only worktree-feat-p5 && git push origin main
gh run list --branch main --limit 3        # 取本次 run id
gh run watch <id> --exit-status || { gh run view <id> --log-failed; exit 1; }
```
红 = 修复轮（workflow/tools 面允许；src/ 语义面零豁免）→ 重推重跑直至绿；
绿 run 的逐字 `gh run view` 结论入 HANDOVER。

- [ ] **Step 3: tag + draft Release + 产物链（R3）**

```bash
git tag -a v0.5.0 -m "my2sql-rs v0.5.0 — 发布面（版本真身对齐 + CI + 双目标 Release）"
git push origin v0.5.0
# 先建 draft（供 release.yml upload）：
gh release create v0.5.0 --draft --title "v0.5.0" --notes-file <(sed -n '/## v0.5.0/,/## v0.4.1/p' CHANGELOG.md)
gh run list --workflow release --limit 3   # 轮询 release 链至 success
```
release 链红 → 按 D5 兜底：`tools/release-build.sh` 本机产物直传
`gh release upload v0.5.0 /tmp/p5-rel-local/* --clobber`，兜底原因逐字入档。

- [ ] **Step 4: 回验 + 发布**

```bash
gh release download v0.5.0 --dir /tmp/p5-rel-dl -p 'my2sql-rs-*'
gh release download v0.5.0 -p SHA256SUMS --clobber 2>/dev/null || cp /tmp/p5-rel-local/SHA256SUMS /tmp/p5-rel-dl/
cd /tmp/p5-rel-dl && sha256sum -c SHA256SUMS
./my2sql-rs-0.5.0-x86_64-unknown-linux-musl --version   # + data/8.0 一次真 to-sql smoke
gh release edit v0.5.0 --draft=false
```
云端 vs 本机产物字节一致则登记「双源同 sha」（最优形）；不一致以云端
SHA256SUMS 自洽 + 功能 smoke 为判、差异原因如实登记（spec §4）。
历史 tag 对拍：`git ls-remote origin 'refs/tags/v*'` 五枚旧 tag sha 逐字未动（DoD-6）。

- [ ] **Step 5: 文档收口 + commit + push**

HANDOVER：P5 节点日志（T1–T4 各一节 + 派发偏差披露）+「P5 DoD 对账」（spec §7 七条逐行）+ 状态行 + 挂账清单 P5 消费注（flashback-reconcile 销账行）；README 如需。收口 commit 后 ff main、push，tag 已在 Step 3（若收口含 docs commit 则 tag 后移至此步尾并重跑 DoD-6 对拍——先例 P4b：tag 指向含收口的 tip）。

---

## 自检记录（writing-plans §Self-Review）

1. **Spec 覆盖:** §1 范围→T1/T2/T3/T4 全映射；§2 任务分解逐字；§3 D1（0.5.0）→T1、D2 单源→T1/T3 双 grep 闸、D3 CI 有界→T1 头注、D4 命名→T3、D5/R3→T3 Step 4+T4 Step 3、D6 禁新造数→T2、D7 首跑兜底→T4 Step 2；§4 证据口径→各 Step 逐字日志 + T4 Step 4；§5 全局约束→Global Constraints；§6 挂账表→T1 Step 3（flashback-reconcile）+ T2 已知限制节 + T4 Step 5 销账注；§7 DoD 1-7→T1（1）/T4-2（2）/T4-3/4（3）/T2（4,5）/T4-3/5（6）/T4-5（7）。**修正一处 spec 事实误差**：`p4a-roundtrip.sh` 已被 FIX E 接线，P5 消费面 = flashback-reconcile.sh 单文件（T1 Files 注记）。
2. **占位扫描:** T1 Step 4「删冗余 step」为定稿指令非占位；T3 jq/sed 二形择一 = 实证后定稿形（两形均全文在案）非 TBD；Release body 用 sed 区间抽取命令全文给出。无占位。
3. **类型一致:** 产物名模板三处一致（D4 = T3 脚本 = release.yml upload = T4 download glob `my2sql-rs-*`）；版本读法三处（T1 Produces / T3 脚本 / release.yml）统一为 cargo metadata 系；R3 名号在 T3/T4 一致；CI job 名 `ci` 在 T1 Produces 与 T4 Step 2 一致。
