#!/usr/bin/env bash
# P5-T3：本机双目标 release 构建（CI release 链的本机等价试验 + 兜底产物源）。
# 产物：$OUT/my2sql-rs-<ver>-x86_64-unknown-linux-{gnu,musl} + SHA256SUMS
# 纪律（spec D2/D4）：版本单源 cargo metadata，脚本内无版本字面量；
# 每 target 独立 CARGO_TARGET_DIR（跨历史构建串味条例，P4b T3 先例）。
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/out/release-local}"
# 版本提取（控制器裁定 1 定稿）：jq 主形（本机在册 /64 实测），python3 兜底。
# sed 串扰风险形弃用（多包 metadata JSON 上 .* 贪婪可误匹配）。
VER=$(cargo metadata --format-version 1 --no-deps --manifest-path "$ROOT/Cargo.toml" \
      | { jq -r '.packages[0].version' 2>/dev/null \
          || python3 -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["version"])'; })
[ -n "$VER" ] && [ "$VER" != null ] || { echo "FATAL: 版本读取失败（cargo metadata 形态变更？）" >&2; exit 1; }
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
