#!/usr/bin/env bash
# 测试用 Redis 容器:start | stop | test [cargo 参数]
# 只动容器 xredis-test-redis(127.0.0.1:6391),不碰其他 Redis。
set -euo pipefail

NAME="xredis-test-redis"
PORT="6391"
IMAGE="redis:8"
# 只用于本机测试容器;可用环境变量 XREDIS_TEST_REDIS_PASSWORD 覆盖
PASSWORD="${XREDIS_TEST_REDIS_PASSWORD:-xredis-test-pw}"
TEST_TIMEOUT="${XREDIS_TEST_TIMEOUT:-900}"

start() {
  if [ "$(docker ps -q -f "name=^${NAME}$")" ]; then
    echo "$NAME already running"
    return
  fi
  docker rm -f "$NAME" >/dev/null 2>&1 || true
  docker run -d --name "$NAME" -p "127.0.0.1:${PORT}:6379" "$IMAGE" \
    redis-server --requirepass "$PASSWORD" --save "" --appendonly no >/dev/null
  for _ in $(seq 1 30); do
    if docker exec "$NAME" redis-cli -a "$PASSWORD" --no-auth-warning ping 2>/dev/null | grep -q PONG; then
      echo "$NAME ready on 127.0.0.1:${PORT}"
      return
    fi
    sleep 1
  done
  echo "$NAME did not become ready in 30s" >&2
  exit 1
}

stop() {
  docker rm -f "$NAME" >/dev/null 2>&1 || true
  echo "$NAME stopped"
}

case "${1:-}" in
  start) start ;;
  stop) stop ;;
  test)
    shift
    start
    export XREDIS_TEST_REDIS_URL="redis://default:${PASSWORD}@127.0.0.1:${PORT}"
    export XREDIS_TEST_REDIS_CONTAINER="$NAME"
    timeout "$TEST_TIMEOUT" cargo test "$@" -- --ignored
    ;;
  *)
    echo "usage: $0 start|stop|test [cargo args]" >&2
    exit 2
    ;;
esac
