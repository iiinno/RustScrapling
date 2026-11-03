#!/usr/bin/env python3
"""Generate a CycloneDX 1.5 JSON SBOM for the Rust workspace and audit licenses.

Uses `cargo metadata --locked` (no network once the lockfile's crates are
fetched) and records every package in the resolved dependency graph with its
version, SPDX license expression, source and purl. Vendored browser assets and
ported Python code that Cargo cannot see are added from NOTICE explicitly.

Exit status 1 when a dependency has no license or a copyleft license
(GPL/AGPL/LGPL/SSPL/EUPL/MPL is allowed only if listed in ALLOWED_WEAK).

  python3 scripts/sbom.py --out SBOM.cdx.json
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
COPYLEFT = re.compile(r"\b(A?GPL|LGPL|SSPL|EUPL|CDDL|OSL)\b", re.IGNORECASE)
# File-level copyleft (MPL-2.0) is acceptable for unmodified dependencies.
ALLOWED_WEAK = {"MPL-2.0"}
EXTRA_COMPONENTS = [
    {"name": "patchright-core-injected-script", "version": "1.64.3", "licenses": "Apache-2.0",
     "purl": "pkg:npm/patchright-core@1.64.3",
     "note": "crates/rustscrapling-browser/vendor/playwright-injected.js (unmodified, header added)"},
    {"name": "python-markdownify", "version": "1.2.3", "licenses": "MIT",
     "purl": "pkg:pypi/markdownify@1.2.3", "note": "ported to crates/rustscrapling-extract/src/markdownify.rs"},
    {"name": "scrapling", "version": "0.5 (95cf9019ab5a5d6f3e9654b7ef1c6a26e29dc5c0)", "licenses": "BSD-3-Clause",
     "purl": "pkg:pypi/scrapling@0.5", "note": "behaviour and selected snippets ported; see NOTICE"},
]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=pathlib.Path, default=ROOT / "SBOM.cdx.json")
    args = parser.parse_args()
    metadata = json.loads(subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked"],
        cwd=ROOT, check=True, capture_output=True, text=True).stdout)
    resolved = {node["id"] for node in metadata["resolve"]["nodes"]}
    workspace = set(metadata["workspace_members"])
    components, problems = [], []
    for package in sorted(metadata["packages"], key=lambda p: (p["name"], p["version"])):
        if package["id"] not in resolved:
            continue
        license_expr = package.get("license") or ""
        if package["id"] in workspace:
            kind = "application" if package["name"] == "rustscrapling-cli" else "library"
        else:
            kind = "library"
            if not license_expr and not package.get("license_file"):
                problems.append(f"{package['name']} {package['version']}: no license metadata")
            elif COPYLEFT.search(license_expr) and not any(
                    option.strip() in ALLOWED_WEAK or not COPYLEFT.search(option)
                    for option in re.split(r"\s+OR\s+|/", license_expr)):
                problems.append(f"{package['name']} {package['version']}: copyleft license {license_expr}")
        components.append({
            "type": kind,
            "name": package["name"],
            "version": package["version"],
            "purl": f"pkg:cargo/{package['name']}@{package['version']}",
            "licenses": [{"expression": license_expr}] if license_expr else [],
            "properties": [{"name": "source", "value": package.get("source") or "workspace"}],
        })
    for extra in EXTRA_COMPONENTS:
        components.append({
            "type": "library", "name": extra["name"], "version": extra["version"], "purl": extra["purl"],
            "licenses": [{"expression": extra["licenses"]}],
            "properties": [{"name": "inclusion", "value": extra["note"]}],
        })
    sbom = {
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "version": 1,
        "metadata": {"component": {"type": "application", "name": "rustscrapling"}},
        "components": components,
    }
    args.out.write_text(json.dumps(sbom, indent=1) + "\n", encoding="utf-8")
    licenses: dict[str, int] = {}
    for component in components:
        for entry in component["licenses"] or [{"expression": "<none>"}]:
            licenses[entry["expression"]] = licenses.get(entry["expression"], 0) + 1
    print(json.dumps({"components": len(components), "licenses": dict(sorted(licenses.items())),
                      "problems": problems}, indent=1))
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
