#!/usr/bin/env bash
# Task 15 差分总入口：容器产 binlog → 构建 Go 裁判 + Rust → 双方 to-sql →
# 语义比较器 → 退出码。`make difftest` 即本脚本。Rust 用 debug 构建（省时；
# 语义与 release 无差，性能测试不归本 harness）。容器经 trap 清理。
# T17 扩展（均为环境开关，默认行为与 T15 一致）：
#   VER=5.6|5.7|8.0|8.4   目标服务器版本（默认 8.0）
#   CKSUM=none|crc32      显式 binlog_checksum（默认不传=服务器默认；产物目录加后缀）
#   V1ROWS=1              v1 rows 事件模式（docker-mysql.sh 透传；产物目录加后缀）
#   gen-data-$VER.sql 存在则优先（5.6 无 JSON 类型的矩阵级裁剪）
#   步骤 7 = 离线 schema 回放（在线 dump → --schema-file 重跑 → 与在线输出逐字节对差分）
#   步骤 3.5（仅 V1ROWS=1）= binlog 事件类型普查（tools/event-census.py，纯 stdlib
#   走读事件头，无需镜像内 mysqlbinlog）；产物 $OUT/EVENT_CENSUS.txt，
#   硬断言行事件全 V1（23/24/25 齐、30/31/32 零），违例即用例 FAIL（T17 修复轮 1）。
# P2-T7 扩展（均为环境开关，WORK_TYPE 缺省=2sql 时行为与 P1 逐字节一致）：
#   WORK_TYPE=2sql|rollback|stats   工作类型维度（产物目录后缀：2sql 无 / rollback
#     加 -rb / stats 加 -stats）：rollback = 裁判 -work-type rollback vs 我方
#     flashback，比较器第三参 rollback（scaffold 剥离 + 注释漂尾绑定 + B 侧结构断言）；
#     stats = 冒烟（不裁判比较）：我方 to-sql 产基线语料 + stats 两报表存在 +
#     Σinserts+updates+deletes（跳 '#' 尾注行）== 同语料 to-sql DML 语句数；
#     裁判 stats 输出仅留档 $OUT/go-stats/，不参与退出码。
# P3-T8 挂账 B 扩展（同维度内增量，仍不裁判）：
#   --dml×stats 一致性冒烟 = 同一 binlog 上分别跑 `to-sql --dml insert` 与
#   `stats --dml insert`，比较器断言 stats 报表 inserts 总和 == to-sql
#   `--dml insert` 的 INSERT 语句行数，且 updates/deletes 总和为 0
#   （dml 过滤器跨通道语义一致；stats 无 Go 裁判，本维度为我方双通道自证）。
# P4a T3 (Lane C) 扩展（环境开关，默认关＝既有行为逐字节不变）：
#   P4A=1   捕获表组切换：GEN=tools/gen-data-p4a.sql + OUT 后缀 -p4a（ENUM>255 /
#     GEOMETRY / LONGBLOB>64K 三形，仅 8.0 主闸）；六步流程原样继承，compat 家族不涉及。
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"
# P4a T5 合流接线：debug 二进制路径随 CARGO_TARGET_DIR 解析（并行 lane/合流
# 役各自独立 target 目录是 spec §6 纪律；本脚本此前硬编码 ./target/debug，
# 只在「worktree 恰好有本地 target」时碰巧成立）。与 shadow-replay.sh 的
# BIN="${CARGO_TARGET_DIR:-$ROOT/target}/debug/my2sql-rs" 同口径。
RSBIN="${CARGO_TARGET_DIR:-$ROOT/target}/debug/my2sql-rs"
VER="${VER:-8.0}"
NAME="my2sql-dt-${VER}"
WORK_TYPE="${WORK_TYPE:-2sql}"
case "$WORK_TYPE" in
  2sql)     SFX="" ;;
  rollback) SFX="-rb" ;;
  stats)    SFX="-stats" ;;
  *) echo "WORK_TYPE must be 2sql|rollback|stats, got '$WORK_TYPE'" >&2; exit 2 ;;
esac
# P4a T3 (Lane C)：P4A 非空 = 捕获表组（GEN 选择见下；产物目录加 -p4a 后缀）。
# 缺省空 = 不进入任何新分支，既有路径逐字节不变。
# 修复轮 1（评审 Minor#3）：P4A=1 时主闸仅 8.0——非 8.0 显式拒绝而非静默
# 按 VER 镜像跑 p4a 表组（GEOMETRY/SRID 等形在 5.x 语义不同）。仍在 P4A 门内，
# 默认路径零变化。
P4A="${P4A:-}"
if [ -n "$P4A" ]; then
  if [ "$VER" != "8.0" ]; then echo "P4A 仅 8.0 主闸" >&2; exit 2; fi
  SFX="$SFX-p4a"
fi
OUT="$ROOT/out/difftest-$VER${CKSUM:+-$CKSUM}${V1ROWS:+-v1rows}$SFX"
rm -rf "$OUT" && mkdir -p "$OUT/go" "$OUT/rs" tools/bin
trap 'rc=$?; if [ $rc -ne 0 ] && [ -n "${KEEP:-}" ]; then echo "FAILED(rc=$rc) — container $NAME kept for debug"; else docker rm -f "$NAME" >/dev/null 2>&1 || true; fi' EXIT

echo "== [1/7] comparator selftest"
python3 tools/comparator/selftest.py

echo "== [2/7] build rust (debug) + go oracle"
cargo build --quiet
(export PATH="$PATH:/opt/go/bin"; cd reference/my2sql-go && go build -o ../../tools/bin/my2sql-go .)

# T17 修复轮 1：裸 `[ ] &&` 同型隐患 → 显式 if（见 docker-mysql.sh 注释）
if [ -n "${V1ROWS:-}" ]; then ROWSMODE=V1; else ROWSMODE=V2; fi
echo "== [3/7] mysql:$VER container + data matrix (checksum=${CKSUM:-server-default}, rows=$ROWSMODE)"
# 清空 datadir（999 属主文件宿主不可删——借 root 容器 wipe，保证每次全新状态）
if [ -d "data/$VER" ]; then
  docker run --rm -u 0 -v "$ROOT/data/$VER:/d" --entrypoint sh mysql:"$VER" \
    -c 'find /d -mindepth 1 -delete' >/dev/null
fi
PORT="$(bash tools/docker-mysql.sh "$VER")"
echo "   host port: $PORT"
docker exec "$NAME" mysql -uroot -N -e \
  "SELECT CONCAT('   server: ', VERSION(), ' binlog_checksum=', @@global.binlog_checksum, ' v1_row_events=', @@global.log_bin_use_v1_row_events)" 2>/dev/null \
  || docker exec "$NAME" mysql -uroot -N -e "SELECT CONCAT('   server: ', VERSION(), ' binlog_checksum=', @@global.binlog_checksum)"
GEN="tools/gen-data-${VER}.sql"
[ -f "$GEN" ] || GEN=tools/gen-data.sql
if [ -n "$P4A" ]; then GEN="tools/gen-data-p4a.sql"; fi
echo "   data script: $GEN"
docker exec -i "$NAME" mysql --default-character-set=utf8mb4 < "$GEN"
docker exec "$NAME" mysql -uroot -e "FLUSH BINARY LOGS"   # 封口，保证数据文件事件完整
# 数据全部落在 FLUSH 前的最后一个文件（SHOW BINARY LOGS 倒数第二个）
BIN="$(docker exec "$NAME" mysql -uroot -N -e "SHOW BINARY LOGS" | awk '{print $1}' | tail -2 | head -1)"
echo "   data binlog: $BIN"
echo "$BIN" > "$OUT/BINLOG"
# binlog 属 uid999(mode 640) 宿主不可读——root 助手容器放开读取（datadir 遍历 + binlog 可读）
docker run --rm -u 0 -v "$ROOT/data/$VER:/d" --entrypoint sh mysql:"$VER" \
  -c "find /d -maxdepth 1 -name 'mysql-bin*' -exec chmod a+r {} + && chmod a+rX /d" >/dev/null

if [ -n "${V1ROWS:-}" ]; then
  echo "== [3.5/7] event-type census (V1ROWS=1 hard gate → $OUT/EVENT_CENSUS.txt)"
  python3 tools/event-census.py "data/$VER/$BIN" --assert-v1-only | tee "$OUT/EVENT_CENSUS.txt"
fi

if [ "$WORK_TYPE" = stats ]; then
  echo "== [4/7] go oracle (-work-type stats) → 留档 $OUT/go-stats/（人工对照，不参与退出码）"
  mkdir -p "$OUT/go-stats"
  (export TZ=UTC; cd "$OUT/go-stats" && "$ROOT/tools/bin/my2sql-go" \
     -mode file -work-type stats -mysql-type mysql \
     -host 127.0.0.1 -port "$PORT" -user root -password "" \
     -local-binlog-file "$ROOT/data/$VER/$BIN" \
     -threads 4 -output-dir . ) > "$OUT/go-stats/go-stats.log" 2>&1 \
     || echo "   go stats FAILED（输出已留档 $OUT/go-stats/，冒烟维度不据此判红）"
else
  echo "== [4/7] go oracle (-mode file -work-type $WORK_TYPE, TZ=UTC)"
  (export TZ=UTC; cd "$OUT/go" && "$ROOT/tools/bin/my2sql-go" \
     -mode file -work-type "$WORK_TYPE" -mysql-type mysql \
     -host 127.0.0.1 -port "$PORT" -user root -password "" \
     -local-binlog-file "$ROOT/data/$VER/$BIN" \
     -add-extraInfo -threads 4 -output-dir . ) > "$OUT/go.log" 2>&1 \
     || { tail -20 "$OUT/go.log"; echo "go oracle FAILED"; exit 1; }
  # 裁判读不到 binlog 时仅 log error 且退出码 0（实踩）——产物存在性硬校验
  if [ "$WORK_TYPE" = rollback ]; then
    ls "$OUT"/go/rollback.*.sql >/dev/null 2>&1 || { echo "go oracle produced no rollback sql"; exit 1; }
  else
    ls "$OUT"/go/forward*.sql >/dev/null 2>&1 || { echo "go oracle produced no forward sql"; exit 1; }
  fi
fi

# 我方子命令映射：2sql→to-sql / rollback→flashback（flashback 无 --to-stdout，其余参数同）
if [ "$WORK_TYPE" = rollback ]; then RSUB=flashback; else RSUB=to-sql; fi
echo "== [5/7] rust $RSUB (online schema + dump)"
"$RSBIN" $RSUB \
  --binlog-dir "data/$VER" --start-file "$BIN" \
  --uri "mysql://root@127.0.0.1:$PORT" --time-zone +00:00 \
  --add-extra-info --threads 4 --output-dir "$OUT/rs" \
  --schema-dump "$OUT/schema.json" > "$OUT/rs.log" 2>&1 \
  || { tail -20 "$OUT/rs.log"; echo "rust FAILED"; exit 1; }

if [ "$WORK_TYPE" = stats ]; then
  echo "== [5.5/7] rust stats + 冒烟断言（两报表存在 + DML 总和配平）"
  "$RSBIN" stats \
    --binlog-dir "data/$VER" --start-file "$BIN" \
    --uri "mysql://root@127.0.0.1:$PORT" --time-zone +00:00 \
    --threads 4 --output-dir "$OUT/rs-stats" > "$OUT/rs-stats.log" 2>&1 \
    || { tail -20 "$OUT/rs-stats.log"; echo "rust stats FAILED"; exit 1; }
  python3 - "$OUT" <<'PYEOF'
import sys, os, glob
out = sys.argv[1]
rep = [os.path.join(out, "rs-stats", n) for n in ("binlog_status.txt", "biglong_trx.txt")]
missing = [p for p in rep if not os.path.isfile(p)]
assert not missing, "stats report missing: " + ", ".join(missing)
# 总和从 binlog_status.txt 数据行取（跳 '#' 尾注与表头；datetime 为下划线
# 形单 token → 列序 binlog start stop startpos stoppos inserts updates deletes db tb）
tot = 0
for line in open(rep[0]):
    s = line.split()
    if line.startswith("#") or len(s) < 10 or not (s[5].isdigit() and s[6].isdigit() and s[7].isdigit()):
        continue
    tot += int(s[5]) + int(s[6]) + int(s[7])
dml = sum(1 for fn in sorted(glob.glob(os.path.join(out, "rs", "*.sql")))
          for l in open(fn) if l.strip().upper().startswith(("INSERT ", "UPDATE ", "DELETE ")))
print(f"stats smoke: report total={tot} to-sql DML lines={dml}")
assert tot == dml, f"stats total {tot} != to-sql DML {dml}"
PYEOF
  echo "== [5.6/7] rust --dml insert × stats 一致性冒烟（P3-T8 挂账 B，同形态增量）"
  "$RSBIN" to-sql \
    --binlog-dir "data/$VER" --start-file "$BIN" \
    --uri "mysql://root@127.0.0.1:$PORT" --time-zone +00:00 \
    --dml insert --threads 4 --output-dir "$OUT/rs-dml-insert" > "$OUT/rs-dml-insert.log" 2>&1 \
    || { tail -20 "$OUT/rs-dml-insert.log"; echo "rust to-sql --dml insert FAILED"; exit 1; }
  "$RSBIN" stats \
    --binlog-dir "data/$VER" --start-file "$BIN" \
    --uri "mysql://root@127.0.0.1:$PORT" --time-zone +00:00 \
    --dml insert --threads 4 --output-dir "$OUT/rs-stats-dml-insert" > "$OUT/rs-stats-dml-insert.log" 2>&1 \
    || { tail -20 "$OUT/rs-stats-dml-insert.log"; echo "rust stats --dml insert FAILED"; exit 1; }
  python3 - "$OUT" <<'PYEOF'
import sys, os, glob
out = sys.argv[1]
# stats 侧：--dml insert 下 binlog_status.txt 的 inserts/updates/deletes 总和
# （列序与解析口径同 [5.5/7]：跳 '#' 尾注与表头，datetime 下划线形单 token）
rep = os.path.join(out, "rs-stats-dml-insert", "binlog_status.txt")
assert os.path.isfile(rep), "stats report missing: " + rep
tot_ins = tot_upd = tot_del = 0
for line in open(rep):
    s = line.split()
    if line.startswith("#") or len(s) < 10 or not (s[5].isdigit() and s[6].isdigit() and s[7].isdigit()):
        continue
    tot_ins += int(s[5]); tot_upd += int(s[6]); tot_del += int(s[7])
# to-sql 侧：--dml insert 产物中的 INSERT 语句行数（无 --add-extra-info，
# 行首即语句；默认无 batch → 一行语句 = 一行数据，与 stats 行计数同口径）
ins_rows = sum(1 for fn in sorted(glob.glob(os.path.join(out, "rs-dml-insert", "*.sql")))
               for l in open(fn) if l.strip().upper().startswith("INSERT "))
print(f"--dml insert × stats: inserts={tot_ins} updates={tot_upd} deletes={tot_del} "
      f"to-sql(--dml insert) INSERT lines={ins_rows}")
assert tot_ins == ins_rows, f"stats inserts {tot_ins} != to-sql --dml insert rows {ins_rows}"
# dml 过滤器必须同时作用于 stats 通道：非 insert 行计数恒 0（跨通道语义一致）
assert tot_upd == 0 and tot_del == 0, \
    f"--dml insert leaked into stats: updates={tot_upd} deletes={tot_del}"
PYEOF
else
  echo "== [6/7] semantic compare (A=go oracle, B=rust)"
  if [ "$WORK_TYPE" = rollback ]; then
    python3 tools/comparator/compare.py "$OUT/go" "$OUT/rs" rollback
  else
    python3 tools/comparator/compare.py "$OUT/go" "$OUT/rs"
  fi
fi

if [ "$WORK_TYPE" = stats ]; then
  echo "OK difftest(stats-smoke) $VER${CKSUM:+ (checksum=$CKSUM)}: reports present + DML totals reconcile + --dml insert cross-channel consistent"
  exit 0
fi
echo "== [7/7] offline schema replay (--schema-file, 与在线输出逐字节对差)"
"$RSBIN" $RSUB \
  --binlog-dir "data/$VER" --start-file "$BIN" \
  --schema-file "$OUT/schema.json" --time-zone +00:00 \
  --add-extra-info --threads 4 --output-dir "$OUT/rs-offline" > "$OUT/rs-offline.log" 2>&1 \
  || { tail -20 "$OUT/rs-offline.log"; echo "rust offline replay FAILED"; exit 1; }
diff -r "$OUT/rs" "$OUT/rs-offline" || { echo "offline replay != online output"; exit 1; }
# T17 同型隐患规避：裸 `[ ] &&` 失败在 set -e 下误杀 → 显式 if
LABEL=""; if [ "$WORK_TYPE" = rollback ]; then LABEL="(rollback)"; fi
echo "OK difftest$LABEL $VER${CKSUM:+ (checksum=$CKSUM)}: diff-green + replay-byte-identical"
