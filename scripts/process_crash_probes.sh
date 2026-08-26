#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if [[ -d "${HOME}/.cargo/bin" ]]; then
  PATH="${HOME}/.cargo/bin:$PATH"
  export PATH
fi

if [[ ! -x "$ROOT/target/debug/queue-process-crash-probe" ]]; then
  echo "queue probe binary is missing: target/debug/queue-process-crash-probe" >&2
  exit 2
fi
if [[ ! -x "$ROOT/target/debug/audit-process-crash-probe" ]]; then
  echo "audit probe binary is missing: target/debug/audit-process-crash-probe" >&2
  exit 2
fi

"$ROOT/target/debug/queue-process-crash-probe"
echo 'queue_process_crash_probe=PASS'
"$ROOT/target/debug/audit-process-crash-probe"
echo 'audit_process_crash_probe=PASS'
echo 'process_crash_probes=PASS'
