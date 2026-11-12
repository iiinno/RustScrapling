#!/usr/bin/env python3
"""Read fixed synthetic inputs and save results from the real pinned Selector."""
import argparse
import hashlib
import importlib.metadata
import json
from pathlib import Path
import re
import sys


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def describe(value):
    if value is None:
        return None
    if value._is_text_node(value._root):
        return {"kind": "text", "value": str(value.text)}
    root = value._root
    return {
        "kind": "element", "path": root.getroottree().getpath(root),
        "tag": root.tag, "text": root.text, "tail": root.tail,
        "text_content": root.text_content(),
        "attrib": [[key, item] for key, item in root.attrib.items()],
    }


def predicate(value):
    if value["kind"] == "text_contains":
        return lambda item: value["value"] in item.text
    if value["kind"] == "attribute_equals":
        return lambda item: item.attrib.get(value["key"]) == value["value"]
    raise ValueError("unknown fixture predicate")


def compile_regex(pattern, flags):
    flag_value = 0
    for flag in flags:
        flag_value |= {"i": re.I, "m": re.M, "s": re.S}[flag]
    return re.compile(pattern, flag_value)


def find_arguments(case):
    args = []
    for item in case.get("args", []):
        kind, value = item["kind"], item["value"]
        if kind in ("tag", "tags", "attributes", "literal"):
            args.append(value)
        elif kind == "regex":
            args.append(compile_regex(value, item.get("flags", "")))
        elif kind == "predicate":
            args.append(predicate(value))
        else:
            raise ValueError("unknown fixture filter")
    return args


def execute(page, case):
    target = case.get("target", {})
    if "css" in target:
        selected = page.css(target["css"])
    elif "xpath" in target:
        selected = page.xpath(target["xpath"])
    else:
        selected = [page]
    operation = case["op"]
    if operation == "index":
        return describe(selected[case["position"]])
    if operation == "slice":
        return [describe(item) for item in selected[slice(case.get("start"), case.get("stop"), case.get("step", 1))]]
    item = selected[target.get("index", 0)]
    if operation in ("find_all", "find"):
        value = getattr(item, operation)(*find_arguments(case), **case.get("kwargs", {}))
        return [describe(entry) for entry in value] if operation == "find_all" else describe(value)
    if operation in ("parent", "next", "previous"):
        return describe(getattr(item, operation))
    if operation == "siblings":
        return [describe(entry) for entry in item.siblings]
    if operation == "getitem":
        return str(item[case["key"]])
    if operation == "contains":
        return case["key"] in item
    if operation == "get_all_text":
        options = dict(case.get("options", {}))
        if "ignore_tags" in options:
            options["ignore_tags"] = tuple(options["ignore_tags"])
        return str(item.get_all_text(**options))
    if operation in ("re", "re_first"):
        pattern = case["pattern"]
        if case.get("compiled", False):
            pattern = compile_regex(pattern, case.get("flags", ""))
        options = case.get("options", {})
        if operation == "re":
            return list(item.re(pattern, **options))
        return item.re_first(pattern, default=case.get("default"), **options)
    raise ValueError("unknown fixture operation")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture", type=Path, required=True)
    parser.add_argument("--oracle-root", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    import scrapling.parser
    from scrapling.parser import Selector

    origin = Path(scrapling.parser.__file__).resolve()
    if not origin.is_relative_to(args.oracle_root.resolve()):
        raise RuntimeError("imported Selector is outside the verified pinned oracle")
    fixture = json.loads(args.fixture.read_text(encoding="utf-8"))
    if fixture["schema_version"] != 1 or fixture["upstream_revision"] != "95cf9019ab5a5d6f3e9654b7ef1c6a26e29dc5c0":
        raise RuntimeError("fixture contract or pinned revision mismatch")
    records = []
    for case in fixture["cases"]:
        document = fixture["documents"][case["document"]]
        page = Selector(document["html"], adaptive=False, keep_comments=document.get("keep_comments", False))
        try:
            outcome = {"value": execute(page, case)}
        except Exception as error:
            outcome = {"error": "RegexError" if isinstance(error, re.error) else type(error).__name__}
        records.append({"id": case["id"], "inventory_ids": case["inventory_ids"], "outcome": outcome})
    packages = ["scrapling", "lxml", "cssselect", "w3lib", "orjson"]
    assets = []
    for name, module in sorted(sys.modules.copy().items()):
        path = getattr(module, "__file__", None)
        if path and name.split(".")[0] in packages:
            path = Path(path).resolve()
            if path.is_file():
                assets.append({"module": name, "path": str(path), "sha256": digest(path)})
    versions = {}
    for name in packages:
        try:
            versions[name] = importlib.metadata.version(name)
        except importlib.metadata.PackageNotFoundError:
            versions[name] = "source checkout"
    import lxml.etree
    output = {
        "schema_version": 1, "upstream_revision": fixture["upstream_revision"],
        "fixture_sha256": digest(args.fixture), "parser_path": str(origin),
        "parser_sha256": digest(origin), "python": sys.version,
        "dependencies": versions, "dependency_assets": assets,
        "libxml_version": list(lxml.etree.LIBXML_VERSION),
        "cases": records,
    }
    with args.output.open("x", encoding="utf-8") as output_file:
        json.dump(output, output_file, ensure_ascii=False, indent=2)
        output_file.write("\n")


if __name__ == "__main__":
    main()
