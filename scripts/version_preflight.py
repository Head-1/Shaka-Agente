#!/usr/bin/env python3
"""Fail-closed release metadata preflight for the Shaka workspace."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tomllib
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
SEMVER_TAG = re.compile(
    r"^v(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)"
    r"(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$"
)


class PreflightError(RuntimeError):
    """Raised when release metadata is inconsistent or incomplete."""


def _workspace_version(root: Path) -> str:
    cargo_path = root / "Cargo.toml"
    try:
        cargo = tomllib.loads(cargo_path.read_text(encoding="utf-8"))
        version = cargo["workspace"]["package"]["version"]
    except (OSError, KeyError, TypeError, tomllib.TOMLDecodeError) as exc:
        raise PreflightError(f"cannot read workspace version from {cargo_path}: {exc}") from exc
    if not isinstance(version, str) or not version:
        raise PreflightError("workspace.package.version must be a non-empty string")
    return version


def _cargo_metadata(root: Path) -> dict[str, Any]:
    completed = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.returncode != 0:
        detail = (completed.stderr or completed.stdout).strip().splitlines()
        message = detail[-1] if detail else "unknown cargo metadata error"
        raise PreflightError(f"cargo metadata failed: {message}")
    try:
        metadata = json.loads(completed.stdout)
    except json.JSONDecodeError as exc:
        raise PreflightError(f"cargo metadata returned invalid JSON: {exc}") from exc
    if not isinstance(metadata, dict) or not isinstance(metadata.get("packages"), list):
        raise PreflightError("cargo metadata did not return a packages array")
    return metadata


def _workspace_packages(root: Path, metadata: dict[str, Any]) -> list[dict[str, Any]]:
    root = root.resolve()
    packages: list[dict[str, Any]] = []
    for package in metadata["packages"]:
        if not isinstance(package, dict):
            raise PreflightError("cargo metadata contains a malformed package entry")
        manifest = package.get("manifest_path")
        name = package.get("name")
        version = package.get("version")
        if not isinstance(manifest, str) or not isinstance(name, str) or not isinstance(version, str):
            raise PreflightError("cargo metadata contains a package without name, version or manifest")
        try:
            is_workspace_package = Path(manifest).resolve().is_relative_to(root)
        except OSError:
            is_workspace_package = False
        if is_workspace_package:
            packages.append(package)
    if not packages:
        raise PreflightError("cargo metadata returned no workspace packages")
    return packages


def _lock_packages(root: Path) -> dict[str, dict[str, Any]]:
    lock_path = root / "Cargo.lock"
    try:
        lock = tomllib.loads(lock_path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as exc:
        raise PreflightError(f"cannot read {lock_path}: {exc}") from exc
    entries = lock.get("package")
    if not isinstance(entries, list):
        raise PreflightError("Cargo.lock does not contain package entries")
    result: dict[str, dict[str, Any]] = {}
    for entry in entries:
        if isinstance(entry, dict) and isinstance(entry.get("name"), str):
            result[entry["name"]] = entry
    return result


def _has_changelog_entry(root: Path, version: str) -> bool:
    try:
        changelog = (root / "CHANGELOG.md").read_text(encoding="utf-8")
    except OSError as exc:
        raise PreflightError(f"cannot read CHANGELOG.md: {exc}") from exc
    heading = re.compile(rf"^##\s+\[(?:v)?{re.escape(version)}\](?:\s|$)", re.MULTILINE)
    return heading.search(changelog) is not None


def preflight(
    root: Path = ROOT,
    *,
    expected_tag: str | None = None,
    allow_no_tag: bool = False,
    allow_unreleased: bool = False,
    metadata: dict[str, Any] | None = None,
) -> list[str]:
    """Validate release metadata and return bounded human-readable findings."""

    root = root.resolve()
    version = _workspace_version(root)
    expected_version_tag = f"v{version}"

    if expected_tag is None:
        if not allow_no_tag:
            raise PreflightError(f"release tag is required; expected {expected_version_tag}")
    else:
        if not SEMVER_TAG.fullmatch(expected_tag):
            raise PreflightError(f"tag {expected_tag!r} is not a valid SemVer tag")
        if expected_tag != expected_version_tag:
            raise PreflightError(
                f"tag {expected_tag} does not match workspace version {expected_version_tag}"
            )

    metadata = _cargo_metadata(root) if metadata is None else metadata
    packages = _workspace_packages(root, metadata)
    mismatched_metadata = [
        f"{package['name']}={package['version']}"
        for package in packages
        if package["version"] != version
    ]
    if mismatched_metadata:
        raise PreflightError(
            "workspace package versions diverge from "
            f"{version}: {', '.join(sorted(mismatched_metadata))}"
        )

    lock_packages = _lock_packages(root)
    missing_from_lock = sorted(package["name"] for package in packages if package["name"] not in lock_packages)
    if missing_from_lock:
        raise PreflightError(
            "workspace packages missing from Cargo.lock: " + ", ".join(missing_from_lock)
        )
    mismatched_lock = [
        f"{package['name']}={lock_packages[package['name']].get('version')}"
        for package in packages
        if lock_packages[package["name"]].get("version") != version
    ]
    if mismatched_lock:
        raise PreflightError(
            "Cargo.lock package versions diverge from "
            f"{version}: {', '.join(sorted(mismatched_lock))}"
        )

    if not _has_changelog_entry(root, version) and not allow_unreleased:
        raise PreflightError(f"CHANGELOG.md has no release heading for [{version}]")

    findings = [f"workspace version: {version}", f"workspace packages: {len(packages)}"]
    if expected_tag is None:
        findings.append("release tag: not supplied (allowed for non-release validation)")
    else:
        findings.append(f"release tag: {expected_tag}")
    if _has_changelog_entry(root, version):
        findings.append(f"changelog: [{version}] present")
    else:
        findings.append("changelog: [Unreleased] accepted for non-release validation")
    return findings


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", dest="tag", help="exact release tag to validate, for example v0.8.2")
    parser.add_argument(
        "--allow-no-tag",
        action="store_true",
        help="allow local/PR validation without a release tag",
    )
    parser.add_argument(
        "--allow-unreleased",
        action="store_true",
        help="allow a workspace version that is still represented by [Unreleased]",
    )
    args = parser.parse_args(argv)
    try:
        findings = preflight(
            expected_tag=args.tag,
            allow_no_tag=args.allow_no_tag,
            allow_unreleased=args.allow_unreleased,
        )
    except PreflightError as exc:
        print(f"version preflight failed: {exc}", file=sys.stderr)
        return 1
    for finding in findings:
        print(f"version preflight: {finding}")
    print("version preflight passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
