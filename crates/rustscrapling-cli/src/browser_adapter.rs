//! Bridges the low-level CDP browser to the shared tiered fetch policy.
//! The default browser Page/FrameManager abstraction is never instantiated.

use std::path::PathBuf;
use std::sync::Arc;

use rustscrapling_browser::{BrowserClient, BrowserGetOptions, LaunchOptions};
use rustscrapling_fetch::{
    BrowserBackend, BrowserFuture, FetchError, FetchErrorKind, FetchResponse, FetchTier,
};
use tokio::sync::Mutex;

pub struct CdpBackend {
    chrome_path: Option<PathBuf>,
    endpoint: Option<String>,
    proxy: Option<String>,
    headless: Mutex<Option<BrowserClient>>,
    headed: Mutex<Option<BrowserClient>>,
}

impl CdpBackend {
    /// Close every browser this backend launched, inside the runtime that owns
    /// it. Relying on `Drop` at process exit left throwaway profiles behind,
    /// because the drop runs after the runtime and its child handles are gone.
    pub async fn shutdown(&self) {
        for slot in [&self.headless, &self.headed] {
            if let Some(client) = slot.lock().await.take() {
                let _ = client.shutdown().await;
            }
        }
    }

    pub fn new(
        chrome_path: Option<PathBuf>,
        endpoint: Option<String>,
        proxy: Option<String>,
    ) -> Option<Arc<Self>> {
        let chrome_path = chrome_path.or_else(chrome_executable);
        if endpoint.is_none() && chrome_path.is_none() {
            return None;
        }
        Some(Arc::new(Self {
            chrome_path,
            endpoint,
            proxy,
            headless: Mutex::new(None),
            headed: Mutex::new(None),
        }))
    }

    async fn ensure_browser(
        &self,
        headed: bool,
    ) -> Result<tokio::sync::MutexGuard<'_, Option<BrowserClient>>, FetchError> {
        let slot = if headed { &self.headed } else { &self.headless };
        let mut guard = slot.lock().await;
        if guard.is_none() {
            let client = if let Some(url) = &self.endpoint {
                BrowserClient::connect(url).await
            } else {
                let path = self
                    .chrome_path
                    .as_ref()
                    .ok_or_else(|| failed("Chrome executable not configured"))?;
                let mut launch = LaunchOptions::new(path);
                launch.headless = !headed;
                launch.proxy = self.proxy.clone();
                launch.extra_args = rustscrapling_browser::upstream_launch_args(!headed);
                BrowserClient::launch(launch).await
            }
            .map_err(|e| failed(&format!("Could not initialize CDP browser: {e}")))?;
            *guard = Some(client);
        }
        Ok(guard)
    }
}

fn failed(message: &str) -> FetchError {
    FetchError {
        kind: FetchErrorKind::Browser,
        message: message.into(),
        hint: "Check --chrome-path, --cdp-url, proxy, and browser permissions".into(),
        status: None,
    }
}

impl CdpBackend {
    /// Upstream StealthyFetcher defaults: Google referrer, 30 s navigation,
    /// and — for browsers launched here (not attached via CDP) — the
    /// headless UA fix and a 1920x1080 viewport.
    fn get_options(&self, headed: bool) -> BrowserGetOptions {
        BrowserGetOptions {
            navigation_timeout: std::time::Duration::from_secs(30),
            capture_network: true,
            settings: self
                .endpoint
                .is_none()
                .then(|| rustscrapling_browser::PageSettings {
                    headless: !headed,
                    viewport: Some((1920, 1080)),
                    ..Default::default()
                }),
            referrer: Some("https://www.google.com/".into()),
        }
    }
}

impl BrowserBackend for CdpBackend {
    fn fetch<'a>(&'a self, url: &'a str, headed: bool) -> BrowserFuture<'a> {
        Box::pin(async move {
            let browser = self.ensure_browser(headed).await?;
            let response = browser
                .as_ref()
                .expect("initialized browser")
                .get(url, self.get_options(headed))
                .await
                .map_err(|e| failed(&format!("CDP navigation failed: {e}")))?;
            if response.blocked.is_some() {
                // Upstream solves the interstitial before reporting a failure, so
                // a Cloudflare page is retried through the same solver the MCP
                // executor uses instead of being refused outright.
                let page = browser
                    .as_ref()
                    .expect("initialized browser")
                    .open_page(false)
                    .await
                    .map_err(|e| failed(&format!("CDP could not open a page: {e}")))?;
                let outcome = match page
                    .navigate(&url, std::time::Duration::from_secs(30))
                    .await
                {
                    Ok(()) => rustscrapling_browser::solve_cloudflare(&page, 30_000.0)
                        .await
                        .map_err(|e| failed(&format!("Cloudflare solve failed: {e}"))),
                    Err(error) => Err(failed(&format!("CDP navigation failed: {error}"))),
                };
                let reloaded = if outcome.is_ok() {
                    Some(browser.as_ref().expect("initialized browser").get(url, self.get_options(headed)).await)
                } else {
                    None
                };
                let _ = page.close().await;
                if let Some(Ok(refreshed)) = reloaded {
                    if refreshed.blocked.is_none() {
                        return Ok(FetchResponse {
                            status: refreshed.status,
                            final_url: refreshed.final_url,
                            headers: refreshed.headers.into_iter().collect(),
                            body: refreshed.body.into_bytes(),
                            tier: if headed { FetchTier::Headed } else { FetchTier::Browser },
                            protocol: Some("CDP".into()),
                        });
                    }
                }
                return Err(FetchError {
                    kind: FetchErrorKind::Blocked,
                    message: "Browser received a CAPTCHA or challenge page".into(),
                    hint: "Site challenge remains unresolved; verify manually".into(),
                    status: response.status,
                });
            }
            let status = response.status.ok_or_else(|| FetchError {
                kind: FetchErrorKind::Transport,
                message: "CDP did not supply an HTTP status for this navigation".into(),
                hint: "Enable network capture or inspect the page".into(),
                status: None,
            })?;
            Ok(FetchResponse {
                status,
                final_url: response.final_url,
                headers: response.headers.into_iter().collect(),
                body: response.body.into_bytes(),
                tier: if headed {
                    FetchTier::Headed
                } else {
                    FetchTier::Browser
                },
                protocol: Some("CDP".into()),
            })
        })
    }
}

fn chrome_executable() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("RUSTSCRAPLING_CHROME_PATH") {
        let executable = PathBuf::from(path);
        if executable.is_file() {
            return Some(executable);
        }
    }
    if let Some(installed) = rustscrapling_browser::installed::installed_chrome() {
        return Some(installed);
    }
    let locations: &[&str] = if cfg!(windows) {
        &[
            "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
            "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
        ]
    } else {
        &[
            "/usr/bin/google-chrome",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/opt/google/chrome/chrome",
        ]
    };
    locations
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
}
