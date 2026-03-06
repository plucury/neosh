#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "missing required command: $1" >&2
    exit 1
  fi
}

require_cmd docker
require_cmd ssh-keygen

IMAGE_TAG="neosh/e2e-sshd:local"
CONTAINER_NAME="neosh-e2e-$RANDOM-$RANDOM"
TMP_DIR="$(mktemp -d /tmp/neosh-e2e.XXXXXX)"
KEY_PATH="$TMP_DIR/id_ed25519"

NEOSH_BIN="${NEOSH_E2E_NEOSH_BIN:-$ROOT_DIR/target/e2e-linux/debug/neosh}"
NEOSHD_BIN="${NEOSH_E2E_NEOSHD_BIN:-$ROOT_DIR/target/e2e-linux/debug/neoshd}"

cleanup() {
  docker rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true
  if [[ "${KEEP_E2E_ARTIFACTS:-0}" == "1" ]]; then
    echo "[e2e] keeping artifacts at: $TMP_DIR"
  else
    rm -rf "$TMP_DIR"
  fi
}
trap cleanup EXIT

fail() {
  echo "[e2e] FAIL: $*" >&2
  docker logs "$CONTAINER_NAME" >&2 || true
  echo "[e2e] --- remote /tmp/neoshd-e2e.log ---" >&2
  docker exec "$CONTAINER_NAME" /bin/bash -lc 'cat /tmp/neoshd-e2e.log 2>/dev/null || true' >&2 || true
  echo "[e2e] --- remote cache tree ---" >&2
  docker exec "$CONTAINER_NAME" /bin/bash -lc 'find /e2e/cache /root/.cache -maxdepth 4 -type f 2>/dev/null || true' >&2 || true
  for f in connect.err detach.err resume.err neoshd.log; do
    if [[ -f "$TMP_DIR/$f" ]]; then
      echo "[e2e] --- $f ---" >&2
      cat "$TMP_DIR/$f" >&2 || true
    fi
  done
  exit 1
}

if [[ -n "${NEOSH_E2E_NEOSH_BIN:-}" && -n "${NEOSH_E2E_NEOSHD_BIN:-}" ]]; then
  echo "[e2e] using prebuilt binaries"
else
  echo "[e2e] building linux neosh/neoshd binaries in docker"
  docker run --rm \
    --user "$(id -u):$(id -g)" \
    -v "$ROOT_DIR:/work" \
    -w /work \
    -e CARGO_TARGET_DIR=/work/target/e2e-linux \
    rust:1-bookworm \
    cargo build --bin neosh --bin neoshd >/dev/null
fi

if [[ ! -x "$NEOSH_BIN" ]]; then
  echo "[e2e] missing executable neosh binary: $NEOSH_BIN" >&2
  exit 1
fi
if [[ ! -x "$NEOSHD_BIN" ]]; then
  echo "[e2e] missing executable neoshd binary: $NEOSHD_BIN" >&2
  exit 1
fi

echo "[e2e] generating ssh keypair"
ssh-keygen -t ed25519 -N "" -f "$KEY_PATH" >/dev/null
PUB_KEY="$(cat "$KEY_PATH.pub")"

echo "[e2e] building docker sshd image"
docker build -t "$IMAGE_TAG" -f scripts/docker/e2e-sshd/Dockerfile scripts/docker/e2e-sshd >/dev/null

echo "[e2e] starting docker test container: $CONTAINER_NAME"
docker rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true
docker run -d \
  --name "$CONTAINER_NAME" \
  -e "E2E_AUTHORIZED_KEY=$PUB_KEY" \
  -v "$NEOSH_BIN:/usr/local/bin/neosh:ro" \
  -v "$NEOSHD_BIN:/usr/local/bin/neoshd:ro" \
  -v "$TMP_DIR:/e2e" \
  "$IMAGE_TAG" >/dev/null

if [[ "$(docker inspect -f '{{.State.Running}}' "$CONTAINER_NAME" 2>/dev/null || true)" != "true" ]]; then
  fail "container failed to start"
fi

echo "[e2e] running in-container full lifecycle"
if ! docker exec "$CONTAINER_NAME" /bin/bash -lc '
  set -euo pipefail
  mkdir -p /root/.ssh /e2e/cache
  cat >/root/.ssh/config <<EOF
Host neosh-e2e
  HostName 127.0.0.1
  Port 22222
  User e2e
  IdentityFile /e2e/id_ed25519
  IdentitiesOnly yes
  StrictHostKeyChecking no
  UserKnownHostsFile /dev/null
  LogLevel ERROR
EOF
  chmod 600 /root/.ssh/config

  for _ in $(seq 1 40); do
    if ssh neosh-e2e "echo ready" >/dev/null 2>&1; then
      break
    fi
    sleep 0.25
  done
  ssh neosh-e2e "echo ready" >/dev/null

  XDG_CACHE_HOME=/e2e/cache /usr/local/bin/neosh connect neosh-e2e \
    --neoshd-path /usr/local/bin/neoshd \
    --neoshd-log-file /tmp/neoshd-e2e.log \
    >/e2e/connect.out 2>/e2e/connect.err &
  connect_pid=$!

  session_file=""
  for _ in $(seq 1 120); do
    session_file="$(ls /e2e/cache/neosh/sessions/*.json 2>/dev/null | head -n1 || true)"
    if [[ -n "$session_file" ]]; then
      break
    fi
    if ! kill -0 "$connect_pid" 2>/dev/null; then
      wait "$connect_pid" || true
      echo "connect exited early" >&2
      exit 1
    fi
    sleep 0.25
  done
  if [[ -z "$session_file" ]]; then
    echo "session cache file not created in time" >&2
    exit 1
  fi

  XDG_CACHE_HOME=/e2e/cache /usr/local/bin/neosh detach >/e2e/detach.out 2>/e2e/detach.err
  wait "$connect_pid"

  session_id="$(basename "$session_file" .json)"
  if [[ -z "$session_id" ]]; then
    echo "empty session id" >&2
    exit 1
  fi

  printf "echo resumed\nexit\n" | XDG_CACHE_HOME=/e2e/cache /usr/local/bin/neosh resume \
    --session-id "$session_id" \
    --target neosh-e2e \
    --neoshd-path /usr/local/bin/neoshd \
    >/e2e/resume.out 2>/e2e/resume.err

  for _ in $(seq 1 40); do
    if grep -q "\"event\":\"server_stop\"" /tmp/neoshd-e2e.log 2>/dev/null && \
      grep -q "\"event\":\"session_terminated\"" /tmp/neoshd-e2e.log 2>/dev/null; then
      break
    fi
    sleep 0.25
  done

  cat /tmp/neoshd-e2e.log >/e2e/neoshd.log

  grep -q "\"event\":\"attach_ok\"" /e2e/connect.err
  grep -q "\"event\":\"detach_sent\"" /e2e/connect.err
  grep -q "\"event\":\"resume_ok\"" /e2e/resume.err
  grep -q "\"event\":\"close_sent\"" /e2e/resume.err
  grep -q "\"event\":\"attach_ok\"" /e2e/neoshd.log
  grep -q "\"event\":\"resume_ok\"" /e2e/neoshd.log
  grep -q "\"event\":\"server_stop\"" /e2e/neoshd.log
  grep -q "\"event\":\"session_terminated\"" /e2e/neoshd.log
'; then
  fail "in-container lifecycle run failed"
fi

echo "[e2e] PASS"
