#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if [[ -d "${HOME}/.cargo/bin" ]]; then
  PATH="${HOME}/.cargo/bin:$PATH"
  export PATH
fi

SMOKE_PORT="${SHAKA_SMOKE_API_PORT:-29143}"
LIFECYCLE_PORT="${SHAKA_LIFECYCLE_API_PORT:-29144}"
LOG_FILE="${SHAKA_VALIDATION_LOG:-}"

usage() {
  cat <<'USAGE'
Usage: scripts/validate_postmerge.sh

Runs the repository validation contract from a clean checkout. The script does
not install dependencies, modify tracked files, push refs, or change branches.
Required tools include Rust with rustfmt/clippy, Python 3, curl, cargo-audit,
and the tools used by the existing repository scripts.

Environment:
  SHAKA_EXPECTED_HEAD       Optional exact commit SHA to require.
  SHAKA_SMOKE_API_PORT      Port for production_smoke.sh (default: 29143).
  SHAKA_LIFECYCLE_API_PORT  Port for lifecycle probe (default: 29144).
  SHAKA_VALIDATION_LOG      Optional path for a copy of the sanitized output.
USAGE
}

run_step() {
  printf '\n=== %s ===\n' "$1"
  shift
  "$@"
}

port_is_listening() {
  ss -ltn "sport = :$1" | grep -q LISTEN
}

main() {
  if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
    usage
    return 0
  fi
  if [[ "$#" -ne 0 ]]; then
    echo "unexpected argument: $1" >&2
    usage >&2
    return 2
  fi

  printf 'repository=%s\n' "$ROOT"
  printf 'head=%s\n' "$(git rev-parse HEAD)"
  printf 'branch=%s\n' "$(git branch --show-current)"
  if [[ -n "${SHAKA_EXPECTED_HEAD:-}" ]]; then
    test "$(git rev-parse HEAD)" = "$SHAKA_EXPECTED_HEAD"
  fi
  test -z "$(git status --porcelain)"
  echo 'working_tree=clean'

  run_step 'toolchain' rustc --version
  run_step 'cargo' cargo --version
  run_step 'python' python3 --version
  command -v curl >/dev/null
  command -v ss >/dev/null
  command -v cargo-audit >/dev/null

  for script in \
    scripts/production_smoke.sh \
    scripts/process_lifecycle_probe.sh \
    scripts/process_crash_probes.sh \
    scripts/validate_postmerge.sh; do
    run_step "syntax:$script" bash -n "$script"
  done
  echo 'shell_syntax=PASS'

  run_step 'format' cargo fmt --all -- --check
  echo 'fmt=PASS'
  run_step 'check' cargo check --workspace --locked
  echo 'check=PASS'
  run_step 'tests' cargo test --workspace --locked --all-targets
  echo 'test_all_targets=PASS'
  run_step 'clippy' cargo clippy --workspace --all-targets --locked -- \
    -D warnings -A missing_docs -A clippy::missing_errors_doc
  echo 'clippy=PASS'

  run_step 'secret_scan' env PYTHONDONTWRITEBYTECODE=1 python3 scripts/secret_scan.py
  echo 'secret_scan=PASS'
  run_step 'workflow_policy' env PYTHONDONTWRITEBYTECODE=1 python3 scripts/workflow_policy_check.py
  echo 'workflow_policy=PASS'
  run_step 'script_tests' env PYTHONDONTWRITEBYTECODE=1 \
    python3 -m unittest discover -s scripts -p 'test_*.py'
  echo 'script_tests=PASS'
  run_step 'version_preflight' env PYTHONDONTWRITEBYTECODE=1 \
    python3 scripts/version_preflight.py --allow-no-tag --allow-unreleased
  echo 'version_preflight=PASS'
  if [[ "${SHAKA_CARGO_AUDIT_NO_FETCH:-0}" == "1" ]]; then
    run_step 'dependency_audit' cargo audit --no-fetch
    echo 'cargo_audit_no_fetch=PASS'
  else
    run_step 'dependency_audit' cargo audit
    echo 'cargo_audit=PASS'
  fi

  run_step 'cli_build' cargo build --locked -p shaka-cli
  test -x target/debug/shaka
  echo 'shaka_cli_build=PASS'
  run_step 'process_probe_build' cargo build --locked -p shaka-probes --bins
  test -x target/debug/queue-process-crash-probe
  test -x target/debug/audit-process-crash-probe
  echo 'process_probe_build=PASS'

  if port_is_listening "$SMOKE_PORT"; then
    echo "smoke port already in use: $SMOKE_PORT" >&2
    return 3
  fi
  if port_is_listening "$LIFECYCLE_PORT"; then
    echo "lifecycle port already in use: $LIFECYCLE_PORT" >&2
    return 3
  fi

  run_step 'production_smoke' env SHAKA_SMOKE_API_PORT="$SMOKE_PORT" \
    bash scripts/production_smoke.sh
  echo 'production_smoke=PASS'

  if port_is_listening "$SMOKE_PORT"; then
    echo "smoke port remained in use: $SMOKE_PORT" >&2
    return 4
  fi

  run_step 'process_lifecycle_probe' env SHAKA_LIFECYCLE_API_PORT="$LIFECYCLE_PORT" \
    bash scripts/process_lifecycle_probe.sh
  echo 'process_lifecycle_probe=PASS'

  if port_is_listening "$LIFECYCLE_PORT"; then
    echo "lifecycle port remained in use: $LIFECYCLE_PORT" >&2
    return 4
  fi

  run_step 'process_crash_probes' bash scripts/process_crash_probes.sh
  echo 'process_crash_probes=PASS'

  test -z "$(git status --porcelain)"
  echo 'working_tree=clean'
  echo 'postmerge_validation=PASS'
}

finish() {
  local status=$?
  trap - EXIT
  printf 'validation_exit=%s\n' "$status"
  exit "$status"
}

trap finish EXIT
if [[ -n "$LOG_FILE" ]]; then
  mkdir -p "$(dirname "$LOG_FILE")"
  exec > >(tee "$LOG_FILE") 2>&1
fi
main "$@"
