# rustscrapling-browser (stage 4)

RustScrapling browser engine using the CDP types from `chromiumoxide_cdp 0.9.1`
and a direct asynchronous WebSocket transport. The `BrowserClient` launch path
starts a Chrome/Chrome for Testing executable with its own temporary profile;
`connect()` attaches to a browser-level CDP WebSocket owned by another process.

```rust,no_run
use rustscrapling_browser::{BrowserClient, BrowserGetOptions, LaunchOptions};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let browser = BrowserClient::launch(
    LaunchOptions::new(r"C:\path\to\chrome-for-testing\chrome.exe")
).await?;

let response = browser.get(
    "https://example.com", BrowserGetOptions::default()
).await?;
println!("{}: {}", response.status.unwrap_or(0), response.body.len());

browser.shutdown().await?;
# Ok(())
# }
```

`BrowserResponse` includes `status: Option<u16>`, `final_url`, `body`, `headers`,
`blocked: Option<BlockKind>`, `network: Vec<NetworkRecord>`. `NetworkRecord` also
includes a page-session-scoped CDP `request_id`, MIME type, and optional captured
textual response `body`. `status` is `None`
for non-HTTP documents or when no matching response was captured. `get()`
reuses one tab across sequential requests; `open_page()` opens independent
tabs. Explicit page APIs include `navigate()`, `source()` (HTML including
open shadow roots where supported), `current_url()`, `network_records()` for a
snapshot, `search_network_bodies(text)` for case-sensitive body searches,
`take_network()` for consume-and-clear, `evaluate()` and `close()`.
`commands()` returns the CDP methods sent by that client for auditing.

## D-007: Runtime.enable

The default and stealth paths send `Target.createTarget`,
`Target.attachToTarget`, `Page.enable`, `Network.enable` (when requested),
`Page.navigate`, `DOM.getDocument`, `DOM.getOuterHTML`, and
`Page.getFrameTree` directly. No `chromiumoxide::Page` or `FrameManager`
is constructed: those enable Runtime unconditionally in version 0.9.1.
`Runtime.enable` is sent **only** by the explicit `BrowserPage::evaluate()`
method, immediately followed by `Runtime.evaluate`. There is no global
script injection or stealth fingerprint claim.

## Browser lifecycle and network

The spawned Chrome uses a random loopback debugging port and an independent
temporary user-data directory. `shutdown()` terminates it and removes the
directory. External `connect()` accepts loopback `ws://` endpoints and remote
`wss://` endpoints. Unencrypted remote CDP connections require a local tunnel;
shutting down an attached client leaves the external browser running.

`LaunchOptions::proxy` accepts Chromium's `--proxy-server` endpoint syntax.
Authenticated proxy challenges are not yet handled. Network capture correlates
`Network.responseReceived`, `Network.loadingFinished` and `Network.loadingFailed`
by `requestId` **within the current page's CDP session**. After a completed
small XHR/Fetch response, `Network.getResponseBody` can retain a JSON or
`text/plain` API response without enabling Runtime. A body is unavailable when
CDP rejects the request, it is base64/binary, missing, too large, unsupported,
or has authentication-related response headers. Records are capped at 128 per
page, bodies at 64 KiB each and 1 MiB combined, with a 2-second total body-fetch
deadline per snapshot. `take_network()` clears the buffered history and returns
the captured records; late requests require another snapshot. Only
Content-Type, Content-Length and Server headers are retained. URLs lose
userinfo, query and fragment; sensitive authentication paths are obscured.
Common credential-shaped JSON keys/values are redacted, and suspicious plain
text responses are dropped. These are conservative heuristics, **not** a
guarantee that arbitrary application response data is free of secrets. Request
bodies are not retained. The current MCP network tools expose summary and
response headers only; response-body access there requires a separate MCP
integration change. `BrowserState` stores cookies through
`Storage.getCookies` / `Storage.setCookies`. `save_state_for_page` captures
the active top-level HTTP(S) origin's localStorage via its CDP storage key,
without Runtime or script evaluation; other origins/frames are excluded.
The JSON files produced by `save_state_file()` may contain session secrets.
This API creates files exclusively (existing files and symlinks are not overwritten),
restricts serialized state and reads to 4 MiB, and saves with `0600` on Unix.
The caller remains responsible for selecting a private directory. Restoring
version 1 state replaces current cookies only; version 2 state may also restore
localStorage after verifying the active top-level origin. sessionStorage and
IndexedDB are not represented. `BrowserPage::screenshot()` captures the viewport or full
page as base64 PNG/JPEG via `Page.captureScreenshot`, bounded to 32 MP for
full pages and 8 MiB encoded output. Neither method enables Runtime.

Cloudflare and CAPTCHA checks detect common visible challenge markers and
Cloudflare's 403 server header. This is only a signal of a block, not a
challenge-solving mechanism. Chrome detects site-specific headless behavior;
browser fingerprint resistance, Cloudflare solving, DNS-over-HTTPS, ad-domain
blocklists, authenticated proxy handling and success-rate comparison with the
pinned Scrapling `StealthyFetcher` baseline remain unverified or unimplemented.
No claim of equal or higher success rates is made yet; Plan.md section 5.1's
patchright-driver alternative must be evaluated with the site corpus.

## Verification

`cargo test -p rustscrapling-browser` includes a CDP WebSocket mock checking
network response-body correlation, redaction, type/size limits and session
isolation, plus a bounded-history unit test. It also runs a local HTTP browser integration
test with navigation, DOM+Shadow DOM extraction, page reuse, cookie state,
external CDP attachment and the D-007 command-history assertion. Set
`RUSTSCRAPLING_CHROME_FOR_TESTING` to an installed Chrome for Testing binary to
force that executable; otherwise Windows Chrome/Edge and common Linux Chrome
paths are used. The integration test does not request external websites.
