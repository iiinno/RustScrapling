# rustscrapling-daemon — Stage 5

A small localhost service for sharing RustScrapling fetches between CLI
processes. The default build is deliberately independent of the still-unmerged
HTTP implementation in this worktree; inject a `FetchBackend` for tests or
enable **`fetch-integration`** on the integrated branch to use the real
`rustscrapling_fetch::Fetcher` and its optional `BrowserBackend`.

## Public API

```rust,ignore
use rustscrapling_daemon::{DaemonServer, DaemonClient, DaemonConfig, FetchRequest};
use rustscrapling_fetch::{Fetcher, FetchOptions};
use rustscrapling_http::{HttpClient, HttpOptions};

// CLI "daemon start" (compiled with --features fetch-integration):
let http = HttpClient::new(HttpOptions::default())?;
let fetcher = Fetcher::new(http, FetchOptions::default());
// Optionally: fetcher.with_browser(Arc::new(cdp_backend));
let service = DaemonServer::start_with_fetcher(DaemonConfig::default(), fetcher).await?;
service.wait().await?; // keep the command/process running

// CLI "daemon status", "daemon stop", and automatic daemon delegation:
let config = DaemonConfig::default();
if let Some(client) = DaemonClient::discover(&config.state_file).await? {
    let status = client.status().await?;
    let result = client.fetch(FetchRequest::get("https://example.com/")).await?;
    client.stop().await?;
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

The daemon **does not implement the CLI command parser**; the CLI owner can
call these library APIs from `daemon start/stop/status`. `discover()` returns
`None` when no working daemon is present and callers should use direct fetch.
The daemon has no globally fixed port: `DaemonServer::start()` selects a free
loopback port, writes it to the state file, and generates a fresh 256-bit token.

The protocol is one JSON object + newline per TCP connection, authenticated
with `{ "token": "…", "request": { "command": "status" } }`. It supports
`status`, `stop`, and `fetch` (typed `FetchRequest`) and returns a typed
`DaemonResponse`. The daemon listens **only on `127.0.0.1`**. Request and reply
frames are limited to 16 MiB. Credential-bearing state files must not be
published or checked into source control. On Unix, new state directories are
`0700` and state files are `0600`; the daemon refuses an existing directory
with insecure permissions. On Windows, `icacls` restricts its dedicated state
directory to the current account before the state file is created; failure
aborts launch. Existing state files are never overwritten, even when stale.
This avoids accidentally taking control of another daemon. `stop` is
authenticated and removes the state file when the loop exits.

## Request policy

* **GET/HEAD singleflight**: concurrent identical requests (URL, method,
  headers, tier) share one upstream operation. POST/PUT/DELETE always run
  independently.
* **Rate limiting**: each hostname gets a shared monotonic start-to-start
  interval (`site_interval`, default 200 ms). No global lock is held while
  waiting for a site. The upstream fetch has a configurable deadline.
* **Conditional responses**: a successful GET with an ETag and/or
  Last-Modified validator is cached (32 entries, 1 MiB body maximum per entry).
  A later request sends `If-None-Match` and/or `If-Modified-Since` and restores
  the original body and status if the server answers 304. An unmatched 304 is
  reported as `cache_miss`, never fabricated as HTTP 200.
* **Tier learning**: successful HTTP/browser/headed tiers are remembered by
  hostname for 24 hours by default; when an `auto` request arrives, a valid
  learned tier is tried first. An error from that tier triggers an auto-policy
  retry. The shared Fetcher also retains its own learning policy.

The cache, per-site schedule and tier learning are **in-memory only** in this
version. The plan's persistent SQLite knowledge database, cross-restart
learning, network-level authenticated proxies, large-body disk spill, and
throughput comparison against Scrapling's fixed oracle remain unimplemented.
This crate does not claim parity with the original benchmark. The IPC
protocol is local and intentionally simple; further schema versioning and CLI
integration belong to their owning workstreams.

## Tests

`cargo test -p rustscrapling-daemon` uses a local TCP E2E fixture backend.
It verifies multi-client request coalescing, 304 restoration, per-host rate
limiting, TTL expiry, 256-bit authentication, typed status/stop and state-file
cleanup. It needs no external network. An independent integration check of
`--features fetch-integration` requires the HTTP crate implementation to be
merged into the baseline first. The current isolated branch's HTTP crate is
only a placeholder.
