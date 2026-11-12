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
                let path = self.chrome_path.as_ref().ok_or_else(|| {
                    failed(
                        FetchErrorKind::BrowserUnavailable,
                        "Chrome executable not configured",
                    )
                })?;
                let mut launch = LaunchOptions::new(path);
                launch.headless = !headed;
                launch.proxy = self.proxy.clone();
                BrowserClient::launch(launch).await
            }
            .map_err(|_| {
                failed(
                    FetchErrorKind::BrowserUnavailable,
                    "Could not initialize the configured CDP browser",
                )
            })?;
            *guard = Some(client);
        }
        Ok(guard)
    }
}

fn failed(kind: FetchErrorKind, message: &str) -> FetchError {
    FetchError {
        kind,
        message: message.into(),
        hint: "Check --chrome-path, --cdp-url, proxy, and browser permissions".into(),
        status: None,
    }
}

impl BrowserBackend for CdpBackend {
    fn fetch<'a>(&'a self, url: &'a str, headed: bool) -> BrowserFuture<'a> {
        Box::pin(async move {
            let browser = self.ensure_browser(headed).await?;
            let response = browser
                .as_ref()
                .expect("initialized browser")
                .get(url, BrowserGetOptions::default())
                .await
                .map_err(|_| failed(FetchErrorKind::Browser, "CDP navigation failed"))?;
            if response.blocked.is_some() {
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
