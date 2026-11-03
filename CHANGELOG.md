# Changelog

All notable changes to RustScrapling. The project has not made a release:
everything below is **Unreleased** on the integration branch, and GA is not
claimed (see `docs/GA_GATES.md`). Upstream reference: Scrapling v0.5
(`95cf9019ab5a5d6f3e9654b7ef1c6a26e29dc5c0`).

## Unreleased

### Added
- CLI `install`: Chrome for Testing into a per-user directory with pinned
  SHA-256/size verification, staging extraction, `--dry-run`, mirrors and an
  install record used by CLI and MCP (Plan C-05).
- CLI `shell`: interactive console with fetch shortcuts, five-page history,
  selector/extraction commands and the upstream `CurlParser` port
  (`uncurl`/`curl2fetcher`) (Plan C-06).
- CLI `mcp` (stdio MCP server) and daemon-hosted `session` commands keeping
  browser/HTTP sessions alive across CLI processes.
- MCP: exact upstream `tools/list` catalog with outputSchema validation; all
  12 `browser_actions`; upstream-identical ARIA snapshots and `aria-ref`
  targets; Convertor-compatible content (ported markdownify); browser
  fetch/session options; every upstream HTTP request argument and session
  cookie jars.
- Spider: rich requests (method/body/headers/cookies/priority/callback/meta/
  `dont_filter`), `on_error` and `on_scraped_item` hooks, robots pacing,
  AutoThrottle, Retry-After, private replay cache.
- Verification: libFuzzer targets (HTML parsing, selectors, adaptive,
  Markdown), CycloneDX SBOM with license audit, Chrome 153/155/156 E2E matrix
  including headed session reuse, same-machine benchmarks against upstream,
  threat model with regression-test mapping, release smoke script.

### Changed
- Browser launches use upstream's effective arguments (Playwright switches,
  Scrapling `DEFAULT_ARGS` + `STEALTH_ARGS`) with merged feature flags
  (D-010, D-011).
- External CDP endpoints must be loopback or listed in
  `RUSTSCRAPLING_CDP_ALLOWED_HOSTS` (D-012).

### Fixed
- Dropped clicks right after navigation in headed Chrome (PaintHolding).
- Adaptive `relocate` deadlock on a sibling-ratio cache miss (fuzzing).
- CSS selector panic on a trailing backslash escape (fuzzing).
- CI real-Chrome test flake caused by the favicon request (Issue #1).
- State files are read with `O_NOFOLLOW`.
