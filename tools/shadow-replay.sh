#!/usr/bin/env bash
# P4a T2 (Lane B, spec §2): 影子库端到端三段闸 —— 前向(to-sql→影子==主后态) /
# 逆向(flashback→影子clone==前态) / 往返(前向影子逆灌==前态)。
# 人工/make 触发；容器 p3e2e-<ver>-p4ash<slug>-<pid> 自建自清(trap)，KEEP=1 失败保留。
#
# 用法:  bash tools/shadow-replay.sh [VER]                 # VER ∈ {8.0, 5.7, ...}
#        SHADOW_NEGCHECK=1 bash tools/shadow-replay.sh [VER]  # 负自检（成功=FAIL）
#        KEEP=1 …                                          # 失败保留容器排障
# 退出码 0 = 三向全过（negcheck 模式 = 漂移被闸抓到）；INT/TERM → 130。
# 证据目录 out/shadow-replay/<VER>/（fix-round-1 Minor #5：按版本命名空间，
# 5.7 不再覆写 8.0 证据；negcheck 日志 = 该 VER 目录内 negcheck.log。
# T5 接线按本头注取路径）。
#
# 设计裁定（沿 P2 flashback-reconcile.sh 母本纪律，升格三向）：
#  - 影子库起点恢复：影子 = CREATE DATABASE <db>_fwd/<db>_rev + 直接回灌本脚本
#    snap 产出的整库 mysqldump 快照（无 --databases → 不含 CREATE DATABASE/USE，
#    含 DROP TABLE IF EXISTS + CREATE TABLE + 数据），灌入前该库为空，快照即
#    「前态(P0)/后态(P1)」的逐字节持久化形态，影子起点==快照态由 create_shadow 后
#    的 setup 闸（SETUP_* == P0/P1）显式验证，不靠口头保证。
#  - 产物库名改写：to-sql/flashback 产物按 `p4ashadow`.`<tbl>` 全限定输出，灌入
#    影子库前 sed 改写为影子库名（改写只发生在灌入通道，留档产物 $OUT/{fwd,rev}/
#    保持原样）；改写模式仅匹配反引号全限定前缀，不会触达值文本。
#  - 行级比对口径 = P2 dump_rows 归一（保留 INSERT/'(' 续行/空 '--' 注释行，
#    剔除头注释、条件注释、LOCK/CREATE 等结构行），checksum 口径 = 逐表
#    CHECKSUM TABLE 钉序（information_schema 表清单 ORDER BY 1，剥库名取基名）。
#  - 负自检 SHADOW_NEGCHECK=1：同一次运行内「重走前向」——同窗产物重建 fwd 影子
#    并灌入后、比对前，注入一行 UPDATE <影子>.t_ord SET qty=qty+1（最小主键行），
#    前向闸必须红；抓到 → 打 NEGCHECK 字样、反转退出码为 0；抓不到 → 1。
#  - BIN 按 CARGO_TARGET_DIR 解析（并行 lane 各自独立 target 目录）；脚本自身
#    执行 `cargo build --quiet`（继承环境）。
#  - 5.7 锚点+豁免裁定（5.7 首跑实踩；diag 容器逐字证据，T5 仲裁可否推广）：
#    5.7 的 CHECKSUM TABLE 对含 JSON 列的表不可定值：同一张表连续四次读取
#    （无任何写入）即得 3642844648/3642844648/3876613648/3843993102（纯
#    INSERT 表 j1）与 3355042310/1207344630/3086637558/1207344630（UPDATE
#    过的 j2）；同一 dump 回灌两个空库 rA.j1=451714574=rB.j1、却与 live
#    u.j1=657417714 互异，FLUSH 后再读=719482358——物理 JSON 二进制的未初始化
#    填充被计入校验和，上游缺陷（8.0 无此症，同灌流全闸绿、值逐轮定值可反证）。
#    故 5.7 面：a) 期望侧改 REF clone（主库快照 dump 的回灌库 ${DB}_ref），由
#    硬锚闸 gate_rows 强制 REF vs live 行级逐字相等（REF 偏离主库态即整轮红），
#    比对仍锚定主库逻辑态；b) 逐表 CHECKSUM TABLE 等值仅对「含 JSON 列的表」
#    豁免（其值仍逐字打印入证据），无 JSON 表照常全强等值（t_ord 实测跨五轮
#    恒为 2830880655）；行级 diff 闸无任何豁免、逐表逐字。8.0 面 a/b 均不启用，
#    维持 spec 原形态（live 锚 + 全表 checksum 等值 + 行级 diff）。
#  - 静绿保险丝（fix-round-1 Important #1）：a) snap 逐表断言 CHECKSUM TABLE
#    值非空且纯数字（NULL 值直接红，杀 NULL==NULL 假等式）；b) gate 硬断言
#    表清单 n>=1、两侧 .checksum 逐行形如 "<table> <digits>"、行级比对输入
#    $a.rows/$e.rows 非空（空集 diff 恒绿即闸空转）；c) assert_stmts 把
#    to-sql/flashback `done:` 摘要的 statements= 与产物 DML 行数钉成硬闸
#    （原 115/115 人工互核升格为机闸）。
#  - trap 窗口（fix-round-1 Important #2 + Minor #4）：EXIT trap 先于
#    p3e2e_container_start 安装（lib 于 docker run 前置 P3E2E_CTR、
#    p3e2e_container_stop 容忍空值——均已按 lib 源码核验），容器起在等待
#    健康期被 Ctrl-C/被杀也不泄漏；INT/TERM 臂统一 exit 130。
#  - 单档窗口假设沿用 lib 头注：灌流窗口内不跨 binlog 档；本件 [f0..f1] 取段
#    天然支持 f0≠f1（docker cp 区段），但 to-sql 不给 stop-pos——p1 之后主库
#    再无写入（snap/闸全只读），窗口尾界由 P1last 在场 + P0* 缺席双向钉住。
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"; VER="${1:-8.0}"
# T5 合流加固（输入面先闸）：VER ∈ {5.6,5.7,8.0,8.4} 白名单在**任何 rm -rf /
# OUT 路径插值之前**校验；不支持的版本直接 exit 2 报错退出，不让任意输入驱动
# 破坏性清理或拼接路径（lib 的 p3e2e_container_start 同款白名单是其后的第二道）。
case "$VER" in
  5.6|5.7|8.0|8.4) ;;
  *) echo "shadow-replay: unsupported VER '$VER' (allowed: 5.6|5.7|8.0|8.4)" >&2; exit 2 ;;
esac
BIN="${CARGO_TARGET_DIR:-$ROOT/target}/debug/my2sql-rs"
source tools/repl-e2e-lib.sh

OUT="$ROOT/out/shadow-replay/$VER"; rm -rf "$OUT" && mkdir -p "$OUT"
[ "${SHADOW_NEGCHECK:-0}" = "1" ] && exec > >(tee "$OUT/negcheck.log") 2>&1

SFX="p4ash$$"
# Important #2：trap 先于容器启动安装——start 内部（等健康 180s）被 INT/TERM
# 或中途失败即成泄漏窗口。lib 契约已核验：P3E2E_CTR 在 docker run 前赋值
# （lib:59），p3e2e_container_stop 对空值直接 return 0（lib:100-101），故
# trap 体统一用 ${P3E2E_CTR:-} 守卫。Minor #4：INT/TERM → exit 130，经 EXIT
# 臂完成清理（与 src repl 的 130 口径一致）。
trap 'rc=$?; if [ "$rc" -ne 0 ] && [ -n "${KEEP:-}" ]; then echo "FAILED(rc=$rc) — container ${P3E2E_CTR:-<none-started>} kept for debug (KEEP=$KEEP)"; else p3e2e_container_stop "${P3E2E_CTR:-}"; fi' EXIT
trap 'exit 130' INT TERM
p3e2e_container_start "$VER" "$SFX" >/dev/null
CTR="$P3E2E_CTR"; PORT="$P3E2E_PORT"
DB=p4ashadow
# 期望侧锚点：默认 live（8.0 实证可保真）；5.7 见头注裁定 → dump 回灌 clone
ANCHOR=live
EXEMPT=""   # CHECKSUM TABLE 等值豁免表（空格分隔；仅 clone 锚启用，= 含 JSON 列表）
case "$VER" in 5.7) ANCHOR=clone ;; esac
echo "== [0/6] container $CTR (port $PORT, image mysql:$VER, negcheck=${SHADOW_NEGCHECK:-0}, anchor=$ANCHOR)"

# snap <tag> <db> —— 整库 dump（回灌源+行级比对源）+ 逐表 CHECKSUM TABLE（钉序）。
# --hex-blob 必给（5.7 实踩）：默认文本转义形态的 BLOB 字节经 utf8mb4 客户端
# 重灌会被服务端非法序列重编码（0x00FF10… ≠ 原值），影子起点恢复即失真；
# 0x 十六进制字面量与字符集无关，且行级比对两侧同为 dump 产物、口径仍逐字一致。
snap() { # <tag> <db>
  local tag=$1 db=$2
  docker exec "$CTR" mysqldump -uroot --no-tablespaces --skip-dump-date --hex-blob \
    --default-character-set=utf8mb4 "$db" > "$OUT/$tag.sql"
  : > "$OUT/$tag.checksum"
  local t ck
  while IFS= read -r t; do
    [ -n "$t" ] || continue
    ck=$(docker exec "$CTR" mysql -uroot -N -e "CHECKSUM TABLE \`$db\`.\`$t\`" | awk '{print $2}')
    # Important #1a：NULL（awk 取空）/非纯数字值一律红——否则两行 "t " 假等式
    # 通过 checksum 腿，闸静绿。
    case "$ck" in ''|*[!0-9]*) echo "snap[$tag]: bad CHECKSUM TABLE value '$ck' for $db.$t (NULL/non-numeric)" >&2; return 1 ;; esac
    printf '%s %s\n' "$t" "$ck" >> "$OUT/$tag.checksum"
    echo "   ck[$tag] $db.$t=$ck"
  done < <(p3e2e_sql "$CTR" -N -e "SELECT TABLE_NAME FROM information_schema.TABLES WHERE TABLE_SCHEMA='$db' ORDER BY 1")
}

# 行级归一口径（P2 母本 dump_rows 同款 + `-a`：dump 含 BLOB 转义/NUL 邻字节，
# GNU grep 无 -a 时对二进制文件抑制匹配行输出，归一即被悄悄截空——实跑暴露后钉死）
dump_rows() { grep -a -E "^[(]|^INSERT|^--$" "$1" | grep -a -v '^-- Dump completed' | sed 's/[[:space:]]*$//'; }

# gate <actual-tag> <expected-tag> —— 行级 diff 必须空（无豁免）+ 逐表 checksum
# 等值（EXEMPT 中的 JSON 表仅当 ANCHOR=clone 的 5.7 面列入，值仍逐字入证据）；
# 打 tables=<n> checksum-equal=<m>/<需等值数> exempt-json=<x>；任一不符返回 1
# （negcheck 依赖该返回码）。前置静绿保险丝（Important #1）：表清单 n>=1、
# 两侧 checksum 行形状 "<table> <digits>"、a.rows/e.rows 非空——任一不满足
# 即红，不允许空集恒绿。
gate() { # <actual> <expected>
  local a=$1 e=$2 n eq nex
  # Important #1a/b 静绿保险丝：表清单非空 + 两侧 checksum 逐行 "<table> <digits>"
  # + 行级比对输入非空（空集 diff 恒绿 = 闸空转）。
  n=$(wc -l < "$OUT/$e.checksum")
  { [ "$n" -ge 1 ] && [ -s "$OUT/$a.checksum" ]; } || {
    echo "GATE RED[$a vs $e]: empty table list (expected lines=$n, actual $(wc -l < "$OUT/$a.checksum")) — gate would silently pass" >&2; return 1; }
  if grep -qvE '^[^[:space:]]+[[:space:]]+[0-9]+$' "$OUT/$a.checksum" "$OUT/$e.checksum"; then
    echo "GATE RED[$a vs $e]: malformed/NULL checksum field (want '<table> <nonempty digits>'):" >&2
    grep -nvE '^[^[:space:]]+[[:space:]]+[0-9]+$' "$OUT/$a.checksum" "$OUT/$e.checksum" >&2 || true
    return 1
  fi
  nex=$(awk -v ex="$EXEMPT" 'BEGIN{split(ex,x," ");for(i in x)E[x[i]]=1} {if($1 in E)c++} END{print c+0}' "$OUT/$e.checksum")
  eq=$(paste -d'|' "$OUT/$a.checksum" "$OUT/$e.checksum" \
       | awk -F'|' -v ex="$EXEMPT" 'BEGIN{split(ex,x," ");for(i in x)E[x[i]]=1}
           {split($1,p," ");t=p[1]} !(t in E) && $1==$2 {c++} END{print c+0}')
  dump_rows "$OUT/$a.sql" > "$OUT/$a.rows"; dump_rows "$OUT/$e.sql" > "$OUT/$e.rows"
  [ -s "$OUT/$a.rows" ] && [ -s "$OUT/$e.rows" ] || {
    echo "GATE RED[$a vs $e]: normalized row set empty (a.rows=$(wc -c < "$OUT/$a.rows")B, e.rows=$(wc -c < "$OUT/$e.rows")B) — row diff would silently pass" >&2; return 1; }
  if ! diff -u "$OUT/$e.rows" "$OUT/$a.rows" > "$OUT/$a.vs.$e.rowdiff.txt"; then
    echo "GATE RED[$a vs $e]: tables=$n checksum-equal=$eq/$((n-nex)) exempt-json=$nex; rowdiff (head 40):" >&2
    head -40 "$OUT/$a.vs.$e.rowdiff.txt" >&2 || true
    echo "GATE RED[$a vs $e]: checksum diff:" >&2
    diff -u "$OUT/$e.checksum" "$OUT/$a.checksum" >&2 || true
    return 1
  fi
  if [ "$eq" != "$((n-nex))" ]; then
    echo "GATE RED[$a vs $e]: checksum collision? row-diff empty but tables=$n equal=$eq/$((n-nex)) exempt-json=$nex" >&2
    diff -u "$OUT/$e.checksum" "$OUT/$a.checksum" >&2 || true
    return 1
  fi
  echo "   GATE GREEN: $a vs $e tables=$n checksum-equal=$eq/$((n-nex)) exempt-json=$nex${EXEMPT:+ ($EXEMPT)} (rowdiff bytes=$(wc -c < "$OUT/$a.vs.$e.rowdiff.txt"))"
}

# gate_rows <actual-tag> <expected-tag> —— 仅行级比对（clone 锚闸：REF clone 必须
# 与 live 快照逐字同构，锚不住主库则整轮证据无效）。
gate_rows() { # <actual> <expected>
  local a=$1 e=$2
  dump_rows "$OUT/$a.sql" > "$OUT/$a.rows"; dump_rows "$OUT/$e.sql" > "$OUT/$e.rows"
  [ -s "$OUT/$a.rows" ] && [ -s "$OUT/$e.rows" ] || {
    echo "GATE RED[$a vs $e]: anchor row-set empty (a.rows=$(wc -c < "$OUT/$a.rows")B, e.rows=$(wc -c < "$OUT/$e.rows")B) — REF anchor unprovable" >&2; return 1; }
  if diff -u "$OUT/$e.rows" "$OUT/$a.rows" > "$OUT/$a.vs.$e.rowdiff.txt"; then
    echo "   GATE GREEN(rows): $a vs $e (rows=$(wc -l < "$OUT/$a.rows") lines, rowdiff bytes=$(wc -c < "$OUT/$a.vs.$e.rowdiff.txt"))"
    return 0
  fi
  echo "GATE RED[$a vs $e]: anchor row-diff nonempty (head 20):" >&2
  head -20 "$OUT/$a.vs.$e.rowdiff.txt" >&2 || true
  return 1
}

# create_shadow <new-db> <dump-file> —— 空库回灌快照 = 影子起点恢复
create_shadow() { # <newdb> <dumpfile>
  local db=$1 src=$2
  p3e2e_sql "$CTR" -e "DROP DATABASE IF EXISTS \`$db\`; CREATE DATABASE \`$db\`;"
  p3e2e_sql "$CTR" "$db" < "$src"
}

# apply_product <proddir> <shadow-db> —— 产物逐文件灌入影子库（sed 全限定库名改写）
apply_product() { # <dir> <shadowdb>
  local dir=$1 db=$2 f
  for f in "$dir"/*.sql; do
    [ -e "$f" ] || { echo "no product sql in $dir" >&2; return 1; }
    echo "   applying $(basename "$f") -> $db"
    sed "s/\`$DB\`\./\`$db\`./g" "$f" | p3e2e_sql "$CTR"
  done
}

# dml_count <dir> —— 产物 DML 行数（简报口径逐字 grep，逐字打印）
dml_count() { cat "$1"/*.sql | grep -a -c '^INSERT \|^UPDATE \|^DELETE ' || true; }

# assert_stmts <log> <done前缀> <产物目录> —— Important #1c：`done:` 摘要行的
# statements= 值必须等于产物 DML 行数（人工 115/115 互核升格为硬闸）；同时
# 隐含断言 statements= 字段在场（缺字段=红）。
assert_stmts() { # <log> <prefix> <dir>
  local log=$1 pfx=$2 dir=$3 st dml
  st=$(sed -n "s/^$pfx done:.*statements=\([0-9]\{1,\}\).*/\1/p" "$log" | tail -1)
  [ -n "$st" ] || { echo "assert FAILED: '$pfx done:' lacks statements=<digits> in $log" >&2; return 1; }
  dml=$(dml_count "$dir")
  [ "$st" = "$dml" ] || { echo "assert FAILED: $pfx statements=$st != applied DML lines=$dml ($dir)" >&2; return 1; }
  echo "   ASSERT OK: $pfx statements=$st == DML lines=$dml in $(basename "$dir")/"
}

# ── [1/6] 基线前态 ───────────────────────────────────────────────────────────
echo "== [1/6] seed $DB + feed 3xP0 (pre-state baseline)"
p3e2e_seed_schema "$CTR" "$DB"
p3e2e_feed_mixed "$CTR" "$DB" 3 P0
snap P0 "$DB"

# ── [2/6] 窗口 DML + binlog 取段 ─────────────────────────────────────────────
echo "== [2/6] feed 3xP1 within window (f0,p0)..(f1,p1) + capture binlogs"
read -r f0 p0 < <(p3e2e_master_pos "$CTR")
p3e2e_feed_mixed "$CTR" "$DB" 3 P1
read -r f1 p1 < <(p3e2e_master_pos "$CTR")
echo "   window: $f0:$p0 -> $f1:$p1"
p3e2e_capture_binlogs "$CTR" "$f0" "$f1" "$OUT/binlog"
snap P1 "$DB"

# 期望侧标签（anchor=clone 时经 REF 中转，REF 自身由 gate_rows 硬锚回 live）
EXP_PRE=P0; EXP_POST=P1
if [ "$ANCHOR" = clone ]; then
  echo "   anchor=clone (5.7 JSON CHECKSUM TABLE 不可定值，见头注裁定)"
  EXEMPT=$(p3e2e_sql "$CTR" -N -e "SELECT DISTINCT TABLE_NAME FROM information_schema.COLUMNS WHERE TABLE_SCHEMA='$DB' AND DATA_TYPE='json' ORDER BY 1" | tr '\n' ' ' | sed 's/ *$//')
  echo "   exempt-json: [$EXEMPT]"
  create_shadow "${DB}_ref" "$OUT/P0.sql"
  snap REF_P0 "${DB}_ref"
  gate_rows REF_P0 P0 || { echo "anchor FAILED: ${DB}_ref(P0 dump) rows != live P0" >&2; exit 1; }
  create_shadow "${DB}_ref" "$OUT/P1.sql"
  snap REF_P1 "${DB}_ref"
  gate_rows REF_P1 P1 || { echo "anchor FAILED: ${DB}_ref(P1 dump) rows != live P1" >&2; exit 1; }
  EXP_PRE=REF_P0; EXP_POST=REF_P1
fi

# ── [3/6] 前向闸：to-sql 产物灌影子 == 主库后态 ─────────────────────────────
echo "== [3/6] forward: cargo build + online-schema to-sql -> ${DB}_fwd == P1"
cargo build --quiet
"$BIN" to-sql \
  --binlog-dir "$OUT/binlog" --start-file "$f0" --start-pos "$p0" \
  --uri "mysql://root@127.0.0.1:$PORT" --time-zone +00:00 \
  --output-dir "$OUT/fwd" --schema-dump "$OUT/schema.json" 2>&1 | tee "$OUT/to-sql.log"
grep -q "^to-sql done:" "$OUT/to-sql.log" || { echo "to-sql produced no summary line" >&2; exit 1; }
# 窗口边界钉查：P1 段在场（含尾界 P1last），P0 段必须缺席（start-pos 生效）
grep -a -q "P1doc1" "$OUT"/fwd/*.sql || { echo "window FAILED: P1doc1 absent from to-sql product" >&2; exit 1; }
grep -a -q "P1last" "$OUT"/fwd/*.sql || { echo "window FAILED: P1last (window-tail boundary row) absent" >&2; exit 1; }
if grep -a -q "P0" "$OUT"/fwd/*.sql; then echo "window FAILED: P0-tagged rows leaked into product" >&2; exit 1; fi
echo "   fwd product: DML lines=$(dml_count "$OUT/fwd") (grep -c '^INSERT \|^UPDATE \|^DELETE ' verbatim)"
assert_stmts "$OUT/to-sql.log" to-sql "$OUT/fwd" || exit 1
create_shadow "${DB}_fwd" "$OUT/P0.sql"
snap SETUP_FWD "${DB}_fwd"
if ! gate SETUP_FWD "$EXP_PRE"; then echo "shadow setup FAILED: ${DB}_fwd != pre-state ($EXP_PRE)" >&2; exit 1; fi
apply_product "$OUT/fwd" "${DB}_fwd"
snap FWD_SHADOW "${DB}_fwd"

if [ "${SHADOW_NEGCHECK:-0}" = "1" ]; then
  # [5/6] 负自检：灌产物后、比对前注入单行漂移 → 前向闸必须抓到（成功=FAIL）
  echo "== [neg] drift inject: UPDATE ${DB}_fwd.t_ord SET qty=qty+1 WHERE id=MIN(id)"
  local_min=$(p3e2e_sql "$CTR" -N -e "SELECT MIN(id) FROM \`${DB}_fwd\`.t_ord")
  p3e2e_sql "$CTR" -e "UPDATE \`${DB}_fwd\`.t_ord SET qty = qty + 1 WHERE id=$local_min"
  snap FWD_SHADOW "${DB}_fwd"
  if gate FWD_SHADOW "$EXP_POST"; then
    echo "NEGCHECK FAILED: drift injected but gate stayed GREEN (gate is toothless)" >&2
    exit 1
  fi
  echo "NEGCHECK: expected mismatch observed — three-way checksum gate catches drift"
  exit 0
fi

if ! gate FWD_SHADOW "$EXP_POST"; then exit 1; fi

# ── [4/6] 逆向闸：flashback 产物灌后态影子 clone == 前态 ─────────────────────
echo "== [4/6] reverse: flashback (P2 offline channel) -> ${DB}_rev == P0"
create_shadow "${DB}_rev" "$OUT/P1.sql"
snap SETUP_REV "${DB}_rev"
if ! gate SETUP_REV "$EXP_POST"; then echo "shadow setup FAILED: ${DB}_rev != post-state ($EXP_POST)" >&2; exit 1; fi
"$BIN" flashback \
  --binlog-dir "$OUT/binlog" --start-file "$f0" --start-pos "$p0" \
  --schema-file "$OUT/schema.json" --time-zone +00:00 \
  --output-dir "$OUT/rev" 2>&1 | tee "$OUT/flashback.log"
grep -q "^flashback done:" "$OUT/flashback.log" || { echo "flashback produced no summary line" >&2; exit 1; }
echo "   rev product: DML lines=$(dml_count "$OUT/rev") (grep -c '^INSERT \|^UPDATE \|^DELETE ' verbatim)"
assert_stmts "$OUT/flashback.log" flashback "$OUT/rev" || exit 1
apply_product "$OUT/rev" "${DB}_rev"
snap REV_SHADOW "${DB}_rev"
if ! gate REV_SHADOW "$EXP_PRE"; then exit 1; fi

# ── [5/6] 往返闸：前向影子逆灌同一产物 == 前态（三角自洽） ───────────────────
echo "== [5/6] roundtrip: ${DB}_fwd --(same rev product)--> == P0"
apply_product "$OUT/rev" "${DB}_fwd"
snap RT_SHADOW "${DB}_fwd"
if ! gate RT_SHADOW "$EXP_PRE"; then exit 1; fi

# ── [6/6] 汇总 ──────────────────────────────────────────────────────────────
echo "== [6/6] summary"
echo "   fwd product DML lines=$(dml_count "$OUT/fwd")"
echo "   rev product DML lines=$(dml_count "$OUT/rev") (roundtrip applies the same files)"
echo "   window=$f0:$p0..$f1:$p1 tables=$(wc -l < "$OUT/P0.checksum")"
echo "OK shadow-replay: forward($EXP_POST)==${DB}_fwd + reverse($EXP_PRE)==${DB}_rev + roundtrip($EXP_PRE)==${DB}_fwd (ver=$VER, anchor=$ANCHOR, ${SECONDS}s elapsed)"
