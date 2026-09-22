#!/usr/bin/env bash
# P4a T1：fuzz 最小闸 —— 每靶 300s×1 轮，crash 归零判据（spec §1）。
#
# 实测修正（报告登记）：cargo-fuzz 0.13 从仓库根解析 <root>/fuzz/Cargo.toml，
# 子 shell `cd fuzz` 会二次拼接成 fuzz/fuzz 而失败（简报已预留「以实测为准
# 修正」）；故 run 在仓库根发起，产物与日志逐靶落 out/fuzz/<t>/。
#
# 评审修复轮 1 两条加固：
#  ① 语料隔离：libfuzzer 会把运行期合并输入写回它跑的那个 corpus 目录。早期
#     版本直接指向仓库内 `fuzz/corpus/<t>`，一轮下来写回数百件、脏了 git 树
#     （与「禁随机入仓」冲突）。现改为 run 前 cp 到 `out/fuzz/<t>/corpus/`
#     跑副本，仓库语料全程只读。
#  ② 禁假绿：`wait`（无操作数）恒返回 0， cargo-fuzz 缺失/构建失败/超时被杀
#     都会以「crashes=0」伪装成通过。现逐靶取 `wait $pid` 退出码，非 0 且
#     无 crash artifact ⇒ 判 FAIL（附 run.log 指引），只有「两靶 exit 0 +
#     0 新 crash」才 exit 0。
set -euo pipefail
cd "$(dirname "$0")/.."
TIME="${FUZZ_TIME:-300}"
ROOT=$PWD
TARGETS=(decode_event event_stream)

pids=()
for t in "${TARGETS[@]}"; do
  D="out/fuzz/$t"
  mkdir -p "$D"
  # 上一轮的 crash 件归档到 prev/：本轮计数只反映本轮，且不销毁证据
  if compgen -G "$D/crash-*" >/dev/null; then
    mkdir -p "$D/prev"
    mv "$D"/crash-* "$D/prev/"
    echo "[fuzz-min] $t: 上轮 crash 件已归档 $D/prev/（不计入本轮）"
  fi
  # 终审轮 FIX C：prev/ 非空（含上条刚归档的）即显式 WARNING——
  # 防「归档后重跑绿」被误读为「从未 crash」。
  if [ -d "$D/prev" ]; then
    pc=$(find "$D/prev" -maxdepth 1 -type f -name 'crash-*' | wc -l)
    if [ "$pc" != 0 ]; then
      echo "[fuzz-min] $t: WARNING: $pc 件历史 crash 于 prev/（本轮计数不含上轮）"
    fi
  fi
  # 语料隔离：仓库内 fuzz/corpus/<t> 只读，副本落 out/
  rm -rf "$D/corpus"
  mkdir -p "$D/corpus"
  cp -a "fuzz/corpus/$t/." "$D/corpus/"
  timeout $((TIME + 240)) cargo +nightly fuzz run "$t" \
     "$D/corpus" -- "-max_total_time=$TIME" "-artifact_prefix=$ROOT/$D/" \
     > "$D/run.log" 2>&1 &
  pids+=("$!")
done

n=0
fail=0
for i in "${!TARGETS[@]}"; do
  t="${TARGETS[$i]}"
  D="out/fuzz/$t"
  st=0
  wait "${pids[$i]}" || st=$?
  c=$(find "$D" -maxdepth 1 -type f -name 'crash-*' | wc -l)
  echo "[fuzz-min] $t: exit=$st crashes=$c"
  n=$((n + c))
  if [ "$st" != 0 ] && [ "$c" = 0 ]; then
    # 退出码非 0 却无 crash 件 = 运行本身失败（缺工具/构建错/超时被杀），
    # 不能算「0 crash 通过」。
    echo "[fuzz-min] $t: 运行失败（exit $st）且非 crash 所致 —— 见 $D/run.log 末 20 行"
    tail -n 20 "$D/run.log" || true
    fail=1
  fi
done
[ "$n" = 0 ] || echo "[fuzz-min] crash artifacts: $n"
if [ "$fail" = 0 ] && [ "$n" = 0 ]; then
  echo "[fuzz-min] OK 0 new crashes"
  exit 0
fi
echo "[fuzz-min] FAIL（crashes=$n, run_failure=$fail）"
exit 1
