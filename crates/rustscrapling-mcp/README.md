# RustScrapling MCP (stage 7)

This crate uses the official Rust `rmcp` SDK and implements MCP over **stdio**. Start the standalone process with:

```bash
cargo run -p rustscrapling-mcp
```

The process reads newline-delimited MCP JSON-RPC requests from stdin and writes only protocol responses to stdout. Supported requests include `initialize`, `ping`, `tools/list`, and `tools/call`. It registers all **16 upstream tools**, preserving their names and help text. `make_request`, `session_make_request` and HTTP session management use `rustscrapling-http`. `browser_fetch_once`, `browser_open`, `browser_fetch` and session reuse use the CDP browser library. `browser_extract` supports Markdown/HTML/text, `browser_evaluate` supports explicit `isolated_context=false`, `browser_actions` supports wait actions, and network tools offer captured summaries and response headers.

Unsupported options or operations return `isError: true` with machine-readable `error.kind` (`unsupported_option`, `unsupported_operation`, `redirect_policy_unavailable`, etc.). HTTP sessions reuse connections but the current `HttpClient` lacks an automatic cookie jar. Screenshots, ARIA snapshots/refs, full Playwright state (localStorage and IndexedDB), recorded request/response bodies and mouse/keyboard automation currently require additional CDP APIs. A default `follow_redirects="safe"` never follows 3xx redirects without the upstream private-IP verification; these requests return `redirect_policy_unavailable`. The upstream `google_search=true` browser default does not yet add a Google Referer. HTTP body decoding currently supports UTF-8. A browser `status=0` means the CDP layer did not capture an HTTP status.

The tool catalog is frozen in [`src/tools.json`](src/tools.json). It comes from the upstream BSD-3-Clause Scrapling source at fixed commit [`95cf9019ab5a5d6f3e9654b7ef1c6a26e29dc5c0`](https://github.com/D4Vinci/Scrapling/tree/95cf9019ab5a5d6f3e9654b7ef1c6a26e29dc5c0), including the parameter names, required/optional fields, Python defaults, help text, and the 12 discriminated browser action shapes. The source of truth is `scrapling/core/ai/server.py`, with types from `scrapling/core/_types.py` and `scrapling/core/ai/_browser_actions.py`.

To regenerate the snapshot, download those three upstream files at the same commit and invoke:

```bash
python crates/rustscrapling-mcp/tests/make_contract.py \
  <upstream-server.py> crates/rustscrapling-mcp/src/tools.json \
  <upstream-browser-actions.py> <upstream-types.py>
```

`cargo test -p rustscrapling-mcp` includes protocol-level tests that launch the binary, initialize through stdio, enumerate tools, and fetch from a local HTTP server through the real HTTP stack. Runtime tests also cover sessions and (when Chrome exists) CDP browser fetching. Native dependencies, including libxml2, are needed to build the workspace on the chosen platform.

**Remaining integration:** Full upstream `tests/ai/` behavioral parity requires more browser APIs, parameter handling and tests. Streamable HTTP MCP transport and the top-level `rustscrapling mcp` CLI entrypoint are separate tasks; this crate exports `serve_stdio(RustScraplingMcp::default())` for embedding in the single CLI binary.
