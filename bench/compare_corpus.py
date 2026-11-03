#!/usr/bin/env python3
"""Plan G-04: same-day, same-machine success-rate comparison on bench/corpus.toml.

For every entry (skipping `allowed_for_testing = false` unless
--include-restricted) both implementations fetch the URL in the selected mode
and the SAME mechanical success check is applied to both results:

  success = status in allowed_status
            and expected_text (case-insensitive) occurs in the page text
            and len(page text) >= min_text_chars

Page text is the whole-page Convertor text (`main_content_only=False`) in both
implementations: upstream `Convertor._extract_content(page, "text", ...)` and
RustScrapling's port (`shell` command `text --full`), so extraction differences
cannot bias the comparison.

Modes:
  static  - upstream `Fetcher.get`            vs `rustscrapling shell: get`
  browser - upstream `StealthyFetcher.fetch`  vs `rustscrapling shell: fetch`

The order of implementations alternates per entry. Every attempt is written to
--out (JSONL raw outcomes) and a per-category summary is printed. Exclusions
are reported, never silently dropped. Results obtained behind a TLS-terminating
proxy are not valid G-04 evidence (fingerprints are the proxy's): run on a
host with direct egress, e.g. the workflow_dispatch CI job.

  python3 bench/compare_corpus.py --mode static --rust target/release/rustscrapling \\
      --upstream ../scrapling-v05-reference --upstream-python venv/bin/python --out raw.jsonl
"""

from __future__ import annotations

import argparse
import datetime
import json
import os
import pathlib
import platform
import subprocess
import sys
import time
import tomllib

ROOT = pathlib.Path(__file__).resolve().parents[1]

UPSTREAM = r'''
import json, sys, logging
sys.path.insert(0, sys.argv[1])
logging.disable(logging.CRITICAL)
from scrapling.core.shell import Convertor
mode, url, timeout = sys.argv[2], sys.argv[3], float(sys.argv[4])
try:
    if mode == "static":
        from scrapling.fetchers import Fetcher
        page = Fetcher.get(url, timeout=timeout)
    else:
        from scrapling.fetchers import StealthyFetcher
        page = StealthyFetcher.fetch(url, headless=True, timeout=timeout * 1000)
    text = "\n".join(Convertor._extract_content(page, "text", main_content_only=False))
    print(json.dumps({"status": page.status, "final_url": page.url, "text": text}))
except Exception as error:
    print(json.dumps({"error": type(error).__name__, "message": str(error)[:300]}))
'''


def rust_attempt(binary: str, mode: str, url: str, timeout: float, chrome: str | None) -> dict:
    command = "get" if mode == "static" else "fetch"
    script = f"{command} '{url}'" + (f" --timeout {int(timeout)}" if mode == "static" else "")
    script += "\npage\ntext --full"
    argv = [binary, "shell", "-c", script]
    if chrome and mode == "browser":
        argv[2:2] = ["--chrome-path", chrome]
    result = subprocess.run(argv, capture_output=True, text=True, timeout=timeout + 60)
    lines = result.stdout.split("\n")
    if result.returncode or len(lines) < 2:
        error = result.stderr.strip().splitlines()[0] if result.stderr.strip() else "no output"
        try:
            parsed = json.loads(error)["error"]
            return {"error": parsed.get("kind"), "message": parsed.get("message", "")[:300]}
        except (ValueError, KeyError, TypeError):
            return {"error": "process", "message": error[:300]}
    page = json.loads(lines[1])
    return {"status": page.get("status"), "final_url": page.get("url"), "text": "\n".join(lines[2:])}


def upstream_attempt(python: str, upstream: str, mode: str, url: str, timeout: float) -> dict:
    result = subprocess.run([python, "-I", "-c", UPSTREAM, upstream, mode, url, str(timeout)],
                            capture_output=True, text=True, timeout=timeout + 90)
    try:
        return json.loads(result.stdout.strip().splitlines()[-1])
    except (ValueError, IndexError):
        return {"error": "process", "message": (result.stderr or "no output")[-300:]}


def judge(entry: dict, outcome: dict) -> tuple[bool, str]:
    if "error" in outcome:
        return False, f"error:{outcome['error']}"
    if outcome.get("status") not in entry.get("allowed_status", [200]):
        return False, f"status:{outcome.get('status')}"
    text = outcome.get("text") or ""
    if entry["expected_text"].lower() not in text.lower():
        return False, "expected_text_missing"
    if len(text) < entry.get("min_text_chars", 0):
        return False, "too_short"
    return True, "ok"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--corpus", type=pathlib.Path, default=ROOT / "bench/corpus.toml")
    parser.add_argument("--mode", choices=["static", "browser"], required=True)
    parser.add_argument("--rust", required=True)
    parser.add_argument("--upstream", required=True)
    parser.add_argument("--upstream-python", required=True)
    parser.add_argument("--chrome")
    parser.add_argument("--out", type=pathlib.Path, required=True)
    parser.add_argument("--include-restricted", action="store_true")
    parser.add_argument("--category", action="append", help="limit to these categories")
    parser.add_argument("--delay", type=float, default=1.0, help="seconds between requests")
    args = parser.parse_args()
    corpus = tomllib.loads(args.corpus.read_text(encoding="utf-8"))
    entries = corpus["entry"]
    rust = str(pathlib.Path(args.rust).resolve())
    upstream = str(pathlib.Path(args.upstream).resolve())
    meta = {
        "kind": "meta", "datetime_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "git_sha": subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True).stdout.strip(),
        "upstream_sha": subprocess.run(["git", "rev-parse", "HEAD"], cwd=upstream, capture_output=True, text=True).stdout.strip(),
        "platform": platform.platform(), "mode": args.mode, "corpus": str(args.corpus),
        "corpus_entries": len(entries),
        "proxy_env": any(os.environ.get(k) for k in ("HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy")),
    }
    out = args.out.open("w")
    out.write(json.dumps(meta) + "\n")
    stats: dict[str, dict[str, dict[str, int]]] = {}
    skipped = []
    for index, entry in enumerate(entries):
        if args.category and entry["category"] not in args.category:
            continue
        if not entry.get("allowed_for_testing", True) and not args.include_restricted:
            skipped.append(entry["id"])
            out.write(json.dumps({"kind": "skipped", "id": entry["id"], "reason": "allowed_for_testing=false"}) + "\n")
            continue
        timeout = float(entry.get("timeout_s", 30))
        order = ("rust", "upstream") if index % 2 == 0 else ("upstream", "rust")
        for implementation in order:
            started = time.perf_counter()
            try:
                if implementation == "rust":
                    outcome = rust_attempt(rust, args.mode, entry["url"], timeout, args.chrome)
                else:
                    outcome = upstream_attempt(args.upstream_python, upstream, args.mode, entry["url"], timeout)
            except subprocess.TimeoutExpired:
                outcome = {"error": "timeout", "message": "harness timeout"}
            ok, reason = judge(entry, outcome)
            seconds = round(time.perf_counter() - started, 2)
            row = stats.setdefault(entry["category"], {}).setdefault(
                implementation, {"attempted": 0, "success": 0, "timeout": 0})
            row["attempted"] += 1
            row["success"] += ok
            row["timeout"] += outcome.get("error") == "timeout"
            out.write(json.dumps({
                "kind": "attempt", "id": entry["id"], "category": entry["category"], "url": entry["url"],
                "implementation": implementation, "success": ok, "reason": reason, "seconds": seconds,
                "status": outcome.get("status"), "final_url": outcome.get("final_url"),
                "text_chars": len(outcome.get("text") or ""), "error": outcome.get("error"),
                "message": outcome.get("message"),
            }, ensure_ascii=False) + "\n")
            out.flush()
            time.sleep(args.delay)
    out.close()
    total = {impl: {"attempted": sum(c[impl]["attempted"] for c in stats.values() if impl in c),
                    "success": sum(c[impl]["success"] for c in stats.values() if impl in c)}
             for impl in ("rust", "upstream")}
    summary = {"meta": meta, "skipped_by_policy": skipped, "by_category": stats, "total": total,
               "success_rate": {impl: round(v["success"] / v["attempted"], 4) if v["attempted"] else None
                                for impl, v in total.items()}}
    print(json.dumps(summary, indent=1, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    sys.exit(main())
