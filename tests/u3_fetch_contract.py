"""Real CLI U3 regression cases. Baseline asserts defects; verify asserts the contract.

All HTTP payloads, CDP diagnostics and profiles are owned synthetic fixtures.
The mock CDP server is only for deterministic error injection. Browser content
checks require an actual Chrome executable and never skip when it is absent.
"""

import argparse
import base64
from contextlib import contextmanager
from dataclasses import dataclass
import hashlib
import http.server
import json
import os
from pathlib import Path
import selectors
import signal
import socketserver
import struct
import subprocess
import tempfile
import threading
import time
from urllib.parse import urlsplit


JAPANESE = "日本語の研究記事です。"
UNICODE = JAPANESE + "Unicode検証🦀。"
BYTE_BUDGET = 4096
CHAR_BUDGET = 4096
STDERR_BUDGET = 8192
CANONICAL_KINDS = {
    "invalid_request", "unavailable", "transport", "stage_timeout", "saturated",
    "invalid_payload", "unsupported_contract", "network_policy_unsupported",
    "destination_denied", "source_refused", "source_http", "rate_limited",
    "tls_rejected", "empty", "needs_rendering", "decode_failed",
    "output_too_large", "cancelled", "deadline_exceeded", "cleanup_unconfirmed",
    "busy", "exhausted", "unknown_provider_failure",
}


@dataclass(frozen=True)
class Payload:
    body: bytes
    content_type: str = "text/plain; charset=utf-8"
    status: int = 200
    declared_length: int | None = None


def html_document(body, head=""):
    return f"<!doctype html><html><head>{head}</head><body>{body}</body></html>"


def fixtures():
    large = "研究本文🦀 と quote\" and backslash\\。" * 3000
    large_ascii = "A synthetic paragraph about reproducible research. " * 500
    metadata = "研究メタデータ🦀" * 2000
    payloads = {
        "/utf8": Payload(UNICODE.encode()),
        "/default-utf8": Payload(UNICODE.encode(), "text/plain"),
        "/shiftjis": Payload(JAPANESE.encode("shift_jis"), "text/plain; charset=Shift_JIS"),
        "/bad-utf8": Payload(JAPANESE.encode("shift_jis")),
        "/bad-shiftjis": Payload(b"\x82", "text/plain; charset=shift_jis"),
        "/unknown-charset": Payload(UNICODE.encode(), "text/plain; charset=x-u3-unknown"),
        "/short": Payload(b"OK"),
        "/short-html": Payload(html_document("<p>短文。</p>").encode(), "text/html; charset=utf-8"),
        "/empty": Payload(b""),
        "/whitespace": Payload(" \t\n\u3000\u00a0".encode()),
        "/204": Payload(b"", status=204),
        "/206": Payload(UNICODE.encode(), status=206),
        "/shell": Payload(html_document("<div id='app'></div><script src='/app.js'></script>").encode(), "text/html"),
        "/metadata-only": Payload(html_document("", "<title>Only metadata</title>").encode(), "text/html"),
        "/large-plain": Payload(large.encode()),
        "/large-paragraph": Payload(html_document(f"<article><p>{large_ascii}</p></article>").encode(), "text/html; charset=utf-8"),
        "/many-paragraphs": Payload(html_document("<article>" + (f"<p>{large_ascii[:500]}</p>" * 80) + "</article>").encode(), "text/html; charset=utf-8"),
        "/large-headings": Payload(html_document(f"<article><h1>{metadata}</h1><p>{JAPANESE}</p></article>").encode(), "text/html; charset=utf-8"),
        "/large-metadata": Payload(html_document(f"<article><p>{UNICODE}</p></article>", f"<title>{metadata}</title><meta name='description' content='{metadata}'><meta name='author' content='{metadata}'><meta property='article:published_time' content='{metadata}'><meta property='article:modified_time' content='{metadata}'><link rel='canonical' href='/{'a' * 12000}'><meta property='og:title' content='{metadata}'><script type='application/ld+json'>" + json.dumps({"description": metadata, "nested": [metadata]}) + "</script>").encode(), "text/html; charset=utf-8"),
        "/truncated": Payload(b"incomplete", declared_length=1000),
        "/oversize": Payload(b"", declared_length=64 * 1024 * 1024),
    }
    for status in (403, 404, 429, 500, 502):
        payloads[f"/status/{status}"] = Payload(b"Synthetic source error. ", status=status)
    for name, declaration, header, encoding in [
        ("meta-shiftjis", "<meta charset='Shift_JIS'>", "text/html", "shift_jis"),
        ("equiv-shiftjis", "<meta http-equiv='Content-Type' content='text/html; charset=Shift_JIS'>", "text/html", "shift_jis"),
        ("header-over-meta", "<meta charset='UTF-8'>", "text/html; charset=Shift_JIS", "shift_jis"),
        ("utf8-over-meta", "<meta charset='Shift_JIS'>", "text/html; charset=UTF-8", "utf-8"),
    ]:
        payloads[f"/{name}"] = Payload(html_document(f"<article><p>{JAPANESE}</p></article>", declaration).encode(encoding), header)
    rendered = json.dumps(UNICODE)
    javascript = f"<script>document.getElementById('app').textContent={rendered};</script>"
    payloads["/js-page"] = Payload(html_document("<article id='app'></article>" + javascript).encode(), "text/html; charset=utf-8")
    payloads["/browser-shiftjis"] = Payload(html_document(f"<article id='app'>{JAPANESE}</article>" + javascript, "<meta charset='Shift_JIS'>").encode("shift_jis"), "text/html; charset=Shift_JIS")
    return payloads


class HttpHandler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.server.seen.append(self.path)
        payload = self.server.payloads.get(self.path, Payload(b"fixture missing", status=404))
        self.send_response(payload.status)
        self.send_header("Content-Type", payload.content_type)
        self.send_header("Content-Length", str(payload.declared_length if payload.declared_length is not None else len(payload.body)))
        self.send_header("Connection", "close")
        self.end_headers()
        try:
            self.wfile.write(payload.body)
        except (BrokenPipeError, ConnectionResetError):
            pass
        self.close_connection = True

    def log_message(self, *_args):
        pass


class CdpErrorHandler(socketserver.BaseRequestHandler):
    def handle(self):
        self.request.settimeout(5)
        self.server.connections.append(self.request)
        try:
            handshake = b""
            while b"\r\n\r\n" not in handshake:
                part = self.request.recv(4096)
                if not part:
                    return
                handshake += part
                if len(handshake) > 16384:
                    raise ValueError("fixture handshake exceeds limit")
            key = next(line.split(b":", 1)[1].strip() for line in handshake.split(b"\r\n") if line.lower().startswith(b"sec-websocket-key:"))
            accept = base64.b64encode(hashlib.sha1(key + b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11").digest())
            self.request.sendall(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: " + accept + b"\r\n\r\n")
            large = b"/large" in handshake.split(b"\r\n", 1)[0]
            while True:
                first, second = self.read_exact(2)
                size = second & 127
                if size == 126:
                    size = struct.unpack("!H", self.read_exact(2))[0]
                elif size == 127:
                    size = struct.unpack("!Q", self.read_exact(8))[0]
                if size > 1024 * 1024:
                    raise ValueError("fixture request exceeds limit")
                mask = self.read_exact(4) if second & 128 else None
                body = self.read_exact(size)
                if mask:
                    body = bytes(value ^ mask[index % 4] for index, value in enumerate(body))
                if first & 15 != 1:
                    return
                request = json.loads(body)
                method = request.get("method")
                self.server.methods.append(method)
                if method == "Target.createTarget":
                    result = {"targetId": "u3-synthetic-tab"}
                elif method == "Target.attachToTarget":
                    result = {"sessionId": "u3-synthetic-session"}
                elif method == "Page.navigate":
                    result = {"errorText": "U3 synthetic navigation diagnostic " + ("診断🦀" * 65536 if large else "small")}
                else:
                    result = {}
                response = json.dumps({"id": request["id"], "result": result}, ensure_ascii=False).encode()
                header = b"\x81" + (bytes([len(response)]) if len(response) < 126 else b"\x7e" + struct.pack("!H", len(response)) if len(response) < 65536 else b"\x7f" + struct.pack("!Q", len(response)))
                self.request.sendall(header + response)
        except (TimeoutError, ConnectionError, OSError):
            pass
        except Exception as error:
            self.server.errors.append(type(error).__name__)

    def read_exact(self, size):
        data = b""
        while len(data) < size:
            part = self.request.recv(size - len(data))
            if not part:
                raise ConnectionError("client closed")
            data += part
        return data


@contextmanager
def fixture_servers():
    http_fixture = http.server.ThreadingHTTPServer(("127.0.0.1", 0), HttpHandler)
    http_fixture.payloads, http_fixture.seen = fixtures(), []
    cdp = socketserver.ThreadingTCPServer(("127.0.0.1", 0), CdpErrorHandler)
    cdp.daemon_threads = True
    cdp.connections, cdp.methods, cdp.errors = [], [], []
    threads = [threading.Thread(target=server.serve_forever, daemon=True) for server in (http_fixture, cdp)]
    for thread in threads:
        thread.start()
    try:
        yield http_fixture, cdp
    finally:
        for connection in cdp.connections:
            try:
                connection.shutdown(2)
            except OSError:
                pass
            connection.close()
        for server in (http_fixture, cdp):
            server.shutdown()
            server.server_close()
        for thread in threads:
            thread.join(timeout=5)
            assert not thread.is_alive(), "fixture server did not stop"


def clean_environment(root):
    env = {key: value for key, value in os.environ.items() if not key.upper().endswith("_PROXY") and not key.startswith(("GIT_", "RUSTSCRAPLING_"))}
    for key in ("TMPDIR", "TMP", "TEMP", "XDG_RUNTIME_DIR", "XDG_CACHE_HOME", "XDG_CONFIG_HOME"):
        directory = root / key.lower()
        directory.mkdir(exist_ok=True)
        env[key] = str(directory)
    env["RUSTSCRAPLING_DAEMON_STATE"] = str(root / "absent-daemon.json")
    return env


def stop_process(process):
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait(timeout=5)


def capped_communicate(process, timeout):
    captured = {process.stdout: bytearray(), process.stderr: bytearray()}
    caps = {process.stdout: 1024 * 1024, process.stderr: STDERR_BUDGET}
    with selectors.DefaultSelector() as selector:
        for stream in captured:
            os.set_blocking(stream.fileno(), False)
            selector.register(stream, selectors.EVENT_READ)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            for key, _events in selector.select(0.05):
                stream = key.fileobj
                part = os.read(stream.fileno(), min(65536, caps[stream] - len(captured[stream])))
                captured[stream].extend(part)
                if not part or len(captured[stream]) == caps[stream]:
                    selector.unregister(stream)
            if process.poll() is not None and not selector.get_map():
                return bytes(captured[process.stdout]), bytes(captured[process.stderr]), False
        return bytes(captured[process.stdout]), bytes(captured[process.stderr]), True


@dataclass
class Observation:
    argv: list[str]
    exit: int
    stdout: bytes
    stderr: bytes
    timed_out: bool
    profiles: int
    requests: list[str]
    cdp_methods: list[str]

    def receipt(self):
        row = {"argv": self.argv, "exit": self.exit, "outer_timeout": self.timed_out,
               "stdout_bytes": len(self.stdout), "stderr_bytes": len(self.stderr),
               "stdout_sha256": hashlib.sha256(self.stdout).hexdigest(),
               "stderr_sha256": hashlib.sha256(self.stderr).hexdigest(),
               "profiles_after_exit": self.profiles, "requests": self.requests,
               "cdp_methods": self.cdp_methods}
        try:
            value = json.loads(self.stdout or self.stderr)
            row.update(schema_version=value.get("schema_version"), source_status=value.get("status"),
                       tier=value.get("tier"), truncated=value.get("truncated"),
                       error_kind=value.get("error", {}).get("kind"), error_status=value.get("error", {}).get("status"))
            text = value.get("content", {}).get("markdown", "")
            row.update(markdown_chars=len(text), replacement_chars=text.count("\ufffd"))
        except (ValueError, AttributeError, TypeError):
            row["json_parseable"] = False
        return row


@dataclass(frozen=True)
class Case:
    name: str
    command: str
    path: str
    expectation: str
    text: str | None = None
    status: int = 200
    flags: tuple[str, ...] = ()
    tier: str = "http"
    kind: str | None = None
    byte_limit: int | None = None
    char_limit: int | None = None


def case_table():
    cases = []
    for command in ("get", "auto"):
        for path, text in (("utf8", UNICODE), ("default-utf8", UNICODE), ("short", "OK"), ("short-html", "短文。"), ("utf8-over-meta", JAPANESE)):
            cases.append(Case(f"{command}_{path}", command, "/" + path, "healthy", text))
        cases.append(Case(f"{command}_206", command, "/206", "healthy", UNICODE, status=206))
        for path in ("shiftjis", "meta-shiftjis", "equiv-shiftjis", "header-over-meta"):
            cases.append(Case(f"{command}_{path}", command, "/" + path, "charset", JAPANESE))
        for path in ("bad-utf8", "bad-shiftjis", "unknown-charset"):
            cases.append(Case(f"{command}_{path}", command, "/" + path, "decode", kind="decode_failed"))
        for path in ("empty", "whitespace", "204", "metadata-only", "shell"):
            cases.append(Case(f"{command}_{path}", command, "/" + path, "empty", status=204 if path == "204" else 200, kind="needs_rendering" if path == "shell" else "empty"))
        for status in (403, 404, 429, 500, 502):
            cases.append(Case(f"{command}_status_{status}", command, f"/status/{status}", "status", status=status, kind="source_refused" if status in (403, 429) else "source_http"))
        cases.append(Case(f"{command}_truncated_transport", command, "/truncated", "transport", kind="transport"))
        cases.append(Case(f"{command}_oversize_body", command, "/oversize", "oversize", kind="output_too_large"))
        cases.append(Case(f"{command}_diagnostic_bound", command, "/short", "option_error", flags=("--impersonate", "u3-invalid-" + "x" * 12000), kind="invalid_request"))
    cases.append(Case("json_unicode_char_budget", "auto", "/large-plain", "char_control", flags=("--max-chars", str(CHAR_BUDGET)), char_limit=CHAR_BUDGET))
    for path in ("large-paragraph", "many-paragraphs", "large-headings", "large-metadata"):
        cases.append(Case("json_chars_" + path, "auto", "/" + path, "chars", flags=("--max-chars", str(CHAR_BUDGET)), char_limit=CHAR_BUDGET))
        cases.append(Case("json_bytes_" + path, "auto", "/" + path, "bytes", flags=("--max-json-bytes", str(BYTE_BUDGET)), byte_limit=BYTE_BUDGET))
    for command in ("get", "auto"):
        cases.append(Case(command + "_json_multibyte_budget", command, "/large-plain", "bytes", flags=("--max-json-bytes", str(BYTE_BUDGET)), byte_limit=BYTE_BUDGET))
    cases.append(Case("ndjson_multibyte_budget", "auto", "/large-plain", "bytes", flags=("--format", "ndjson", "--max-json-bytes", str(BYTE_BUDGET)), byte_limit=BYTE_BUDGET))
    cases.append(Case("combined_output_budgets", "auto", "/large-plain", "bytes", flags=("--max-chars", "1024", "--max-json-bytes", "2048"), byte_limit=2048, char_limit=1024))
    for limit in (0, 32):
        cases.append(Case(f"insufficient_byte_budget_{limit}", "get", "/short", "small_bytes", flags=("--max-json-bytes", str(limit)), kind="invalid_request"))
    cases.append(Case("insufficient_char_budget", "get", "/short", "small_chars", flags=("--max-chars", "32"), kind="invalid_request"))
    cases.append(Case("text_unicode_char_budget", "get", "/large-plain", "text", flags=("--format", "text", "--max-chars", "63"), char_limit=63))
    cases.append(Case("spawn_failure_profile_cleanup", "fetch", "/short", "spawn", kind="unavailable"))
    cases.append(Case("missing_chrome_creates_no_profile", "fetch", "/short", "missing", kind="unavailable"))
    cases.append(Case("cdp_small_diagnostic", "fetch", "/short", "cdp_small", kind="transport"))
    cases.append(Case("cdp_large_diagnostic", "fetch", "/short", "cdp_large", kind="transport"))
    cases.append(Case("cdp_large_diagnostic_capped_reader", "fetch", "/short", "cdp_capped", kind="transport"))
    cases.append(Case("chrome_dom_shiftjis_is_already_unicode", "fetch", "/browser-shiftjis", "chrome", UNICODE, tier="browser"))
    cases.append(Case("chrome_javascript_content", "fetch", "/js-page", "chrome", UNICODE, tier="browser"))
    cases.append(Case("auto_renders_empty_javascript_shell", "auto", "/js-page", "chrome_auto", UNICODE, tier="browser"))
    return cases


def requires_chrome(case):
    return case.expectation in ("chrome", "chrome_auto")


def one_json(raw):
    assert raw.endswith(b"\n"), "wire output must end in LF"
    text = raw.decode("utf-8", errors="strict")
    decoder = json.JSONDecoder()
    value, end = decoder.raw_decode(text)
    assert text[end:] == "\n", "wire must contain one JSON value and one trailing LF"
    assert isinstance(value, dict), "wire JSON must be an object"
    return value


def success(observation, case, require_text=True):
    assert not observation.timed_out, "CLI did not exit within fixture deadline"
    assert observation.exit == 0, "expected successful CLI exit"
    assert not observation.stderr, "successful CLI emitted diagnostics"
    doc = one_json(observation.stdout)
    assert type(doc.get("schema_version")) is int and doc["schema_version"] == 1
    assert type(doc.get("status")) is int and doc["status"] == case.status
    assert doc.get("tier") == case.tier
    assert type(doc.get("truncated")) is bool
    assert "error" not in doc
    url = urlsplit(doc["url"])
    assert url.scheme in ("http", "https") and url.hostname and not url.username and not url.password
    assert isinstance(doc.get("content"), dict)
    for name in ("markdown", "html", "text"):
        assert isinstance(doc["content"].get(name), str), "content representation lost its schema"
    text = doc["content"]["markdown"]
    if require_text:
        assert text.strip(), "successful fetch has no usable markdown"
    if case.text is not None:
        assert text.strip() == case.text, "decoded or rendered body differs from the expected Unicode text"
        assert "\ufffd" not in text
    return doc


def failure(observation, kinds, status=None, bounded=True):
    assert not observation.timed_out, "failure blocked on a pipe or failed to exit"
    assert observation.exit != 0, "failure returned success exit code"
    assert not observation.stdout, "failure emitted successful stdout"
    if bounded:
        assert len(observation.stderr) <= STDERR_BUDGET, "error JSON exceeds the stderr budget"
    doc = one_json(observation.stderr)
    assert type(doc.get("schema_version")) is int and doc["schema_version"] == 1
    assert isinstance(doc.get("error"), dict) and doc["error"].get("kind") in kinds
    assert "content" not in doc
    if status is not None:
        assert type(doc["error"].get("status")) is int and doc["error"]["status"] == status
    return doc


def assert_case(case, observation, mode, payloads):
    old = mode == "baseline"
    expectation = case.expectation
    if expectation in ("healthy", "chrome"):
        doc = success(observation, case)
        assert doc["truncated"] is False
    elif expectation == "charset":
        if old:
            uncorrected = Case(case.name, case.command, case.path, expectation)
            doc = success(observation, uncorrected)
            assert doc["content"]["markdown"].strip() != case.text
            assert "\ufffd" in doc["content"]["markdown"], "old lossy decode defect was not observed"
        else:
            success(observation, case)
    elif expectation == "decode":
        if old:
            doc = success(observation, case)
            if case.path != "/unknown-charset":
                assert "\ufffd" in doc["content"]["markdown"]
        else:
            failure(observation, {case.kind}, status=case.status)
    elif expectation in ("empty", "chrome_auto"):
        if old:
            old_case = Case(case.name, case.command, case.path, expectation, status=case.status)
            doc = success(observation, old_case, require_text=False)
            assert not doc["content"]["markdown"].strip(), "old false empty success was not observed"
        elif expectation == "chrome_auto":
            success(observation, case)
        else:
            failure(observation, {case.kind}, status=case.status)
    elif expectation == "status":
        legacy = "blocked" if case.status in (403, 429) else "http_status"
        failure(observation, {legacy if old else case.kind}, status=None if old else case.status)
    elif expectation == "transport":
        failure(observation, {case.kind})
    elif expectation == "oversize":
        legacy = "bodytoolarge" if case.command == "auto" else "body_too_large"
        failure(observation, {legacy if old else case.kind})
    elif expectation in ("bytes", "small_bytes") and old:
        assert not observation.timed_out and observation.exit != 0 and not observation.stdout
        assert b"--max-json-bytes" in observation.stderr and b"unexpected argument" in observation.stderr
        assert not observation.requests, "unknown option must fail before HTTP"
    elif expectation == "chars" and old:
        failure(observation, {"invalid_request"})
    elif expectation in ("bytes", "chars", "char_control"):
        doc = success(observation, case)
        assert doc["truncated"] is True
        assert isinstance(doc.get("metadata"), dict)
        assert isinstance(doc.get("headings"), list)
        assert isinstance(doc.get("paragraphs"), list)
        assert "\ufffd" not in doc["content"]["markdown"]
        if case.byte_limit is not None:
            assert len(observation.stdout) <= case.byte_limit, "encoded JSON including LF exceeds byte budget"
        if case.char_limit is not None:
            assert len(observation.stdout.decode()) <= case.char_limit, "JSON including LF exceeds character budget"
        if expectation == "char_control":
            assert len(observation.stdout) > len(observation.stdout.decode()), "multibyte control did not exercise byte/character distinction"
    elif expectation in ("small_bytes", "small_chars"):
        failure(observation, {case.kind})
    elif expectation == "text":
        assert not observation.timed_out and observation.exit == 0 and not observation.stderr
        assert observation.stdout.decode() == payloads[case.path].body.decode()[:case.char_limit]
    elif expectation == "option_error":
        failure(observation, {case.kind}, bounded=not old)
        assert not observation.requests
        if old:
            assert len(observation.stderr) > STDERR_BUDGET, "old unbounded input diagnostic was not observed"
    elif expectation in ("spawn", "missing"):
        failure(observation, {"browser" if old else case.kind})
        expected_profiles = 1 if old and expectation == "spawn" else 0
        assert observation.profiles == expected_profiles, "browser startup profile ownership was not observed"
    elif expectation.startswith("cdp_"):
        assert "Page.navigate" in observation.cdp_methods, "real CLI never reached injected CDP failure"
        if expectation == "cdp_capped" and old:
            assert observation.timed_out and len(observation.stderr) == STDERR_BUDGET
            assert not observation.stdout
        else:
            failure(observation, {"browser" if old else case.kind}, bounded=not old)
            if expectation == "cdp_large" and old:
                assert len(observation.stderr) > STDERR_BUDGET
    else:
        raise AssertionError("unimplemented test expectation")
    if not old and observation.stderr:
        assert one_json(observation.stderr)["error"]["kind"] in CANONICAL_KINDS


class Runner:
    def __init__(self, cli, work, http, cdp):
        self.cli, self.work, self.http, self.cdp = cli, work, http, cdp
        self.base = f"http://127.0.0.1:{http.server_port}"
        self.endpoint = None

    def observe(self, case):
        with tempfile.TemporaryDirectory(prefix="case-", dir=self.work) as directory:
            root = Path(directory)
            env = clean_environment(root)
            missing = root / "missing-chrome"
            flags = list(case.flags)
            if case.expectation == "spawn":
                missing.write_bytes(b"synthetic nonexecutable Chrome fixture")
            if case.expectation.startswith("cdp_"):
                suffix = "small" if case.expectation == "cdp_small" else "large"
                flags += ["--cdp-url", f"ws://127.0.0.1:{self.cdp.server_address[1]}/devtools/browser/{suffix}"]
            if requires_chrome(case):
                assert self.endpoint is not None, "real Chrome has not been started"
                flags += ["--cdp-url", self.endpoint]
            args = [str(self.cli), case.command, self.base + case.path, "--direct", "--timeout", "4", "--retries", "0", "--chrome-path", str(missing)]
            if "--format" not in flags:
                args += ["--format", "json"]
            args += flags
            before = len(self.http.seen)
            methods_before = len(self.cdp.methods)
            process = subprocess.Popen(args, cwd=root, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
            timed_out = False
            try:
                if case.expectation == "cdp_capped":
                    stdout, stderr, timed_out = capped_communicate(process, 6)
                    if timed_out:
                        stop_process(process)
                    process.wait(timeout=5)
                else:
                    try:
                        stdout, stderr = process.communicate(timeout=20)
                    except subprocess.TimeoutExpired:
                        timed_out = True
                        stop_process(process)
                        stdout, stderr = process.communicate(timeout=5)
                return Observation(args, process.returncode, stdout, stderr, timed_out,
                                   len(list((root / "tmpdir").glob("rustscrapling-cdp-*"))),
                                   self.http.seen[before:], self.cdp.methods[methods_before:])
            finally:
                stop_process(process)
                process.stdout.close()
                process.stderr.close()


@contextmanager
def real_chrome(chrome, work, no_sandbox, evidence, loopback_only=False):
    with tempfile.TemporaryDirectory(prefix="rustscrapling-u3-chrome-", dir=work) as directory:
        root = Path(directory)
        env = clean_environment(root)
        profile = root / "profile"
        evidence["chrome_owned_temporary_directory"] = str(root)
        args = [str(chrome), "--enable-logging=stderr", "--headless=new", f"--user-data-dir={profile}", "--remote-debugging-port=0",
                "--remote-debugging-address=127.0.0.1", "--no-first-run", "--no-default-browser-check",
                "--no-proxy-server", "--disable-background-networking", "--disable-component-update",
                "--disable-sync", "--disable-dev-shm-usage", "--password-store=basic", "about:blank"]
        if no_sandbox:
            args.append("--no-sandbox")
        if loopback_only:
            args.append("--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE 127.0.0.1")
        evidence["chrome_argv"] = args
        diagnostic = bytearray()
        diagnostic_bytes = 0
        process = subprocess.Popen(args, cwd=work, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, start_new_session=True)

        def drain_diagnostic():
            nonlocal diagnostic_bytes
            while part := process.stderr.read(4096):
                diagnostic_bytes += len(part)
                diagnostic.extend(part[:max(0, STDERR_BUDGET - len(diagnostic))])

        reader = threading.Thread(target=drain_diagnostic, daemon=True)
        reader.start()
        try:
            active_port = profile / "DevToolsActivePort"
            deadline = time.monotonic() + 20
            while not active_port.is_file():
                assert process.poll() is None, "Chrome exited before exposing CDP"
                assert time.monotonic() < deadline, "Chrome did not expose a debugging endpoint"
                time.sleep(0.05)
            port, browser_path = active_port.read_text().splitlines()[:2]
            assert port.isdigit() and browser_path.startswith("/devtools/browser/")
            yield f"ws://127.0.0.1:{port}{browser_path}"
            assert process.poll() is None, "attached CLI terminated the owned fixture Chrome"
            evidence["attached_chrome_survived_cli"] = True
        finally:
            evidence["chrome_exit_before_cleanup"] = process.poll()
            stop_process(process)
            reader.join(timeout=5)
            assert not reader.is_alive(), "Chrome diagnostic pipe did not close"
            process.stderr.close()
            evidence["chrome_diagnostic_bytes_seen"] = diagnostic_bytes
            evidence["chrome_diagnostic_bytes_retained"] = len(diagnostic)
            evidence["chrome_diagnostic_sha256"] = hashlib.sha256(diagnostic).hexdigest()
            if not evidence.get("attached_chrome_survived_cli"):
                evidence["chrome_startup_diagnostic"] = diagnostic.decode("utf-8", errors="replace")
            evidence["chrome_process_reaped"] = process.poll() is not None
    evidence["chrome_fixture_profile_removed"] = not root.exists()
    assert evidence["chrome_fixture_profile_removed"], "owned fixture profile was not removed"


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def interrupted(_signum, _frame):
    raise InterruptedError("test process received termination")


def main():
    signal.signal(signal.SIGTERM, interrupted)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", type=Path, required=True)
    parser.add_argument("--chrome", type=Path)
    parser.add_argument("--part", choices=("all", "http", "browser"), default="all")
    parser.add_argument("--chrome-loopback-only", action="store_true")
    parser.add_argument("--work-dir", type=Path, required=True)
    parser.add_argument("--receipt", type=Path, required=True)
    parser.add_argument("--mode", choices=("baseline", "verify"), default="verify")
    parser.add_argument("--case", action="append", default=[])
    parser.add_argument("--chrome-no-sandbox", action="store_true")
    args = parser.parse_args()
    catalog = case_table()
    selected = [case for case in catalog
                if (not args.case or case.name in args.case)
                and (args.part == "all" or requires_chrome(case) == (args.part == "browser"))]
    receipt = {"schema_version": 1, "mode": args.mode, "part": args.part, "production": False,
               "result": "not_run", "cases": [], "artifact_hashes": {}, "full_acceptance": False,
               "total_cli_cases": len(catalog), "selected_case_ids": [case.name for case in selected]}
    try:
        assert os.name == "posix", "this fixture uses POSIX process groups"
        for path in (args.cli, args.work_dir, args.receipt):
            assert path.is_absolute(), "all paths must be absolute"
        assert args.cli.is_file(), "required CLI executable is missing"
        receipt["artifact_hashes"][str(args.cli)] = digest(args.cli)
        receipt["artifact_hashes"][str(Path(__file__).resolve())] = digest(Path(__file__))
        args.work_dir.mkdir(parents=True, exist_ok=True)
        assert selected and (not args.case or set(args.case) == {case.name for case in selected}), "unknown or conflicting case selector"
        with fixture_servers() as (http, cdp):
            runner = Runner(args.cli, args.work_dir, http, cdp)
            regular = [case for case in selected if not requires_chrome(case)]
            browser = [case for case in selected if requires_chrome(case)]

            def run_case(case):
                row = {"name": case.name, "surface": "real_chrome_cli" if requires_chrome(case) else "real_cli",
                       "requires_chrome": requires_chrome(case), "passed": False, "status": "FAIL"}
                try:
                    observation = runner.observe(case)
                    row.update(observation.receipt())
                    assert_case(case, observation, args.mode, http.payloads)
                    row["passed"] = True
                    row["status"] = "PASS"
                except InterruptedError:
                    raise
                except Exception as error:
                    row["failure"] = type(error).__name__ + ": " + str(error)[:300]
                receipt["cases"].append(row)
                print(json.dumps({"case": case.name, "passed": row["passed"]}), flush=True)

            for case in regular:
                run_case(case)
            if browser:
                if args.chrome is None or not args.chrome.is_absolute() or not args.chrome.is_file():
                    receipt["cases"].extend({"name": case.name, "surface": "real_chrome_cli",
                                             "requires_chrome": True, "passed": False, "status": "BLOCKED",
                                             "reason": "A qualified Chrome executable was not supplied"} for case in browser)
                else:
                    receipt["artifact_hashes"][str(args.chrome)] = digest(args.chrome)
                    with real_chrome(args.chrome, args.work_dir, args.chrome_no_sandbox, receipt,
                                     args.chrome_loopback_only) as endpoint:
                        runner.endpoint = endpoint
                        for case in browser:
                            run_case(case)
            assert not cdp.errors, "CDP injection fixture failed"
        assert len(receipt["cases"]) == len(selected), "some required cases were not executed"
        if any(row["status"] == "BLOCKED" for row in receipt["cases"]):
            receipt["result"] = "blocked"
            return 1
        assert all(row["passed"] for row in receipt["cases"]), "one or more case assertions failed"
        receipt["result"] = "expected_old_defects_observed" if args.mode == "baseline" else "verified"
        return 0
    except Exception as error:
        receipt["result"] = "failed"
        receipt["failure"] = type(error).__name__ + ": " + str(error)[:300]
        return 1
    finally:
        by_name = {row["name"]: row for row in receipt["cases"]}
        receipt["case_statuses"] = [{"name": case.name, "requires_chrome": requires_chrome(case),
                                    "status": by_name.get(case.name, {}).get("status", "NOT_RUN")} for case in catalog]
        receipt["required_cases"] = len(selected)
        receipt["passed_cases"] = sum(row["status"] == "PASS" for row in receipt["cases"])
        receipt["failed_cases"] = sum(row["status"] == "FAIL" for row in receipt["cases"])
        receipt["blocked_cases"] = sum(row["status"] == "BLOCKED" for row in receipt["cases"])
        receipt["covered_count"] = receipt["passed_cases"] + receipt["failed_cases"]
        receipt["full_cli_acceptance"] = args.mode == "verify" and len(catalog) == 77 and receipt["passed_cases"] == 77
        args.receipt.parent.mkdir(parents=True, exist_ok=True)
        args.receipt.write_text(json.dumps(receipt, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    raise SystemExit(main())
