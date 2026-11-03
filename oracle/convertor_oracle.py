#!/usr/bin/env python3
"""Record pinned-upstream `Convertor._extract_content` outputs for fixtures.

Writes `fixtures/oracle/convertor/expected.json`: for every fixture HTML
under `fixtures/oracle/aria_snapshot` and `fixtures/oracle/convertor`, the
chunks upstream yields (its trailing empty chunk removed) for each
extraction type, with/without `main_content_only` and for selected
`css_selector` values. Requires the pinned checkout's `[ai]` dependencies:

  python3 oracle/convertor_oracle.py --upstream ../scrapling-v05-reference \
      --upstream-python /path/to/venv/bin/python
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

from inventory_upstream import UPSTREAM_SHA, verify_source_tree

ROOT = Path(__file__).resolve().parents[1]
SOURCES = [ROOT / "fixtures/oracle/aria_snapshot", ROOT / "fixtures/oracle/convertor"]
OUT = ROOT / "fixtures/oracle/convertor/expected.json"
SELECTORS = [None, "p", "li", "table", "select", "h1, h2", "a", "p::text"]

PROBE = r'''
import json, sys
upstream, cases = sys.argv[1], json.loads(sys.stdin.read())
sys.path.insert(0, upstream)
from scrapling import Selector
from scrapling.core.shell import Convertor
out = []
for case in cases:
    page = Selector(content=case["html"], url="https://example.test/page")
    try:
        chunks = list(Convertor._extract_content(page, extraction_type=case["format"],
                       css_selector=case["selector"], main_content_only=case["main"]))
        out.append({"chunks": chunks[:-1] if chunks and chunks[-1] == "" else chunks})
    except Exception as exc:
        out.append({"error": type(exc).__name__})
print(json.dumps(out, ensure_ascii=False))
'''


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", type=Path, required=True)
    parser.add_argument("--upstream-python", required=True)
    args = parser.parse_args()
    upstream = args.upstream.resolve(strict=True)
    verify_source_tree(upstream)
    cases = []
    for directory in SOURCES:
        for path in sorted(directory.glob("*.html")):
            html = path.read_text(encoding="utf-8")
            for fmt in ("html", "text", "markdown"):
                for main in (True, False):
                    for selector in SELECTORS:
                        cases.append({"fixture": path.relative_to(ROOT).as_posix(), "html": html,
                                      "format": fmt, "main": main, "selector": selector})
    result = subprocess.run([args.upstream_python, "-I", "-c", PROBE, str(upstream)],
                            input=json.dumps(cases), capture_output=True, text=True, timeout=300, cwd=upstream)
    if result.returncode:
        print(result.stderr[-2000:], file=sys.stderr)
        return 2
    outputs = json.loads(result.stdout)
    records = [{k: v for k, v in case.items() if k != "html"} | output for case, output in zip(cases, outputs)]
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(json.dumps({"upstream_sha": UPSTREAM_SHA, "cases": records}, ensure_ascii=False, indent=1) + "\n",
                   encoding="utf-8")
    print(f"wrote {len(records)} cases to {OUT.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
