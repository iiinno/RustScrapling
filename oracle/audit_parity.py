#!/usr/bin/env python3
"""Conservative M0 parity audit: source declarations != working executors.

Compares pinned AST inventory with Rust's published MCP tools.json. In
particular, matching schemas never change a runtime status to `verified`.
"""

from __future__ import annotations

import argparse
import ast
import hashlib
import json
import re
from pathlib import Path

from inventory_upstream import UPSTREAM_SHA
from rust_api_index import index_rust_functions, same_name_candidates


CLASS_CRATE = {
    "SelectorsGeneration": "rustscrapling-select",
    "Selector": "rustscrapling-select", "Selectors": "rustscrapling-select",
    "TextHandler": "rustscrapling-dom", "TextHandlers": "rustscrapling-dom",
    "AttributesHandler": "rustscrapling-dom", "Response": "rustscrapling-fetch",
    "BaseFetcher": "rustscrapling-fetch", "Fetcher": "rustscrapling-http",
    "AsyncFetcher": "rustscrapling-http", "FetcherSession": "rustscrapling-http",
    "StealthyFetcher": "rustscrapling-browser", "StealthySession": "rustscrapling-browser",
    "AsyncStealthySession": "rustscrapling-browser", "Request": "rustscrapling-spider",
    "CrawlResult": "rustscrapling-spider", "LinkExtractor": "rustscrapling-spider",
    "Spider": "rustscrapling-spider", "CrawlerEngine": "rustscrapling-spider",
    "SessionManager": "rustscrapling-spider", "Scheduler": "rustscrapling-spider",
    "CrawlRule": "rustscrapling-spider", "CrawlSpider": "rustscrapling-spider",
    "XMLFeedSpider": "rustscrapling-spider", "CSVFeedSpider": "rustscrapling-spider",
    "ShopifySpider": "rustscrapling-spider", "SitemapSpider": "rustscrapling-spider",
    "ContentSpider": "rustscrapling-spider",
}


def parse_default(text: str | None):
    if text is None:
        return None
    try:
        return ast.literal_eval(text)
    except (SyntaxError, TypeError, ValueError):
        return None


def annotated_field_constraints(annotation: str | None) -> dict:
    """Only literal Field(...) keyword constraints; no imports or evaluation.

    Unsupported expressions and referenced type aliases are intentionally not
    expanded. This is a *partial* static audit of upstream Pydantic settings.
    """
    if not annotation:
        return {}
    try:
        node = ast.parse(annotation, mode="eval")
    except SyntaxError:
        return {}
    mapping = {
        "description": "description", "min_length": "min_length", "max_length": "max_length",
        "ge": "minimum", "gt": "exclusiveMinimum", "le": "maximum", "lt": "exclusiveMaximum",
    }
    result = {}
    for part in ast.walk(node):
        if not isinstance(part, ast.Call) or not isinstance(part.func, (ast.Name, ast.Attribute)):
            continue
        if (part.func.id if isinstance(part.func, ast.Name) else part.func.attr) != "Field":
            continue
        for kw in part.keywords:
            if not kw.arg or (kw.arg not in mapping and kw.arg not in {"default", "default_factory"}):
                continue
            try:
                value = ast.literal_eval(kw.value)
            except (ValueError, TypeError, SyntaxError):
                continue
            result[mapping.get(kw.arg, kw.arg)] = value
    return result


def observed_constraint(schema: dict, name: str, annotation: str | None):
    """Extract a constraint from an optional JSON Schema union safely.

    Pydantic represents min_length as minItems for lists, minLength for
    strings. Optional int bounds appear inside anyOf's non-null branch.
    """
    if name in {"min_length", "max_length"}:
        kind = "Items" if annotation and any(x in annotation for x in ("List[", "Sequence[", "list[", "tuple[")) else "Length"
        name = ("min" if name == "min_length" else "max") + kind
    if name in schema:
        return schema[name]
    for union in ("anyOf", "oneOf", "allOf"):
        for member in schema.get(union, []):
            if not isinstance(member, dict) or member.get("type") == "null":
                continue
            result = observed_constraint(member, name, annotation)
            if result != "<absent>":
                return result
    return "<absent>"


def compare_args(source_parameters: list[dict], schema: dict) -> dict:
    upstream_args = {p["name"]: p for p in source_parameters if p["name"] not in {"self", "cls"}}
    rust_args = schema.get("properties", {})
    missing = sorted(set(upstream_args) - set(rust_args))
    extra = sorted(set(rust_args) - set(upstream_args))
    default_diffs = []
    constraint_diffs = []
    expected_required = set()
    for key, param in upstream_args.items():
        constraints = annotated_field_constraints(param.get("annotation"))
        if not param["has_default"] and "default" not in constraints and "default_factory" not in constraints:
            expected_required.add(key)
    rust_required = set(schema.get("required", []))
    for key in sorted(set(rust_args) & set(upstream_args)):
        upstream = upstream_args[key]
        actual = rust_args[key]
        if not isinstance(actual, dict):
            continue
        constraints = annotated_field_constraints(upstream.get("annotation"))
        if upstream["has_default"] or "default" in constraints:
            source_default = upstream["default"]
            wanted = parse_default(source_default) if upstream["has_default"] else constraints["default"]
            if (source_default == "None" or wanted is not None or "default" in constraints):
                if "default" not in actual or actual["default"] != wanted:
                    default_diffs.append({"parameter": key, "upstream": wanted, "rust": actual.get("default", "<absent>")})
        for constraint, expected in sorted(constraints.items()):
            if constraint not in {"description", "minimum", "exclusiveMinimum", "maximum", "exclusiveMaximum", "min_length", "max_length"}:
                continue
            found = observed_constraint(actual, constraint, upstream.get("annotation"))
            if found != expected:
                constraint_diffs.append({"parameter": key, "constraint": constraint, "upstream": expected, "rust": found})
    return {
        "upstream_args": len(upstream_args), "rust_args": len(rust_args),
        "missing_rust": missing, "extra_rust": extra, "default_diffs": default_diffs,
        "missing_required": sorted(expected_required - rust_required),
        "extra_required": sorted(rust_required - expected_required),
        "constraint_diffs": constraint_diffs,
    }


def action_schema(schema: dict) -> dict[str, dict]:
    actions = schema.get("properties", {}).get("actions", {}).get("items", {}).get("oneOf", [])
    found = {}
    for variant in actions:
        prop = variant.get("properties", {}).get("type", {})
        labels = prop.get("enum", [])
        if len(labels) != 1 or labels[0] in found:
            raise ValueError("Rust action has ambiguous or duplicate type discriminator")
        found[labels[0]] = variant
    return found


def compare_actions(upstream: list[dict], rust_actions: dict[str, dict]) -> list[dict]:
    rows = []
    for action in upstream:
        variant = rust_actions.get(action["type"])
        expected_fields = {f["name"]: f for f in action["fields"]}
        props = variant.get("properties", {}) if variant else {}
        required = set(variant.get("required", [])) if variant else set()
        missing = sorted(set(expected_fields) - set(props))
        extra = sorted(set(props) - set(expected_fields))
        missing_required = sorted(k for k, f in expected_fields.items() if f["required"] and k not in required)
        extra_required = sorted(required - {k for k, f in expected_fields.items() if f["required"]})
        default_diffs = []
        for key in sorted(set(props) & set(expected_fields)):
            default = expected_fields[key]["default"]
            if default is None:
                continue
            intended = parse_default(default)
            if intended is None and default != "None":
                continue
            if props[key].get("default", "<absent>") != intended:
                default_diffs.append(key)
        rows.append({
            "name": action["type"], "upstream_source": action["source"], "line": action["line"],
            "schema_present": variant is not None, "missing_fields": missing, "extra_fields": extra,
            "missing_required": missing_required, "extra_required": extra_required,
            "default_diffs": default_diffs, "runtime_status": "unverified",
        })
    return rows


def canonical_catalog_hash(data: bytes) -> str:
    """Hash the JSON structure rather than checkout-dependent whitespace."""
    parsed = json.loads(data)
    canonical = json.dumps(parsed, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(canonical.encode("utf-8")).hexdigest()


def canonical_backend_hash(data: bytes) -> str:
    """Preserve source changes but normalize Git's LF/CRLF checkout policy."""
    canonical = data.decode("utf-8").replace("\r\n", "\n").replace("\r", "\n")
    return hashlib.sha256(canonical.encode("utf-8")).hexdigest()


def executor_action_branches(backend: str) -> set[str]:
    """Source-only scan: branch presence does not establish full behavior."""
    start = backend.find("async fn browser_actions(")
    if start < 0:
        raise ValueError("Rust backend is missing browser_actions")
    after = backend[start:]
    match = after.find("match action_type {")
    stop = after.find("_ => Err(BackendError::new(", match)
    if match < 0 or stop < match:
        raise ValueError("Unexpected Rust browser_actions match block")
    return set(re.findall(r'^\s*"([a-z_]+)"\s*=>\s*\{', after[match:stop], re.MULTILINE))


def audit(api: dict, mcp: dict, rust_catalog: list[dict], catalog_bytes: bytes, backend_bytes: bytes, rust_symbols: dict | None = None) -> dict:
    rust_symbols = rust_symbols or {}
    if api["upstream_sha"] != UPSTREAM_SHA or mcp["upstream_sha"] != UPSTREAM_SHA:
        raise ValueError("Wrong upstream SHA in inventory")
    tools = {tool["name"]: tool for tool in rust_catalog}
    if len(tools) != len(rust_catalog):
        raise ValueError("Duplicate Rust tool names")
    mcp_rows = []
    for source_tool in mcp["tools"]:
        rust = tools.get(source_tool["name"])
        args = compare_args(source_tool["parameters"], rust.get("inputSchema", {}) if rust else {})
        row = {
            "name": source_tool["name"], "upstream_source": source_tool["source"],
            "upstream_line": source_tool["line"], "upstream_registration_line": source_tool["registration_line"],
            "rust_catalog_present": rust is not None,
            "title_matches": rust is not None and rust.get("title") == source_tool["tool_title"],
            "description_matches": rust is not None and rust.get("description") == source_tool["tool_description"],
            "runtime_status": "unverified",
            **args,
        }
        mcp_rows.append(row)
    browser_actions = action_schema(tools.get("browser_actions", {}).get("inputSchema", {}))
    action_rows = compare_actions(mcp["actions"], browser_actions)
    branches = executor_action_branches(backend_bytes.decode("utf-8"))
    for row in action_rows:
        row["executor_branch_present"] = row["name"] in branches
        row["runtime_status"] = "partial_unverified" if row["executor_branch_present"] else "unsupported_operation"
    rows = []
    for method in api["api_methods"]:
        cls = method["id"].split(".", 1)[0]
        rows.append({
            "id": method["id"], "source": method["source"], "line": method["line"],
            "rust_crate_candidate": CLASS_CRATE.get(cls), "rust_entrypoint": None,
            "same_name_candidates": same_name_candidates(method, rust_symbols, CLASS_CRATE.get(cls)),
            "test_ids": [], "status": "unverified",
        })
    inherited_rows = []
    for method in api["inherited_methods"]:
        cls = method["id"].split(".", 1)[0]
        inherited_rows.append({
            "id": method["id"], "declared_in": method["declared_in"],
            "via_base": method["via_base"], "source": method["source"], "line": method["line"],
            "rust_crate_candidate": CLASS_CRATE.get(cls), "rust_entrypoint": None,
            "same_name_candidates": same_name_candidates(method, rust_symbols, CLASS_CRATE.get(cls)),
            "test_ids": [], "status": "unverified",
        })
    module_rows = [{
        "id": item["id"], "source": item["source"], "line": item["line"],
        "explicit_export": item["exported_via_all"], "rust_entrypoint": None,
        "same_name_candidates": same_name_candidates(item, rust_symbols, None),
        "test_ids": [], "status": "unverified",
    } for item in api["module_functions"]]
    cli_rows = []
    for command in api["cli_commands"]:
        cli_rows.append({"id": command["id"], "source": command["source"], "line": command["line"],
                         "static_options_count": len(command["decorator_options"]),
                         "expanded_decorator_factories": command["expanded_decorator_factories"],
                         "inherited_options_unresolved": command["inherited_or_unknown_decorators"], "status": "unverified"})
    return {
        "schema_version": 1, "upstream_sha": UPSTREAM_SHA,
        "rust_catalog_sha256": canonical_catalog_hash(catalog_bytes),
        "rust_backend_sha256": canonical_backend_hash(backend_bytes),
        "limitations": [
            "Audit compares source-level MCP arguments and schemas, not actual Pydantic runtime schema, description serialization or executor semantics.",
            "Selected classes and public-by-convention module functions are inventoried; built-in/external inherited APIs and dynamic exports still need review.",
            "Known Click decorator factories are expanded statically; Click runtime/schema validation remains necessary.",
            "All runtime statuses deliberately remain unverified until evidence links to passing tests.",
            "Rust same-name candidates use a lexical regex of pub fn declarations, not a Rust parser or module visibility analysis.",
        ],
        "methods": rows, "inherited_methods": inherited_rows, "module_functions": module_rows, "exported_aliases": api["exported_aliases"],
        "cli": cli_rows, "mcp": mcp_rows, "actions": action_rows,
        "extra_rust_mcp_tools": sorted(set(tools) - {t["name"] for t in mcp["tools"]}),
        "extra_rust_browser_actions": sorted(set(browser_actions) - {a["type"] for a in mcp["actions"]}),
        "unrecognized_executor_action_branches": sorted(branches - {a["type"] for a in mcp["actions"]}),
    }


def markdown(report: dict) -> str:
    methods = report["methods"]
    inherited = report["inherited_methods"]
    functions = report["module_functions"]
    mcp = report["mcp"]
    actions = report["actions"]
    lines = [
        "# RustScrapling v0.5 機能対応台帳（M0・暫定）", "",
        "> **これはAPIの実装完了証明ではない。** `unverified` は「未実装」と同義ではなく、Rustの対応入口・試験・本家との同等性がまだ証明されていないことを示す。",
        "> 生成: `python oracle/inventory_upstream.py --upstream PATH --out docs/compat` と `python oracle/audit_parity.py`。手で編集せず、検証済みの実装対応・test IDは別途監査してから反映する。",
        "", f"- 上流固定SHA: `{report['upstream_sha']}`",
        f"- 主要公開クラスの直接定義メソッド宣言: **{len(methods)}件**（overload宣言を別行で保持。継承・module-level APIは別途棚卸し）",
        f"- 選定クラス間で静的に解決できた継承メソッド: **{len(inherited)}件**（外部・組込み基底クラスは未監査）",
        f"- `scrapling/`全Pythonモジュール内の非privateトップレベル関数: **{len(functions)}件**（パッケージ直下からのimportを保証しない）",
        f"- export alias: **{len(report['exported_aliases'])}件**",
        f"- CLIのコマンド関数: **{len(report['cli'])}件**（既知の共通decoratorはAST展開済み。Click実行時のschemaは未検証）",
        f"- MCP: **{len(mcp)}ツール**、BrowserAction: **{len(actions)}種**",
        f"- Rust公開宣言の同名候補がある直接メソッド: **{sum(bool(m['same_name_candidates']) for m in methods)}件**（同名候補であり実装・互換の証明ではない）",
        "- 本家のメソッド/引数/デコレーターは `UPSTREAM_API_V05.json` と `MCP_ACTIONS_V05.json` に固定。ここではRust側の名前・静的schemaのみ比較。",
        "", "## 1. MCPツールの静的schema差分", "",
        "| Tool | 上流引数 | Rust属性 | Rustにない引数 | Rust固有引数 | 既定値差 | required差 | Field制約差 | 説明一致 | 実動検証 |",
        "| --- | ---: | ---: | --- | --- | --- | --- | --- | --- | --- |",
    ]
    for row in mcp:
        missing = ", ".join(f"`{x}`" for x in row["missing_rust"]) or "—"
        extra = ", ".join(f"`{x}`" for x in row["extra_rust"]) or "—"
        defaults = ", ".join(f"`{x['parameter']}`" for x in row["default_diffs"]) or "—"
        required = ", ".join(f"`{x}`" for x in (row["missing_required"] + row["extra_required"])) or "—"
        constraints = ", ".join(f"`{x['parameter']}.{x['constraint']}`" for x in row["constraint_diffs"]) or "—"
        lines.append(f"| `{row['name']}` | {row['upstream_args']} | {row['rust_args']} | {missing} | {extra} | {defaults} | {required} | {constraints} | {'Yes' if row['description_matches'] else 'No'} | 未検証 |")
    lines += ["", "## 2. BrowserAction静的差分", "", "| Action | Rust schema登録 | 欠落フィールド | 必須指定差 | default差 | Rust実装分岐 | 実機E2E |", "| --- | --- | --- | --- | --- | --- | --- |"]
    for row in actions:
        req = row["missing_required"] + row["extra_required"]
        branch = "あり・部分対応" if row["executor_branch_present"] else "なし・unsupported"
        lines.append(f"| `{row['name']}` | {'Yes' if row['schema_present'] else 'No'} | {', '.join(row['missing_fields']) or '—'} | {', '.join(req) or '—'} | {', '.join(row['default_diffs']) or '—'} | {branch} | 未検証 |")
    lines += ["", "## 3. CLI公開コマンド", "", "| ID | 上流出典 | 静的option/argument数 | 共通decorator展開 | Rust対応・E2E |", "| --- | --- | ---: | --- | --- |"]
    for row in report["cli"]:
        decorators = ", ".join("`" + d + "`" for d in row["expanded_decorator_factories"]) or "—"
        if row["inherited_options_unresolved"]:
            decorators += " / 未解析: " + ", ".join(row["inherited_options_unresolved"])
        lines.append(f"| `{row['id']}` | `{row['source']}:{row['line']}` | {row['static_options_count']} | {decorators} | 未検証 |")
    lines += ["", "## 4. 公開クラスのメソッド別追跡", "", "| 上流API | ソース | 対応クレート候補 | Rustの同名候補 | 実動テスト |", "| --- | --- | --- | --- | --- |"]
    for row in methods:
        candidate = ", ".join(f"`{c['source']}:{c['line']}`" for c in row['same_name_candidates'][:3]) or "—"
        if len(row['same_name_candidates']) > 3:
            candidate += " (ほか)"
        lines.append(f"| `{row['id']}` | `{row['source']}:{row['line']}` | `{row['rust_crate_candidate'] or '未分類'}` | {candidate} | 未検証 |")
    lines += ["", "## 5. 継承・エクスポートalias", "", "| 継承API | 宣言元 | 対応クレート候補 | 実動作 |", "| --- | --- | --- | --- |"]
    for row in inherited:
        lines.append(f"| `{row['id']}` | `{row['declared_in']}` via `{row['via_base']}` | `{row['rust_crate_candidate'] or '未分類'}` | 未検証 |")
    if report["exported_aliases"]:
        lines += ["", "| 公開alias | 実体 | 出典 |", "| --- | --- | --- |"]
        for alias in report["exported_aliases"]:
            lines.append(f"| `{alias['alias']}` | `{alias['target']}` | `{alias['source']}:{alias['line']}` |")
    lines += ["", "## 6. モジュールレベル関数", "", "| 関数 | 参照元 | `__all__`明示 | Rust対応・テスト |", "| --- | --- | --- | --- |"]
    for func in functions:
        lines.append(f"| `{func['id']}` | `{func['source']}:{func['line']}` | {'Yes' if func['explicit_export'] else 'No'} | 未検証 |")
    lines += ["", "## 7. 残作業（重要）", "",
              "1. `Unpack[TypedDict]`と共通CLIデコレーターのAST抽出結果、外部基底クラスの継承API、module-level公開関数を本家実行時schemaと照合する。",
              "2. Rustの実entrypoint・引数の既定値・結果/エラーにunit/fixture/E2Eを紐付ける。名前やschema一致だけでverifiedにしない。",
              "3. 特にBrowserActionはschema登録済みでも各executorが動く保証はない。`docs/COMPAT.md`のunsupportedを実機テストで解消する。",
              "4. 最新の統合HEADで再監査し、上流とRustを同一環境で比較する。", ""]
    return "\n".join(lines)


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--inventory-dir", type=Path, default=Path("docs/compat"))
    p.add_argument("--rust-tools", type=Path, default=Path("crates/rustscrapling-mcp/src/tools.json"))
    p.add_argument("--rust-backend", type=Path, default=Path("crates/rustscrapling-mcp/src/backend.rs"))
    p.add_argument("--rust-root", type=Path, default=Path("."))
    p.add_argument("--check", action="store_true")
    args = p.parse_args()
    api = json.loads((args.inventory_dir / "UPSTREAM_API_V05.json").read_text(encoding="utf-8"))
    mcp = json.loads((args.inventory_dir / "MCP_ACTIONS_V05.json").read_text(encoding="utf-8"))
    raw = args.rust_tools.read_bytes()
    report = audit(api, mcp, json.loads(raw), raw, args.rust_backend.read_bytes(), index_rust_functions(args.rust_root))
    files = {
        args.inventory_dir / "PARITY_AUDIT.json": json.dumps(report, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        args.inventory_dir / "PARITY_MATRIX.md": markdown(report),
    }
    for path, result in files.items():
        if args.check:
            if not path.exists() or path.read_text(encoding="utf-8") != result:
                raise SystemExit(f"STALE: {path}")
        else:
            path.write_text(result, encoding="utf-8", newline="\n")
    tool_gaps = sum(bool(row["missing_rust"] or row["extra_rust"] or row["default_diffs"] or row["missing_required"] or row["extra_required"] or row["constraint_diffs"]) for row in report["mcp"])
    action_gaps = sum(bool(row["missing_fields"] or row["extra_fields"] or row["missing_required"] or row["extra_required"] or row["default_diffs"]) for row in report["actions"])
    branches = sum(row["executor_branch_present"] for row in report["actions"])
    print(f"Audit: {len(report['mcp'])} MCP tools ({tool_gaps} with static parameter differences), {len(report['actions'])} actions ({action_gaps} with static field differences; {branches} executor branches); runtime parity NOT verified")


if __name__ == "__main__":
    main()
