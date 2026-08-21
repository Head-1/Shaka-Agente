#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
TMP="$(mktemp -d)"
API_PID=""
cleanup() {
  if [[ -n "$API_PID" ]] && kill -0 "$API_PID" 2>/dev/null; then
    kill "$API_PID" 2>/dev/null || true
    wait "$API_PID" 2>/dev/null || true
  fi
  rm -rf "$TMP"
}
trap cleanup EXIT
DB="$TMP/shaka.db"
SKILLS="$TMP/skills.json"
TRUST="$TMP/trusted_keys.json"
SIGNING_KEY="$TMP/reviewer.key"
BACKUP="$TMP/backup.db"
ARTIFACT="$TMP/skill.wasm"
BIN="$ROOT/target/debug/shaka"

cargo build --quiet -p shaka-cli

run() {
  "$BIN" --database "$DB" --skills-file "$SKILLS" --trust-file "$TRUST" --tenant tenant-a "$@"
}

run --role operator run "registrar uma execução segura" > "$TMP/run.json"
run --role operator memory recent --limit 5 > "$TMP/recent.json"
run --role operator skill candidate report "skill de teste" --permissions memory-write > "$TMP/candidate.txt"
printf 'artefato de teste\n' > "$ARTIFACT"
run --role administrator skill trust-generate reviewer --output "$SIGNING_KEY" --description "chave efêmera de smoke test" > "$TMP/trust.json"
run --role administrator skill approve report --artifact "$ARTIFACT" --key-id reviewer --signing-key-file "$SIGNING_KEY" --reason "revisão de smoke test" > "$TMP/approve.json"
run --role administrator skill revoke report > "$TMP/revoke.json"
run --role administrator backup --output "$BACKUP" > "$TMP/backup.txt"
run --role administrator restore --input "$BACKUP" > "$TMP/restore.txt"
run --role administrator verify-audit > "$TMP/audit.json"
run --role administrator doctor > "$TMP/doctor.json"
run --role operator sandbox-demo > "$TMP/sandbox.json"

API_PORT="${SHAKA_SMOKE_API_PORT:-$((18080 + RANDOM % 1000))}"
run --role operator serve --bind "127.0.0.1:${API_PORT}" --workers 1 > "$TMP/api.log" 2>&1 &
API_PID=$!
for _ in $(seq 1 50); do
  if curl -fsS "http://127.0.0.1:${API_PORT}/healthz" > "$TMP/health.json"; then
    break
  fi
  sleep 0.1
done
if ! kill -0 "$API_PID" 2>/dev/null; then
  cat "$TMP/api.log" >&2
  exit 1
fi
curl -fsS "http://127.0.0.1:${API_PORT}/healthz" > "$TMP/health.json"

python3 - "$TMP" "$API_PORT" <<'PY'
import json
import pathlib
import sys
import time
import urllib.request

root = pathlib.Path(sys.argv[1])
port = sys.argv[2]
run = json.loads((root / "run.json").read_text())
approve = json.loads((root / "approve.json").read_text())
audit = json.loads((root / "audit.json").read_text())
doctor = json.loads((root / "doctor.json").read_text())
sandbox = json.loads((root / "sandbox.json").read_text())
health = json.loads((root / "health.json").read_text())
assert run["success"] is True
assert approve["approval"]["attestation"]["protocol"] == "shaka-skill-approval-v1"
assert approve["approval"]["attestation"]["key_id"] == "reviewer"
assert audit["valid"] is True and audit["checked_events"] >= 5
assert doctor["status"] == "ready"
assert sandbox["exit_code"] == 42
assert health["status"] == "ok"

def request(path, method="GET", payload=None, headers=None):
    data = None if payload is None else json.dumps(payload).encode()
    request_headers = {"content-type": "application/json"}
    request_headers.update(headers or {})
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}{path}",
        data=data,
        headers=request_headers,
        method=method,
    )
    with urllib.request.urlopen(request, timeout=3) as response:
        return response.status, json.loads(response.read())

status, session = request("/v1/sessions", "POST", {"metadata": {"source": "smoke"}})
assert status == 201
session_id = session["session_id"]
headers = {"Idempotency-Key": "smoke-task-1"}
status, task = request(
    f"/v1/sessions/{session_id}/tasks",
    "POST",
    {"objective": "validar a API persistente", "priority": 5},
    headers,
)
assert status == 202
status, duplicate = request(
    f"/v1/sessions/{session_id}/tasks",
    "POST",
    {"objective": "validar a API persistente", "priority": 5},
    headers,
)
assert status == 200 and duplicate["task_id"] == task["task_id"]
for _ in range(50):
    status, current = request(f"/v1/tasks/{task['task_id']}")
    if current["status"] in {"succeeded", "failed", "cancelled"}:
        break
    time.sleep(0.1)
assert current["status"] == "succeeded"
assert (root / "backup.db").exists()
print(json.dumps({"run": "ok", "audit": audit, "doctor": doctor, "sandbox": sandbox, "api": {"health": health, "task": current}}, indent=2, ensure_ascii=False))
PY
