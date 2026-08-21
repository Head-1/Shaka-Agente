#!/usr/bin/env python3
"""Valida invariantes mínimos de supply chain dos workflows do Shaka."""

from __future__ import annotations

import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
WORKFLOWS = [
    ROOT / ".github" / "workflows" / "ci.yml",
    ROOT / ".github" / "workflows" / "release.yml",
    ROOT / ".github" / "workflows" / "fuzz.yml",
]


def fail(message: str) -> None:
    raise SystemExit(f"workflow policy failed: {message}")


def main() -> None:
    for workflow in WORKFLOWS:
        if not workflow.is_file():
            fail(f"arquivo ausente: {workflow}")
        text = workflow.read_text(encoding="utf-8")
        if "pull_request_target" in text:
            fail(f"gatilho pull_request_target proibido em {workflow.name}")
        if "persist-credentials: false" not in text:
            fail(f"checkout deve desabilitar credenciais persistentes em {workflow.name}")
        for match in re.finditer(r"uses:\s*([^\s#]+)@([^\s#]+)", text):
            action, ref = match.groups()
            if ref in {"main", "master", "HEAD", "latest", "stable"}:
                fail(f"action {action} usa referência móvel {ref} em {workflow.name}")
            if not ref.startswith("sha256:") and not re.fullmatch(
                r"(?:v?\d+(\.\d+){0,2}|nightly-\d{4}-\d{2}-\d{2})", ref
            ):
                fail(f"action {action} usa ref não versionada {ref} em {workflow.name}")
        if workflow.name == "fuzz.yml":
            continue
        if "cargo check --workspace --locked" not in text:
            fail(f"cargo check sem --locked em {workflow.name}")
        if "cargo test --workspace --locked" not in text:
            fail(f"cargo test sem --locked em {workflow.name}")
        if "cargo clippy --workspace --all-targets --locked" not in text:
            fail(f"cargo clippy sem --locked em {workflow.name}")
        cargo_audit_pinned = (
            "cargo install cargo-audit --version 0.22.2 --locked" in text
            or (
                "tool: cargo-audit@0.22.2" in text
                and "fallback: none" in text
            )
        )
        if not cargo_audit_pinned:
            fail(f"cargo-audit deve usar versão fixada e sem fallback de compilação em {workflow.name}")
    release = (ROOT / ".github" / "workflows" / "release.yml").read_text(encoding="utf-8")
    for permission in ("contents: write", "packages: write", "id-token: write", "attestations: write"):
        if permission not in release:
            fail(f"permissão de release ausente: {permission}")
    print("workflow policy passed")


if __name__ == "__main__":
    main()
