# MCP runtime schema Oracle (Scrapling v0.5)

This independent Oracle probes the **actual `tools/list` protocol response**
of the pinned upstream Scrapling v0.5 (`95cf9019ab5a5d6f3e9654b7ef1c6a26e29dc5c0`)
and compares its public tool declarations with RustScrapling's
`crates/rustscrapling-mcp/src/tools.json` catalog.

It is a **schema/catalog audit**. Matching schemas do not prove that the Rust
backends execute those tools, enforce all constraints, or match upstream behavior.
Conversely, a catalog difference is not automatically an executable bug.

## Run

Use a Python installation that already has the upstream `scrapling[ai]`
requirements installed (including **`mcp>=2.0.0`**). The Oracle does not
install packages, download dependencies, launch browsers, or perform HTTP
requests. It imports the fixed checkout and calls the upstream SDK's in-process
`Client.list_tools()` against `ScraplingMCPServer._build_server()`.

From the **isolated RustScrapling worktree**:

```powershell
python oracle/runtime_schema_oracle.py --upstream C:\Users\inoue\Project\scrapling-v05-reference --upstream-python C:\path\to\compatible\python.exe --report docs\compat\MCP_RUNTIME_SCHEMA_REPORT.json
```

For standard-library-only unit tests:

```powershell
python -m unittest discover -s tests -p test_m0_runtime_schema.py -v
```

To additionally run the optional end-to-end discovery test when a compatible
Python environment exists:

```powershell
$env:SCRAPLING_MCP_LIVE_ORACLE = '1'
python -m unittest discover -s tests -p test_m0_runtime_schema.py -v
```

The program verifies the fixed Git HEAD and unchanged files under upstream's
`scrapling/` before importing them. The subprocess verifies that imported
`scrapling.__file__` actually lives in that checkout. It rejects duplicate
JSON object keys, duplicate/malformed tools, and non-finite JSON values.

Tool names are compared as sets. For shared tool names the comparison is
structural, covering `title`, exact `description`, `inputSchema`,
`outputSchema`, and `annotations` when present. Other SDK/transport metadata
outside the Rust tool catalog is not evaluated. Every nested schema key
participates: `required`, `additionalProperties`, `default`, `enum`, `$defs`,
`anyOf`/`oneOf`/`allOf`, `minimum`/`maximum`, `minLength`/`maxLength`,
`minItems`/`maxItems`, `pattern`, `format`, and other extensions. Missing keys
are distinct from JSON `null`; a boolean differs from a number; array order is
retained; **only JSON object key ordering is ignored**. The full report gives
JSON Pointer paths to individual differences. No schema reference expansion or
semantic normalization is performed.

Exit codes: `0` exact catalog match, `1` discovered schema differences, `2`
source/dependency/probe failure. A failed probe cannot produce a parity report.
`--max-diffs` limits console display only; `--report` retains all differences.

## Local environment evidence (2026-11-03)

At implementation time, the existing Windows Python installations did not
satisfy the pinned upstream's MCP SDK requirement:

| Interpreter | Observed `mcp` SDK | Required upstream `mcp` SDK |
| --- | --- | --- |
| Python 3.12 | 1.28.0 | >=2.0.0 |
| Python 3.13 | 1.27.2 | >=2.0.0 |

The Python 3.12 probe failed explicitly with
`RuntimeError: mcp SDK 1.28.0 is incompatible; pinned Scrapling requires mcp>=2.0.0`
(exit code `2` at the Oracle boundary). The pinned upstream `tools/list` was
**not successfully executed** in these environments; no runtime parity claim
follows. Use a compatible existing Python environment to obtain the report.
