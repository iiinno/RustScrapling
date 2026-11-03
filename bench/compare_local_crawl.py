#!/usr/bin/env python3
"""Same-machine 150-URL / concurrency-8 crawl: RustScrapling vs pinned Scrapling.

Both implementations crawl the same loopback site (1 index + 149 article pages,
~24 KiB each, optional per-response latency) with 8 concurrent requests, no
delay and robots disabled. Runs alternate (Rust, upstream, Rust, ...) to limit
ordering bias. For each run the script records wall time, successful pages and
the peak RSS of the crawler process tree (Chrome is not involved).

This is a controlled localhost workload for Plan G-05/G-08. It is NOT the
200-URL public-site comparison (G-04) and says nothing about bot defences.

  python3 bench/compare_local_crawl.py target/release/rustscrapling \
      --upstream ../scrapling-v05-reference --upstream-python /path/venv/bin/python
"""

from __future__ import annotations

import argparse
import http.server
import json
import pathlib
import socketserver
import statistics
import subprocess
import tempfile
import threading
import time

PAGES = 150
FILLER = " ".join(f"word{i % 97}" for i in range(3000))

UPSTREAM_SPIDER = r'''
import sys, json
sys.path.insert(0, sys.argv[1])
from scrapling.spiders import Spider

class Bench(Spider):
    name = "bench"
    start_urls = [sys.argv[2]]
    allowed_domains = {"127.0.0.1"}
    concurrent_requests = 8
    logging_level = 40

    async def parse(self, response):
        yield {"url": response.url, "status": response.status, "title": response.css("h1::text").get()}
        for href in response.css("a::attr(href)").getall():
            yield response.follow(href)

result = Bench().start()
print(json.dumps({"items": len(result.items)}))
'''


class Site(http.server.BaseHTTPRequestHandler):
    latency = 0.0

    def do_GET(self):
        if self.latency:
            time.sleep(self.latency)
        if self.path == "/start":
            links = "".join(f'<a href="/page/{i}">Page {i}</a>' for i in range(PAGES - 1))
            page = f"<html><body><h1>Index</h1>{links}</body></html>"
        elif self.path.startswith("/page/"):
            number = self.path.removeprefix("/page/")
            if not number.isdigit() or int(number) >= PAGES - 1:
                self.send_error(404)
                return
            page = (f"<html><head><title>Page {number}</title></head><body><article><h1>Page {number}</h1>"
                    f"<p>{FILLER}</p></article></body></html>")
        else:
            self.send_error(404)
            return
        body = page.encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        pass


class ThreadingServer(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    request_queue_size = 256


def tree_rss_kib(pid: int) -> int:
    """Sum VmRSS of `pid` and all descendants (Linux /proc)."""
    total = 0
    pending = [pid]
    while pending:
        current = pending.pop()
        try:
            for line in pathlib.Path(f"/proc/{current}/status").read_text().splitlines():
                if line.startswith("VmRSS:"):
                    total += int(line.split()[1])
            children = pathlib.Path(f"/proc/{current}/task/{current}/children").read_text().split()
            pending.extend(int(child) for child in children)
        except (FileNotFoundError, ProcessLookupError, PermissionError, ValueError):
            continue
    return total


def run(command: list[str], cwd: str | None = None) -> tuple[float, float, str]:
    start = time.perf_counter()
    proc = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, cwd=cwd)
    peak = 0
    while proc.poll() is None:
        peak = max(peak, tree_rss_kib(proc.pid))
        time.sleep(0.005)
    stdout, stderr = proc.communicate()
    elapsed = time.perf_counter() - start
    if proc.returncode:
        raise RuntimeError(f"{command[0]} exit={proc.returncode}: {stderr.decode(errors='replace')[-800:]}")
    return elapsed, peak / 1024, stdout.decode()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=pathlib.Path)
    parser.add_argument("--upstream", type=pathlib.Path, required=True)
    parser.add_argument("--upstream-python", required=True)
    parser.add_argument("--iterations", type=int, default=3)
    parser.add_argument("--latency-ms", type=float, default=50.0, help="per-response server latency")
    args = parser.parse_args()
    Site.latency = args.latency_ms / 1000
    server = ThreadingServer(("127.0.0.1", 0), Site)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    start_url = f"http://127.0.0.1:{server.server_port}/start"
    results: dict[str, list[dict]] = {"rust": [], "upstream": []}
    try:
        for iteration in range(args.iterations):
            order = ("rust", "upstream") if iteration % 2 == 0 else ("upstream", "rust")
            for implementation in order:
                with tempfile.TemporaryDirectory(prefix="rs-bench-") as directory:
                    root = pathlib.Path(directory)
                    if implementation == "rust":
                        spec = root / "spec.toml"
                        spec.write_text(
                            f'start_urls = ["{start_url}"]\nallowed_domains = ["127.0.0.1"]\n'
                            f"max_pages = {PAGES}\nmax_depth = 1\nconcurrency = 8\ndelay_ms = 0\n"
                            "obey_robots = false\n")
                        output = root / "pages.ndjson"
                        elapsed, rss, _ = run([str(args.binary.resolve()), "crawl", str(spec), "--out", str(output)])
                        records = [json.loads(line) for line in output.read_text().splitlines() if line.strip()]
                        ok = sum(1 for record in records if record.get("status") == 200)
                    else:
                        script = root / "spider.py"
                        script.write_text(UPSTREAM_SPIDER)
                        elapsed, rss, stdout = run(
                            [args.upstream_python, "-I", str(script), str(args.upstream.resolve()), start_url],
                            cwd=directory)
                        ok = json.loads(stdout.strip().splitlines()[-1])["items"]
                    results[implementation].append(
                        {"iteration": iteration, "seconds": round(elapsed, 3), "peak_rss_mib": round(rss, 1),
                         "successful_pages": ok})
    finally:
        server.shutdown()
    summary = {
        name: {
            "median_seconds": statistics.median(r["seconds"] for r in runs),
            "max_peak_rss_mib": max(r["peak_rss_mib"] for r in runs),
            "successful_pages": sorted({r["successful_pages"] for r in runs}),
        }
        for name, runs in results.items()
    }
    print(json.dumps({"pages": PAGES, "concurrency": 8, "latency_ms": args.latency_ms,
                      "runs": results, "summary": summary}, indent=1))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
