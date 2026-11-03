#!/usr/bin/env python3
"""Record upstream `scrapling extract` outputs for fixtures/oracle/extract_cli.

  <venv>/bin/python -I oracle/extract_cli_oracle.py ../scrapling-v05-reference

Serves fixtures/extraction/*.html plus an echo endpoint on loopback, runs the
pinned upstream CLI (`scrapling.cli.main`) for every case in CASES and stores
the written files. Port numbers never appear in the outputs.
"""
import http.server
import json
import pathlib
import subprocess
import sys
import tempfile
import re
import threading

ROOT = pathlib.Path(__file__).resolve().parents[1]
PAGES = ROOT / "fixtures" / "extraction"
OUT = ROOT / "fixtures" / "oracle" / "extract_cli"

# (name, cli args after "extract"; {base} is the server URL, {out} the output file)
CASES = [
    ("get_md", ["get", "{base}/article_en.html", "{out}.md"]),
    ("get_html", ["get", "{base}/table.html", "{out}.html"]),
    ("get_txt", ["get", "{base}/listing.html", "{out}.txt"]),
    ("get_ai_md", ["get", "{base}/nohidden/article_en.html", "{out}.md", "--ai-targeted"]),
    ("get_ai_txt_ja", ["get", "{base}/nohidden/article_ja.html", "{out}.txt", "--ai-targeted"]),
    ("get_selector_md", ["get", "{base}/listing.html", "{out}.md", "-s", "li h3"]),
    ("get_selector_text", ["get", "{base}/techdoc_code.html", "{out}.txt", "-s", "pre code"]),
    ("get_selector_ai_md", ["get", "{base}/nohidden/product.html", "{out}.md", "--ai-targeted", "-s", "ul li"]),
    ("post_json_md", ["post", "{base}/echo", "{out}.md", "--json", "{\"q\": 1}", "-H", "X-Test: yes"]),
    ("post_data_txt", ["post", "{base}/echo", "{out}.txt", "-d", "a=1&b=2"]),
    ("put_json_html", ["put", "{base}/echo", "{out}.html", "-j", "[1, 2]"]),
    ("delete_txt", ["delete", "{base}/echo", "{out}.txt", "--cookies", "sid=abc; theme=dark"]),
    ("get_params_txt", ["get", "{base}/echo", "{out}.txt", "-p", "x=1", "-p", "y=two"]),
]


class Handler(http.server.BaseHTTPRequestHandler):
    def _echo(self):
        length = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(length).decode() if length else ""
        cookie = self.headers.get("Cookie", "")
        page = (f"<html><body><main><h1>Echo</h1><p id=m>{self.command}</p><p id=p>{self.path}</p>"
                f"<p id=t>{self.headers.get('Content-Type', '')}</p><p id=b>{body}</p>"
                f"<p id=c>{cookie}</p><p id=x>{self.headers.get('X-Test', '')}</p></main></body></html>").encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(page)))
        self.end_headers()
        self.wfile.write(page)

    def do_GET(self):
        if self.path.startswith("/echo"):
            return self._echo()
        # /nohidden/<page> drops `hidden`-attribute elements: main-content
        # output intentionally differs from upstream on those (D-013).
        nohidden = self.path.startswith("/nohidden/")
        path = PAGES / self.path.removeprefix("/nohidden").lstrip("/")
        if not path.is_file():
            self.send_error(404)
            return
        data = path.read_bytes()
        if nohidden:
            data = re.sub(rb"<p hidden>.*?</p>", b"", data)
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    do_POST = do_PUT = do_DELETE = _echo

    def log_message(self, *_):
        pass


def main():
    upstream = sys.argv[1]
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base = f"http://127.0.0.1:{server.server_port}"
    results = {}
    with tempfile.TemporaryDirectory() as tmp:
        for name, args in CASES:
            out = str(pathlib.Path(tmp) / name)
            argv = [a.replace("{base}", base).replace("{out}", out) for a in args]
            code = (f"import sys; sys.path.insert(0, {upstream!r}); sys.argv = ['scrapling', 'extract', *{argv!r}];"
                    "from scrapling.cli import main; main()")
            proc = subprocess.run([sys.executable, "-I", "-c", code], capture_output=True, text=True)
            target = pathlib.Path(argv[2])
            results[name] = {"args": args, "exit": proc.returncode,
                             "content": target.read_text(encoding="utf-8") if target.exists() else None}
            if proc.returncode:
                results[name]["stderr_tail"] = proc.stderr[-300:]
    server.shutdown()
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "expected.json").write_text(json.dumps(results, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
    print({k: (v["exit"], len(v["content"] or "")) for k, v in results.items()})


if __name__ == "__main__":
    main()
