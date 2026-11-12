"""Loopback integration smoke tests for the RustScrapling CLI.

Usage: python3 tests/e2e_local.py path/to/rustscrapling

Only Python's standard library is needed. No external web services are contacted.
This is a *product integration test*, not an oracle for anti-bot success.
"""

import http.server
import json
import pathlib
import subprocess
import sys
import tempfile
import threading
import unittest


ARTICLE = b"""<!doctype html><html><head><title>Local RustScrapling test</title>
<meta property="og:title" content="Local article"><link rel="canonical" href="/article">
</head><body><nav>Unrelated navigation</nav><main><article>
<h1 id="title">Integration article</h1><p class="content">A unique test paragraph about deterministic local web extraction and reliable metadata.</p>
<p>Another paragraph about extraction and content security.</p>
</article></main><script>ignore me</script></body></html>"""


class Handler(http.server.BaseHTTPRequestHandler):
    requests = {}

    def do_GET(self):
        Handler.requests[self.path] = Handler.requests.get(self.path, 0) + 1
        if self.path == "/redirect":
            self.send_response(302)
            self.send_header("Location", "/article")
            self.end_headers()
            return
        if self.path == "/blocked":
            self.send_response(403)
            self.end_headers()
            self.wfile.write(b"Blocked")
            return
        if self.path == "/etag" and self.headers.get("If-None-Match") == '"local-1"':
            self.send_response(304)
            self.send_header("ETag", '"local-1"')
            self.end_headers()
            return
        content_type = "text/html; charset=utf-8"
        if self.path == "/robots.txt":
            data = b"User-agent: *\nDisallow: /blocked\n"
            content_type = "text/plain"
        elif self.path in ("/shiftjis", "/misdeclared"):
            data = "<html><body><p>日本語の研究記事。</p></body></html>".encode("shift_jis")
            if self.path == "/shiftjis":
                content_type = "text/html; charset=Shift_JIS"
        else:
            data = ARTICLE
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("ETag", '"local-1"')
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        size = int(self.headers.get("Content-Length", 0))
        data = self.rfile.read(min(size, 1024))
        self.send_response(200)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, fmt, *args):
        pass


class CliSmoke(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.binary = pathlib.Path(sys.argv[1]).resolve()
        cls.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()
        cls.url = f"http://127.0.0.1:{cls.server.server_port}"

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()
        cls.thread.join(timeout=3)

    def cli(self, *args, ok=True):
        proc = subprocess.run([str(self.binary), *args], capture_output=True, text=True, timeout=30)
        if ok:
            self.assertEqual(proc.returncode, 0, proc.stderr)
            try:
                return json.loads(proc.stdout)
            except json.JSONDecodeError:
                self.fail(f"CLI returned non-JSON: {proc.stdout!r}")
        self.assertNotEqual(proc.returncode, 0, proc.stdout)
        return proc

    def test_http_get(self):
        result = self.cli("get", self.url + "/redirect", "--format", "json")
        self.assertEqual(result["schema_version"], 1)
        self.assertIn("article", json.dumps(result).lower())

    def test_markdown_and_metadata(self):
        result = self.cli("md", self.url + "/article", "--format", "json")
        self.assertEqual(result["schema_version"], 1)
        serialized = json.dumps(result)
        self.assertIn("Integration article", serialized)
        self.assertNotIn("ignore me", serialized)

    def test_auto_tier_and_blocked_error(self):
        result = self.cli("auto", self.url + "/article", "--format", "json", "--direct")
        self.assertEqual(result["schema_version"], 1)
        self.assertEqual(result["tier"], "http")
        failure = self.cli("get", self.url + "/blocked", "--direct", ok=False)
        self.assertEqual(json.loads(failure.stderr)["error"]["kind"], "source_refused")
        self.assertEqual(json.loads(failure.stderr)["error"]["status"], 403)

    def test_post_body(self):
        result = self.cli("post", self.url + "/echo", "--data", "agent-echo-test", "--direct")
        self.assertEqual(result["status"], 200)
        self.assertIn("agent-echo-test", result["content"]["text"])

    def test_crawl_resume(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            spec = root / "spec.toml"
            output = root / "output.jsonl"
            checkpoint = root / "crawldir"
            spec.write_text(
                f'start_urls = ["{self.url}/article"]\n'
                f'allowed_domains = ["127.0.0.1"]\n'
                f'crawldir = "{checkpoint.as_posix()}"\n'
                "max_pages = 1\nmax_depth = 1\nconcurrency = 2\nobey_robots = true\n"
            )
            finished = subprocess.run([str(self.binary), "crawl", str(spec), "--out", str(output)],
                                      capture_output=True, text=True, timeout=30)
            self.assertEqual(finished.returncode, 0, finished.stderr)
            entries = [json.loads(line) for line in output.read_text().splitlines()]
            self.assertEqual(len(entries), 1)
            self.assertEqual(entries[0]["status"], 200)
            self.assertEqual(entries[0]["schema_version"], 1)
            repeated = subprocess.run([str(self.binary), "crawl", str(spec), "--out", str(output)],
                                      capture_output=True, text=True, timeout=30)
            self.assertEqual(repeated.returncode, 0, repeated.stderr)
            self.assertEqual(output.read_text(), "")

    def test_crawl_decodes_declared_charset_and_rejects_malformed_bytes(self):
        for endpoint in ("shiftjis", "misdeclared"):
            with self.subTest(endpoint=endpoint), tempfile.TemporaryDirectory() as directory:
                spec = pathlib.Path(directory) / "spec.toml"
                output = pathlib.Path(directory) / "pages.jsonl"
                spec.write_text(
                    f'start_urls = ["{self.url}/{endpoint}"]\n'
                    'allowed_domains = ["127.0.0.1"]\n'
                    'max_pages = 1\nmax_depth = 0\nconcurrency = 1\nobey_robots = false\n'
                )
                result = subprocess.run(
                    [str(self.binary), "crawl", str(spec), "--out", str(output)],
                    capture_output=True, text=True, timeout=30,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                entries = [json.loads(line) for line in output.read_text().splitlines()]
                self.assertEqual(len(entries), 1)
                if endpoint == "shiftjis":
                    self.assertEqual(entries[0]["status"], 200)
                    self.assertIn("日本語の研究記事。", entries[0]["content"])
                    self.assertNotIn("\ufffd", entries[0]["content"])
                else:
                    self.assertIn("decode_failed", entries[0]["error"])
                    self.assertIsNone(entries[0]["content"])

    def test_local_selector_and_receipt(self):
        with tempfile.TemporaryDirectory() as tmp:
            page = pathlib.Path(tmp) / "article.html"
            dest = pathlib.Path(tmp) / "result.json"
            page.write_bytes(ARTICLE)
            result = self.cli("select", str(page), "--css", "h1", "--format", "json")
            self.assertEqual(result["schema_version"], 1)
            self.assertEqual(len(result["results"]), 1)
            receipt = self.cli("select", str(page), "--css", "h1", "--out", str(dest))
            self.assertEqual(receipt["schema_version"], 1)
            self.assertEqual(len(json.loads(dest.read_text())["results"]), 1)
            selector = self.cli("selector-gen", str(page), "--css", "h1", "--compat")
            self.assertEqual(selector["schema_version"], 1)
            # Scrapling intentionally shortens an element with an ID to
            # "#title" instead of redundantly retaining its h1 tag.
            self.assertEqual(selector["css"], "#title")
            self.assertIn("title", selector["xpath"])
            roundtrip = self.cli("select", str(page), "--css", selector["css"], "--compat")
            self.assertEqual(len(roundtrip["results"]), 1)


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: python3 tests/e2e_local.py /absolute/path/to/rustscrapling")
    unittest.main(argv=[sys.argv[0]], verbosity=2)
