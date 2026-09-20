#!/usr/bin/env bash
# Task 15: 参数化起官方 mysql 容器（5.6|5.7|8.0|8.4），ROW 全镜像 binlog，
# 挂载 data/<version>/ 到 /var/lib/mysql。stdout 打印映射到宿主的端口。
# 8.x 追加 mysql_native_password（Go 裁判 vendor 的旧 go-sql-driver 不支持
# caching_sha2；8.4 该选项已改名 --authentication-policy，T17 处理）。
# 容器生命周期归调用方（run-difftest.sh 用 trap 清理）。
set -euo pipefail

VER="${1:?usage: docker-mysql.sh <5.6|5.7|8.0|8.4>}"
NAME="my2sql-dt-${VER}"
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

EXTRA=()
case "$VER" in
  5.6) EXTRA+=(--log-bin=mysql-bin --binlog-format=row --binlog-row-image=full --server-id=1) ;;
  5.7) EXTRA+=(--log-bin=mysql-bin --binlog-format=row --binlog-row-image=full --server-id=1) ;;
  8.0) EXTRA+=(--log-bin=mysql-bin --binlog-format=row --binlog-row-image=full --server-id=1
               --default-authentication-plugin=mysql_native_password) ;;
  8.4) EXTRA+=(--log-bin=mysql-bin --binlog-format=row --binlog-row-image=full --server-id=1
               --authentication-policy=mysql_native_password) ;; # 8.4 默认禁用 native password，须显式放开（T17 验证）
  *)   echo "unsupported version $VER (want 5.6|5.7|8.0|8.4)" >&2; exit 1 ;;
esac

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

docker port "$NAME" 3306/tcp | head -1 | sed 's/.*://'
