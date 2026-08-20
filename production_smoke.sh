#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
DB="$TMP/shaka.db"
SKILLS="$TMP/skills.json"
BACKUP="$TMP/backup.db"
ARTIFACT="$TMP/skill.wasm"
BIN="$ROOT/target/debug/shaka"

cargo build --quiet -p shaka-cli

run() {
  "$BIN" --database "$DB" --skills-file "$SKILLS" --tenant tenant-a "$@"
}

run --role operator run "registrar uma execução segura" > "$TMP/run.json"
run --role operator memory recent --limit 5 > "$TMP/recent.json"
run --role operator skill candidate report "skill de teste" --permissions memory-write > "$TMP/candidate.txt"
printf 'artefato de teste\n' > "$ARTIFACT"
run --role administrator skill approve report --artifact "$ARTIFACT" --reason "revisão de smoke test" > "$TMP/approve.json"
run --role administrator skill revoke report > "$TMP/revoke.json"
run --role administrator backup --output "$BACKUP" > "$TMP/backup.txt"
run --role administrator restore --input "$BACKUP" > "$TMP/restore.txt"
run --role administrator verify-audit > "$TMP/audit.json"
run --role administrator doctor > "$TMP/doctor.json"
run --role operator sandbox-demo > "$TMP/sandbox.json"

python3 - "$TMP" <<'PY'
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
run = json.loads((root / "run.json").read_text())
audit = json.loads((root / "audit.json").read_text())
doctor = json.loads((root / "doctor.json").read_text())
sandbox = json.loads((root / "sandbox.json").read_text())
assert run["success"] is True
assert audit["valid"] is True and audit["checked_events"] >= 4
assert doctor["status"] == "ready"
assert sandbox["exit_code"] == 42
assert (root / "backup.db").exists()
print(json.dumps({"run": "ok", "audit": audit, "doctor": doctor, "sandbox": sandbox}, indent=2, ensure_ascii=False))
PY
