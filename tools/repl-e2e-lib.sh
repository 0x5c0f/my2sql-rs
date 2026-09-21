#!/usr/bin/env bash
# P3 T6：repl live e2e 复用件——容器生命周期 + DML 灌流器（`make repl-test`
# 消费；T7 compat 矩阵 repl 族 source 本文件同函数复用，勿再造第二套）。
#
# 契约（对 tests/repl.rs 等价性总闸与 T7 矩阵同时成立）：
#   * source 零副作用：本文件只定义函数，直接执行仅打 usage 即退；
#   * 容器名唯一 `p3e2e-<ver>-<suffix>`（suffix 缺省 = 调用方 bash PID，
#     与 difftest/compat 的 my2sql-* 命名族、T5 的 p3t5live* 永不互撞）；
#     生命周期归调用方：trap 'p3e2e_container_stop "$P3E2E_CTR"' EXIT（T6
#     实踩教训：断言失败路径不清容器即泄漏——EXIT trap 是唯一保险形态）；
#   * 端口宿主随机（-p 127.0.0.1::3306），并发矩阵互不感知；
#   * 混合 DML 灌流器 p3e2e_feed_mixed：2 表 × insert/update/delete +
#     显式多行事务 + 回滚事务 + JSON 列 + BLOB 列（x'..' 二进制含 0x00）。
#     值文本只由 (tag, round) 决定、无时钟 → 同参数重复灌流逐字节同构。
#     tag 是测试的窗口指纹：A 段（stop 前）产物必须含 'Adoc1'/'Atrx5'，
#     B 段（stop 后）必须不见 'Bdoc1'，回滚垃圾必须不见 'JUNKRB'。
#   * 单档窗口假设：等价性总闸 / kill9 接续 / mtw 水位各件的断言按「灌流窗口
#     内不跨 binlog 档」设计（含 f==f0 直断）——master 侧 FLUSH/自发轮转会把
#     它们打成假红；跨档矩阵归 T7，勿拿本套件的跨档红当缺陷证据。
#   * 版本自适应（T7 免参数）：seed 探测 SELECT VERSION()——5.6 无 JSON 类型
#     （json 列降级 LONGTEXT，灌流器只用字符串 JSON 字面量 + 普通 UPDATE，
#     不触 JSON_* 函数面，两形态共用）；5.7+/8.x 真 JSON 列。
#   * 8.4 面：SHOW MASTER STATUS 已除（1064）→ p3e2e_master_pos 自动改口
#     SHOW BINARY LOG STATUS（与 src/metadata/store.rs:master_status 同臂）；
#     native 认证插件按 docker-mysql.sh 的 T17 真机勘误形态启用并 ALTER root。
#
# 导出的函数（T7 接口清单）：
#   p3e2e_container_start <ver> [suffix] [hostport]  起容器+等健康；置全局
#                                          P3E2E_CTR/P3E2E_PORT/P3E2E_URI，
#                                          stdout 打 URI。hostport 给定则
#                                          钉死 127.0.0.1 该端口（docker
#                                          restart 重分配动态口的场景，
#                                          T6b 重启件用）
#   p3e2e_wait_healthy <ctr>               就绪探测（TCP ping，180s 上限）
#   p3e2e_sql <ctr> [客户端参...]           docker exec mysql 客户端（stdin=SQL）
#   p3e2e_master_pos <ctr>                 原子位点：stdout "<file> <pos>"
#   p3e2e_seed_schema <ctr> <db>           建库 + t_doc(JSON/BLOB) + t_ord
#   p3e2e_feed_mixed <ctr> <db> <rounds> <tag>  混合 DML 灌流器
#   p3e2e_feed_bigtrx <ctr> <db> <rows> <tag>   单事务 N 行大事务（T6b kill 窗口）
#   p3e2e_capture_binlogs <ctr> <from> <to> <hostdir>
#                                          docker cp [from..to] 区段 binlog
#   p3e2e_container_stop [ctr]             docker rm -f（缺省 $P3E2E_CTR）
set -u

# ── 容器生命周期 ────────────────────────────────────────────────────────────

p3e2e_container_start() { # <5.6|5.7|8.0|8.4> [name-suffix，缺省 $$] [hostport]
  local ver=${1:?usage: p3e2e_container_start <ver> [suffix] [hostport]}
  local sfx=${2:-$$}
  local hp=${3:-}
  case "$ver" in 5.6|5.7|8.0|8.4) : ;;
    *) echo "p3e2e: unsupported version $ver (want 5.6|5.7|8.0|8.4)" >&2; return 1 ;;
  esac
  docker image inspect "mysql:$ver" >/dev/null 2>&1 || {
    echo ">> pulling mysql:$ver (timeout 900s)" >&2
    timeout 900 docker pull "mysql:$ver" >/dev/null || {
      echo "p3e2e: cannot pull mysql:$ver" >&2; return 2; }
  }
  P3E2E_CTR="p3e2e-${ver//./-}-${sfx}"
  docker rm -f "$P3E2E_CTR" >/dev/null 2>&1 || true
  local extra=(--log-bin=mysql-bin --binlog-format=row --binlog-row-image=full --server-id=1)
  local native_fixup=""
  case "$ver" in
    5.6|5.7) : ;;
    8.0) extra+=(--default-authentication-plugin=mysql_native_password) ;;
    8.4) extra+=(--mysql-native-password=ON); native_fixup=1 ;;
  esac
  # 不落 data/ 挂卷：repl 件跑完即 rm，overlay 里的 binlog 不污染 P1/P2
  # difftest 复用的 data/<ver> 目录（等价性件靠 docker cp 取段）。
  local port_arg=(-p "127.0.0.1::3306")
  [ -n "$hp" ] && port_arg=(-p "127.0.0.1:$hp:3306")
  docker run -d --name "$P3E2E_CTR" \
    -e MYSQL_ALLOW_EMPTY_PASSWORD=1 -e TZ=UTC \
    "${port_arg[@]}" --memory=2g \
    "mysql:$ver" "${extra[@]}" >/dev/null
  p3e2e_wait_healthy "$P3E2E_CTR"
  if [ -n "$native_fixup" ]; then
    docker exec "$P3E2E_CTR" mysql -uroot -e \
      "ALTER USER 'root'@'%' IDENTIFIED WITH mysql_native_password BY ''; FLUSH PRIVILEGES;"
  fi
  P3E2E_PORT=$(docker port "$P3E2E_CTR" 3306/tcp | head -1 | sed 's/.*://')
  P3E2E_URI="mysql://root@127.0.0.1:$P3E2E_PORT"
  echo "$P3E2E_URI"
}

# 就绪 = 容器内 TCP ping（首启 initialize 阶段 skip-networking，
# socket ping 会误报就绪——docker-mysql.sh 同款实踩）。
p3e2e_wait_healthy() { # <ctr>
  local ctr=${1:?usage: p3e2e_wait_healthy <ctr>} i
  for i in $(seq 1 180); do
    docker exec "$ctr" mysqladmin -uroot -h127.0.0.1 -P3306 ping >/dev/null 2>&1 && return 0
    sleep 1
  done
  echo "p3e2e: $ctr not healthy in 180s" >&2
  docker logs "$ctr" >&2 || true
  return 1
}

p3e2e_container_stop() { # [ctr]（缺省 $P3E2E_CTR）
  local ctr=${1:-${P3E2E_CTR:-}}
  [ -n "$ctr" ] || return 0
  docker rm -f "$ctr" >/dev/null 2>&1 || true
}

# ── SQL 通道与位点 ──────────────────────────────────────────────────────────

# 客户端统一 --default-character-set=utf8mb4（灌入中文/emoji 指纹值的字面
# 量在 5.6+ 各版本均按 utf8mb4 落 binlog，绕开 latin1 默认猜测）。
p3e2e_sql() { # <ctr> [mysql 客户端参...]；SQL 走 stdin
  local ctr=${1:?usage: p3e2e_sql <ctr> [client args...]}; shift
  docker exec -i "$ctr" mysql --default-character-set=utf8mb4 -uroot "$@"
}

# 原子位点：单条 SHOW MASTER STATUS 同回 File+Position（8.4 回退
# SHOW BINARY LOG STATUS）。stdout = "<file> <pos>"。
p3e2e_master_pos() { # <ctr>
  local ctr=${1:?usage: p3e2e_master_pos <ctr>} out
  out=$(p3e2e_sql "$ctr" -N -e "SHOW MASTER STATUS" 2>/dev/null) || \
    out=$(p3e2e_sql "$ctr" -N -e "SHOW BINARY LOG STATUS")
  [ -n "$out" ] || { echo "p3e2e: master status empty on $ctr (log-bin on?)" >&2; return 1; }
  echo "$out" | awk '{print $1, $2}'
}

# ── schema + 灌流 ───────────────────────────────────────────────────────────

p3e2e_seed_schema() { # <ctr> <db>
  local ctr=${1:?usage: p3e2e_seed_schema <ctr> <db>} db=${2:?db required}
  local json_type
  case $(p3e2e_sql "$ctr" -N -e "SELECT VERSION()" | cut -d. -f1,2) in
    5.6) json_type=LONGTEXT ;;   # 5.6 无 JSON 类型（灌流器只喂字面量，兼容）
    *)   json_type=JSON ;;
  esac
  p3e2e_sql "$ctr" <<SQL
DROP DATABASE IF EXISTS \`$db\`;
CREATE DATABASE \`$db\`;
CREATE TABLE \`$db\`.\`t_doc\` (
  id INT NOT NULL AUTO_INCREMENT PRIMARY KEY,
  name VARCHAR(64) NOT NULL,
  payload $json_type NULL,
  buf BLOB NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
CREATE TABLE \`$db\`.\`t_ord\` (
  id INT NOT NULL AUTO_INCREMENT PRIMARY KEY,
  sku VARCHAR(32) NOT NULL,
  qty INT NOT NULL,
  note VARCHAR(64) NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
SQL
}

# 混合 DML 灌流器：每轮 = t_doc 双 insert（JSON+BLOB / NULL 形）+ JSON 列
# UPDATE + BLOB 追加 UPDATE + t_ord 双行 insert + DELETE + 一个显式多行事务
# （10 行 INSERT、跨表 UPDATE、COMMIT）+ 一个回滚事务（垃圾行必不出现在产物）。
# 值文本 = f(tag, round) 纯函数（无时钟/随机），tag 即窗口指纹：
#   tag=A 段必须可见（Adoc1/Atrx5/0x00FF10DE2AD0/中文），tag=B 段必须不可见。
# 收尾刻意留一条完整事务（'Alast'/Blast）之后的边界由调用方的 stop 位点
# 判定，不在此函数内做。
p3e2e_feed_mixed() { # <ctr> <db> <rounds>=<N> <tag>
  local ctr=${1:?usage: p3e2e_feed_mixed <ctr> <db> <rounds> <tag>} db=${2:?db} rounds=${3:?rounds} tag=${4:?tag} r
  case "$rounds" in (*[!0-9]*|'') echo "p3e2e_feed_mixed: rounds must be a positive integer" >&2; return 1;; esac
  for r in $(seq 1 "$rounds"); do
    p3e2e_sql "$ctr" <<SQL
USE \`$db\`;
SET autocommit=1;
INSERT INTO t_doc (name, payload, buf)
  VALUES ('${tag}doc${r}中文', '{"r": ${r}, "tag": "${tag}", "列表": [1, 2, null]}', x'00ff10de2ad0');
INSERT INTO t_doc (name, payload, buf) VALUES ('${tag}nul${r}', NULL, NULL);
INSERT INTO t_ord (sku, qty, note) VALUES ('${tag}sku${r}', ${r}, NULL), ('${tag}skub${r}', ${r} * 2, '${tag}备注');
UPDATE t_doc SET payload = '{"r": ${r}, "tag": "${tag}", "upd": true}' WHERE name = '${tag}doc${r}中文';
UPDATE t_doc SET buf = CONCAT(buf, x'0102ff') WHERE name = '${tag}nul${r}' OR name = '${tag}doc${r}中文';
DELETE FROM t_ord WHERE sku = '${tag}skub${r}';
START TRANSACTION;
INSERT INTO t_ord (sku, qty, note) VALUES
  ('${tag}trx1', 1, 'a'), ('${tag}trx2', 2, NULL), ('${tag}trx3', 3, '多行'),
  ('${tag}trx4', 4, 'd'), ('${tag}trx5', 5, 'e'), ('${tag}trx6', 6, 'f'),
  ('${tag}trx7', 7, 'g'), ('${tag}trx8', 8, 'h'), ('${tag}trx9', 9, 'i'),
  ('${tag}trx10', 10, 'j');
UPDATE t_ord SET qty = qty + 100 WHERE sku LIKE '${tag}trx%';
INSERT INTO t_doc (name, payload, buf) VALUES ('${tag}trxdoc', '{"trx": 1}', x'0a0d1a00');
COMMIT;
START TRANSACTION;
INSERT INTO t_ord (sku, qty, note) VALUES ('JUNKRB${tag}${r}', 999999, 'must never appear');
ROLLBACK;
SQL
  done
  # 末条独立 autocommit 语句：让「stop 位点恰落在事务边界」的窗口收尾形态
  # 可预期（该条可能被 stop 的等号排除，双闸口径由测试比较器兜住）。
  p3e2e_sql "$ctr" <<SQL
USE \`$db\`;
INSERT INTO t_ord (sku, qty, note) VALUES ('${tag}last', 0, 'boundary');
SQL
}

# 大事务灌流器（T6b kill-9 件专用）：单事务 = 一个 N 行 INSERT（binlog 侧被
# binlog_row_event_max_size 切成几十~上百个 rows 事件，全部只在 COMMIT 时落
# binlog → 副本端在 commit 瞬间收到一整段背靠背事件流）。用途：kill/restart
# 必须落在「事务已开流、XID 未到」的窗口内才有可证的跨界重复，这段窗口由
# 本函数拉长到几十毫秒量级（逐事件构建+落盘），而该事务的提交位点（下一个
# checkpoint 边界）在其后 ≥80 个事件之外——水位在 kill 时必然仍停在事务前
# 界。值 =f(tag,i) 纯函数，行标签 '${tag}1'..'${tag}N' 全局唯一可点验。
p3e2e_feed_bigtrx() { # <ctr> <db> <rows>=<N> <tag>
  local ctr=${1:?usage: p3e2e_feed_bigtrx <ctr> <db> <rows> <tag>} db=${2:?db}
  local rows=${3:?rows} tag=${4:?tag}
  case "$rows" in (*[!0-9]*|'') echo "p3e2e_feed_bigtrx: rows must be positive int" >&2; return 1;; esac
  {
    printf 'USE `%s`;\nSTART TRANSACTION;\nINSERT INTO t_ord (sku, qty, note) VALUES\n' "$db"
    awk -v n="$rows" -v t="$tag" \
      'BEGIN{for(i=1;i<=n;i++){if(i>1)printf ",";printf "(\x27%s%d\x27,%d,\x27b\x27)",t,i,i}printf ";\nCOMMIT;\n"}'
  } | p3e2e_sql "$ctr"
  echo BIGTRX_DONE
}

# 取段：docker cp [from..to]（含）区段 binlog 到宿主目录，供 file 模式对照。
# 名次序 = 前缀 + %06d 十进制（跨 999999→1000000 进位自然 +1）。
p3e2e_capture_binlogs() { # <ctr> <from-file> <to-file> <hostdir>
  local ctr=${1:?usage: p3e2e_capture_binlogs <ctr> <from> <to> <hostdir>}
  local from=${2:?from} to=${3:?to} hostdir=${4:?hostdir}
  mkdir -p "$hostdir"
  local dd; dd=$(p3e2e_sql "$ctr" -N -e "SELECT @@datadir")
  dd=${dd%/}
  local f=$from prefix n guard=0
  while :; do
    docker cp "$ctr:$dd/$f" "$hostdir/" >/dev/null || {
      echo "p3e2e: docker cp $ctr:$dd/$f failed" >&2; return 1; }
    [ "$f" = "$to" ] && break
    guard=$((guard + 1)); [ "$guard" -gt 64 ] && { echo "p3e2e: binlog range walk >64 files, aborting" >&2; return 1; }
    prefix=${f%.*}; n=$((10#${f##*.} + 1))
    f=$(printf '%s.%06d' "$prefix" "$n")
  done
}

# 直接执行（非 source）= 独立冒烟：起容器→灌一轮→打位点→清理（调试入口，
# 不跑测试；正常消费方是 source + make repl-test / T7 矩阵）。
if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  set -euo pipefail
  v=${1:-8.0}
  p3e2e_container_start "$v" "$$"
  trap 'p3e2e_container_stop' EXIT
  p3e2e_seed_schema "$P3E2E_CTR" p3e2e_smoke
  p3e2e_feed_mixed "$P3E2E_CTR" p3e2e_smoke 1 A
  echo "uri=$P3E2E_URI pos=$(p3e2e_master_pos "$P3E2E_CTR")"
fi
