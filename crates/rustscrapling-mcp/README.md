# RustScrapling MCP (stage 7 foundation)

This crate uses the official Rust `rmcp` SDK and implements MCP over **stdio**. Start the standalone process with:

```bash
cargo run -p rustscrapling-mcp
```

The process reads newline-delimited MCP JSON-RPC requests from stdin and writes only protocol responses to stdout. Supported requests include `initialize`, `ping`, `tools/list`, and `tools/call`. It registers all **16 upstream tools**. `list_sessions` accurately reports an empty collection until a session backend is connected; all network, browser, session creation, screenshot, network capture, and extraction calls currently return `isError: true` with a JSON `error.kind: backend_unavailable` object. Invalid arguments return `invalid_arguments` and never trigger a network request.

The tool catalog is frozen in [`src/tools.json`](src/tools.json). It comes from the upstream BSD-3-Clause Scrapling source at fixed commit [`95cf9019ab5a5d6f3e9654b7ef1c6a26e29dc5c0`](https://github.com/D4Vinci/Scrapling/tree/95cf9019ab5a5d6f3e9654b7ef1c6a26e29dc5c0), including the parameter names, required/optional fields, Python defaults, help text, and the 12 discriminated browser action shapes. The source of truth is `scrapling/core/ai/server.py`, with types from `scrapling/core/_types.py` and `scrapling/core/ai/_browser_actions.py`.

To regenerate the snapshot, download those three upstream files at the same commit and invoke:

```bash
python crates/rustscrapling-mcp/tests/make_contract.py \
  <upstream-server.py> crates/rustscrapling-mcp/src/tools.json \
  <upstream-browser-actions.py> <upstream-types.py>
```

`cargo test -p rustscrapling-mcp` includes protocol-level tests that launch the binary, initialize through stdio, enumerate tools, check descriptions, and exercise successful and failed calls. `cargo clippy -p rustscrapling-mcp --all-targets -- -D warnings` is also supported.

**Remaining integration:** The feature executors from stages 3–5 must be wired to the MCP dispatch, followed by upstream `tests/ai/` behavioral coverage. The browser snapshot/ref, storage state, network log, actions, and screenshot APIs are not implemented in this isolated branch. HTTP MCP transport and the `rustscrapling mcp` top-level CLI command also remain for integration; this stage provides a standalone `rustscrapling-mcp` executable.
