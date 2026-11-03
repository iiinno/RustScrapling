#!/usr/bin/env python3
"""Deterministic, dependency-free AST inventory of the pinned Scrapling v0.5.

Usage: python oracle/inventory_upstream.py --upstream DIR --out docs/compat

The upstream checkout is deliberately *not* vendored into RustScrapling.
This is a source inventory, not proof of compatible Rust behavior. The output
records unresolved inherited decorators and **kwargs for later review.
"""

from __future__ import annotations

import argparse
import ast
import hashlib
import json
import subprocess
from pathlib import Path


UPSTREAM_SHA = "95cf9019ab5a5d6f3e9654b7ef1c6a26e29dc5c0"
API_CLASSES = {
    "scrapling/core/mixins.py": ("SelectorsGeneration",),
    "scrapling/parser.py": ("Selector", "Selectors"),
    "scrapling/core/custom_types.py": ("TextHandler", "TextHandlers", "AttributesHandler"),
    "scrapling/engines/toolbelt/custom.py": ("Response", "BaseFetcher"),
    "scrapling/fetchers/requests.py": ("Fetcher", "AsyncFetcher"),
    "scrapling/fetchers/stealth_chrome.py": ("StealthyFetcher",),
    "scrapling/engines/static.py": ("FetcherSession",),
    "scrapling/engines/_browsers/_stealth.py": ("StealthySession", "AsyncStealthySession"),
    "scrapling/spiders/request.py": ("Request",),
    "scrapling/spiders/result.py": ("CrawlResult",),
    "scrapling/spiders/links.py": ("LinkExtractor",),
    "scrapling/spiders/spider.py": ("Spider",),
    "scrapling/spiders/engine.py": ("CrawlerEngine",),
    "scrapling/spiders/session.py": ("SessionManager",),
    "scrapling/spiders/scheduler.py": ("Scheduler",),
    "scrapling/spiders/templates/crawler.py": ("CrawlRule", "CrawlSpider"),
    "scrapling/spiders/templates/feed.py": ("XMLFeedSpider", "CSVFeedSpider"),
    "scrapling/spiders/templates/shopify.py": ("ShopifySpider",),
    "scrapling/spiders/templates/sitemap.py": ("SitemapSpider",),
    "scrapling/spiders/templates/site_to_markdown.py": ("ContentSpider",),
}
EXPORTED_MODULES = {
    "scrapling/__init__.py",
    "scrapling/fetchers/__init__.py",
    "scrapling/spiders/__init__.py",
}
ACTION_FILE = "scrapling/core/ai/_browser_actions.py"
SERVER_FILE = "scrapling/core/ai/server.py"
CLI_FILE = "scrapling/cli.py"
TYPED_DICT_FILE = "scrapling/engines/_browsers/_types.py"
ALLOWED_DUNDERS = {"__init__", "__getitem__", "__iter__", "__len__", "__contains__", "__bool__", "__call__"}


def expr(node: ast.AST | None) -> str | None:
    return ast.unparse(node) if node is not None else None


def name(node: ast.AST) -> str:
    return expr(node) or ""


def call_name(node: ast.AST) -> str:
    if isinstance(node, ast.Call):
        return call_name(node.func)
    if isinstance(node, ast.Attribute):
        return node.attr
    if isinstance(node, ast.Name):
        return node.id
    return name(node)


def literal(node: ast.AST | None):
    if node is None:
        return None
    try:
        return ast.literal_eval(node)
    except (ValueError, TypeError, SyntaxError, RecursionError, MemoryError):
        return expr(node)


def parameters(fn: ast.FunctionDef | ast.AsyncFunctionDef) -> list[dict]:
    args = fn.args
    ordered = args.posonlyargs + args.args
    pad = len(ordered) - len(args.defaults)
    output = []
    for index, arg in enumerate(ordered):
        default = args.defaults[index - pad] if index >= pad else None
        output.append({
            "name": arg.arg,
            "kind": "positional_only" if index < len(args.posonlyargs) else "positional_or_keyword",
            "annotation": expr(arg.annotation),
            "has_default": default is not None,
            "default": expr(default),
        })
    if args.vararg:
        output.append({"name": args.vararg.arg, "kind": "var_positional", "annotation": expr(args.vararg.annotation), "has_default": False, "default": None})
    for arg, default in zip(args.kwonlyargs, args.kw_defaults):
        output.append({"name": arg.arg, "kind": "keyword_only", "annotation": expr(arg.annotation), "has_default": default is not None, "default": expr(default)})
    if args.kwarg:
        output.append({"name": args.kwarg.arg, "kind": "var_keyword", "annotation": expr(args.kwarg.annotation), "has_default": False, "default": None})
    return output


def method_item(fn: ast.FunctionDef | ast.AsyncFunctionDef, path: str, owner: str) -> dict:
    doc = ast.get_docstring(fn) or ""
    return {
        "id": f"{owner}.{fn.name}",
        "source": path,
        "line": fn.lineno,
        "kind": "async" if isinstance(fn, ast.AsyncFunctionDef) else "sync",
        "signature": expr(fn.args),
        "parameters": parameters(fn),
        "returns": expr(fn.returns),
        "decorators": [expr(d) for d in fn.decorator_list],
        "doc_summary": doc.strip().splitlines()[0] if doc.strip() else "",
        "doc_sha256": hashlib.sha256(doc.encode()).hexdigest(),
    }


def typed_dict_classes(tree: ast.Module, path: str) -> dict[str, dict]:
    definitions = {}
    for node in tree.body:
        if not isinstance(node, ast.ClassDef):
            continue
        bases = [name(base) for base in node.bases]
        if "TypedDict" not in bases and not any(b in definitions for b in bases):
            continue
        total = not any(kw.arg == "total" and isinstance(kw.value, ast.Constant) and kw.value.value is False for kw in node.keywords)
        fields = []
        for child in node.body:
            if not isinstance(child, ast.AnnAssign) or not isinstance(child.target, ast.Name):
                continue
            annotation = child.annotation
            required = total
            if isinstance(annotation, ast.Subscript) and call_name(annotation.value) in {"NotRequired", "Required"}:
                required = call_name(annotation.value) == "Required"
            default = None
            for part in ast.walk(annotation):
                if isinstance(part, ast.Call) and call_name(part.func) == "Field":
                    default = next((expr(k.value) for k in part.keywords if k.arg == "default"), None)
            fields.append({"name": child.target.id, "annotation": expr(annotation), "required": required, "default": default, "line": child.lineno})
        definitions[node.name] = {"id": node.name, "source": path, "line": node.lineno, "bases": bases, "total": total, "fields_local": fields}
    for definition in definitions.values():
        merged = {}
        for base in definition["bases"]:
            if base in definitions:
                for f in definitions[base].get("fields_resolved", definitions[base]["fields_local"]):
                    merged[f["name"]] = f.copy()
        for f in definition["fields_local"]:
            merged[f["name"]] = f.copy()
        definition["fields_resolved"] = list(merged.values())
    return definitions


def extract_literal_values(annotation: ast.AST) -> list[str]:
    for part in ast.walk(annotation):
        if isinstance(part, ast.Subscript) and call_name(part.value) == "Literal":
            values = part.slice.elts if isinstance(part.slice, ast.Tuple) else [part.slice]
            return [v.value for v in values if isinstance(v, ast.Constant) and isinstance(v.value, str)]
    return []


def action_variants(tree: ast.Module) -> list[str]:
    # Resolve the TypeAliasType("BrowserAction", Annotated[Union[...], Field(...)])
    for node in tree.body:
        if not isinstance(node, ast.Assign) or not any(isinstance(t, ast.Name) and t.id == "BrowserAction" for t in node.targets):
            continue
        refs = []
        for sub in ast.walk(node.value):
            if isinstance(sub, ast.Subscript) and call_name(sub.value) == "Union":
                values = sub.slice.elts if isinstance(sub.slice, ast.Tuple) else [sub.slice]
                refs = [v.id for v in values if isinstance(v, ast.Name) and v.id.startswith("_")]
                break
        if refs:
            return refs
    raise ValueError("Cannot find discriminated BrowserAction Union")


def server_registrations(tree: ast.Module) -> list[dict]:
    server_cls = next((n for n in tree.body if isinstance(n, ast.ClassDef) and n.name == "ScraplingMCPServer"), None)
    if server_cls is None:
        raise ValueError("Cannot find ScraplingMCPServer")
    funcs = {n.name: n for n in server_cls.body if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))}
    builder = funcs.get("_build_server")
    if builder is None:
        raise ValueError("Missing _build_server")
    tools = []
    for call in ast.walk(builder):
        if not isinstance(call, ast.Call) or not isinstance(call.func, ast.Attribute) or call.func.attr != "add_tool":
            continue
        if not call.args or not isinstance(call.args[0], ast.Call) or not call.args[0].args:
            raise ValueError("Unknown server.add_tool invocation")
        wrapped = call.args[0].args[0]
        if not isinstance(wrapped, ast.Attribute) or not isinstance(wrapped.value, ast.Name) or wrapped.value.id != "self":
            raise ValueError("Unexpected tool registration shape")
        tool_name = wrapped.attr
        fn = funcs.get(tool_name)
        if fn is None:
            raise ValueError("Registered tool has no method: " + tool_name)
        item = method_item(fn, SERVER_FILE, "ScraplingMCPServer")
        item["name"] = tool_name
        item["tool_title"] = next((literal(k.value) for k in call.keywords if k.arg == "title"), None)
        item["tool_description"] = ast.get_docstring(fn) or ""
        item["registration_line"] = call.lineno
        item["structured_output"] = next((literal(k.value) for k in call.keywords if k.arg == "structured_output"), None)
        tools.append(item)
    tools.sort(key=lambda item: item["registration_line"])
    if len(tools) != 16 or len({t["name"] for t in tools}) != 16:
        raise ValueError(f"Expected exactly 16 unique upstream MCP tools, got {len(tools)}")
    return tools


def cli_commands(tree: ast.Module) -> list[dict]:
    def click_option(call: ast.Call) -> dict:
        return {"decorator": call_name(call), "args": [expr(a) for a in call.args], "kwargs": {k.arg: expr(k.value) for k in call.keywords}}

    # Common Click decorator factories build a list and apply it in a loop.
    # Expand their static option declarations instead of silently losing them.
    factories = {}
    for candidate in tree.body:
        if not isinstance(candidate, ast.FunctionDef):
            continue
        for assignment in candidate.body:
            if not isinstance(assignment, ast.Assign) or not isinstance(assignment.value, (ast.List, ast.Tuple)):
                continue
            if not any(isinstance(t, ast.Name) and t.id == "decorators" for t in assignment.targets):
                continue
            values = assignment.value.elts
            if values and all(isinstance(v, ast.Call) and call_name(v) in {"option", "argument"} for v in values):
                factories[candidate.name] = [click_option(v) for v in values]

    commands = []
    for fn in tree.body:
        if not isinstance(fn, (ast.FunctionDef, ast.AsyncFunctionDef)):
            continue
        decs = fn.decorator_list
        direct = [d for d in decs if call_name(d) in {"command", "group"} or (isinstance(d, ast.Call) and isinstance(d.func, ast.Attribute) and d.func.attr == "command")]
        if not direct:
            continue
        options = []
        unresolved_decorators = []
        expanded = []
        for d in decs:
            label = call_name(d)
            if label in {"option", "argument", "version_option"} and isinstance(d, ast.Call):
                options.append(click_option(d))
            elif label in factories:
                options.extend(factories[label])
                expanded.append(label)
            elif label not in {"command", "group", "option", "argument", "version_option"}:
                unresolved_decorators.append(expr(d))
        item = method_item(fn, CLI_FILE, "CLI")
        item.update({"name": fn.name, "decorator_options": options, "expanded_decorator_factories": expanded, "inherited_or_unknown_decorators": unresolved_decorators, "requires_click_runtime_check": True})
        commands.append(item)
    return sorted(commands, key=lambda it: it["line"])


def exported_names(tree: ast.Module) -> list[str]:
    for assign in tree.body:
        if isinstance(assign, ast.Assign) and any(isinstance(t, ast.Name) and t.id == "__all__" for t in assign.targets):
            try:
                result = ast.literal_eval(assign.value)
                if isinstance(result, (tuple, list)):
                    return result
            except (ValueError, TypeError):
                pass
    return []


def verify_source_tree(upstream: Path, expected_sha: str = UPSTREAM_SHA) -> set[str]:
    """Reject modified or untracked upstream Python sources, even at the pin.

    A Git HEAD check alone is insufficient: its object ID does not change when
    the working tree's Python files are edited. Non-Python artifacts such as
    __pycache__ are irrelevant to this source-level oracle.
    """

    upstream = upstream.resolve()

    def git(*args: str) -> str:
        return subprocess.check_output(
            ["git", "-C", str(upstream), *args], encoding="utf-8"
        ).strip()

    root = Path(git("rev-parse", "--show-toplevel")).resolve()
    if root != upstream:
        raise ValueError(f"Upstream checkout must be its Git root: {upstream} != {root}")
    actual = git("rev-parse", "HEAD")
    if actual != expected_sha:
        raise ValueError(f"Source SHA mismatch: expected {expected_sha}, got {actual}")

    def paths(*args: str) -> set[str]:
        output = subprocess.check_output(
            ["git", "-C", str(upstream), *args], encoding="utf-8"
        )
        return {p.replace("\\", "/") for p in output.split("\0") if p}

    tracked = paths("ls-files", "-z", "--", "scrapling")
    changes = (
        paths("diff", "--name-only", "-z", "--", "scrapling")
        | paths("diff", "--cached", "--name-only", "-z", "--", "scrapling")
        | paths("ls-files", "--others", "--exclude-standard", "-z", "--", "scrapling")
    )
    modified_python = sorted(p for p in changes if p.endswith(".py"))
    if modified_python:
        raise ValueError("Upstream Python source has working-tree changes: " + ", ".join(modified_python[:10]))
    return {p for p in tracked if p.endswith(".py")}


def parse_sources(upstream: Path) -> dict:
    sources = {}
    # Parse *all* Python modules for public module-level functions. Previously
    # only selected class files were inspected, silently omitting helpers
    # exposed as scrapling.some_module.some_function.
    paths = {p.relative_to(upstream).as_posix() for p in (upstream / "scrapling").rglob("*.py")}
    paths.update(set(API_CLASSES) | EXPORTED_MODULES | {SERVER_FILE, ACTION_FILE, CLI_FILE, TYPED_DICT_FILE})
    for rel in paths:
        file = upstream / rel
        if not file.is_file():
            raise FileNotFoundError(f"Pinned upstream missing {rel}")
        sources[rel] = ast.parse(file.read_text(encoding="utf-8-sig"), filename=rel)
    return sources


def build(upstream: Path) -> tuple[dict, dict]:
    tracked_python = verify_source_tree(upstream)
    source = parse_sources(upstream)
    non_git = sorted(set(source) - tracked_python)
    if non_git:
        raise ValueError("Parsed upstream Python files not tracked by Git: " + ", ".join(non_git[:10]))
    module_functions = []
    for rel, tree in sorted(source.items()):
        module_name = rel.removesuffix("/__init__.py").removesuffix(".py").replace("/", ".")
        for fn in tree.body:
            if isinstance(fn, (ast.FunctionDef, ast.AsyncFunctionDef)) and not fn.name.startswith("_"):
                item = method_item(fn, rel, module_name)
                item["public_by_convention"] = True
                item["exported_via_all"] = fn.name in exported_names(tree)
                module_functions.append(item)
    module_functions.sort(key=lambda it: (it["source"], it["line"]))
    methods = []
    class_bases = {}
    for rel, target_classes in API_CLASSES.items():
        classes = {n.name: n for n in source[rel].body if isinstance(n, ast.ClassDef)}
        for cls_name in target_classes:
            if cls_name not in classes:
                raise ValueError(f"Missing {rel}:{cls_name}")
            class_bases[cls_name] = [name(base) for base in classes[cls_name].bases]
            for fn in classes[cls_name].body:
                if isinstance(fn, (ast.FunctionDef, ast.AsyncFunctionDef)) and (not fn.name.startswith("_") or fn.name in ALLOWED_DUNDERS):
                    item = method_item(fn, rel, cls_name)
                    # @overload declarations have the *same* Python method
                    # name, but distinct signatures and source lines. Retain
                    # their own audit IDs instead of silently dropping them.
                    if "overload" in item["decorators"]:
                        item["id"] += f"@overload:{fn.lineno}"
                        item["overload_only"] = True
                    else:
                        item["overload_only"] = False
                    methods.append(item)
    methods.sort(key=lambda m: (m["source"], m["line"], m["id"]))
    direct_by_class = {}
    for item in methods:
        direct_by_class.setdefault(item["id"].split(".", 1)[0], []).append(item)

    def method_name(item: dict) -> str:
        return item["id"].split(".", 1)[1].split("@overload:")[0]

    # Resolve inheritance where both classes are part of this inventory. In
    # particular, Selector exposes methods defined on SelectorsGeneration,
    # Response inherits Selector, and Spider templates inherit Spider hooks.
    inherited = []
    def effective(cls: str, visiting: set[str]) -> list[dict]:
        if cls in visiting:
            raise ValueError("Cycle in selected public class inheritance: " + cls)
        own = direct_by_class.get(cls, [])
        own_names = {method_name(m) for m in own}
        prior = []
        seen = set(own_names)
        for base in class_bases.get(cls, []):
            if base not in class_bases:
                continue
            for item in effective(base, visiting | {cls}):
                label = method_name(item)
                if label in seen:
                    continue
                prior.append(item)
            seen.update(method_name(m) for m in prior)
        return own + prior

    for cls in class_bases:
        own_names = {method_name(m) for m in direct_by_class.get(cls, [])}
        seen = set(own_names)
        for base in class_bases[cls]:
            if base not in class_bases:
                continue
            grouped = {}
            for item in effective(base, {cls}):
                label = method_name(item)
                grouped.setdefault(label, []).append(item)
            for label, overloads in grouped.items():
                if label in seen:
                    continue
                for item in overloads:
                    inherited.append({
                        "id": f"{cls}.{label}" + (f"@overload:{item['line']}" if item["overload_only"] else ""),
                        "declared_in": item["id"], "source": item["source"], "line": item["line"],
                        "via_base": base, "overload_only": item["overload_only"],
                    })
                seen.add(label)
    inherited.sort(key=lambda item: (item["id"], item["source"], item["line"]))

    aliases = []
    for rel, tree in source.items():
        published = set(exported_names(tree))
        for node in tree.body:
            if not isinstance(node, ast.Assign) or not isinstance(node.value, ast.Name):
                continue
            for target in node.targets:
                if isinstance(target, ast.Name) and target.id in published and target.id != node.value.id:
                    aliases.append({"alias": target.id, "target": node.value.id, "source": rel, "line": node.lineno})
    aliases.sort(key=lambda item: (item["source"], item["alias"]))
    actions_defs = typed_dict_classes(source[ACTION_FILE], ACTION_FILE)
    action_names = action_variants(source[ACTION_FILE])
    variants = []
    for cls_name in action_names:
        definition = actions_defs.get(cls_name)
        if definition is None:
            raise ValueError(f"Unknown action TypedDict {cls_name}")
        discriminators = []
        for f in definition["fields_resolved"]:
            if f["name"] == "type":
                cls = next((n for n in source[ACTION_FILE].body if isinstance(n, ast.ClassDef) and n.name == cls_name), None)
                if cls:
                    typ = next((node.annotation for node in cls.body if isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name) and node.target.id == "type"), None)
                    if typ:
                        discriminators = extract_literal_values(typ)
        if len(discriminators) != 1:
            raise ValueError(f"Action {cls_name} does not have one discriminator")
        variants.append({"type": discriminators[0], "class": cls_name, "source": ACTION_FILE, "line": definition["line"], "fields": definition["fields_resolved"]})
    if len(variants) != 12 or len({a["type"] for a in variants}) != 12:
        raise ValueError(f"Expected 12 actions, got {len(variants)}")
    types = typed_dict_classes(source[TYPED_DICT_FILE], TYPED_DICT_FILE)
    registered = server_registrations(source[SERVER_FILE])
    cli = cli_commands(source[CLI_FILE])
    api = {
        "schema_version": 1, "upstream_sha": UPSTREAM_SHA,
        "notes": ["AST signatures are declarations, not tested runtime schemas", "Unpack[TypedDict] is resolved in typed_dicts; known CLI decorator factories are expanded, but Click runtime validation remains required", "Presence in this manifest does not establish Rust parity"],
        "exports": {rel: exported_names(source[rel]) for rel in sorted(EXPORTED_MODULES)},
        "api_methods": methods, "module_functions": module_functions,
        "inherited_methods": inherited, "class_bases": class_bases,
        "exported_aliases": aliases, "typed_dicts": types, "cli_commands": cli,
        "mcp_tool_names": [t["name"] for t in registered],
    }
    mcp = {"schema_version": 1, "upstream_sha": UPSTREAM_SHA, "tool_count": len(registered), "action_count": len(variants), "tools": registered, "actions": variants}
    return api, mcp


def write_manifest(path: Path, obj: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(obj, ensure_ascii=False, sort_keys=True, indent=2) + "\n", encoding="utf-8", newline="\n")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", type=Path, required=True, help="Checkout of D4Vinci/Scrapling at the exact pinned SHA")
    parser.add_argument("--out", type=Path, default=Path("docs/compat"))
    parser.add_argument("--check", action="store_true", help="Fail if existing manifests differ; do not write")
    args = parser.parse_args()
    api, mcp = build(args.upstream.resolve())
    files = {"UPSTREAM_API_V05.json": api, "MCP_ACTIONS_V05.json": mcp}
    for filename, obj in files.items():
        target = args.out / filename
        data = json.dumps(obj, ensure_ascii=False, sort_keys=True, indent=2) + "\n"
        if args.check:
            if not target.exists() or target.read_text(encoding="utf-8") != data:
                raise SystemExit(f"STALE: {target}")
        else:
            write_manifest(target, obj)
    print(f"Pinned {UPSTREAM_SHA}: {len(api['api_methods'])} direct + {len(api['inherited_methods'])} inherited signatures, {len(api['module_functions'])} module functions, {len(api['cli_commands'])} CLI functions, {len(mcp['tools'])} MCP tools, {len(mcp['actions'])} actions")


if __name__ == "__main__":
    main()
