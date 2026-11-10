#!/usr/bin/env bash
# 测试用 Redis 容器:start | start-tls | stop | test [cargo 参数] | test-old
#
# - xredis-test-redis       127.0.0.1:6391  普通连接,带密码,--databases 20
# - xredis-test-redis-tls   127.0.0.1:6392  只开 TLS,要求客户端证书(tls-auth-clients yes)
# - xredis-test-redis5      127.0.0.1:6393  redis:5,只在 test-old 里临时起,测完删掉(验证旧版 Redis 的回退)
#
# 只动这两个容器,不碰其他 Redis。TLS 证书用 openssl 现生成到 target/test-tls(git 忽略,不提交)。
set -euo pipefail

# Git Bash(MSYS)会把以 / 开头的参数(比如 openssl 的 -subj "/CN=x")当成路径改写,关掉
export MSYS_NO_PATHCONV=1
export MSYS2_ARG_CONV_EXCL='*'

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NAME="xredis-test-redis"
PORT="6391"
TLS_NAME="xredis-test-redis-tls"
TLS_PORT="6392"
IMAGE="redis:8"
# 只用于本机测试容器;可用环境变量 XREDIS_TEST_REDIS_PASSWORD 覆盖
PASSWORD="${XREDIS_TEST_REDIS_PASSWORD:-xredis-test-pw}"
TEST_TIMEOUT="${XREDIS_TEST_TIMEOUT:-900}"
TLS_DIR="${XREDIS_TEST_TLS_DIR:-$ROOT/target/test-tls}"
OLD_NAME="xredis-test-redis5"
OLD_PORT="6393"
OLD_IMAGE="redis:5"

# 传给原生程序(docker)的路径:Windows 上要 D:/... 这种形式
native_path() {
  if command -v cygpath >/dev/null 2>&1; then cygpath -m "$1"; else printf '%s' "$1"; fi
}

start() {
  if [ "$(docker ps -q -f "name=^${NAME}$")" ]; then
    echo "$NAME already running"
    return
  fi
  docker rm -f "$NAME" >/dev/null 2>&1 || true
  docker run -d --name "$NAME" -p "127.0.0.1:${PORT}:6379" "$IMAGE" \
    redis-server --requirepass "$PASSWORD" --save "" --appendonly no --databases 20 >/dev/null
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

# 生成测试用 CA、服务端证书、客户端证书(客户端私钥给 PKCS#8 和 PKCS#1 两种格式)
gen_certs() {
  if [ -f "$TLS_DIR/.done" ]; then
    return
  fi
  rm -rf "$TLS_DIR"
  mkdir -p "$TLS_DIR"
  (
    cd "$TLS_DIR"
    openssl req -x509 -newkey rsa:2048 -nodes -keyout ca.key -out ca.crt \
      -subj "/CN=xredis-test-ca" -days 2 2>/dev/null
    # 服务端:SAN 含 127.0.0.1 和 localhost
    openssl req -newkey rsa:2048 -nodes -keyout server.key -out server.csr \
      -subj "/CN=localhost" 2>/dev/null
    printf 'subjectAltName=IP:127.0.0.1,DNS:localhost\nextendedKeyUsage=serverAuth\n' > server.ext
    openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial \
      -out server.crt -days 2 -extfile server.ext 2>/dev/null
    # 客户端:PKCS#8("PRIVATE KEY")和传统 PKCS#1("RSA PRIVATE KEY")同一把钥匙两种编码
    openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out client.pkcs8.key 2>/dev/null
    openssl rsa -in client.pkcs8.key -traditional -out client.pkcs1.key 2>/dev/null
    openssl req -new -key client.pkcs8.key -out client.csr -subj "/CN=xredis-test-client" 2>/dev/null
    printf 'extendedKeyUsage=clientAuth\n' > client.ext
    openssl x509 -req -in client.csr -CA ca.crt -CAkey ca.key -CAcreateserial \
      -out client.crt -days 2 -extfile client.ext 2>/dev/null
    # 容器里 redis 用户要能读;这些是只活两天的测试密钥
    chmod 644 ./*
    touch .done
  )
}

start_tls() {
  if [ "$(docker ps -q -f "name=^${TLS_NAME}$")" ]; then
    echo "$TLS_NAME already running"
    return
  fi
  gen_certs
  docker rm -f "$TLS_NAME" >/dev/null 2>&1 || true
  docker create --name "$TLS_NAME" -p "127.0.0.1:${TLS_PORT}:6379" "$IMAGE" \
    redis-server --port 0 --tls-port 6379 \
    --tls-cert-file /tls/server.crt --tls-key-file /tls/server.key \
    --tls-ca-cert-file /tls/ca.crt --tls-auth-clients yes \
    --requirepass "$PASSWORD" --save "" --appendonly no >/dev/null
  docker cp "$(native_path "$TLS_DIR")/." "$TLS_NAME:/tls" >/dev/null
  docker start "$TLS_NAME" >/dev/null
  for _ in $(seq 1 30); do
    if REDISCLI_AUTH="$PASSWORD" docker exec -e REDISCLI_AUTH "$TLS_NAME" redis-cli --tls \
      --cacert /tls/ca.crt --cert /tls/client.crt --key /tls/client.pkcs8.key ping 2>/dev/null | grep -q PONG; then
      echo "$TLS_NAME ready on 127.0.0.1:${TLS_PORT}"
      return
    fi
    sleep 1
  done
  echo "$TLS_NAME did not become ready in 30s" >&2
  docker logs "$TLS_NAME" 2>&1 | tail -5 >&2 || true
  exit 1
}

stop() {
  docker rm -f "$NAME" >/dev/null 2>&1 || true
  docker rm -f "$TLS_NAME" >/dev/null 2>&1 || true
  echo "$NAME and $TLS_NAME stopped"
}

case "${1:-}" in
  start) start ;;
  start-tls) start_tls ;;
  stop) stop ;;
  test)
    shift
    start
    start_tls
    export XREDIS_TEST_REDIS_URL="redis://default:${PASSWORD}@127.0.0.1:${PORT}"
    export XREDIS_TEST_REDIS_CONTAINER="$NAME"
    export XREDIS_TEST_REDIS_TLS_PORT="$TLS_PORT"
    # 测试程序是原生进程,路径要用它认得的形式(Windows 上是 D:/...)
    export XREDIS_TEST_TLS_DIR="$(native_path "$TLS_DIR")"
    # keyring_manual 会碰真实的系统钥匙串,只手动跑,这里跳过
    # 不捕获输出:测试里的诊断 WARNING 要能在日志里看到
    export RUST_TEST_NOCAPTURE=1
    # 旧版 Redis 的测试只在 test-old 里跑
    timeout "$TEST_TIMEOUT" cargo test "$@" -- --ignored --skip keyring_manual --skip old_redis
    ;;
  test-old)
    shift
    docker rm -f "$OLD_NAME" >/dev/null 2>&1 || true
    # 不管测试怎么结束,都把临时容器删掉
    trap 'docker rm -f "$OLD_NAME" >/dev/null 2>&1 || true' EXIT
    docker run -d --name "$OLD_NAME" -p "127.0.0.1:${OLD_PORT}:6379" "$OLD_IMAGE" \
      redis-server --requirepass "$PASSWORD" --save "" --appendonly no >/dev/null
    for _ in $(seq 1 30); do
      if docker exec "$OLD_NAME" redis-cli -a "$PASSWORD" --no-auth-warning ping 2>/dev/null | grep -q PONG; then
        break
      fi
      sleep 1
    done
    # redis:5 没有 ACL,不认 AUTH 用户名 密码,只带密码
    export XREDIS_TEST_OLD_REDIS_URL="redis://:${PASSWORD}@127.0.0.1:${OLD_PORT}"
    timeout "$TEST_TIMEOUT" cargo test -p xredis-core old_redis "$@" -- --ignored
    ;;
  *)
    echo "usage: $0 start|start-tls|stop|test [cargo args]|test-old" >&2
    exit 2
    ;;
esac
