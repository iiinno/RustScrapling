"""Strict, dependency-free tests for the pinned live MCP schema Oracle."""

from __future__ import annotations

import contextlib
import hashlib
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from subprocess import CompletedProcess
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "oracle"))

from runtime_schema_oracle import (  # noqa: E402
    UPSTREAM_SHA,
    checked_upstream,
    collect_runtime,
    compare_tools,
    json_differences,
    load_json,
    main,
    named_tools,
    pointer,
)
from inventory_upstream import verify_source_tree  # noqa: E402


def tool(name: str, schema: dict | None = None, **fields: object) -> dict:
    return {"name": name, "inputSchema": schema or {"type": "object", "properties": {}}, **fields}


class JsonComparisonTests(unittest.TestCase):
    def test_ignores_only_object_key_order(self) -> None:
        source = {"properties": {"a": {"description": "A", "default": None}, "b": {"type": "number"}}}
        other = {"properties": {"b": {"type": "number"}, "a": {"default": None, "description": "A"}}}
        self.assertEqual(json_differences(source, other), [])

    def test_distinguishes_missing_default_from_explicit_null(self) -> None:
        differences = json_differences({"default": None}, {})
        self.assertEqual(differences, [{"path": "/default", "reason": "missing_field", "upstream": None}])

    def test_reports_nested_json_schema_constraints(self) -> None:
        expected = {"properties": {"actions": {"items": {"oneOf": [
            {"properties": {"timeout": {"minimum": 0, "maximum": 200, "default": 10}}, "required": ["type", "timeout"]}
        ]}}}}
        actual = {"properties": {"actions": {"items": {"oneOf": [
            {"properties": {"timeout": {"minimum": -1, "maximum": 100, "default": 11}}, "required": ["timeout", "type"]}
        ]}}}}
        paths = {entry["path"] for entry in json_differences(expected, actual)}
        self.assertEqual(paths, {
            "/properties/actions/items/oneOf/0/properties/timeout/minimum",
            "/properties/actions/items/oneOf/0/properties/timeout/maximum",
            "/properties/actions/items/oneOf/0/properties/timeout/default",
            "/properties/actions/items/oneOf/0/required/0",
            "/properties/actions/items/oneOf/0/required/1",
        })

    def test_preserves_enum_and_array_order(self) -> None:
        differences = json_differences({"enum": ["a", "b"]}, {"enum": ["b", "a"]})
        self.assertEqual([d["path"] for d in differences], ["/enum/0", "/enum/1"])

    def test_distinguishes_boolean_and_integer(self) -> None:
        self.assertEqual(json_differences({"default": True}, {"default": 1})[0]["reason"], "type")

    def test_json_pointer_escaping(self) -> None:
        self.assertEqual(pointer("/properties", "a~/b"), "/properties/a~0~1b")

    def test_does_not_silently_ignore_defs_or_format_or_extensions(self) -> None:
        upstream = {"$defs": {"Filter": {"format": "uri"}}, "x-extra": 1}
        rust = {"$defs": {"Filter": {"format": "email"}}}
        self.assertEqual([d["path"] for d in json_differences(upstream, rust)],
                         ["/$defs/Filter/format", "/x-extra"])

    def test_different_array_length_not_padded_with_null(self) -> None:
        self.assertEqual(json_differences(["a", "b"], ["a"]),
                         [{"path": "/", "reason": "array_length", "upstream": 2, "rust": 1}])

    def test_duplicate_json_properties_and_nonfinite_numbers_rejected(self) -> None:
        for text in ('{"x":1,"x":2}', '{"x":{"a":1,"a":2}}', '{"x":NaN}', '{"x":Infinity}'):
            with self.subTest(text=text), self.assertRaises(ValueError):
                load_json(text)


class ToolComparisonTests(unittest.TestCase):
    def test_exact_match(self) -> None:
        self.assertEqual(compare_tools([tool("one", title="A", description="desc")],
                                       [tool("one", description="desc", title="A")])["status"], "match")

    def test_title_description_and_missing_output_schema(self) -> None:
        upstream = [tool("one", title="Title", description="Documented", outputSchema={"type": "object"})]
        rust = [tool("one", title="Other", description="Documentation")]
        report = compare_tools(upstream, rust)
        self.assertEqual(report["differing_tools"], 1)
        self.assertEqual({d["path"] for d in report["differences"]["one"]},
                         {"/title", "/description", "/outputSchema"})

    def test_missing_extra_tools_reported_without_claiming_parity(self) -> None:
        report = compare_tools([tool("a"), tool("b")], [tool("a"), tool("c")])
        self.assertEqual(report["status"], "mismatch")
        self.assertEqual(report["missing_in_rust"], ["b"])
        self.assertEqual(report["extra_in_rust"], ["c"])

    def test_rejects_duplicate_or_malformed_tool_catalog(self) -> None:
        for raw in ([tool("a"), tool("a")], [tool("")], [{"name": "a"}], {"name": "a"}):
            with self.subTest(raw=raw), self.assertRaises(ValueError):
                named_tools(raw, "source")

    def test_rust_catalog_is_parseable_with_unique_16_tools(self) -> None:
        raw = load_json((ROOT / "crates/rustscrapling-mcp/src/tools.json").read_text(encoding="utf-8"))
        self.assertEqual(len(named_tools(raw, "rust")), 16)

    def test_committed_live_report_preserves_complete_difference_counts(self) -> None:
        report = load_json((ROOT / "docs/compat/MCP_RUNTIME_SCHEMA_LIVE_V05.json").read_text(encoding="utf-8"))
        self.assertEqual(report["upstream_sha"], UPSTREAM_SHA)
        self.assertEqual(report["mcp_sdk_version"], "2.3.0")
        self.assertEqual(report["source_file"], "scrapling/__init__.py")
        # Since the Rust catalog adopted the live upstream tools/list verbatim,
        # the strict wire comparison is an exact match.
        self.assertEqual(report["status"], "match")
        self.assertEqual((report["upstream_count"], report["rust_count"]), (16, 16))
        self.assertEqual(report["missing_in_rust"], [])
        self.assertEqual(report["extra_in_rust"], [])
        self.assertEqual(report["differing_tools"], len(report["differences"]))
        self.assertEqual(report["difference_count"],
                         sum(len(changes) for changes in report["differences"].values()))
        self.assertEqual(report["difference_count"], 0)
        self.assertEqual(report["differences"], {})
        catalog = (ROOT / "crates/rustscrapling-mcp/src/tools.json").read_bytes()
        self.assertEqual(report["rust_catalog_sha256"], hashlib.sha256(catalog).hexdigest(),
                         "regenerate the live report after changing tools.json")

    def test_upstream_provenance_guard_rejects_missing_checkout(self) -> None:
        fake_checkout = ROOT / "tests"  # Exists but does not have upstream source
        with self.assertRaisesRegex(ValueError, "Not a Scrapling upstream checkout"):
            checked_upstream(fake_checkout)

    def test_upstream_provenance_guard_delegates_to_shared_verifier(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            upstream = Path(directory)
            server = upstream / "scrapling/core/ai/server.py"
            server.parent.mkdir(parents=True)
            server.touch()
            with patch("runtime_schema_oracle.verify_source_tree") as guard:
                checked_upstream(upstream)
            guard.assert_called_once_with(upstream)
            with patch("runtime_schema_oracle.verify_source_tree", side_effect=ValueError("Source SHA mismatch")):
                with self.assertRaisesRegex(ValueError, "Source SHA mismatch"):
                    checked_upstream(upstream)

    def test_second_probe_accepts_python_bytecode_but_rejects_python_source_changes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            upstream = Path(directory)
            server = upstream / "scrapling/core/ai/server.py"
            server.parent.mkdir(parents=True)
            server.write_text("# pinned fixture\n", encoding="utf-8")

            def git(*arguments: str) -> str:
                return subprocess.check_output(["git", "-C", str(upstream), *arguments], text=True).strip()

            git("init", "-q")
            git("config", "user.email", "tests@example.invalid")
            git("config", "user.name", "Runtime Schema Test")
            git("add", "--", "scrapling/core/ai/server.py")
            git("commit", "-qm", "fixture")
            sha = git("rev-parse", "HEAD")

            # The shared verifier is exercised with the fixture's SHA; the
            # real Oracle passes the pinned SHA via its default argument.
            def fixture_guard(source: Path) -> set[str]:
                return verify_source_tree(source, expected_sha=sha)

            with patch("runtime_schema_oracle.verify_source_tree", side_effect=fixture_guard):
                checked_upstream(upstream)
                bytecode = server.parent / "__pycache__/server.cpython-313.pyc"
                bytecode.parent.mkdir()
                bytecode.write_bytes(b"compiled bytecode")
                checked_upstream(upstream)  # repeated probe stays valid
                (server.parent / "new_tool.py").write_text("VALUE = 1\n", encoding="utf-8")
                with self.assertRaisesRegex(ValueError, "working-tree changes"):
                    checked_upstream(upstream)

    def test_missing_runtime_python_is_explicit_failure(self) -> None:
        with patch("runtime_schema_oracle.subprocess.run", side_effect=FileNotFoundError("missing-python")):
            with self.assertRaisesRegex(FileNotFoundError, "missing-python"):
                collect_runtime(ROOT, "missing-python", 2)

    def test_probe_keeps_user_site_packages_while_ignoring_python_env(self) -> None:
        with patch("runtime_schema_oracle.subprocess.run", return_value=CompletedProcess(
            [], 0, '{"sdk_version":"2.0.0","tools":[]}', "")) as fake:
            self.assertEqual(collect_runtime(ROOT, "chosen-python", 5)["sdk_version"], "2.0.0")
            args = fake.call_args.args[0]
            self.assertEqual(args[:3], ["chosen-python", "-E", "-c"])

    def test_cli_returns_diff_exit_code_and_writes_complete_report(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            report_path = Path(directory) / "report.json"
            rust_path = Path(directory) / "tools.json"
            rust_path.write_text(json.dumps([tool("a", description="Rust")]), encoding="utf-8")
            live = {"sdk_version": "2.0.0", "python_version": "3.13", "source_file": str(ROOT / "scrapling/core/ai/server.py"),
                    "tools": [tool("a", description="Upstream")]}
            with patch("runtime_schema_oracle.checked_upstream"), patch(
                "runtime_schema_oracle.collect_runtime", return_value=live), contextlib.redirect_stdout(io.StringIO()):
                exit_code = main(["--upstream", str(ROOT), "--rust", str(rust_path),
                                  "--report", str(report_path)])
            self.assertEqual(exit_code, 1)
            report = load_json(report_path.read_text(encoding="utf-8"))
            self.assertEqual(report["difference_count"], 1)
            self.assertEqual(report["differences"]["a"][0]["path"], "/description")
            self.assertEqual(report["source_file"], "scrapling/core/ai/server.py")
@unittest.skipUnless(os.environ.get("SCRAPLING_MCP_LIVE_ORACLE") == "1",
                     "Set SCRAPLING_MCP_LIVE_ORACLE=1 with upstream mcp>=2.0 dependencies to probe live tools/list")
class LiveProtocolProbeTests(unittest.TestCase):
    def test_fixed_upstream_returns_16_real_protocol_tools(self) -> None:
        upstream = ROOT.parent / "scrapling-v05-reference"
        checked_upstream(upstream)
        response = collect_runtime(upstream, sys.executable, 45)
        self.assertEqual(len(named_tools(response["tools"], "upstream")), 16)
        self.assertTrue(response["source_file"].startswith(str(upstream.resolve())))
        self.assertEqual(len(UPSTREAM_SHA), 40)

    def test_committed_live_report_reproduces_on_identical_sdk_and_catalog(self) -> None:
        upstream = ROOT.parent / "scrapling-v05-reference"
        checked_upstream(upstream)
        response = collect_runtime(upstream, sys.executable, 45)
        report = load_json((ROOT / "docs/compat/MCP_RUNTIME_SCHEMA_LIVE_V05.json").read_text(encoding="utf-8"))
        if response["sdk_version"] != report["mcp_sdk_version"]:
            self.skipTest("Runtime SDK version differs from captured snapshot")
        current_catalog_bytes = (ROOT / "crates/rustscrapling-mcp/src/tools.json").read_bytes()
        if hashlib.sha256(current_catalog_bytes).hexdigest() != report["rust_catalog_sha256"]:
            self.skipTest("Rust MCP catalog has changed since captured snapshot")
        self.assertEqual(compare_tools(response["tools"], load_json(current_catalog_bytes.decode("utf-8"))),
                         {key: report[key] for key in (
                             "status", "upstream_count", "rust_count", "missing_in_rust", "extra_in_rust",
                             "differing_tools", "difference_count", "differences")})


if __name__ == "__main__":
    unittest.main()
