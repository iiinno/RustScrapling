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
    /// Decoded text of this response. The browser tiers hand back text that
    /// CDP already decoded, so only the HTTP tier applies the declared charset.
    pub fn text(&self) -> (String, &'static str) {
        match self.tier {
            FetchTier::Http => {
                rustscrapling_http::decode_body(&self.headers, &self.body, &self.final_url)
            }
            FetchTier::Browser | FetchTier::Headed | FetchTier::Auto => {
                (String::from_utf8_lossy(&self.body).into_owned(), "utf-8")
            }
        }
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
        // The best HTTP result seen so far. A thin page is escalated rather than
        // returned, but it is still better than an error when the browser also
        // finds nothing.
        let mut best: Option<FetchResponse> = None;
        for &choice in candidates {
            if choice != FetchTier::Http && self.browser.is_none() {
                break;
            }
            match self.attempt(&request, choice).await {
                Ok(response) => {
                    let escalate = self.options.detect_blocks && needs_browser(&response);
                    if !escalate {
                        if response.success() {
                            self.remember(&request.url, choice).await;
                        }
                        return Ok(response);
                    }
                    // Keep the most complete thin response as a fallback: a
                    // successful but short page beats reporting an error.
                    if best.is_none() && (response.success() || response.status < 400) {
                        best = Some(response);
                    }
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
        if let Some(response) = best {
            // Every candidate produced a thin page; the HTTP one is still the
            // most complete, so return it instead of an error.
            return Ok(response);
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

/// Minimum visible text an HTML page must carry before the HTTP tier is
/// trusted. A JavaScript shell or an unrecognised interstitial renders far
/// less, so the browser is tried; if the browser also finds little, the HTTP
/// response is still returned rather than turning a usable page into an error.
const MIN_USEFUL_TEXT: usize = 200;

/// Visible text length of an HTML body, or `None` when the body is not HTML and
/// must never be judged by length. Script and style bodies are excluded because
/// a JavaScript shell can carry more code than prose.
fn visible_text_len(body: &[u8]) -> Option<usize> {
    let head = &body[..body.len().min(65_536)];
    let lowered = String::from_utf8_lossy(head).to_ascii_lowercase();
    let bytes = lowered.as_bytes();
    // Partial documents often omit <html> and <!doctype>, so any tag pair is
    // enough evidence that this is markup rather than JSON or an image.
    let markup = bytes.windows(5).any(|w| w == b"<html")
        || bytes.windows(9).any(|w| w == b"<!doctype")
        || bytes.windows(5).any(|w| w == b"<body")
        || (bytes.windows(4).any(|w| w == b"<div") && bytes.windows(5).any(|w| w == b"</div"));
    if !markup {
        return None;
    }
    const SCRIPT: &[u8] = b"<script";
    const SCRIPT_END: &[u8] = b"</script";
    const STYLE: &[u8] = b"<style";
    const STYLE_END: &[u8] = b"</style";
    let mut count = 0usize;
    let mut skipping: u8 = 0;
    let mut index = 0;
    while index < bytes.len() {
        let rest = &bytes[index..];
        if rest.starts_with(SCRIPT) {
            skipping = 2;
            index += SCRIPT.len();
            continue;
        }
        if rest.starts_with(SCRIPT_END) {
            skipping = 0;
            index += SCRIPT_END.len();
            continue;
        }
        if rest.starts_with(STYLE) {
            skipping = 3;
            index += STYLE.len();
            continue;
        }
        if rest.starts_with(STYLE_END) {
            skipping = 0;
            index += STYLE_END.len();
            continue;
        }
        let byte = bytes[index];
        // Inside <script> or <style>, a '<' is code, not a tag opener; letting it
        // reset the state leaked the whole script body into the visible count.
        if skipping == 2 || skipping == 3 {
            index += 1;
            continue;
        }
        if byte == b'<' {
            skipping = 1;
        } else if byte == b'>' {
            skipping = 0;
        } else if skipping == 0 && !byte.is_ascii_whitespace() {
            count += 1;
        }
        index += 1;
    }
    Some(count)
}

/// Whether the HTTP result should not be trusted yet.
fn needs_browser(response: &FetchResponse) -> bool {
    if response.blocked() {
        return true;
    }
    visible_text_len(&response.body).is_some_and(|visible| visible < MIN_USEFUL_TEXT)
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
    fn a_javascript_shell_is_too_thin_to_trust() {
        let body = br#"<!DOCTYPE html><html><head><script src="/static/jquery.js"></script>
            <script>$(document).ready(function(){ $.getJSON("/api/quotes", function(d){}); });</script>
            </head><body><div class="container"><div class="row"><h1>Quotes to Scrape</h1>
            <p><a href="/login">Login</a></p></div></div></body></html>"#;
        let visible = visible_text_len(body).expect("markup must be recognised");
        assert!(visible < MIN_USEFUL_TEXT, "visible text was {visible}");
        let response = FetchResponse {
            status: 200,
            final_url: "https://quotes.toscrape.com/js/".into(),
            headers: vec![],
            body: body.to_vec(),
            tier: FetchTier::Http,
            protocol: None,
        };
        assert!(needs_browser(&response));
    }

    #[test]
    fn a_real_article_is_not_escalated() {
        let prose = "This article explains how a crawler resolves links. ".repeat(20);
        let body = format!("<html><body><article><p>{prose}</p></article></body></html>");
        let response = FetchResponse {
            status: 200,
            final_url: "https://example.com/article".into(),
            headers: vec![],
            body: body.into_bytes(),
            tier: FetchTier::Http,
            protocol: None,
        };
        assert!(!needs_browser(&response));
    }

    #[test]
    fn a_json_body_is_never_judged_by_length() {
        assert_eq!(visible_text_len(br#"{"total":0,"quotes":[]}"#), None);
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
