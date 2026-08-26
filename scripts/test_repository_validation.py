"""Contract tests for the repository-first validation scripts."""

from __future__ import annotations

import os
import socket
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
VALIDATOR = ROOT / "scripts" / "validate_postmerge.sh"
LIFECYCLE = ROOT / "scripts" / "process_lifecycle_probe.sh"


class RepositoryValidationScriptTests(unittest.TestCase):
    def run_script(self, script: Path, *args: str) -> subprocess.CompletedProcess[str]:
        environment = os.environ.copy()
        environment.pop("SHAKA_VALIDATION_LOG", None)
        return subprocess.run(
            ["bash", str(script), *args],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
            env=environment,
        )

    def test_validator_help_is_available_without_running_gates(self) -> None:
        result = self.run_script(VALIDATOR, "--help")
        self.assertEqual(result.returncode, 0)
        self.assertIn("Runs the repository validation contract", result.stdout)
        self.assertIn("SHAKA_EXPECTED_HEAD", result.stdout)

    def test_validator_rejects_unexpected_arguments(self) -> None:
        result = self.run_script(VALIDATOR, "unexpected")
        self.assertEqual(result.returncode, 2)
        self.assertIn("unexpected argument: unexpected", result.stderr)

    def test_validator_propagates_failure_and_logs_exit_status(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "validation.log"
            environment = os.environ.copy()
            environment["SHAKA_VALIDATION_LOG"] = str(log)
            result = subprocess.run(
                ["bash", str(VALIDATOR), "unexpected"],
                cwd=ROOT,
                capture_output=True,
                text=True,
                check=False,
                env=environment,
            )
            self.assertEqual(result.returncode, 2)
            self.assertIn("unexpected argument: unexpected", result.stdout)
            self.assertIn("validation_exit=2", result.stdout)
            self.assertIn("validation_exit=2", log.read_text(encoding="utf-8"))

    def test_shell_scripts_are_syntactically_valid(self) -> None:
        for script in (VALIDATOR, LIFECYCLE):
            with self.subTest(script=script.name):
                result = subprocess.run(
                    ["bash", "-n", str(script)],
                    cwd=ROOT,
                    capture_output=True,
                    text=True,
                    check=False,
                )
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_validator_contract_keeps_locked_and_security_gates(self) -> None:
        text = VALIDATOR.read_text(encoding="utf-8")
        for expected in (
            "cargo check --workspace --locked",
            "cargo test --workspace --locked --all-targets",
            "cargo clippy --workspace --all-targets --locked",
            "cargo audit --no-fetch",
            "scripts/secret_scan.py",
            "scripts/workflow_policy_check.py",
            "scripts/process_lifecycle_probe.sh",
            "working_tree=clean",
        ):
            with self.subTest(expected=expected):
                self.assertIn(expected, text)
        self.assertNotIn("git push", text)
        self.assertNotIn("git switch", text)
        self.assertNotIn("git checkout", text)
        self.assertNotIn("--force", text)

    def test_lifecycle_probe_rejects_an_occupied_port(self) -> None:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
            listener.bind(("127.0.0.1", 0))
            listener.listen(1)
            port = listener.getsockname()[1]
            environment = os.environ.copy()
            environment["SHAKA_LIFECYCLE_API_PORT"] = str(port)
            result = subprocess.run(
                ["bash", str(LIFECYCLE)],
                cwd=ROOT,
                capture_output=True,
                text=True,
                check=False,
                env=environment,
            )
        self.assertEqual(result.returncode, 3)
        self.assertIn("port already in use", result.stderr)

    def test_lifecycle_probe_uses_sigterm_and_checks_port_cleanup(self) -> None:
        text = LIFECYCLE.read_text(encoding="utf-8")
        self.assertIn("kill -TERM", text)
        self.assertIn("kill -KILL", text)
        self.assertIn('"$status" -ne 137', text)
        self.assertIn("port_cleanup=PASS", text)
        self.assertIn("shaka.db", text)
        self.assertNotIn('kill "$SHELL_PID"', text)
        self.assertNotIn("--force", text)


if __name__ == "__main__":
    unittest.main()
