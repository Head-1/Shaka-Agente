#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if [[ -d "${HOME}/.cargo/bin" ]]; then
  PATH="${HOME}/.cargo/bin:$PATH"
  export PATH
fi

PORT="${SHAKA_LIFECYCLE_API_PORT:-29144}"
TMP="$(mktemp -d)"
API_PID=""
printf '{}\n' > "$TMP/skills.json"
printf '{}\n' > "$TMP/trusted_keys.json"

cleanup() {
  if [[ -n "$API_PID" ]] && kill -0 "$API_PID" 2>/dev/null; then
    kill -TERM "$API_PID" 2>/dev/null || true
    wait "$API_PID" 2>/dev/null || true
  fi
  rm -rf "$TMP"
}
trap cleanup EXIT

command -v ss >/dev/null

process_running() {
  local state
  state="$(ps -o stat= -p "$1" 2>/dev/null | tr -d '[:space:]')"
  [[ -n "$state" && "$state" != Z* ]]
}

stop_server() {
  local pid="$1"
  if process_running "$pid"; then
    kill -TERM "$pid"
    for _ in $(seq 1 50); do
      if ! process_running "$pid"; then
        break
      fi
      sleep 0.1
    done
    if process_running "$pid"; then
      echo "server did not stop after SIGTERM: pid=$pid" >&2
      return 1
    fi
  fi
  local status=0
  wait "$pid" 2>/dev/null || status=$?
  if [[ "$status" -ne 0 ]]; then
    echo "SIGTERM did not exit cleanly: pid=$pid status=$status" >&2
    return 1
  fi
  echo "sigterm_status=$status"
}

crash_server() {
  local pid="$1"
  kill -KILL "$pid"
  local status=0
  wait "$pid" 2>/dev/null || status=$?
  if [[ "$status" -ne 137 ]]; then
    echo "SIGKILL did not produce status 137: pid=$pid status=$status" >&2
    return 1
  fi
  echo "sigkill_status=$status"
}

wait_health() {
  for _ in $(seq 1 50); do
    if curl --fail --silent "http://127.0.0.1:${PORT}/healthz" > "$TMP/health.json"; then
      return 0
    fi
    sleep 0.1
  done
  return 1
}

start_server() {
  "$ROOT/target/debug/shaka" \
    --database "$TMP/shaka.db" \
    --skills-file "$TMP/skills.json" \
    --trust-file "$TMP/trusted_keys.json" \
    --tenant tenant-lifecycle \
    --role operator \
    serve --bind "127.0.0.1:${PORT}" --workers 1 \
    > "$TMP/api-${1}.log" 2>&1 &
  API_PID=$!
  if ! wait_health; then
    cat "$TMP/api-${1}.log" >&2
    return 1
  fi
  python3 - "$TMP/health.json" <<'PY'
import json
import pathlib
import sys

health = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
assert health["status"] == "ok"
assert health["circuit"]["state"] == "closed"
print(json.dumps({"status": health["status"], "circuit": health["circuit"]}, ensure_ascii=False))
PY
}

if ss -ltn "sport = :$PORT" | grep -q LISTEN; then
  echo "lifecycle port already in use: $PORT" >&2
  exit 3
fi

cargo build --quiet --locked -p shaka-cli

echo 'start=first'
start_server first
FIRST_PID="$API_PID"
echo "first_pid=$FIRST_PID"
stop_server "$FIRST_PID"
API_PID=""
if ss -ltn "sport = :$PORT" | grep -q LISTEN; then
  echo "port remained in use after first SIGTERM: $PORT" >&2
  exit 4
fi

echo 'first_shutdown=PASS'
echo 'start=crash-recovery'
start_server crash
CRASH_PID="$API_PID"
echo "crash_pid=$CRASH_PID"
crash_server "$CRASH_PID"
API_PID=""
if ss -ltn "sport = :$PORT" | grep -q LISTEN; then
  echo "port remained in use after SIGKILL: $PORT" >&2
  exit 4
fi

echo 'crash_recovery_cleanup=PASS'
echo 'start=third'
start_server third
THIRD_PID="$API_PID"
echo "third_pid=$THIRD_PID"
stop_server "$THIRD_PID"
API_PID=""
if ss -ltn "sport = :$PORT" | grep -q LISTEN; then
  echo "port remained in use after third SIGTERM: $PORT" >&2
  exit 4
fi

echo 'third_shutdown=PASS'
echo 'port_cleanup=PASS'
