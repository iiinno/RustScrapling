# RustScrapling

Rust-native web acquisition and extraction for research agents, following [Plan.md](Plan.md) (v4, production-first; the GA contract is frozen in docs/PLAN_GA_CONTRACT.md). It uses the original Python Scrapling only for differential testing; production code is Rust.

> **Development status:** This is an integration build, not a claim that all Plan.md stage-8 requirements are complete. In particular, bot-evasion success rates, the 200-URL corpus, 24-hour stability test and some compatibility/performance thresholds need further evidence. See [docs/RESULTS.md](docs/RESULTS.md), [docs/QA_AUDIT.md](docs/QA_AUDIT.md), and [docs/INTEGRATION.md](docs/INTEGRATION.md).

## Building

Recommended development environment: Ubuntu 24.04, Rust 1.99.1+, Clang and libxml2 development headers.

```sh
sudo apt-get install -y build-essential pkg-config libxml2-dev clang
cargo build --release --locked
cargo test --workspace --locked --lib --tests
```

The Windows native build requires a libxml2 installation discoverable by the `libxml` crate, and NASM for the bundled BoringSSL build. Building on WSL Ubuntu is a supported development path. Where available, Chrome for Testing is recommended for reproducible CDP tests; you can use an existing browser-level websocket with `--cdp-url` instead.

## CLI

The CLI defaults to versioned JSON output and returns typed JSON errors on stderr. Common commands:

```sh
rustscrapling select page.html --css 'article h1' --compat
rustscrapling select page.html --xpath '//article//a/@href' --format ndjson
rustscrapling select page.html --css '.product' --id product --auto-save --url https://example.com
rustscrapling relocate changed.html --id product --compat --url https://example.com
rustscrapling get https://example.com --format json
rustscrapling md page.html --format md
rustscrapling auto https://example.com --format md
rustscrapling fetch https://example.com --chrome-path /path/to/chrome
rustscrapling crawl examples/crawl.toml --out pages.jsonl
rustscrapling install --dry-run            # Chrome for Testing, pinned SHA-256
rustscrapling install
rustscrapling shell                        # interactive console (scrapling shell)
rustscrapling shell -c $'get https://example.com\ncss h1::text'
rustscrapling mcp                          # upstream-compatible MCP server (stdio)
rustscrapling daemon start && rustscrapling session open --session-id s1
```

`install` downloads Chrome for Testing into a per-user directory and verifies its size and SHA-256 against the pinned manifest (or `--sha256`); it never installs OS packages and reports missing shared libraries instead. The CLI and MCP use that browser after explicit configuration (`--chrome-path`, `RUSTSCRAPLING_CHROME_PATH`) and before system browsers. `shell` offers `get/post/put/delete/fetch/load`, `page`/`pages`, `css/xpath/find-text/re/similar/selector-gen`, `text/md/html`, `uncurl`/`curl2fetcher` and `view`; type `help` for details (differences from the IPython-based upstream shell are listed in [docs/COMPAT.md](docs/COMPAT.md)).

`--out FILE` saves the full output and prints a compact JSON receipt; `--max-chars N` limits the standard output without cutting JSON in the middle of a token. Network commands use `wreq` with certificate verification enabled. If HTTP/3 fails or is unsupported behind a proxy, the negotiated `protocol` records the actual HTTP/2 or HTTP/1.x outcome.

The browser path uses a low-level Chrome DevTools Protocol transport. It does **not** automatically send `Runtime.enable` when reading the DOM (decision D-007), but this alone does not guarantee resistance to bot detection. Browser sessions are separate from a user's normal Chrome profile. Unsupported authentication/DoH and Cloudflare-solver features must not be assumed to work.

## Crawler specification

```toml
start_urls = ["https://example.com/"]
allowed_domains = ["example.com"]
max_pages = 150
max_depth = 2
concurrency = 8
delay_ms = 200
auto_throttle = true
obey_robots = true
crawldir = ".rustscrapling-crawl"
```

`obey_robots` defaults to false to preserve the original tool's behavior. Enabling it checks robots.txt, with a conservative failure policy. `auto_throttle` also defaults to false; enable it for per-host adaptive pacing based on observed latency and throttling errors such as 429/503. The configured `delay_ms` remains the lower bound for the adaptive interval. The crawler emits NDJSON incrementally and writes compact visited-URL fingerprints and pending URLs to its checkpoint directory. Do not assume a live network crawl has exactly-once output semantics under sudden process failure without verifying the resumed output.

## Verification

The `tests/e2e_local.py` suite serves test pages on `127.0.0.1` without external network access. It verifies CLI HTTP, Markdown, selection, POST, typed errors and crawl resume:

```sh
cargo build -p rustscrapling-cli --locked
python3 tests/e2e_local.py target/debug/rustscrapling
```

The pinned upstream Scrapling commit is `95cf9019ab5a5d6f3e9654b7ef1c6a26e29dc5c0`; reference fixtures and the upstream-compatible SQLite files live in `fixtures/`. Actual performance and bot-detection claims must be established against that exact commit on the same machine and day, as specified in the plan.

## License

Project code is BSD-3-Clause. See [NOTICE](NOTICE) and audit third-party dependency licenses before redistribution.
