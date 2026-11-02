//! Shared acquisition policy: pooled HTTP, optional CDP browser fallback,
//! and a 24-hour per-host success memory. This is a library for the daemon
//! and CLI, not an HTTP server.

use rustscrapling_http::{
    ErrorKind as HttpErrorKind, HttpClient, HttpError, HttpRequest, HttpResponse,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
use url::Url;

pub type BrowserFuture<'a> =
    Pin<Box<dyn Future<Output = Result<FetchResponse, FetchError>> + Send + 'a>>;

/// A CDP backend can implement this without owning the fetch policy.
pub trait BrowserBackend: Send + Sync {
    fn fetch<'a>(&'a self, url: &'a str, headed: bool) -> BrowserFuture<'a>;
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FetchTier {
    Auto,
    Http,
    Browser,
    Headed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchResponse {
    pub status: u16,
    pub final_url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub tier: FetchTier,
    pub protocol: Option<String>,
}
impl FetchResponse {
    pub fn blocked(&self) -> bool {
        self.status == 403 || self.status == 429 || is_challenge(&self.body)
    }
    pub fn success(&self) -> bool {
        (200..300).contains(&self.status) && !self.blocked()
    }
}
impl From<HttpResponse> for FetchResponse {
    fn from(r: HttpResponse) -> Self {
        Self {
            status: r.status,
            final_url: r.final_url,
            headers: r.headers,
            body: r.body,
            tier: FetchTier::Http,
            protocol: Some(r.http_version),
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FetchErrorKind {
    InvalidRequest,
    Timeout,
    Dns,
    Tls,
    Proxy,
    Blocked,
    HttpStatus,
    BodyTooLarge,
    BrowserUnavailable,
    Browser,
    Transport,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchError {
    pub kind: FetchErrorKind,
    pub message: String,
    pub hint: String,
    pub status: Option<u16>,
}
impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for FetchError {}
impl From<HttpError> for FetchError {
    fn from(err: HttpError) -> Self {
        let kind = match err.kind {
            HttpErrorKind::InvalidRequest => FetchErrorKind::InvalidRequest,
            HttpErrorKind::Timeout => FetchErrorKind::Timeout,
            HttpErrorKind::Dns => FetchErrorKind::Dns,
            HttpErrorKind::Tls => FetchErrorKind::Tls,
            HttpErrorKind::Proxy => FetchErrorKind::Proxy,
            HttpErrorKind::HttpStatus => FetchErrorKind::HttpStatus,
            HttpErrorKind::Blocked => FetchErrorKind::Blocked,
            HttpErrorKind::BodyTooLarge => FetchErrorKind::BodyTooLarge,
            HttpErrorKind::UnsupportedProtocol | HttpErrorKind::Transport => {
                FetchErrorKind::Transport
            }
        };
        Self {
            kind,
            message: err.to_string(),
            hint: match kind {
                FetchErrorKind::Blocked => "try --tier browser",
                FetchErrorKind::Timeout => "increase --timeout",
                FetchErrorKind::Tls => "check trusted CA bundle",
                FetchErrorKind::Proxy => "check proxy URL and credentials",
                _ => "check request and network",
            }
            .into(),
            status: err.status,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FetchOptions {
    pub learning_ttl: Duration,
    pub detect_blocks: bool,
}
impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            learning_ttl: Duration::from_secs(86400),
            detect_blocks: true,
        }
    }
}
#[derive(Clone, Copy)]
struct Learned {
    tier: FetchTier,
    expires: Instant,
}

#[derive(Clone)]
pub struct Fetcher {
    http: HttpClient,
    browser: Option<Arc<dyn BrowserBackend>>,
    options: FetchOptions,
    learned: Arc<Mutex<HashMap<String, Learned>>>,
}
impl Fetcher {
    pub fn new(http: HttpClient, options: FetchOptions) -> Self {
        Self {
            http,
            browser: None,
            options,
            learned: Arc::new(Mutex::new(HashMap::new())),
        }
    }
    pub fn with_browser(mut self, backend: Arc<dyn BrowserBackend>) -> Self {
        self.browser = Some(backend);
        self
    }
    pub async fn learned_tier(&self, url: &str) -> Option<FetchTier> {
        let domain = domain(url).ok()?;
        let mut map = self.learned.lock().await;
        match map.get(&domain).copied() {
            Some(entry) if entry.expires > Instant::now() => Some(entry.tier),
            Some(_) => {
                map.remove(&domain);
                None
            }
            None => None,
        }
    }
    async fn remember(&self, url: &str, tier: FetchTier) {
        if let Ok(domain) = domain(url) {
            self.learned.lock().await.insert(
                domain,
                Learned {
                    tier,
                    expires: Instant::now() + self.options.learning_ttl,
                },
            );
        }
    }
    pub async fn fetch(
        &self,
        request: HttpRequest,
        tier: FetchTier,
    ) -> Result<FetchResponse, FetchError> {
        let _ = domain(&request.url)?;
        if tier != FetchTier::Auto {
            return self.attempt(&request, tier).await;
        }
        let initial = self
            .learned_tier(&request.url)
            .await
            .unwrap_or(FetchTier::Http);
        let candidates = match initial {
            FetchTier::Http => &[FetchTier::Http, FetchTier::Browser, FetchTier::Headed][..],
            FetchTier::Browser => &[FetchTier::Browser, FetchTier::Headed][..],
            FetchTier::Headed => &[FetchTier::Headed][..],
            FetchTier::Auto => unreachable!(),
        };
        let mut first_error = None;
        for &choice in candidates {
            if choice != FetchTier::Http && self.browser.is_none() {
                break;
            }
            match self.attempt(&request, choice).await {
                Ok(response) => {
                    if response.success() {
                        self.remember(&request.url, choice).await;
                        return Ok(response);
                    }
                    if !self.options.detect_blocks || !response.blocked() {
                        return Ok(response);
                    }
                    first_error.get_or_insert(FetchError {
                        kind: FetchErrorKind::Blocked,
                        message: format!("HTTP {} or verification challenge", response.status),
                        hint: "try --tier browser".into(),
                        status: Some(response.status),
                    });
                }
                Err(err) => {
                    if !matches!(
                        err.kind,
                        FetchErrorKind::Blocked
                            | FetchErrorKind::Timeout
                            | FetchErrorKind::Dns
                            | FetchErrorKind::Proxy
                            | FetchErrorKind::Transport
                            | FetchErrorKind::Browser
                    ) {
                        return Err(err);
                    }
                    first_error.get_or_insert(err);
                }
            }
        }
        Err(first_error.unwrap_or(FetchError {
            kind: FetchErrorKind::BrowserUnavailable,
            message: "no acquisition backend available".into(),
            hint: "configure a browser backend".into(),
            status: None,
        }))
    }
    async fn attempt(
        &self,
        req: &HttpRequest,
        tier: FetchTier,
    ) -> Result<FetchResponse, FetchError> {
        match tier {
            FetchTier::Http => self
                .http
                .request(req.clone())
                .await
                .map(Into::into)
                .map_err(Into::into),
            FetchTier::Browser | FetchTier::Headed => {
                if req.method != "GET" || req.body.is_some() {
                    return Err(FetchError {
                        kind: FetchErrorKind::InvalidRequest,
                        message: "browser tier only supports GET requests".into(),
                        hint: "use HTTP for POST or PUT".into(),
                        status: None,
                    });
                }
                let browser = self.browser.as_ref().ok_or_else(|| FetchError {
                    kind: FetchErrorKind::BrowserUnavailable,
                    message: "browser backend not configured".into(),
                    hint: "install and configure browser support".into(),
                    status: None,
                })?;
                let mut result = browser.fetch(&req.url, tier == FetchTier::Headed).await?;
                result.tier = tier;
                Ok(result)
            }
            FetchTier::Auto => unreachable!(),
        }
    }
}

fn domain(input: &str) -> Result<String, FetchError> {
    let parsed = Url::parse(input).map_err(|_| FetchError {
        kind: FetchErrorKind::InvalidRequest,
        message: "invalid URL".into(),
        hint: "provide an absolute HTTP(S) URL".into(),
        status: None,
    })?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(FetchError {
            kind: FetchErrorKind::InvalidRequest,
            message: "unsupported URL scheme".into(),
            hint: "only HTTP and HTTPS are supported".into(),
            status: None,
        });
    }
    parsed
        .host_str()
        .map(str::to_ascii_lowercase)
        .ok_or(FetchError {
            kind: FetchErrorKind::InvalidRequest,
            message: "missing hostname".into(),
            hint: "provide a URL hostname".into(),
            status: None,
        })
}

fn is_challenge(body: &[u8]) -> bool {
    let prefix = &body[..body.len().min(16384)];
    let page = String::from_utf8_lossy(prefix).to_ascii_lowercase();
    page.contains("cf-chl-")
        || page.contains("/cdn-cgi/challenge-platform/")
        || (page.contains("<title>just a moment") && page.contains("cloudflare"))
        || (page.contains("<title>attention required") && page.contains("cloudflare"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustscrapling_http::HttpOptions;

    struct Mock {
        calls: Arc<Mutex<Vec<bool>>>,
    }
    impl BrowserBackend for Mock {
        fn fetch<'a>(&'a self, url: &'a str, headed: bool) -> BrowserFuture<'a> {
            Box::pin(async move {
                self.calls.lock().await.push(headed);
                Ok(FetchResponse {
                    status: 200,
                    final_url: url.into(),
                    headers: vec![],
                    body: b"actual content".to_vec(),
                    tier: FetchTier::Browser,
                    protocol: None,
                })
            })
        }
    }
    #[test]
    fn block_detector_does_not_confuse_articles_with_challenges() {
        assert!(is_challenge(b"<title>Just a moment</title> Cloudflare"));
        assert!(!is_challenge(b"<article>How CAPTCHA works</article>"));
    }
    #[tokio::test]
    async fn explicit_browser_and_learned_auto() {
        let client = HttpClient::new(HttpOptions {
            direct: true,
            ..Default::default()
        })
        .unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let fetcher = Fetcher::new(client, FetchOptions::default()).with_browser(Arc::new(Mock {
            calls: calls.clone(),
        }));
        let req = HttpRequest::get("https://example.com/p");
        assert!(fetcher
            .fetch(req.clone(), FetchTier::Browser)
            .await
            .unwrap()
            .success());
        assert_eq!(fetcher.learned_tier("https://example.com").await, None);
        fetcher
            .remember("https://example.com", FetchTier::Browser)
            .await;
        assert_eq!(
            fetcher.fetch(req, FetchTier::Auto).await.unwrap().tier,
            FetchTier::Browser
        );
        assert_eq!(&*calls.lock().await, &[false, false]);
        let err = fetcher
            .fetch(
                HttpRequest::new("POST", "https://example.com"),
                FetchTier::Browser,
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind, FetchErrorKind::InvalidRequest);
    }
    #[tokio::test]
    async fn expired_learning_is_ignored() {
        let client = HttpClient::new(HttpOptions {
            direct: true,
            ..Default::default()
        })
        .unwrap();
        let fetcher = Fetcher::new(
            client,
            FetchOptions {
                learning_ttl: Duration::ZERO,
                ..Default::default()
            },
        );
        fetcher
            .remember("http://example.com", FetchTier::Headed)
            .await;
        assert_eq!(fetcher.learned_tier("http://EXAMPLE.COM").await, None);
    }
}
