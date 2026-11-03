# Pinned Scrapling v0.5 MCP `tools/list` wire-schema comparison

Captured 2026-11-03 using actual in-process MCP protocol discovery against
the fixed upstream checkout. The full, unnormalized path-by-path result is
[`MCP_RUNTIME_SCHEMA_LIVE_V05.json`](MCP_RUNTIME_SCHEMA_LIVE_V05.json).

| Provenance | Value |
| --- | --- |
| Upstream Scrapling Git SHA | `95cf9019ab5a5d6f3e9654b7ef1c6a26e29dc5c0` |
| Python | 3.13.14 (dedicated venv) |
| MCP SDK | 2.3.0 |
| Pydantic | 2.14.2 |
| Upstream tool count | 16 |
| Rust `tools.json` tool count | 16 |
| Missing/extra tool names | 0 / 0 |
| Tools with differing wire catalog fields | **0** (was 16 before `tools.json` adopted the live catalog) |
| Individual strict JSON Pointer differences | **0** (was 348) |

**Update (後継セッション):** the original capture found 348 structural
differences (Pydantic `$defs`/`$ref`/`const`/`title`, missing `outputSchema`
and annotation metadata) while names, titles and descriptions already agreed.
`crates/rustscrapling-mcp/src/tools.json` now stores the pinned upstream live
`tools/list` entries verbatim, the Rust validator resolves local `$ref`/`$defs`,
`const`, discriminated `oneOf` and exclusive bounds, and every structured tool
result is checked against the advertised `outputSchema` before it is sent. The
re-captured report (Linux, Python 3.13.18, MCP SDK 2.3.0) is an exact match and
the oracle exits 0. Wire-schema equality still says nothing about executor
behaviour; that is covered by the separate runtime/E2E tests.

## Reproduce

The report was collected with a newly created Python virtual environment,
without changing global packages or downloading browser executables:

```powershell
py -3.13 -m venv C:\Users\inoue\Project\scrapling-v05-runtime-venv
C:\Users\inoue\Project\scrapling-v05-runtime-venv\Scripts\python.exe -m pip install -e 'C:\Users\inoue\Project\scrapling-v05-reference[ai]'
C:\Users\inoue\Project\scrapling-v05-runtime-venv\Scripts\python.exe oracle/runtime_schema_oracle.py --upstream C:\Users\inoue\Project\scrapling-v05-reference --upstream-python C:\Users\inoue\Project\scrapling-v05-runtime-venv\Scripts\python.exe --report docs\compat\MCP_RUNTIME_SCHEMA_LIVE_V05.json --max-diffs 0
```

The final command exits **1 by design**, because its strict comparison finds
catalog differences. Exit 0 indicates exact catalog match; exit 2 indicates
that the runtime probe could not complete. The verified source checkout must
have the exact pinned Git SHA; the shared `verify_source_tree()` rejects
modified/untracked `scrapling/**/*.py`, while allowing generated `__pycache__`
and `.pyc` files so repeated probes remain valid.

Tests:

```powershell
python -m unittest discover -s tests -p test_m0_runtime_schema.py -v
$env:SCRAPLING_MCP_LIVE_ORACLE = '1'
C:\Users\inoue\Project\scrapling-v05-runtime-venv\Scripts\python.exe -m unittest discover -s tests -p test_m0_runtime_schema.py -v
```

The opt-in live test compares the generated JSON diff to the committed snapshot
when the SDK version and Rust catalog SHA-256 match the captured environment.
The fixture intentionally excludes authenticated request data and session
secrets; it contains tool declarations only. The report's source path is
repository-relative to avoid leaking a workstation-specific absolute path.
