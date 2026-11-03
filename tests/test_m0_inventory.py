"""Standard-library M0 tests: do not need Scrapling/Pydantic/Rust to run."""

from __future__ import annotations

import ast
import json
import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "oracle"))

from audit_parity import (  # noqa: E402
    action_schema,
    annotated_field_constraints,
    audit,
    canonical_backend_hash,
    canonical_catalog_hash,
    compare_args,
    compare_actions,
    executor_action_branches,
    markdown,
    observed_constraint,
)
from inventory_upstream import (  # noqa: E402
    UPSTREAM_SHA,
    action_variants,
    cli_commands,
    extract_literal_values,
    parameters,
    typed_dict_classes,
)
from rust_api_index import PUBLIC_FUNCTION, index_rust_functions, same_name_candidates  # noqa: E402


class InventoryAstTests(unittest.TestCase):
    def test_pydantic_annotations_map_to_list_and_optional_bounds(self) -> None:
        actions = 'Annotated[List[BrowserAction], Field(min_length=1, description="actions")]'
        expected = annotated_field_constraints(actions)
        self.assertEqual(expected["min_length"], 1)
        self.assertEqual(observed_constraint({"type": "array", "minItems": 1}, "min_length", actions), 1)
        quality = 'Annotated[Optional[Annotated[int, Field(ge=0, le=100)]], Field(description="JPEG quality")]'
        self.assertEqual(annotated_field_constraints(quality)["minimum"], 0)
        schema = {"anyOf": [{"type": "integer", "minimum": 0, "maximum": 100}, {"type": "null"}]}
        self.assertEqual(observed_constraint(schema, "minimum", quality), 0)
        self.assertEqual(observed_constraint(schema, "maximum", quality), 100)
        schema["anyOf"][0]["maximum"] = 99
        got = compare_args([{"name": "quality", "annotation": quality, "has_default": True, "default": "None"}], {"properties": {"quality": {**schema, "description": "JPEG quality", "default": None}}})
        self.assertEqual([(d["parameter"], d["constraint"]) for d in got["constraint_diffs"]], [("quality", "maximum")])

    def test_rust_function_scan_is_only_an_unverified_candidate(self) -> None:
        example = 'pub fn css(input: &str) -> String { todo!() }\n pub(crate) async fn get() {}\nfn hidden() {}\npub async fn fetch<T>(x: T) {}'
        self.assertEqual(PUBLIC_FUNCTION.findall(example), ["css", "get", "fetch"])
        symbols = {"css": [{"crate": "rustscrapling-select", "source": "src/x.rs", "line": 3}]}
        self.assertEqual(len(same_name_candidates({"id": "Selector.css"}, symbols, "rustscrapling-select")), 1)
        self.assertEqual(same_name_candidates({"id": "Selector.other"}, symbols, None), [])

    def test_preserves_argument_kinds_defaults_and_unpack(self) -> None:
        fn = ast.parse("def f(a, /, b: int = 2, *args, flag=False, **kw: Unpack[Opts]): pass").body[0]
        items = parameters(fn)
        self.assertEqual([i["name"] for i in items], ["a", "b", "args", "flag", "kw"])
        self.assertEqual([i["kind"] for i in items], ["positional_only", "positional_or_keyword", "var_positional", "keyword_only", "var_keyword"])
        self.assertEqual(items[1]["default"], "2")
        self.assertEqual(items[3]["default"], "False")
        self.assertEqual(items[4]["annotation"], "Unpack[Opts]")

    def test_inherited_typed_dict_optional_and_required_fields(self) -> None:
        tree = ast.parse("""
class Base(TypedDict, total=False):
    a: int
    x: Required[str]
class Child(Base):
    b: int
    c: NotRequired[Annotated[int, Field(default=3)]]
""")
        parsed = typed_dict_classes(tree, "synthetic.py")
        fields = {f["name"]: f for f in parsed["Child"]["fields_resolved"]}
        self.assertFalse(fields["a"]["required"])
        self.assertTrue(fields["x"]["required"])
        self.assertTrue(fields["b"]["required"])
        self.assertFalse(fields["c"]["required"])
        self.assertEqual(fields["c"]["default"], "3")

    def test_action_union_and_literal_identification(self) -> None:
        tree = ast.parse('BrowserAction = Annotated[Union[_A, _B], Field(discriminator="type")]')
        self.assertEqual(action_variants(tree), ["_A", "_B"])
        annotation = ast.parse('x: Annotated[Literal["click"], Field(description="target")]').body[0].annotation
        self.assertEqual(extract_literal_values(annotation), ["click"])

    def test_click_options_from_shared_decorator_are_expanded(self) -> None:
        tree = ast.parse('''
def _shared_options(f):
    decorators = [option("--timeout", default=30), option("--proxy")]
    for decorator in decorators:
        f = decorator(f)
    return f

@command()
@argument("url", required=True)
@_shared_options
def get(url, timeout, proxy):
    pass
''')
        (get,) = cli_commands(tree)
        self.assertEqual(get["expanded_decorator_factories"], ["_shared_options"])
        self.assertEqual(len(get["decorator_options"]), 3)
        self.assertEqual(get["inherited_or_unknown_decorators"], [])
        self.assertTrue(get["requires_click_runtime_check"])

    def test_detects_missing_extra_and_wrong_defaults(self) -> None:
        upstream = [
            {"name": "self", "has_default": False, "default": None},
            {"name": "url", "has_default": False, "default": None},
            {"name": "timeout", "has_default": True, "default": "30"},
        ]
        got = compare_args(upstream, {"properties": {"timeout": {"default": 40}, "unwanted": {}}})
        self.assertEqual(got["missing_rust"], ["url"])
        self.assertEqual(got["extra_rust"], ["unwanted"])
        self.assertEqual(got["default_diffs"][0]["parameter"], "timeout")

    def test_action_schema_and_required_diffs(self) -> None:
        browser = {"properties": {"actions": {"items": {"oneOf": [
            {"properties": {"type": {"enum": ["checkbox"]}, "value": {}}, "required": ["type"]}
        ]}}}}
        got = action_schema(browser)
        self.assertEqual(list(got), ["checkbox"])
        rows = compare_actions([{"type": "checkbox", "source": "x", "line": 1, "fields": [
            {"name": "type", "required": True, "default": None},
            {"name": "value", "required": True, "default": None},
            {"name": "target", "required": False, "default": None},
        ]}], got)
        self.assertEqual(rows[0]["missing_fields"], ["target"])
        self.assertEqual(rows[0]["missing_required"], ["value"])

    def test_executor_scan_does_not_call_schema_support_verified(self) -> None:
        source = '''async fn browser_actions(&self) {
            match action_type {
                "wait_time" => { Ok(()) }
                "click" => { Ok(()) }
                _ => Err(BackendError::new("unsupported_operation", "x")),
            }
        }'''
        self.assertEqual(executor_action_branches(source), {"click", "wait_time"})


class PinnedSnapshotTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.root = ROOT / "docs" / "compat"
        cls.api = json.loads((cls.root / "UPSTREAM_API_V05.json").read_text(encoding="utf-8"))
        cls.mcp = json.loads((cls.root / "MCP_ACTIONS_V05.json").read_text(encoding="utf-8"))
        cls.report = json.loads((cls.root / "PARITY_AUDIT.json").read_text(encoding="utf-8"))

    def test_pinned_sha_and_public_names(self) -> None:
        self.assertEqual(self.api["upstream_sha"], UPSTREAM_SHA)
        self.assertEqual(self.mcp["upstream_sha"], UPSTREAM_SHA)
        self.assertEqual(self.mcp["tool_count"], 16)
        self.assertEqual(self.mcp["action_count"], 12)
        self.assertEqual(len(self.api["api_methods"]), len({m["id"] for m in self.api["api_methods"]}))
        self.assertEqual(len(self.api["api_methods"]), 196)
        self.assertEqual(len(self.api["inherited_methods"]), 130)
        self.assertTrue(any(m["id"] == "Selector.generate_css_selector" for m in self.api["inherited_methods"]))
        self.assertTrue(any(m["id"] == "Response.css" for m in self.api["inherited_methods"]))
        self.assertTrue(any(a["alias"] == "SiteToMarkdownSpider" and a["target"] == "ContentSpider" for a in self.api["exported_aliases"]))
        self.assertEqual(len(self.api["cli_commands"]), 10)
        self.assertTrue(any(c["expanded_decorator_factories"] for c in self.api["cli_commands"]))
        self.assertEqual(len(self.report["methods"]), len(self.api["api_methods"]))
        self.assertEqual(len(self.report["inherited_methods"]), len(self.api["inherited_methods"]))
        self.assertEqual(len(self.report["module_functions"]), len(self.api["module_functions"]))
        self.assertFalse(any(item["id"] == "scrapling.parser._escape_css_string" for item in self.api["module_functions"]))
        self.assertTrue(any(item["source"] == "scrapling/cli.py" and item["id"] == "scrapling.cli.install" for item in self.api["module_functions"]))
        self.assertTrue(all(not item["id"].rsplit(".", 1)[-1].startswith("_") for item in self.api["module_functions"]))

    def test_rust_catalog_and_rust_backend_unchanged(self) -> None:
        tool_bytes = (ROOT / "crates" / "rustscrapling-mcp" / "src" / "tools.json").read_bytes()
        backend_bytes = (ROOT / "crates" / "rustscrapling-mcp" / "src" / "backend.rs").read_bytes()
        current = audit(self.api, self.mcp, json.loads(tool_bytes), tool_bytes, backend_bytes, index_rust_functions(ROOT))
        self.assertEqual(current, self.report)
        self.assertEqual((self.root / "PARITY_MATRIX.md").read_text(encoding="utf-8"), markdown(current))

    def test_statuses_not_marked_verified_by_schema_presence(self) -> None:
        self.assertTrue(all(t["runtime_status"] == "unverified" for t in self.report["mcp"]))
        present = {a["name"] for a in self.report["actions"] if a["executor_branch_present"]}
        self.assertEqual(present, {"wait_time", "click", "textbox", "press_key"})
        self.assertTrue(all(a["runtime_status"] != "verified" for a in self.report["actions"]))
        self.assertTrue(all(x["status"] != "verified" for x in self.report["methods"]))

    def test_crlf_vs_lf_checkouts_share_audit_fingerprints(self) -> None:
        backend = b"async fn browser_actions() {\n match action_type {\n \"click\" => { Ok(()) }\n _ => Err(BackendError::new(\"x\", \"y\"))\n }}"
        self.assertEqual(canonical_backend_hash(backend), canonical_backend_hash(backend.replace(b"\n", b"\r\n")))
        self.assertEqual(canonical_catalog_hash(b'{"a":1,"b":2}\n'), canonical_catalog_hash(b'{\r\n "b": 2, "a": 1\r\n}'))


if __name__ == "__main__":
    unittest.main()
