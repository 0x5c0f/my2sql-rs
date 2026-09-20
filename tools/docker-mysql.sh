#!/usr/bin/env bash
# Task 15: 参数化起官方 mysql 容器（5.6|5.7|8.0|8.4），ROW 全镜像 binlog，
# 挂载 data/<version>/ 到 /var/lib/mysql。stdout 打印映射到宿主的端口。
# 8.x 追加 mysql_native_password（Go 裁判 vendor 的旧 go-sql-driver 不支持
# caching_sha2）。T17 真机勘误：8.4.11 下 --authentication-policy=mysql_native_password
# 直接启动失败（MY-013797，插件默认 OFF 不算合法 policy）；正确姿势 =
# --mysql-native-password=ON 启插件 + 建库后 ALTER root@'%' 为 native
# （MYSQL_ALLOW_EMPTY_PASSWORD 把 root@% 建成 caching_sha2 空密码，旧驱动拒）。
# T17 环境开关：
#   CKSUM=none|crc32  显式设 --binlog-checksum（默认不传=服务器默认：
#                     实测 5.6.51/5.7.44/8.x 镜像默认均 CRC32；NONE 路径由 CKSUM=none 用例覆盖）
#   V1ROWS=1          追加 --log-bin-use-v1-row-events=1（5.6/5.7 选项；
#                     真机 V1 rows 事件 23/24/25 解码路径的唯一产法，T17 矩阵用例）
#   AUTH=stock        不传 native policy（T17 用 8.4 默认 caching_sha2 在线元数据探针）
#   DT_NAME=name      覆盖容器名（T17 探针与差分容器共存/错开）
# 容器生命周期归调用方（run-difftest.sh 用 trap 清理）。
set -euo pipefail

VER="${1:?usage: docker-mysql.sh <5.6|5.7|8.0|8.4>}"
NAME="${DT_NAME:-my2sql-dt-${VER}}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DATADIR="$ROOT/data/$VER"
mkdir -p "$DATADIR"
# 官方镜像内 mysql 用户 uid=999；bind mount 须可写。宿主 chown 无权限时
# 借 docker daemon（root）跑一次性 --entrypoint chown 助手容器修正属主。
if [ "$(stat -c %u "$DATADIR")" != "999" ]; then
  chown -R 999:999 "$DATADIR" 2>/dev/null || \
    docker run --rm -u 0 -v "$DATADIR:/d" --entrypoint chown mysql:"$VER" -R 999:999 /d
fi
docker rm -f "$NAME" >/dev/null 2>&1 || true

EXTRA=(--log-bin=mysql-bin --binlog-format=row --binlog-row-image=full --server-id=1)
NATIVE_FIXUP=""
case "$VER" in
  5.6|5.7) : ;;
  8.0) # T17 修复轮 1：原为裸 `[ cond ] && EXTRA+=(...)`（set -e 下的脆弱形态，
       # AUTH=stock 条件为假时分支状态即 1；bash 5.2 实测豁免未 abort，但
       # 该模式依赖 && 列表豁免，函数化/换 shell 即成雷）→ 显式 if。
       if [ "${AUTH:-native}" = native ]; then
         EXTRA+=(--default-authentication-plugin=mysql_native_password)
       fi ;;
  8.4) if [ "${AUTH:-native}" = native ]; then
         EXTRA+=(--mysql-native-password=ON)   # 8.4 默认禁用 native 插件（T17 真机勘误）
         NATIVE_FIXUP=1                        # 就绪后把 root@% 改回 native（Go 裁判用）
       fi ;;
  *)   echo "unsupported version $VER (want 5.6|5.7|8.0|8.4)" >&2; exit 1 ;;
esac
if [ -n "${CKSUM:-}" ]; then EXTRA+=("--binlog-checksum=$CKSUM"); fi
if [ -n "${V1ROWS:-}" ]; then EXTRA+=(--log-bin-use-v1-row-events=1); fi

docker run -d --name "$NAME" \
  -e MYSQL_ALLOW_EMPTY_PASSWORD=1 \
  -e TZ=UTC \
  -v "$DATADIR:/var/lib/mysql" \
  -p 127.0.0.1::3306 \
  --memory=2g \
  mysql:"$VER" "${EXTRA[@]}" >/dev/null

# 等就绪：必须 TCP ping——首启的 initialize 阶段临时 server 走 skip-networking，
# socket ping 会误报就绪并在真正 server 切换时断连（实踩）。
for i in $(seq 1 180); do
  if docker exec "$NAME" mysqladmin -uroot -h127.0.0.1 -P3306 ping >/dev/null 2>&1; then break; fi
  if [ "$i" = 180 ]; then echo "mysql $VER not ready in 180s" >&2; docker logs "$NAME" >&2; exit 1; fi
  sleep 1
done

# 8.4 native 修：root@% 初建为 caching_sha2（空密码），旧驱动裁判连不上 → 改 native
if [ -n "$NATIVE_FIXUP" ]; then
  docker exec "$NAME" mysql -uroot -e \
    "ALTER USER 'root'@'%' IDENTIFIED WITH mysql_native_password BY ''; FLUSH PRIVILEGES;"
fi

docker port "$NAME" 3306/tcp | head -1 | sed 's/.*://'
