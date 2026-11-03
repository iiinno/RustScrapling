#!/usr/bin/env python3
"""Compare pinned Scrapling's *live MCP tools/list* with Rust's tools.json.

This is a wire-schema/catalog audit, not an executor/runtime behavior test.
The comparison deliberately preserves JSON Schema details, including exact
descriptions, defaults, nullability, constraints, nested branches, and required
field declarations. Only JSON object key ordering is immaterial.

Usage:
  python oracle/runtime_schema_oracle.py --upstream ../scrapling-v05-reference
  python oracle/runtime_schema_oracle.py --upstream ../scrapling-v05-reference \
      --upstream-python /path/to/installed/python --report /tmp/mcp-schema.json
"""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
from pathlib import Path
from typing import Any

from inventory_upstream import UPSTREAM_SHA, verify_source_tree


ROOT = Path(__file__).resolve().parents[1]
FIELDS = ("title", "description", "inputSchema", "outputSchema", "annotations")
MISSING = object()

# Run against the actual source checkout in a separate interpreter. Importing
# modules through the oracle process would risk accidentally loading a different
# installed Scrapling version or contaminating subsequent unit tests.
RUNTIME_PROBE = r'''
import asyncio
from importlib.metadata import PackageNotFoundError, version
import json
from pathlib import Path
import re
import sys

upstream = Path(sys.argv[1]).resolve()
try:
    sdk_version = version("mcp")
except PackageNotFoundError as exc:
    raise RuntimeError("mcp SDK is missing; pinned Scrapling requires mcp>=2.0.0") from exc
match = re.match(r"^(\d+)\.", sdk_version)
if not match or int(match.group(1)) < 2:
    raise RuntimeError(f"mcp SDK {sdk_version} is incompatible; pinned Scrapling requires mcp>=2.0.0")
sys.path.insert(0, str(upstream))

import scrapling
actual = Path(scrapling.__file__).resolve()
if not actual.is_relative_to(upstream):
    raise RuntimeError(f"loaded Scrapling from {actual}, expected {upstream}")
from mcp.client import Client
from scrapling.core.ai import ScraplingMCPServer

async def collect():
    server = ScraplingMCPServer()._build_server("127.0.0.1", 8000)
    async with Client(server) as client:
        tools = (await client.list_tools()).tools
    result = []
    for tool in tools:
        raw = tool.model_dump(mode="json", by_alias=True, exclude_none=True)
        # Compare the fields corresponding to tool catalog entries. Transport-
        # specific MCP SDK metadata is outside this catalog audit's scope.
        result.append({name: raw[name] for name in
                       ("name", "title", "description", "inputSchema", "outputSchema", "annotations")
                       if name in raw})
    return result

print(json.dumps({"sdk_version": sdk_version,
                  "python_version": sys.version.split()[0],
                  "source_file": str(actual),
                  "tools": asyncio.run(collect())}, ensure_ascii=False))
'''


def reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    obj: dict[str, Any] = {}
    for key, value in pairs:
        if key in obj:
            raise ValueError(f"Duplicate JSON object key: {key!r}")
        obj[key] = value
    return obj


def load_json(text: str) -> Any:
    def reject_constant(value: str) -> None:
        raise ValueError(f"Invalid non-finite JSON number: {value}")

    return json.loads(text, object_pairs_hook=reject_duplicate_keys, parse_constant=reject_constant)


def named_tools(raw: Any, origin: str) -> dict[str, dict[str, Any]]:
    if not isinstance(raw, list):
        raise ValueError(f"{origin}: tool list must be a JSON array")
    result: dict[str, dict[str, Any]] = {}
    for index, tool in enumerate(raw):
        if not isinstance(tool, dict) or not isinstance(tool.get("name"), str) or not tool["name"]:
            raise ValueError(f"{origin}[{index}]: tool must have a nonempty string name")
        name = tool["name"]
        if name in result:
            raise ValueError(f"{origin}: duplicate tool name {name!r}")
        if "inputSchema" not in tool or not isinstance(tool["inputSchema"], dict):
            raise ValueError(f"{origin}.{name}: inputSchema must be an object")
        result[name] = tool
    return result


def pointer(path: str, key: str | int) -> str:
    return path + "/" + str(key).replace("~", "~0").replace("/", "~1")


def json_differences(expected: Any, actual: Any, path: str = "") -> list[dict[str, Any]]:
    """Strict structural JSON diff; array order and bool-vs-number are significant."""
    differences: list[dict[str, Any]] = []

    def visit(left: Any, right: Any, position: str) -> None:
        if left is MISSING and right is MISSING:
            return
        if left is MISSING or right is MISSING:
            entry: dict[str, Any] = {"path": position or "/", "reason": "missing_field"}
            if left is not MISSING:
                entry["upstream"] = left
            if right is not MISSING:
                entry["rust"] = right
            differences.append(entry)
            return
        if type(left) is not type(right):
            differences.append({"path": position or "/", "reason": "type", "upstream": left, "rust": right})
            return
        if isinstance(left, dict):
            for key in sorted(left.keys() | right.keys()):
                visit(left.get(key, MISSING), right.get(key, MISSING), pointer(position, key))
        elif isinstance(left, list):
            if len(left) != len(right):
                differences.append({"path": position or "/", "reason": "array_length", "upstream": len(left), "rust": len(right)})
            for index, (a, b) in enumerate(zip(left, right)):
                visit(a, b, pointer(position, index))
        elif left != right:
            differences.append({"path": position or "/", "reason": "value", "upstream": left, "rust": right})

    visit(expected, actual, path)
    return differences


def compare_tools(upstream: Any, rust: Any) -> dict[str, Any]:
    source = named_tools(upstream, "upstream")
    target = named_tools(rust, "rust")
    overlap = sorted(source.keys() & target.keys())
    differences: dict[str, list[dict[str, Any]]] = {}
    for name in overlap:
        left = source[name]
        right = target[name]
        changes: list[dict[str, Any]] = []
        for field in FIELDS:
            changes.extend(json_differences(left.get(field, MISSING), right.get(field, MISSING), pointer("", field)))
        if changes:
            differences[name] = changes
    return {
        "status": "match" if len(source) == len(target) == len(overlap) and not differences else "mismatch",
        "upstream_count": len(source),
        "rust_count": len(target),
        "missing_in_rust": sorted(source.keys() - target.keys()),
        "extra_in_rust": sorted(target.keys() - source.keys()),
        "differing_tools": len(differences),
        "difference_count": sum(map(len, differences.values())),
        "differences": differences,
    }


def checked_upstream(upstream: Path) -> None:
    if not (upstream / "scrapling" / "core" / "ai" / "server.py").is_file():
        raise ValueError(f"Not a Scrapling upstream checkout: {upstream}")
    # Share the AST oracle's tracked/untracked Python-source guard. Python
    # imports create __pycache__/*.pyc, which must not invalidate later probes.
    verify_source_tree(upstream)


def collect_runtime(upstream: Path, python: str, timeout: int) -> dict[str, Any]:
    # -E ignores PYTHON* environment overrides while retaining user-site
    # packages: on Windows Python 3.13, -I would hide an otherwise valid SDK.
    result = subprocess.run([python, "-E", "-c", RUNTIME_PROBE, str(upstream)],
                            cwd=upstream, capture_output=True, text=True, timeout=timeout)
    if result.returncode:
        # Python tracebacks can include paths but never need to contain tokens.
        # Keep enough detail to identify the missing import and SDK version.
        detail = result.stderr.strip().splitlines()
        raise RuntimeError(f"Upstream runtime schema unavailable (exit {result.returncode}): "
                           + " | ".join(detail[-4:]))
    payload = load_json(result.stdout)
    if not isinstance(payload, dict) or not isinstance(payload.get("tools"), list):
        raise ValueError("Runtime probe returned an invalid tool list")
    return payload


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", type=Path, default=ROOT.parent / "scrapling-v05-reference")
    parser.add_argument("--rust", type=Path, default=ROOT / "crates/rustscrapling-mcp/src/tools.json")
    parser.add_argument("--upstream-python", default=sys.executable,
                        help="Existing Python interpreter with upstream Scrapling[ai] dependencies installed")
    parser.add_argument("--timeout", type=int, default=45)
    parser.add_argument("--report", type=Path, help="Write complete JSON report (including differences)")
    parser.add_argument("--max-diffs", type=int, default=15, help="Maximum differences printed to console")
    args = parser.parse_args(argv)
    if args.timeout < 1 or args.max_diffs < 0:
        parser.error("--timeout must be positive and --max-diffs nonnegative")
    try:
        upstream = args.upstream.resolve(strict=True)
        checked_upstream(upstream)
        rust_bytes = args.rust.read_bytes()
        rust = load_json(rust_bytes.decode("utf-8-sig"))
        live = collect_runtime(upstream, args.upstream_python, args.timeout)
        report = {
            "oracle_kind": "MCP tools/list wire-schema versus Rust tools.json catalog; not executor parity",
            "upstream_sha": UPSTREAM_SHA,
            "rust_catalog_sha256": hashlib.sha256(rust_bytes).hexdigest(),
            "mcp_sdk_version": live["sdk_version"],
            "python_version": live["python_version"],
            # Do not embed a workstation-specific absolute path in a portable
            # committed audit artifact. The probe already verifies provenance.
            "source_file": Path(live["source_file"]).relative_to(upstream).as_posix(),
            **compare_tools(live["tools"], rust),
        }
        if args.report:
            args.report.parent.mkdir(parents=True, exist_ok=True)
            args.report.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        print(f"Runtime schema: {report['status']}; upstream={report['upstream_count']}, "
              f"rust={report['rust_count']}, changed_tools={report['differing_tools']}, "
              f"differences={report['difference_count']}")
        printed = 0
        for name, changes in report["differences"].items():
            for change in changes:
                if printed >= args.max_diffs:
                    break
                print(f"  {name}{change['path']}: {change['reason']} "
                      f"upstream={change.get('upstream', '<missing>')!r} "
                      f"rust={change.get('rust', '<missing>')!r}")
                printed += 1
        for key in ("missing_in_rust", "extra_in_rust"):
            if report[key]:
                print(f"  {key}: {', '.join(report[key])}")
        return 0 if report["status"] == "match" else 1
    except (OSError, ValueError, subprocess.CalledProcessError, subprocess.TimeoutExpired, RuntimeError) as exc:
        print(f"Runtime schema audit unavailable: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
