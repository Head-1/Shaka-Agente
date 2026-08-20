#!/usr/bin/env python3
"""Fail when common credential-shaped values appear in tracked source files."""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

PATTERNS = [
    re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
    re.compile(r"\bgh[pousr]_[A-Za-z0-9]{20,}\b"),
    re.compile(r"\bgithub_pat_[A-Za-z0-9_]{20,}\b"),
    re.compile(r"\bAKIA[0-9A-Z]{16}\b"),
    re.compile(r"\bsk-[A-Za-z0-9]{20,}\b"),
    re.compile(r"(?i)SHAKA_MODEL_API_KEY\s*=\s*[^\s#\"']+"),
    re.compile(r"(?i)(password|secret|access[_-]?token)\s*[:=]\s*[^\s#\"']{12,}"),
]

SKIP_SUFFIXES = (".md", ".example")
SKIP_NAMES = {".env.example"}


def tracked_files() -> list[Path]:
    result = subprocess.run(
        ["git", "ls-files", "-co", "--exclude-standard", "-z"], check=True, capture_output=True, text=False
    )
    return [Path(item) for item in result.stdout.decode().split("\0") if item]


def main() -> int:
    findings: list[str] = []
    for path in tracked_files():
        if path.name in SKIP_NAMES or path.suffix in SKIP_SUFFIXES:
            continue
        try:
            content = path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue
        if "\0" in content:
            continue
        for line_number, line in enumerate(content.splitlines(), start=1):
            if any(pattern.search(line) for pattern in PATTERNS):
                findings.append(f"{path}:{line_number}")
    if findings:
        print("credential-shaped value found in tracked files:", file=sys.stderr)
        print("\n".join(findings), file=sys.stderr)
        return 1
    print("secret scan passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
