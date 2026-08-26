"""Contract tests for the versioned process-crash probe wrapper."""

from __future__ import annotations

import os
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
WRAPPER = ROOT / "scripts" / "process_crash_probes.sh"


class ProcessCrashProbeWrapperTests(unittest.TestCase):
    def run_wrapper(self, target: Path, wrapper: Path = WRAPPER) -> subprocess.CompletedProcess[str]:
        environment = os.environ.copy()
        environment["PATH"] = "/usr/bin:/bin"
        return subprocess.run(
            ["bash", str(wrapper)],
            cwd=target,
            capture_output=True,
            text=True,
            check=False,
            env=environment,
        )

    def test_wrapper_rejects_missing_probe_binary(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            target = Path(temporary)
            (target / "scripts").mkdir()
            target_wrapper = target / "scripts" / "process_crash_probes.sh"
            target_wrapper.write_text(WRAPPER.read_text(encoding="utf-8"), encoding="utf-8")
            target_wrapper.chmod(0o755)
            result = self.run_wrapper(target, target_wrapper)
        self.assertEqual(result.returncode, 2)
        self.assertIn("queue probe binary is missing", result.stderr)

    def test_wrapper_contract_runs_binaries_directly(self) -> None:
        text = WRAPPER.read_text(encoding="utf-8")
        self.assertIn('"$ROOT/target/debug/queue-process-crash-probe"', text)
        self.assertIn('"$ROOT/target/debug/audit-process-crash-probe"', text)
        self.assertIn("queue_process_crash_probe=PASS", text)
        self.assertIn("audit_process_crash_probe=PASS", text)
        self.assertIn("process_crash_probes=PASS", text)
        self.assertNotIn("cargo run", text)
        self.assertNotIn("git ", text)
        self.assertNotIn("/home/ubuntu/", text)


if __name__ == "__main__":
    unittest.main()
