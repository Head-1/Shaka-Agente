#!/usr/bin/env python3
"""Tests for the Shaka release metadata preflight."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

import version_preflight


class VersionPreflightTests(unittest.TestCase):
    def make_root(self, *, changelog: str = "## [0.8.2] - 2026-08-22\n") -> tuple[Path, dict[str, object]]:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        (root / "Cargo.toml").write_text(
            "[workspace]\n"
            "members = [\"crates/shaka-cli\", \"crates/shaka-core\"]\n\n"
            "[workspace.package]\n"
            "version = \"0.8.2\"\n",
            encoding="utf-8",
        )
        (root / "Cargo.lock").write_text(
            "version = 4\n\n"
            "[[package]]\nname = \"shaka-cli\"\nversion = \"0.8.2\"\n\n"
            "[[package]]\nname = \"shaka-core\"\nversion = \"0.8.2\"\n",
            encoding="utf-8",
        )
        (root / "CHANGELOG.md").write_text(changelog, encoding="utf-8")
        metadata: dict[str, object] = {
            "packages": [
                {
                    "name": "shaka-cli",
                    "version": "0.8.2",
                    "manifest_path": str(root / "crates/shaka-cli/Cargo.toml"),
                },
                {
                    "name": "shaka-core",
                    "version": "0.8.2",
                    "manifest_path": str(root / "crates/shaka-core/Cargo.toml"),
                },
            ]
        }
        return root, metadata

    def test_valid_release_tag_passes(self) -> None:
        root, metadata = self.make_root()
        findings = version_preflight.preflight(root, expected_tag="v0.8.2", metadata=metadata)
        self.assertIn("release tag: v0.8.2", findings)
        self.assertIn("changelog: [0.8.2] present", findings)

    def test_no_tag_is_allowed_only_explicitly(self) -> None:
        root, metadata = self.make_root(changelog="## [Unreleased]\n")
        findings = version_preflight.preflight(
            root,
            allow_no_tag=True,
            allow_unreleased=True,
            metadata=metadata,
        )
        self.assertIn("release tag: not supplied (allowed for non-release validation)", findings)

    def test_tag_mismatch_fails_closed(self) -> None:
        root, metadata = self.make_root()
        with self.assertRaisesRegex(version_preflight.PreflightError, "does not match workspace"):
            version_preflight.preflight(root, expected_tag="v0.8.1", metadata=metadata)

    def test_invalid_tag_fails_closed(self) -> None:
        root, metadata = self.make_root()
        with self.assertRaisesRegex(version_preflight.PreflightError, "not a valid SemVer tag"):
            version_preflight.preflight(root, expected_tag="release-0.8.2", metadata=metadata)

    def test_metadata_mismatch_fails_closed(self) -> None:
        root, metadata = self.make_root()
        metadata["packages"][1]["version"] = "0.8.1"  # type: ignore[index]
        with self.assertRaisesRegex(version_preflight.PreflightError, "workspace package versions"):
            version_preflight.preflight(root, expected_tag="v0.8.2", metadata=metadata)

    def test_lockfile_mismatch_fails_closed(self) -> None:
        root, metadata = self.make_root()
        (root / "Cargo.lock").write_text(
            "version = 4\n\n"
            "[[package]]\nname = \"shaka-cli\"\nversion = \"0.8.1\"\n\n"
            "[[package]]\nname = \"shaka-core\"\nversion = \"0.8.2\"\n",
            encoding="utf-8",
        )
        with self.assertRaisesRegex(version_preflight.PreflightError, "Cargo.lock package versions"):
            version_preflight.preflight(root, expected_tag="v0.8.2", metadata=metadata)

    def test_missing_changelog_fails_closed_for_release(self) -> None:
        root, metadata = self.make_root(changelog="## [Unreleased]\n")
        with self.assertRaisesRegex(version_preflight.PreflightError, "no release heading"):
            version_preflight.preflight(root, expected_tag="v0.8.2", metadata=metadata)


if __name__ == "__main__":
    unittest.main()
