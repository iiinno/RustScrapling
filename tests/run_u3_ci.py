"""Record Rust checks and the preserved non-Chrome U3 cases on GitHub-hosted CI."""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import selectors
import signal
import subprocess
import sys
import time


CALLER_TARGETS = (
    "daemon_commands", "local_e2e", "u3_errors", "u3_content", "stdio",
    "mock_cdp", "network_cdp", "http_local", "http3_local", "tls_local",
    "actions_mock", "navigation_timeout", "network_bodies_mock",
)
OPTIONAL_CHROME = {
    "local_browser_navigation_reuse_external_connection_and_shadow_dom",
    "browser_get_and_session_if_chrome_is_available",
}
LOG_LIMIT = 4 * 1024 * 1024
PUBLIC_LOG_LIMIT = 64 * 1024
PUBLIC_JSON_LIMIT = 4 * 1024 * 1024
DIAGNOSTIC_REDACTIONS = (
    (r"(?is)-----BEGIN [^-\r\n]*PRIVATE KEY-----.*?(?:-----END [^-\r\n]*PRIVATE KEY-----|$)", "[REDACTED PRIVATE KEY]"),
    (r"(?im)^(\s*(?:left|right|actual|expected)\s*:\s*).*$", r"\1[REDACTED VALUE]"),
    (r'''(?i)(\b(?:authorization|proxy-authorization|cookie|set-cookie|x-api-key|x-auth-token|[\w-]*token|[\w-]*credential[s]?|[\w-]*secret|pass(?:word|wd)|pwd|api[_-]?key)\b[\s"'\\]*[:=])[^\r\n]*''', r"\1[REDACTED]"),
    (r"(?i)\b(?:Bearer|Basic)\s+[A-Za-z0-9._~+/=-]+", "[REDACTED AUTHORIZATION]"),
    (r'''(?i)\b((?:https?|socks5h?|wss?)://)[^/\s"'<>]*@''', r"\1[REDACTED]@"),
    (r'''([?&][A-Za-z0-9_.%~-]+=)[^&\s"'<>\\]*''', r"\1[REDACTED]"),
    (r"""(?<!\\)(?:"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*')""", "[REDACTED STRING]"),
    (r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b", "[REDACTED ACCESS KEY]"),
    (r"(?<![A-Za-z0-9])[A-Za-z0-9+/_=-]{24,}(?![A-Za-z0-9])", "[REDACTED OPAQUE VALUE]"),
)
CASE_FIELDS = (
    "name", "surface", "requires_chrome", "passed", "status", "exit", "outer_timeout",
    "stdout_bytes", "stderr_bytes", "stdout_sha256", "stderr_sha256", "profiles_after_exit",
    "schema_version", "source_status", "error_status", "truncated", "markdown_chars",
    "replacement_chars", "json_parseable", "failure", "target", "libtest_result", "reason",
)


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def public_diagnostic(text):
    text = re.sub(*DIAGNOSTIC_REDACTIONS[0], text)
    lines = []
    for line in text.splitlines():
        if len(line) > 8192:
            lines.append("[OMITTED OVERSIZE DIAGNOSTIC LINE]")
            continue
        loopback_error = line.startswith("owned loopback browser_fetch ")
        for pattern, replacement in DIAGNOSTIC_REDACTIONS[1:]:
            if loopback_error and replacement == "[REDACTED STRING]":
                continue
            line = re.sub(pattern, replacement, line)
        lines.append(line[:512] if loopback_error else line)
    return "\n".join(lines)


def public_case(row):
    selected = {key: value for key, value in row.items() if key in CASE_FIELDS}
    text_fields = {"name", "surface", "status", "failure", "target", "libtest_result", "reason"}
    for key, value in selected.items():
        if key.endswith("_sha256"):
            require(isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value), "invalid case digest")
        elif key in text_fields:
            require(isinstance(value, str), "invalid case text")
        else:
            require(value is None or type(value) in (int, bool), "invalid case measurement")
    return {key: public_value(value, key) for key, value in selected.items()}


def public_value(value, key=""):
    if key == "cases":
        return [public_case(row) for row in value]
    if isinstance(value, dict):
        return {name: public_value(item, name) for name, item in value.items()}
    if isinstance(value, list):
        return [public_value(item, key) for item in value]
    if isinstance(value, str) and key in {"failure", "reason", "error", "libtest_result"}:
        return public_diagnostic(value)[:1000]
    if isinstance(value, str) and key in {"name", "target"}:
        return re.sub(r"\b[0-9a-fA-F]{32,}\b", "[REDACTED HEX VALUE]", value)
    return value


def save_public(output, name, value):
    path = output / "public" / name
    temporary = path.with_suffix(".tmp")
    encoded = (json.dumps(public_value(value), ensure_ascii=False, indent=2) + "\n").encode("utf-8")
    require(len(encoded) <= PUBLIC_JSON_LIMIT, "public JSON evidence exceeds its size limit")
    temporary.write_bytes(encoded)
    temporary.replace(path)


def save(output, receipt):
    save_public(output, "ci-receipt.json", receipt)


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args], text=True).strip()


def source_identity(root):
    files = sorted(filter(None, git(root, "ls-files", "-z").split("\0")))
    rows = [{"path": name, "sha256": digest(root / name)} for name in files]
    encoded = json.dumps(rows, sort_keys=True, separators=(",", ":")).encode()
    return {"checkout_sha": git(root, "rev-parse", "HEAD"),
            "tracked_files_sha256": hashlib.sha256(encoded).hexdigest(), "files": rows}


def hosted_environment(root):
    require(sys.platform == "linux" and "microsoft" not in platform.release().lower(), "GitHub-hosted Linux is required")
    require(os.environ.get("GITHUB_ACTIONS") == "true"
            and os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted", "GitHub-hosted runner is required")
    require(os.environ.get("GITHUB_SERVER_URL") == "https://github.com"
            and os.environ.get("GITHUB_REPOSITORY") == "iiinno/RustScrapling", "unexpected repository")
    require(root == Path(os.environ["GITHUB_WORKSPACE"]).resolve(), "unexpected checkout location")
    require(not git(root, "status", "--porcelain", "--untracked-files=no"), "tracked checkout is dirty")
    require(not any(key.startswith("CARGO_PROFILE_") or key in {
        "CARGO_BUILD_JOBS", "CARGO_TARGET_DIR", "CARGO_INCREMENTAL", "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS", "RUSTC", "RUSTDOC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER",
        "RUSTSCRAPLING_U3_EXPECT_BASELINE",
    } for key in os.environ), "unexpected build or baseline override")
    event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
    event_name = os.environ["GITHUB_EVENT_NAME"]
    if event_name == "pull_request":
        require(event["action"] in ("opened", "synchronize", "reopened"), "unexpected PR action")
        expected = event["pull_request"]["head"]["sha"]
        base = event["pull_request"]["base"]["sha"]
        number = event["number"]
    else:
        require(event_name == "push" and os.environ["GITHUB_REF"] == "refs/heads/main", "expected PR or main push")
        expected, base, number = os.environ["GITHUB_SHA"], None, None
    require(re.fullmatch(r"[0-9a-f]{40}", expected) is not None, "invalid expected SHA")
    require(git(root, "rev-parse", "HEAD") == expected, "checkout differs from event head")
    os_release = dict(line.split("=", 1) for line in Path("/etc/os-release").read_text().splitlines() if "=" in line)
    require(os_release.get("ID", "").strip('"') == "ubuntu"
            and os_release.get("VERSION_ID", "").strip('"') == "24.04", "Ubuntu 24.04 is required")
    memory = dict(line.split(":", 1) for line in Path("/proc/meminfo").read_text().splitlines())
    ram = int(memory["MemTotal"].split()[0]) * 1024
    require(ram >= 8 * 1024**3, "host RAM is below the 8 GiB CI prerequisite")
    group = Path("/sys/fs/cgroup") / Path("/proc/self/cgroup").read_text().strip().removeprefix("0::").lstrip("/")
    limits = {name: (group / name).read_text().strip() for name in ("cpu.max", "memory.max", "memory.swap.max") if (group / name).is_file()}
    require(limits.get("memory.max", "max") == "max" or int(limits["memory.max"]) >= 8 * 1024**3,
            "cgroup RAM is below the 8 GiB CI prerequisite")
    cpu = next((line.split(":", 1)[1].strip() for line in Path("/proc/cpuinfo").read_text().splitlines()
                if line.startswith("model name")), "unknown")
    return {"repository": "iiinno/RustScrapling", "event": event_name, "pr_number": number,
            "expected_head_sha": expected, "event_sha": os.environ["GITHUB_SHA"], "base_sha": base,
            "run_id": os.environ["GITHUB_RUN_ID"], "run_attempt": os.environ["GITHUB_RUN_ATTEMPT"],
            "run_url": "https://github.com/iiinno/RustScrapling/actions/runs/" + os.environ["GITHUB_RUN_ID"],
            "runner_environment": "github-hosted", "runner_os": os.environ["RUNNER_OS"],
            "image_os": os.environ.get("ImageOS"), "image_version": os.environ.get("ImageVersion"),
            "os_release": os_release, "kernel": platform.release(), "architecture": platform.machine(),
            "cpu_model": cpu, "cpu_count": os.cpu_count(), "cpu_affinity_count": len(os.sched_getaffinity(0)),
            "ram_bytes": ram, "available_ram_bytes": int(memory["MemAvailable"].split()[0]) * 1024,
            "cgroup_limits": limits, "python": platform.python_version()}


def command_plan(root, output):
    cargo = ["cargo", "+stable"]
    test_output = ["--", "--format", "pretty", "--test-threads=1"]
    caller_flags = [flag for name in CALLER_TARGETS for flag in ("--test", name)]
    binary = root / "target/debug/rustscrapling"
    specs = [
        ("fmt", cargo + ["fmt", "--all", "--", "--check"], 180, "check"),
        ("workspace", cargo + ["test", "--workspace", "--lib", "--tests", "--locked", "--no-fail-fast"] + test_output, 2400, "rust-tests"),
        ("clippy", cargo + ["clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings"], 2400, "check"),
        ("fetcher", cargo + ["test", "-p", "rustscrapling-cli", "-p", "rustscrapling-fetch", "--test", "content_acceptance", "--locked"] + test_output, 600, "rust-tests"),
        ("callers", cargo + ["test", "--workspace", *caller_flags, "--locked", "--no-fail-fast"] + test_output, 600, "rust-tests"),
        ("mcp-http", cargo + ["test", "-p", "rustscrapling-mcp", "--test", "runtime", "--locked", *test_output, "--exact", "real_http_and_http_session_round_trip"], 180, "rust-tests"),
        ("mcp-no-session", cargo + ["test", "-p", "rustscrapling-mcp", "--test", "runtime", "--locked", *test_output, "--exact", "browser_session_operations_require_a_real_session"], 180, "rust-tests"),
        ("build-cli", cargo + ["build", "-p", "rustscrapling-cli", "--locked"], 2400, "build"),
        ("u3-cli", [sys.executable, "-I", "-B", str(root / "tests/u3_fetch_contract.py"), "--cli", str(binary),
                    "--work-dir", str(output / "private/u3"), "--receipt", str(output / "private/u3-cli.json"), "--mode", "verify", "--part", "http"], 1200, "cli-tests"),
        ("localhost-e2e", [sys.executable, "-I", "-B", str(root / "tests/e2e_local.py"), str(binary)], 600, "python-tests"),
    ]
    return [{"name": name, "argv": argv, "timeout_seconds": timeout, "kind": kind,
             "status": "NOT_RUN", "exit_code": None, "cases": []} for name, argv, timeout, kind in specs]


def rust_results(text):
    rows, summaries = [], []
    target = "unknown"
    for line in text.splitlines():
        if match := re.search(r"Running (.+?) \((.+)\)", line):
            target = match[1]
        if match := re.fullmatch(r"test (\S+) \.\.\. (ok|FAILED|ignored.*)", line):
            name, result = match.groups()
            status = "PASS" if result == "ok" else "FAIL" if result == "FAILED" else "NOT_RUN"
            row = {"target": target, "name": name, "status": status, "libtest_result": result}
            if name in OPTIONAL_CHROME and status == "PASS":
                row.update(status="NOT_ACCEPTED", reason="optional Chrome test can return without executing Chrome")
            rows.append(row)
        if match := re.search(r"test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out", line):
            summaries.append({"target": target, **dict(zip(("passed", "failed", "ignored", "measured", "filtered_out"), map(int, match.groups())))})
    return rows, summaries


def run_command(root, output, row, receipt, deadline):
    remaining = min(row["timeout_seconds"], deadline - time.monotonic())
    require(remaining > 10, "CI driver deadline exhausted")
    row.update(status="RUNNING", started_at=datetime.now(timezone.utc).isoformat())
    save(output, receipt)
    retained, seen, process = bytearray(), 0, None
    env = dict(os.environ, CARGO_TERM_COLOR="never", RUSTSCRAPLING_DAEMON_STATE=str(output / "absent-daemon.json"))
    try:
        process = subprocess.Popen(row["argv"], cwd=root, env=env, stdin=subprocess.DEVNULL,
                                   stdout=subprocess.PIPE, stderr=subprocess.STDOUT, start_new_session=True)
        os.set_blocking(process.stdout.fileno(), False)
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            expires = time.monotonic() + remaining
            while selector.get_map() or process.poll() is None:
                if time.monotonic() >= expires:
                    raise TimeoutError("command deadline exceeded")
                for key, _ in selector.select(0.1):
                    part = os.read(key.fileobj.fileno(), 65536)
                    if not part:
                        selector.unregister(key.fileobj)
                    seen += len(part)
                    retained.extend(part)
                    if len(retained) > LOG_LIMIT:
                        del retained[:-LOG_LIMIT]
        row["exit_code"] = process.wait(timeout=5)
        row["status"] = "PASS" if row["exit_code"] == 0 else "FAIL"
    except InterruptedError:
        row.update(status="INTERRUPTED", error="InterruptedError")
        raise
    except (OSError, TimeoutError) as error:
        row.update(status="TIMEOUT" if isinstance(error, TimeoutError) else "BLOCKED", error=type(error).__name__)
    finally:
        if process is not None:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait(timeout=10)
            process.stdout.close()
            row["exit_code"] = process.returncode
        text = retained.decode("utf-8", errors="replace")
        diagnostic = text.partition("\n")[2] if seen > LOG_LIMIT else text
        redacted = public_diagnostic(diagnostic).encode("utf-8")
        log = output / "public" / (row["name"] + ".log")
        log.write_bytes(redacted[-PUBLIC_LOG_LIMIT:].decode("utf-8", errors="ignore").encode("utf-8"))
        row.update(log=log.name, log_sha256=digest(log), output_bytes=seen,
                   log_truncated=seen > LOG_LIMIT, public_log_bytes=log.stat().st_size,
                   public_log_truncated=len(redacted) > PUBLIC_LOG_LIMIT, diagnostics_redacted=True,
                   finished_at=datetime.now(timezone.utc).isoformat())
        if row["log_truncated"]:
            row["status"] = "INCOMPLETE_EVIDENCE"
        if row["kind"] == "rust-tests":
            row["cases"], row["libtest_summaries"] = rust_results(text)
            counted = sum(item["passed"] + item["failed"] + item["ignored"] for item in row["libtest_summaries"])
            if row["status"] == "PASS" and (not row["libtest_summaries"] or counted != len(row["cases"]) or not row["cases"]):
                row.update(status="INCOMPLETE_EVIDENCE", reason="libtest case rows do not match summaries")
        if row["kind"] == "python-tests":
            row["cases"] = [{"name": name, "status": "PASS" if status == "ok" else "FAIL" if status in ("FAIL", "ERROR") else "NOT_RUN"}
                            for name, status in re.findall(r"^(test_\S+) \([^\n]+\) \.\.\. (ok|FAIL|ERROR|skipped[^\n]*)$", text, re.MULTILINE)]
            if row["status"] == "PASS" and (len(row["cases"]) != 7 or any(case["status"] != "PASS" for case in row["cases"])):
                row.update(status="INCOMPLETE_EVIDENCE", reason="all seven localhost E2E methods must execute")
        save(output, receipt)
        print(json.dumps({"command": row["name"], "status": row["status"], "exit_code": row["exit_code"]}), flush=True)


def update_results(output, receipt):
    by_name = {row["name"]: row for row in receipt["cases"]}
    commands = {row["name"]: row for row in receipt["commands"]}
    fetcher = commands["fetcher"]
    expected_fetcher = {name for name, row in by_name.items() if row["surface"] == "public_fetcher_real_http"}
    actual_fetcher = {row["name"] for row in fetcher["cases"]}
    if actual_fetcher - expected_fetcher or (fetcher["status"] == "PASS"
                                             and (actual_fetcher != expected_fetcher or len(fetcher["cases"]) != 9)):
        fetcher.update(status="INCOMPLETE_EVIDENCE", reason="Fetcher case identities do not match the nine-case catalog")
    for case in fetcher["cases"]:
        if case["name"] in expected_fetcher:
            by_name[case["name"]]["status"] = case["status"]
    child_path = output / "private/u3-cli.json"
    if child_path.is_file():
        require(child_path.stat().st_size <= PUBLIC_JSON_LIMIT, "internal CLI receipt exceeds its size limit")
        child = json.loads(child_path.read_text())
        expected_cli = {name for name, row in by_name.items() if row["surface"] == "real_cli"}
        observed = child.get("cases", [])
        identities = [row["name"] for row in observed]
        identity_ok = (len(identities) == len(set(identities)) and set(identities) <= expected_cli
                       and child.get("mode") == "verify" and child.get("part") == "http"
                       and child.get("selected_case_ids") is not None
                       and set(child["selected_case_ids"]) == expected_cli
                       and child.get("required_cases") == 74
                       and child.get("artifact_hashes", {}).get(receipt.get("binary", {}).get("path")) == receipt.get("binary", {}).get("sha256"))
        if identity_ok:
            commands["u3-cli"]["cases"] = observed
            for case in observed:
                if case.get("surface") == by_name[case["name"]]["surface"] and case.get("status") in ("PASS", "FAIL", "BLOCKED", "NOT_RUN"):
                    by_name[case["name"]]["status"] = case["status"]
                else:
                    identity_ok = False
        if not identity_ok:
            commands["u3-cli"].update(status="INCOMPLETE_EVIDENCE", reason="CLI receipt identity or binary hash differs")
        public_child = {
            "schema_version": 1, "mode": "verify", "part": "http",
            "result": commands["u3-cli"]["status"], "identity_verified": identity_ok,
            "required_cases": 74, "cases": observed if identity_ok else [],
            "internal_receipt_sha256": digest(child_path),
            "binary_sha256": receipt.get("binary", {}).get("sha256"),
            "full_cli_acceptance": False, "full_acceptance": False, "production": False,
        }
        save_public(output, "u3-cli.json", public_child)
        public_path = output / "public/u3-cli.json"
        receipt["cli_receipt"] = {"path": public_path.name, "sha256": digest(public_path)}
    elif commands["u3-cli"]["status"] == "PASS":
        commands["u3-cli"].update(status="INCOMPLETE_EVIDENCE", reason="CLI receipt missing")
    for command in receipt["commands"]:
        command["observed_case_counts"] = {status: sum(row["status"] == status for row in command["cases"])
                                           for status in ("PASS", "FAIL", "BLOCKED", "NOT_RUN", "NOT_ACCEPTED")}
        command["expected_case_count"] = {"fetcher": 9, "u3-cli": 74, "localhost-e2e": 7,
                                          "mcp-http": 1, "mcp-no-session": 1}.get(command["name"])
    receipt["case_counts"] = {status: sum(row["status"] == status for row in receipt["cases"])
                              for status in ("PASS", "FAIL", "BLOCKED", "NOT_RUN")}
    selected = [row for row in receipt["cases"] if not row["requires_chrome"]]
    receipt["non_chrome_83_pass"] = (len(selected) == 83 and all(row["status"] == "PASS" for row in selected)
                                    and fetcher["status"] == commands["u3-cli"]["status"] == "PASS")
    caller_rows = commands["callers"]["cases"]
    target_names = {Path(row["target"]).stem for row in caller_rows}
    receipt["callers_pass"] = (set(CALLER_TARGETS) == target_names and bool(caller_rows)
                               and all(row["status"] == "PASS" for row in caller_rows)
                               and all(commands[name]["status"] == "PASS" for name in ("callers", "mcp-http", "mcp-no-session"))
                               and all(len(commands[name]["cases"]) == 1 and commands[name]["cases"][0]["status"] == "PASS"
                                       for name in ("mcp-http", "mcp-no-session")))


def interrupted(_signum, _frame):
    raise InterruptedError("CI driver interrupted")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--prepare", action="store_true")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    output = args.output.resolve()
    os.umask(0o077)
    output.mkdir(parents=True, exist_ok=True, mode=0o700)
    require(output.is_relative_to(Path(os.environ["RUNNER_TEMP"]).resolve()), "evidence must stay in runner temporary storage")
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    if args.prepare:
        require(not any(output.iterdir()), "CI preparation requires a new empty evidence directory")
        (output / "private").mkdir(mode=0o700)
        (output / "public").mkdir(mode=0o700)
        receipt = {"schema_version": 1, "result": "NOT_RUN", "suite": "github-hosted-u3-non-chrome",
                   "started_at": datetime.now(timezone.utc).isoformat(), "commands": command_plan(root, output),
                   "cases": [], "non_chrome_83_pass": False, "callers_pass": False,
                   "full_86_pass": False, "normal_chrome_accepted": False, "ga_pass": False, "production": False}
    else:
        receipt = json.loads((output / "public/ci-receipt.json").read_text())
        require(receipt["result"] == "PREPARED", "successful fresh preparation is required")
    try:
        environment = hosted_environment(root)
        source = source_identity(root)
        if args.prepare:
            require(not (root / "target").exists(), "fresh checkout must not contain an old target directory")
            catalog = json.loads((root / "tests/u3_case_manifest.json").read_text())["cases"]
            require(len(catalog) == 86 and len({row["name"] for row in catalog}) == 86, "preserved 86-case catalog differs")
            require(sum(row["surface"] == "public_fetcher_real_http" for row in catalog) == 9
                    and sum(row["requires_chrome"] for row in catalog) == 3, "preserved U3 case routes differ")
            receipt.update(environment=environment, source_before=source, result="PREPARED",
                           catalog_sha256=digest(root / "tests/u3_case_manifest.json"),
                           cases=[{**row, "status": "NOT_RUN"} for row in catalog],
                           formal_case_count=86, selected_case_count=83,
                           chrome_reason="U3 Chrome cases and normal Chrome acceptance require separate evidence")
            return 0
        require(receipt["source_before"] == source and receipt["environment"]["expected_head_sha"] == environment["expected_head_sha"], "prepared source changed")
        receipt["result"] = "RUNNING"
        receipt["toolchain"] = {}
        for tool, version_args in (("cargo", ["--version"]), ("rustc", ["-vV"])):
            path = Path(subprocess.check_output(["rustup", "which", "--toolchain", "stable", tool], text=True).strip())
            receipt["toolchain"][tool] = {"path": str(path), "sha256": digest(path),
                                          "version_argv": [str(path), *version_args],
                                          "version": subprocess.check_output([str(path), *version_args], text=True).strip()}
        deadline = time.monotonic() + 75 * 60
        for row in receipt["commands"]:
            if row["kind"] in ("cli-tests", "python-tests") and "binary" not in receipt:
                row.update(status="NOT_RUN", reason="fresh CLI build did not succeed")
                continue
            run_command(root, output, row, receipt, deadline)
            if row["name"] == "build-cli" and row["status"] == "PASS":
                binary = root / "target/debug/rustscrapling"
                require(binary.is_file(), "successful build produced no CLI")
                receipt["binary"] = {"path": str(binary), "sha256": digest(binary), "checkout_sha": source["checkout_sha"]}
            update_results(output, receipt)
            save(output, receipt)
        receipt["source_after"] = source_identity(root)
        require(receipt["source_before"] == receipt["source_after"], "tracked source changed during CI")
        if "binary" in receipt:
            require(digest(Path(receipt["binary"]["path"])) == receipt["binary"]["sha256"], "tested binary changed")
        passed = (all(row["status"] == "PASS" for row in receipt["commands"])
                  and receipt["non_chrome_83_pass"] and receipt["callers_pass"])
        receipt["result"] = "PASS" if passed else "FAIL"
        return 0 if passed else 1
    except Exception as error:
        receipt.update(result="BLOCKED" if args.prepare else "FAIL", failure=type(error).__name__ + ": " + str(error)[:500])
        return 1
    finally:
        try:
            if receipt["cases"]:
                update_results(output, receipt)
        except (OSError, ValueError, KeyError, TypeError) as error:
            receipt.update(result="FAIL", evidence_error=type(error).__name__)
            save(output, receipt)
            raise
        receipt["updated_at"] = datetime.now(timezone.utc).isoformat()
        save(output, receipt)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as error:
        print(json.dumps({"status": "FAIL", "error": type(error).__name__,
                          "diagnostic": public_diagnostic(str(error))[:1000]}), file=sys.stderr)
        raise SystemExit(1) from None
