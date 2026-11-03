#!/usr/bin/env python3
"""Plan G-10 endurance (soak) driver for the CLI + daemon + browser stack.

Runs for --duration seconds (24 h for G-10; shorter pilots are allowed but
are NOT G-10 evidence). Each cycle, against a loopback site:
  - opens a browser session and an HTTP session in the daemon, fetches and
    requests several pages, saves/loads state, closes both sessions;
  - runs one-shot CLI `get` and `shell -c` commands;
  - every --restart-every cycles stops and restarts the daemon.
Every --sample seconds it records the daemon process tree RSS, open file
descriptors, Chrome process count, temp profile directories and state-dir
size, plus per-operation latency and failures, to a JSONL file. At the end it
prints a summary with linear-regression slopes so monotonic growth (leaks)
is visible, and exits 1 if any operation failed or a leak threshold is hit.

  python3 bench/soak.py target/release/rustscrapling --chrome /path/chrome \
      --duration 1800 --out soak.jsonl
"""

from __future__ import annotations

import argparse
import http.server
import json
import os
import pathlib
import socketserver
import subprocess
import tempfile
import threading
import time


class Site(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = (f"<html><head><title>Soak {self.path}</title></head><body><article>"
                f"<h1>Page {self.path}</h1><p>{'lorem ipsum ' * 200}</p>"
                f"<script>document.cookie='visit={int(time.time())}'</script>"
                f"</article></body></html>").encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Set-Cookie", "sid=soak; Path=/")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):
        pass


class Server(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True


def tree(pid: int) -> list[int]:
    out, pending = [], [pid]
    while pending:
        current = pending.pop()
        out.append(current)
        try:
            children = pathlib.Path(f"/proc/{current}/task/{current}/children").read_text().split()
        except OSError:
            continue
        pending.extend(int(c) for c in children)
    return out


def rss_kib(pids: list[int]) -> int:
    total = 0
    for pid in pids:
        try:
            for line in pathlib.Path(f"/proc/{pid}/status").read_text().splitlines():
                if line.startswith("VmRSS:"):
                    total += int(line.split()[1])
        except OSError:
            pass
    return total


def fds(pids: list[int]) -> int:
    total = 0
    for pid in pids:
        try:
            total += len(os.listdir(f"/proc/{pid}/fd"))
        except OSError:
            pass
    return total


def dir_size(path: pathlib.Path) -> int:
    return sum(p.stat().st_size for p in path.rglob("*") if p.is_file()) if path.exists() else 0


def slope(points: list[tuple[float, float]]) -> float:
    if len(points) < 2:
        return 0.0
    n = len(points)
    mx = sum(x for x, _ in points) / n
    my = sum(y for _, y in points) / n
    den = sum((x - mx) ** 2 for x, _ in points)
    return sum((x - mx) * (y - my) for x, y in points) / den if den else 0.0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("binary", type=pathlib.Path)
    parser.add_argument("--chrome", required=True)
    parser.add_argument("--duration", type=float, default=86400)
    parser.add_argument("--sample", type=float, default=30)
    parser.add_argument("--restart-every", type=int, default=20)
    parser.add_argument("--out", type=pathlib.Path, default=pathlib.Path("soak.jsonl"))
    args = parser.parse_args()
    binary = str(args.binary.resolve())
    server = Server(("127.0.0.1", 0), Site)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base = f"http://127.0.0.1:{server.server_port}"
    work = pathlib.Path(tempfile.mkdtemp(prefix="rs-soak-"))
    state_file = work / "daemon.json"
    env = dict(os.environ, RUSTSCRAPLING_STATE_DIR=str(work / "states"), TMPDIR=str(work / "tmp"))
    (work / "tmp").mkdir()
    out = args.out.open("w")
    failures: list[dict] = []
    latencies: dict[str, list[float]] = {}

    def run(name: str, *argv: str, timeout: float = 120) -> subprocess.CompletedProcess | None:
        start = time.perf_counter()
        try:
            result = subprocess.run([binary, *argv], capture_output=True, text=True, timeout=timeout, env=env)
        except subprocess.TimeoutExpired:
            failures.append({"op": name, "error": "timeout", "t": time.time()})
            return None
        latencies.setdefault(name, []).append(time.perf_counter() - start)
        if result.returncode:
            failures.append({"op": name, "code": result.returncode, "stderr": result.stderr[-300:], "t": time.time()})
            return None
        return result

    def daemon_pid() -> int | None:
        try:
            return json.loads(state_file.read_text())["pid"]
        except (OSError, ValueError, KeyError):
            return None

    def start_daemon():
        run("daemon_start", "daemon", "start", "--state-file", str(state_file), "--direct")

    def sample(cycle: int, started: float):
        pid = daemon_pid()
        pids = tree(pid) if pid else []
        chrome = sum(1 for p in pids if "chrome" in (pathlib.Path(f"/proc/{p}/comm").read_text().strip()
                                                     if pathlib.Path(f"/proc/{p}/comm").exists() else ""))
        record = {
            "t": round(time.time() - started, 1), "cycle": cycle, "daemon_pid": pid,
            "rss_mib": round(rss_kib(pids) / 1024, 1), "daemon_rss_mib": round(rss_kib([pid]) / 1024, 1) if pid else 0,
            "fds": fds(pids), "processes": len(pids), "chrome_processes": chrome,
            "temp_profiles": len(list((work / "tmp").glob("rustscrapling-cdp-*"))),
            "state_dir_bytes": dir_size(work / "states"), "failures": len(failures),
        }
        out.write(json.dumps(record) + "\n")
        out.flush()
        return record

    started = time.time()
    start_daemon()
    samples, cycle, last_sample = [], 0, 0.0
    try:
        while time.time() - started < args.duration:
            cycle += 1
            sd = ["--state-file", str(state_file)]
            run("browser_open", "session", "open", *sd, "--session-id", "b", "--executable-path", args.chrome)
            for i in range(3):
                run("browser_fetch", "session", "fetch", *sd, "b", f"{base}/b/{cycle}/{i}", "--format", "text")
            run("save_state", "session", "save-state", *sd, "b", f"soak-{cycle}.json")
            run("load_state", "session", "load-state", *sd, "b", f"soak-{cycle}.json")
            # State files are never overwritten by design; remove the old one
            # so the directory size reflects leaks, not retained user data.
            for old in (work / "states").glob("soak-*.json"):
                if old.name != f"soak-{cycle}.json":
                    old.unlink()
            run("browser_close", "session", "close", *sd, "b")
            run("http_open", "session", "open", *sd, "--http", "--session-id", "h")
            for i in range(5):
                run("http_request", "session", "request", *sd, "h", f"{base}/h/{cycle}/{i}")
            run("http_close", "session", "close", *sd, "h")
            run("cli_get", "get", f"{base}/g/{cycle}", "--direct", "--format", "text")
            run("cli_shell", "shell", "-c", f"get {base}/s/{cycle}\ncss h1::text")
            if cycle % args.restart_every == 0:
                run("daemon_stop", "daemon", "stop", "--state-file", str(state_file))
                time.sleep(1)
                start_daemon()
            if time.time() - last_sample >= args.sample:
                samples.append(sample(cycle, started))
                last_sample = time.time()
    finally:
        samples.append(sample(cycle, started))
        run("daemon_stop", "daemon", "stop", "--state-file", str(state_file))
        server.shutdown()
        out.close()
    time.sleep(2)
    def mentions_work(proc: pathlib.Path) -> bool:
        try:
            return str(work) in (proc / "cmdline").read_bytes().decode(errors="replace")
        except OSError:
            return False

    orphans = sum(1 for proc in pathlib.Path("/proc").glob("[0-9]*") if mentions_work(proc))
    leftover_profiles = len(list((work / "tmp").glob("rustscrapling-cdp-*")))
    # Leak heuristics over the second half (after warm-up), per hour.
    half = [s for s in samples if s["t"] >= samples[-1]["t"] / 2] or samples
    per_hour = {key: round(slope([(s["t"], s[key]) for s in half]) * 3600, 2)
                for key in ("rss_mib", "fds", "processes", "chrome_processes", "temp_profiles")}
    summary = {
        "duration_s": round(time.time() - started, 1), "cycles": cycle, "samples": len(samples),
        "failures": len(failures), "first_failures": failures[:5],
        "max_rss_mib": max(s["rss_mib"] for s in samples),
        "final": samples[-1], "slope_per_hour_second_half": per_hour,
        "after_stop": {"orphan_processes": orphans, "temp_profiles": leftover_profiles},
        "latency_p50_s": {k: round(sorted(v)[len(v) // 2], 3) for k, v in latencies.items()},
        "latency_max_s": {k: round(max(v), 3) for k, v in latencies.items()},
    }
    print(json.dumps(summary, indent=1))
    leaked = orphans > 0 or leftover_profiles > 0
    return 1 if failures or leaked else 0


if __name__ == "__main__":
    raise SystemExit(main())
