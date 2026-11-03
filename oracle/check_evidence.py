#!/usr/bin/env python3
"""Validate docs/compat/EVIDENCE.json and render docs/compat/EVIDENCE.md (Plan G-01).

Each entry links one inventoried upstream item (MCP tool, browser action,
CLI command, module function, method or inherited method) to a Rust
entrypoint and to test functions. This script checks mechanically that:
  - the id exists in the pinned upstream inventory for its kind;
  - every referenced test file exists and defines that function;
  - 'verified'/'partial' entries cite at least one test.
It does not run the tests: CI runs every referenced test, so an entry is
only evidence on a SHA whose CI is green. Coverage per kind (verified /
partial / unsupported / not yet mapped) is written to EVIDENCE.md.

  python3 oracle/check_evidence.py          # rewrite EVIDENCE.md
  python3 oracle/check_evidence.py --check  # fail if invalid or stale
"""
import argparse
import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
COMPAT = ROOT / "docs" / "compat"
LEVELS = ("verified", "partial", "unsupported")
KINDS = ("mcp_tool", "action", "cli", "module_function", "method", "inherited_method")


def inventory() -> dict[str, set[str]]:
    api = json.loads((COMPAT / "UPSTREAM_API_V05.json").read_text(encoding="utf-8"))
    mcp = json.loads((COMPAT / "MCP_ACTIONS_V05.json").read_text(encoding="utf-8"))
    return {
        "mcp_tool": {tool["name"] for tool in mcp["tools"]},
        "action": {action["type"] for action in mcp["actions"]},
        "cli": {command["id"] for command in api["cli_commands"]},
        "module_function": {item["id"] for item in api["module_functions"]},
        "method": {item["id"] for item in api["api_methods"]},
        "inherited_method": {item["id"] for item in api["inherited_methods"]},
    }


def test_exists(reference: str) -> bool:
    path, _, name = reference.partition("::")
    file = ROOT / path
    if not name or not file.is_file():
        return False
    return re.search(rf"\bfn {re.escape(name)}\s*\(", file.read_text(encoding="utf-8")) is not None


def validate(evidence: dict, known: dict[str, set[str]]) -> list[str]:
    problems, seen = [], set()
    for entry in evidence["entries"]:
        key = (entry.get("kind"), entry.get("id"))
        if entry.get("kind") not in KINDS:
            problems.append(f"{key}: unknown kind")
            continue
        if entry["id"] not in known[entry["kind"]]:
            problems.append(f"{key}: id not in the pinned upstream inventory")
        if key in seen:
            problems.append(f"{key}: duplicate entry")
        seen.add(key)
        if entry.get("level") not in LEVELS:
            problems.append(f"{key}: level must be one of {LEVELS}")
        tests = entry.get("tests", [])
        if entry.get("level") in ("verified", "partial") and not tests:
            problems.append(f"{key}: {entry.get('level')} without tests")
        for reference in tests:
            if not test_exists(reference):
                problems.append(f"{key}: test not found: {reference}")
    return problems


def render(evidence: dict, known: dict[str, set[str]]) -> str:
    by_kind: dict[str, dict[str, int]] = {}
    for kind in KINDS:
        counts = {level: 0 for level in LEVELS}
        for entry in evidence["entries"]:
            if entry["kind"] == kind:
                counts[entry["level"]] += 1
        counts["unmapped"] = len(known[kind]) - sum(counts.values())
        counts["total"] = len(known[kind])
        by_kind[kind] = counts
    lines = [
        "# EVIDENCE.md: 本家公開機能 → Rust入口 → 試験（G-01）",
        "",
        "`docs/compat/EVIDENCE.json`から`oracle/check_evidence.py`で生成。各行のテスト関数の実在は機械検査済み。",
        "テストの合否はCI（参照テストを全て実行）で確認する：**緑のSHAでのみ証跡**。`unmapped`は未紐付け（未検証）であり、PASSではない。",
        "",
        "| 種別 | 総数 | verified | partial | unsupported | unmapped |",
        "| --- | ---: | ---: | ---: | ---: | ---: |",
    ]
    for kind, counts in by_kind.items():
        lines.append(f"| {kind} | {counts['total']} | {counts['verified']} | {counts['partial']} | {counts['unsupported']} | {counts['unmapped']} |")
    total = {key: sum(c[key] for c in by_kind.values()) for key in ("total", "verified", "partial", "unsupported", "unmapped")}
    lines.append(f"| **合計** | {total['total']} | {total['verified']} | {total['partial']} | {total['unsupported']} | {total['unmapped']} |")
    lines += ["", "| 種別 | ID | 判定 | Rust | テスト | 備考 |", "| --- | --- | --- | --- | --- | --- |"]
    for entry in sorted(evidence["entries"], key=lambda e: (KINDS.index(e["kind"]), e["id"])):
        tests = "<br>".join(f"`{t.split('/')[-1]}`" for t in entry.get("tests", []))
        lines.append(f"| {entry['kind']} | `{entry['id']}` | {entry['level']} | {entry.get('rust', '')} | {tests} | {entry.get('notes', '')} |")
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    evidence = json.loads((COMPAT / "EVIDENCE.json").read_text(encoding="utf-8"))
    known = inventory()
    problems = validate(evidence, known)
    if problems:
        print("\n".join(problems), file=sys.stderr)
        return 1
    rendered = render(evidence, known)
    target = COMPAT / "EVIDENCE.md"
    if args.check:
        if not target.exists() or target.read_text(encoding="utf-8") != rendered:
            print("STALE: docs/compat/EVIDENCE.md (run python3 oracle/check_evidence.py)", file=sys.stderr)
            return 1
    else:
        target.write_text(rendered, encoding="utf-8")
    print(rendered.split("\n\n")[2].splitlines()[-1])
    return 0


if __name__ == "__main__":
    sys.exit(main())
