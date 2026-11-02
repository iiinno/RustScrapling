#!/usr/bin/env python3
"""Reproducible 150-page localhost crawl, concurrency and RSS smoke benchmark.

This is not the public 200-URL Scrapling comparison. It tests that the native
CLI can crawl a bounded workload in one process without a huge memory spike.
"""

import argparse
import http.server
import json
import pathlib
import subprocess
import tempfile
import threading
import time


class Fixture(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/start":
            links = "".join(f'<a href="/page/{i}">Page {i}</a>' for i in range(149))
            page = f"<html><body><h1>Index</h1>{links}</body></html>"
        elif self.path.startswith("/page/"):
            try:
                number = int(self.path.removeprefix("/page/"))
            except ValueError:
                number = -1
            if not (0 <= number < 149):
                self.send_error(404)
                return
            page = f"<html><body><article><h1>Page {number}</h1><p>Content number {number}</p></article></body></html>"
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


def current_rss_kib(pid):
    status = pathlib.Path(f"/proc/{pid}/status")
    try:
        for line in status.read_text().splitlines():
            if line.startswith("VmRSS:"):
                return int(line.split()[1])
    except (FileNotFoundError, PermissionError, ValueError):
        pass
    return None


def measure(command):
    start = time.perf_counter()
    proc = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    peak = 0
    observed = False
    while proc.poll() is None:
        rss = current_rss_kib(proc.pid)
        if rss is not None:
            observed = True
            peak = max(peak, rss)
        time.sleep(0.005)
    stdout, stderr = proc.communicate()
    duration = time.perf_counter() - start
    if proc.returncode:
        raise RuntimeError(
            f"crawl exit={proc.returncode}\nstdout={stdout.decode(errors='replace')}\n"
            f"stderr={stderr.decode(errors='replace')}"
        )
    return duration, peak / 1024 if observed else None


def main():
    p = argparse.ArgumentParser()
    p.add_argument("binary", type=pathlib.Path)
    p.add_argument("--max-rss-mib", type=int, default=256)
    p.add_argument("--concurrency", type=int, default=8)
    args = p.parse_args()
    if not args.binary.is_file():
        p.error(f"binary not found: {args.binary}")
    if not 1 <= args.concurrency <= 64:
        p.error("concurrency must be 1..64")

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix="rustscrapling-stress-") as directory:
            root = pathlib.Path(directory)
            start_url = f"http://127.0.0.1:{server.server_port}/start"
            spec = root / "spec.toml"
            spec.write_text(
                f'start_urls = ["{start_url}"]\n'
                f'allowed_domains = ["127.0.0.1"]\n'
                f'max_pages = 150\nmax_depth = 1\nconcurrency = {args.concurrency}\n'
                f'delay_ms = 0\nobey_robots = false\n'
                f'crawldir = "{root.joinpath("state").as_posix()}"\n'
            )
            output = root / "pages.ndjson"
            command = [str(args.binary.resolve()), "crawl", str(spec), "--out", str(output)]
            elapsed, peak_rss = measure(command)
            records = [json.loads(line) for line in output.read_text().splitlines() if line.strip()]
            urls = {item["url"] for item in records}
            if len(records) != 150 or len(urls) != 150 or any(r["status"] != 200 for r in records):
                raise AssertionError(
                    f"expected 150 distinct successful pages; got {len(records)} records, {len(urls)} URLs"
                )
            # A completed checkpoint must not re-fetch the same pages.
            next_elapsed, _ = measure(command)
            if output.read_text().strip():
                raise AssertionError("resume re-fetched completed pages")
            result = {
                "fixture": "localhost/150",
                "concurrency": args.concurrency,
                "pages": 150,
                "elapsed_seconds": round(elapsed, 3),
                "peak_rss_mib": round(peak_rss, 2) if peak_rss is not None else None,
                "resume_seconds": round(next_elapsed, 3),
                "rss_limit_mib": args.max_rss_mib,
            }
            print(json.dumps(result, sort_keys=True))
            if peak_rss is not None and peak_rss > args.max_rss_mib:
                raise AssertionError(f"peak RSS {peak_rss:.1f} MiB exceeds {args.max_rss_mib} MiB")
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


if __name__ == "__main__":
    main()
