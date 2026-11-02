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
`blocked: Option<BlockKind>`, `network: Vec<NetworkRecord>`. `status` is `None`
for non-HTTP documents or when no matching response was captured. `get()`
reuses one tab across sequential requests; `open_page()` opens independent
tabs. Explicit page APIs include `navigate()`, `source()` (HTML including
open shadow roots where supported), `current_url()`, `evaluate()` and `close()`.
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
Authenticated proxy challenges are not yet handled. Network capture uses the
CDP `Network.responseReceived` and `Network.loadingFailed` events to collect
response status, URL, headers, resource type and failures. Captured request
bodies are not currently retained. `BrowserState` stores cookies through
`Storage.getCookies` / `Storage.setCookies`; localStorage is intentionally not
read automatically because its normal access would require script evaluation.
The JSON files produced by `save_state_file()` may contain session secrets;
store them with appropriate filesystem permissions.

Cloudflare and CAPTCHA checks detect common visible challenge markers and
Cloudflare's 403 server header. This is only a signal of a block, not a
challenge-solving mechanism. Chrome detects site-specific headless behavior;
browser fingerprint resistance, Cloudflare solving, DNS-over-HTTPS, ad-domain
blocklists, authenticated proxy handling and success-rate comparison with the
pinned Scrapling `StealthyFetcher` baseline remain unverified or unimplemented.
No claim of equal or higher success rates is made yet; Plan.md section 5.1's
patchright-driver alternative must be evaluated with the site corpus.

## Verification

`cargo test -p rustscrapling-browser` runs a local HTTP browser integration
test with navigation, DOM+Shadow DOM extraction, page reuse, cookie state,
external CDP attachment and the D-007 command-history assertion. Set
`RUSTSCRAPLING_CHROME_FOR_TESTING` to an installed Chrome for Testing binary to
force that executable; otherwise Windows Chrome/Edge and common Linux Chrome
paths are used. The integration test does not request external websites.
