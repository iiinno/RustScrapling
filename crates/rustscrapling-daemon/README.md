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

The CLI now uses these library APIs with `rustscrapling daemon start`,
`rustscrapling daemon status` and `rustscrapling daemon stop`. `start` launches
a separate, silent background CLI process and prints a JSON receipt (never
the token). The optional `--direct` on `daemon start` disables environment
proxy handling inside the service. Use `--state-file PATH` on the daemon
subcommand, or set `RUSTSCRAPLING_DAEMON_STATE_FILE` for automatic discovery
by `get` and `auto`. The CLI checks the daemon's state before normal GET/auto;
if no service is available, it runs direct fetch. Authentication/state errors
are reported rather than silently fetching without the daemon. Options not
supported by daemon IPC (e.g. an explicit proxy/CA/timeout override) select
the normal direct path instead of being ignored.

`DaemonClient::discover()` returns `None` when no working daemon is present.
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
aborts launch. Existing custom Windows directories must have been created by
RustScrapling (a small `.rustscrapling-private` marker is used) so an accidental
`--state-file` cannot rewrite ACLs on arbitrary shared directories. Existing
state files are never overwritten, even when stale.
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

* **Restart persistence**: learned tiers and eligible GET responses survive
  daemon shutdown in `<state-file>.sqlite3`. Entries retain their original
  expiry and the same 32-entry/1 MiB body cap applies. Sensitive GETs with
  credentials, query strings or unrecognized headers are kept in memory only;
  responses with `Set-Cookie`, `Cache-Control: private/no-store`, or `Vary`
  are excluded from disk. The database lives in the private state directory,
  uses file mode `0600` on Unix, and rejects symlinks. A damaged database
  returns a typed `storage` startup error and does not overwrite it.
* **Recent daemon requests**: `rustscrapling network search --url-contains /path
  --limit 20` queries the 128 most recent completed upstream fetches through
  authenticated daemon IPC. Results contain URL (without query, fragment or
  credentials), HTTP method, status and fetch tier; no headers or bodies.
  This in-memory record is cleared when the daemon stops. It records daemon
  fetches only, not browser subrequests or a persistent browser session.
  CLI `session open/close/list/save-state/load-state` currently return a typed
  `unsupported` error until a persistent browser session host exists.

Per-site rate-limit schedules are still in memory. Network-level authenticated
proxies, large-body disk spill, and throughput comparison against Scrapling's
fixed oracle remain unimplemented.
This crate does not claim parity with the original benchmark. The IPC
protocol is local and intentionally simple; further schema versioning and
cross-process session persistence require additional work.

## Tests

`cargo test -p rustscrapling-daemon` uses a local TCP E2E fixture backend.
It verifies multi-client request coalescing, 304 restoration, per-host rate
limiting, TTL expiry, 256-bit authentication, typed status/stop and state-file
cleanup. It also verifies restart persistence, sensitive-request exclusion,
expired/corrupt entries and damaged-database handling. It needs no external
network. `cargo test -p rustscrapling-cli
--test daemon_commands` uses the real CLI binary, a private background daemon
process and local HTTP fixture for conditional GET, `auto`, override handling,
and fallback after stop. The CLI dependency enables `fetch-integration` on
the now-integrated HTTP crate. Authenticated-proxy and production-site
success-rate tests remain outstanding.
