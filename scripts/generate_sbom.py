#!/usr/bin/env python3
"""Generate a small CycloneDX inventory from cargo metadata."""

from __future__ import annotations

import json
import sys
from datetime import datetime, timezone
from pathlib import Path


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: generate_sbom.py <cargo-metadata.json> <sbom.cdx.json>", file=sys.stderr)
        return 2

    metadata_path = Path(sys.argv[1])
    output_path = Path(sys.argv[2])
    metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
    components = []
    for package in sorted(metadata.get("packages", []), key=lambda item: (item["name"], item["version"])):
        component = {
            "type": "library",
            "group": "rust-crate",
            "name": package["name"],
            "version": package["version"],
            "purl": f"pkg:cargo/{package['name']}@{package['version']}",
        }
        source = package.get("source")
        if source:
            component["externalReferences"] = [{"type": "distribution", "url": source}]
        components.append(component)

    document = {
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "serialNumber": "urn:uuid:shaka-cargo-metadata",
        "version": 1,
        "metadata": {
            "timestamp": datetime.now(timezone.utc).isoformat(),
            "tools": [{"vendor": "Shaka", "name": "generate_sbom.py", "version": "0.3.0"}],
        },
        "components": components,
    }
    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(json.dumps(document, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
