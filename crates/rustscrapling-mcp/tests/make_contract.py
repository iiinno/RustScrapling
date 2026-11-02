"""Extract the stage-7 MCP tool contract from the pinned Scrapling server.

Usage: python tests/make_contract.py upstream-server.py src/tools.json upstream-browser-actions.py upstream-types.py
Source commit: 95cf9019ab5a5d6f3e9654b7ef1c6a26e29dc5c0
Uses Python AST only, so fetching/recording does not import Scrapling.
"""
import ast
import json
import sys
from pathlib import Path


TITLES = {
    "browser_open": "Open browser",
    "browser_save_state": "Save browser state",
    "browser_load_state": "Load browser state",
    "open_request_session": "Open HTTP session",
    "close_session": "Close session",
    "list_sessions": "List sessions",
    "make_request": "Send HTTP request",
    "browser_fetch_once": "Fetch page in browser",
    "browser_fetch": "Fetch in browser session",
    "session_make_request": "Send HTTP request in HTTP session",
    "browser_extract": "Extract page content",
    "browser_actions": "Browser actions",
    "browser_evaluate": "Run JavaScript",
    "browser_network_requests": "List network requests",
    "browser_network_request": "Read network request",
    "browser_screenshot": "Take browser session screenshot",
}

ACTION_SCHEMAS = None
COOKIE_SCHEMA = None


def schema(node):
    if isinstance(node, ast.Subscript):
        name = ast.unparse(node.value)
        if name == "Annotated":
            result = schema(node.slice.elts[0])
            for metadata in node.slice.elts[1:]:
                if isinstance(metadata, ast.Call) and ast.unparse(metadata.func) == "Field":
                    for kw in metadata.keywords:
                        key = {"ge": "minimum", "le": "maximum", "min_length": "minItems", "max_length": "maxItems"}.get(kw.arg)
                        if key:
                            result[key] = ast.literal_eval(kw.value)
            return result
        if name == "NotRequired":
            return schema(node.slice)
        if name == "Union":
            values = node.slice.elts if isinstance(node.slice, ast.Tuple) else [node.slice]
            return {"anyOf": [schema(value) for value in values]}
        if name == "Optional":
            return {"anyOf": [schema(node.slice), {"type": "null"}]}
        if name in ("List", "Sequence", "Set"):
            result = {"type": "array", "items": schema(node.slice)}
            if name == "Set":
                result["uniqueItems"] = True
            return result
        if name in ("Dict", "Mapping"):
            child = node.slice.elts[1] if isinstance(node.slice, ast.Tuple) else None
            return {"type": "object", **({"additionalProperties": schema(child)} if child else {})}
        if name == "Literal":
            values = node.slice.elts if isinstance(node.slice, ast.Tuple) else [node.slice]
            cases = []
            for value in values:
                if isinstance(value, ast.Name) and value.id == "extraction_types":
                    cases.extend(["markdown", "html", "text"])
                else:
                    cases.append(ast.literal_eval(value))
            return {"enum": cases}
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.BitOr):
        return {"anyOf": [schema(node.left), schema(node.right)]}
    value = ast.unparse(node)
    known = {
        "str": {"type": "string"},
        "NonEmptyString": {"type": "string", "minLength": 1},
        "FiniteFloat": {"type": "number"},
        "PositiveInt": {"type": "integer", "minimum": 1},
        "MouseButton": {"enum": ["left", "right", "middle"]},
        "_Target": {"anyOf": [{"type": "string", "minLength": 1}, {"type": "null"}]},
        "_FieldTimeout": {"type": "number", "minimum": 0},
        "int": {"type": "integer"},
        "float": {"type": "number"},
        "NonNegativeFiniteFloat": {"type": "number", "minimum": 0},
        "bool": {"type": "boolean"},
        "None": {"type": "null"},
        "Any": {},
        "Dict": {"type": "object"},
        "List": {"type": "array"},
        "BrowserAction": {"type": "object"},
        "SetCookieParam": {"type": "object"},
        "extraction_types": {"enum": ["text", "html", "markdown"]},
        "SessionExtractionType": {"enum": ["text", "html", "markdown", "snapshot"]},
        "ScreenshotType": {"enum": ["png", "jpeg"]},
        "SessionType": {"enum": ["stealthy", "static"]},
        "SelectorWaitStates": {"enum": ["attached", "detached", "visible", "hidden"]},
        "FollowRedirects": {"anyOf": [{"enum": ["safe", "all", "obeycode", "firstonly"]}, {"type": "boolean"}]},
        "SUPPORTED_HTTP_METHODS": {"enum": ["GET", "POST", "PUT", "DELETE"]},
        "NetworkPart": {"enum": ["summary", "request_headers", "request_body", "response_headers", "response_body"]},
        "_MCPImpersonateType": {"anyOf": [{"type": "string"}, {"type": "array", "items": {"type": "string"}}, {"type": "null"}]},
    }
    if value not in known:
        raise ValueError(f"unhandled parameter annotation: {value}")
    if value == "BrowserAction" and ACTION_SCHEMAS:
        return ACTION_SCHEMAS
    if value == "SetCookieParam" and COOKIE_SCHEMA:
        return COOKIE_SCHEMA
    return dict(known[value])


def parse_cookie(source):
    tree = ast.parse(Path(source).read_text(encoding="utf-8"))
    cookie = next(node for node in tree.body if isinstance(node, ast.ClassDef) and node.name == "SetCookieParam")
    return {"type": "object", "properties": {
        field.target.id: schema(field.annotation)
        for field in cookie.body if isinstance(field, ast.AnnAssign)
    }, "additionalProperties": False}


def parse_actions(source):
    tree = ast.parse(Path(source).read_text(encoding="utf-8"))
    classes = {node.name: node for node in tree.body if isinstance(node, ast.ClassDef)}
    variants = ["_MouseMove", "_MouseClick", "_MouseWheel", "_TextFormField",
                "_CheckboxFormField", "_RadioFormField", "_SelectFormField", "_KeyPress",
                "_DialogAction", "_TimeWait", "_ElementWait", "_LoadWait"]

    def definition(name):
        cls = classes[name]
        properties, required = {}, []
        for parent in cls.bases:
            if isinstance(parent, ast.Name) and parent.id in classes:
                base = definition(parent.id)
                properties.update(base["properties"])
                required.extend(base["required"])
        total_false = any(kw.arg == "total" and isinstance(kw.value, ast.Constant)
                          and kw.value.value is False for kw in cls.keywords)
        for stmt in cls.body:
            if not isinstance(stmt, ast.AnnAssign) or not isinstance(stmt.target, ast.Name):
                continue
            annotation = stmt.annotation
            optional = total_false
            if isinstance(annotation, ast.Subscript) and ast.unparse(annotation.value) == "NotRequired":
                optional = True
                annotation = annotation.slice
            properties[stmt.target.id] = schema(annotation)
            if not optional:
                required.append(stmt.target.id)
            # Pydantic Field(default=..) is an optional parameter's default.
            if isinstance(annotation, ast.Subscript) and ast.unparse(annotation.value) == "Annotated":
                for meta in annotation.slice.elts[1:]:
                    if isinstance(meta, ast.Call) and ast.unparse(meta.func) == "Field":
                        for kw in meta.keywords:
                            if kw.arg == "default":
                                properties[stmt.target.id]["default"] = ast.literal_eval(kw.value)
        return {"type": "object", "properties": properties,
                "required": list(dict.fromkeys(required)), "additionalProperties": False}

    return {"oneOf": [definition(name) for name in variants]}


def description(annotation):
    if not isinstance(annotation, ast.Subscript) or ast.unparse(annotation.value) != "Annotated":
        raise ValueError("parameter lacks annotated description")
    for meta in annotation.slice.elts[1:]:
        if isinstance(meta, ast.Call) and ast.unparse(meta.func) == "Field":
            desc = next((kw.value for kw in meta.keywords if kw.arg == "description"), None)
            if desc is None:
                raise ValueError("parameter has no description")
            return ast.literal_eval(desc), {kw.arg: ast.literal_eval(kw.value)
                                               for kw in meta.keywords if kw.arg in ("ge", "le", "min_length", "max_length")}
    raise ValueError("parameter has no Field")


def extract(source):
    tree = ast.parse(Path(source).read_text(encoding="utf-8"))
    functions = {node.name: node for node in ast.walk(tree)
                 if isinstance(node, ast.AsyncFunctionDef) and node.name in TITLES}
    if set(functions) != set(TITLES):
        raise ValueError("unexpected tool names in pinned source")
    tools = []
    for name, title in TITLES.items():
        node = functions[name]
        inputs, required = {}, []
        args = [arg for arg in node.args.args if arg.arg != "self"]
        defaults = [None] * (len(args) - len(node.args.defaults)) + list(node.args.defaults)
        for arg, default in zip(args, defaults):
            doc, constraints = description(arg.annotation)
            s = schema(arg.annotation)
            s["description"] = doc
            for key, value in constraints.items():
                s[{"ge": "minimum", "le": "maximum", "min_length": "minItems", "max_length": "maxItems"}[key]] = value
            if default is None:
                required.append(arg.arg)
            else:
                s["default"] = ast.literal_eval(default)
            inputs[arg.arg] = s
        docs = (ast.get_docstring(node) or "").strip()
        tools.append({"name": name, "title": title, "description": docs,
                      "inputSchema": {"type": "object", "properties": inputs,
                                      "required": required, "additionalProperties": False}})
    return tools


if __name__ == "__main__":
    if len(sys.argv) > 3:
        ACTION_SCHEMAS = parse_actions(sys.argv[3])
    if len(sys.argv) > 4:
        COOKIE_SCHEMA = parse_cookie(sys.argv[4])
    tools = extract(sys.argv[1])
    Path(sys.argv[2]).write_text(json.dumps(tools, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"extracted {len(tools)} pinned MCP tools with {sum(len(t['inputSchema']['properties']) for t in tools)} parameters")
