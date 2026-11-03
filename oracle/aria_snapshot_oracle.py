#!/usr/bin/env python3
"""Generate pinned-upstream ARIA snapshot fixtures for differential tests.

Runs the pinned Scrapling v0.5 MCP server (`browser_open`, `browser_fetch`,
`browser_extract(extraction_type="snapshot")`) against every
`fixtures/oracle/aria_snapshot/*.html` served from loopback, and writes the
exact snapshot text to `<name>.boxes-false.yaml` (boxes are omitted because
coordinates depend on window size and fonts) plus `<name>.target.yaml` for a
`target` subtree. A manifest records provenance.

Requires a Python environment with the pinned checkout's `[ai]` extra and an
existing Chromium binary; nothing is downloaded:

  python3 oracle/aria_snapshot_oracle.py --upstream ../scrapling-v05-reference \
      --upstream-python /path/to/venv/bin/python --chrome /usr/bin/google-chrome
"""

from __future__ import annotations

import argparse
import functools
import http.server
import json
import subprocess
import sys
import threading
from pathlib import Path

from inventory_upstream import UPSTREAM_SHA, verify_source_tree

ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "fixtures" / "oracle" / "aria_snapshot"
TARGETS = {"article": "main", "form": "select", "shadow": "x-card"}

PROBE = r'''
import asyncio, json, sys
from importlib.metadata import version
upstream, chrome, base, names, targets = sys.argv[1], sys.argv[2], sys.argv[3], json.loads(sys.argv[4]), json.loads(sys.argv[5])
sys.path.insert(0, upstream)
from scrapling.core.ai.server import ScraplingMCPServer

async def main():
    server = ScraplingMCPServer(executable_path=chrome)
    await server.browser_open(session_id="oracle")
    out = {}
    try:
        for name in names:
            await server.browser_fetch(url=f"{base}/{name}.html", session_id="oracle", extraction_type="markdown")
            full = await server.browser_extract(session_id="oracle", boxes=False)
            target = await server.browser_extract(session_id="oracle", boxes=False, target=targets[name])
            out[name] = {"full": full, "target": target}
    finally:
        await server.close_session(session_id="oracle")
    print(json.dumps({"patchright": version("patchright"), "snapshots": out}, ensure_ascii=False))

asyncio.run(main())
'''


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", type=Path, required=True)
    parser.add_argument("--upstream-python", required=True)
    parser.add_argument("--chrome", required=True)
    args = parser.parse_args()
    upstream = args.upstream.resolve(strict=True)
    verify_source_tree(upstream)
    names = sorted(path.stem for path in FIXTURES.glob("*.html"))
    handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=str(FIXTURES))
    handler.log_message = lambda *a, **k: None  # type: ignore[attr-defined]
    httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    thread.start()
    try:
        base = f"http://127.0.0.1:{httpd.server_address[1]}"
        result = subprocess.run(
            [args.upstream_python, "-I", "-c", PROBE, str(upstream), args.chrome, base,
             json.dumps(names), json.dumps(TARGETS)],
            cwd=upstream, capture_output=True, text=True, timeout=300)
    finally:
        httpd.shutdown()
    if result.returncode:
        print(result.stderr[-2000:], file=sys.stderr)
        return 2
    payload = json.loads(result.stdout.strip().splitlines()[-1])
    chrome_version = subprocess.run([args.chrome, "--version"], capture_output=True, text=True).stdout.strip()
    for name, snapshot in payload["snapshots"].items():
        (FIXTURES / f"{name}.boxes-false.yaml").write_text(snapshot["full"] + "\n", encoding="utf-8")
        (FIXTURES / f"{name}.target.yaml").write_text(snapshot["target"] + "\n", encoding="utf-8")
    manifest = {
        "upstream_sha": UPSTREAM_SHA,
        "patchright": payload["patchright"],
        "chrome": chrome_version,
        "targets": TARGETS,
        "fixtures": names,
        "note": "boxes=False; refs depend on snapshot order within one page load",
    }
    (FIXTURES / "MANIFEST.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(f"wrote {len(names)} fixtures ({chrome_version}, patchright {payload['patchright']})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
