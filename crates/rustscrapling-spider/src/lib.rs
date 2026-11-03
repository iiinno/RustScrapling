//! Bounded, restartable crawler with a fetcher supplied by the acquisition layer.
//! Its `crawl` callback emits each result immediately instead of retaining page bodies.

use futures_util::stream::{FuturesUnordered, StreamExt};
use rustscrapling_dom::html5_backend::{parse_html, Html5Options};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
use url::Url;

mod cache;
mod templates;
pub use templates::CrawlTemplate;

pub type FetchFuture<'a> = Pin<Box<dyn Future<Output = Result<FetchPage, String>> + Send + 'a>>;

/// An explicit failure for adapters that only implement the legacy URL-only API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpiderFetchError {
    Unsupported(&'static str),
    InvalidRequest(&'static str),
    SensitivePersistenceDenied,
    FrontierCapacityExceeded,
    Backend(String),
}

impl std::fmt::Display for SpiderFetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(feature) => write!(f, "unsupported spider request: {feature}"),
            Self::InvalidRequest(reason) => write!(f, "invalid spider request: {reason}"),
            Self::SensitivePersistenceDenied => write!(
                f,
                "cannot checkpoint request body or headers without persist_sensitive_requests opt-in"
            ),
            Self::FrontierCapacityExceeded => write!(f, "spider request frontier payload exceeds 32 MiB"),
            Self::Backend(reason) => write!(f, "spider fetch failed: {reason}"),
        }
    }
}

impl std::error::Error for SpiderFetchError {}

pub type RequestFetchFuture<'a> =
    Pin<Box<dyn Future<Output = Result<FetchPage, SpiderFetchError>> + Send + 'a>>;

/// Adapter for the HTTP / automatic acquisition crate. Tests use a deterministic mock.
pub trait Fetcher: Send + Sync {
    fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a>;

    /// Override to support HTTP methods, bodies, and headers. A legacy fetcher
    /// must fail explicitly instead of silently turning a POST into a GET.
    fn fetch_request<'a>(&'a self, request: &'a SpiderRequest) -> RequestFetchFuture<'a> {
        Box::pin(async move {
            let mut normalized = request.clone();
            normalized.validate_fields()?;
            if !normalized.is_plain_get_transport() {
                return Err(SpiderFetchError::Unsupported(
                    "method, body, or headers require fetch_request support",
                ));
            }
            self.fetch(&normalized.url)
                .await
                .map_err(SpiderFetchError::Backend)
        })
    }
}

/// Adapter that connects the crawler to RustScrapling's HTTP / browser tier
/// policy. It can be shared across several crawler instances.
#[derive(Clone)]
pub struct AutoFetcher(pub rustscrapling_fetch::Fetcher);

impl Fetcher for AutoFetcher {
    fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
        Box::pin(async move {
            let request = SpiderRequest::new(url);
            self.fetch_request(&request)
                .await
                .map_err(|e| e.to_string())
        })
    }

    fn fetch_request<'a>(&'a self, request: &'a SpiderRequest) -> RequestFetchFuture<'a> {
        Box::pin(async move {
            let mut request = request.clone();
            request.validate_fields()?;
            let http_request = rustscrapling_http::HttpRequest {
                method: request.method.clone(),
                url: request.url.clone(),
                headers: request.headers.clone(),
                body: request.body.clone(),
            };
            // The browser backend only understands URL navigation. Keeping
            // rich requests on HTTP avoids losing headers or sending a GET
            // after a failed POST.
            let tier = if request.is_plain_get_transport() {
                rustscrapling_fetch::FetchTier::Auto
            } else {
                rustscrapling_fetch::FetchTier::Http
            };
            let response = self
                .0
                .fetch(http_request, tier)
                .await
                .map_err(|error| SpiderFetchError::Backend(error.to_string()))?;
            let content_type = response
                .headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("content-type"))
                .map(|(_, val)| val.to_ascii_lowercase())
                .unwrap_or_default();
            let (body, _charset) = response.text();
            Ok(FetchPage {
                status: response.status,
                url: response.final_url,
                content_type,
                body,
                headers: response.headers.into_iter().collect(),
            })
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FetchPage {
    pub status: u16,
    pub url: String,
    pub content_type: String,
    pub body: String,
    /// Response headers are available to the scheduler for Retry-After.
    /// Older JSON cache entries without this field remain readable.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
}

fn default_max_pages() -> usize {
    150
}
fn default_concurrency() -> usize {
    8
}
fn default_max_depth() -> usize {
    2
}
fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CrawlSpec {
    pub start_urls: Vec<String>,
    pub template: CrawlTemplate,
    pub allowed_domains: Vec<String>,
    /// URL regular expressions controlling followed HTML links, comparable
    /// to CrawlSpider LinkExtractor allow/deny. Seeds are not filtered.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub link_allow: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub link_deny: Vec<String>,
    #[serde(default = "default_max_pages")]
    pub max_pages: usize,
    #[serde(default = "default_max_depth")]
    pub max_depth: usize,
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    pub obey_robots: bool,
    pub delay_ms: u64,
    /// Opt-in per-host pacing based on response latency and throttling status.
    /// Omitted/false is not serialized, retaining legacy checkpoint hashes.
    #[serde(skip_serializing_if = "is_false")]
    pub auto_throttle: bool,
    /// Replay previously fetched successful pages for extraction development.
    /// An independent crawl starts on every call; no checkpoint is consumed.
    #[serde(skip_serializing_if = "is_false")]
    pub development_mode: bool,
    /// Explicit permission to store request bodies and headers in plaintext
    /// in owner-only checkpoint files. Omitted/false preserves old spec hashes.
    #[serde(skip_serializing_if = "is_false")]
    pub persist_sensitive_requests: bool,
    /// Optional private on-disk cache root (defaults to crawldir/dev-cache or
    /// .rustscrapling-dev-cache relative to the current directory).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub development_cache_dir: Option<PathBuf>,
    pub crawldir: Option<PathBuf>,
}

impl Default for CrawlSpec {
    fn default() -> Self {
        Self {
            start_urls: Vec::new(),
            template: CrawlTemplate::default(),
            allowed_domains: Vec::new(),
            link_allow: Vec::new(),
            link_deny: Vec::new(),
            max_pages: default_max_pages(),
            max_depth: default_max_depth(),
            concurrency: default_concurrency(),
            obey_robots: false,
            delay_ms: 0,
            auto_throttle: false,
            development_mode: false,
            persist_sensitive_requests: false,
            development_cache_dir: None,
            crawldir: None,
        }
    }
}

impl CrawlSpec {
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let spec: Self = toml::from_str(text).map_err(|e| format!("invalid crawler TOML: {e}"))?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, String> {
        let source =
            std::fs::read_to_string(path).map_err(|e| format!("reading crawl spec: {e}"))?;
        Self::from_toml(&source)
    }

    fn validate(&self) -> Result<(), String> {
        if self.start_urls.is_empty() {
            return Err("start_urls must not be empty".into());
        }
        if !(1..=64).contains(&self.concurrency) {
            return Err("concurrency must be 1..=64".into());
        }
        if self.max_pages == 0 || self.max_pages > 1_000_000 {
            return Err("max_pages must be 1..=1000000".into());
        }
        // Rust's regex engine has no catastrophic backtracking; still bound
        // untrusted specifications so compilation cannot exhaust memory.
        for (name, patterns) in [
            ("link_allow", &self.link_allow),
            ("link_deny", &self.link_deny),
        ] {
            if patterns.len() > 64 {
                return Err(format!("{name} supports at most 64 regex patterns"));
            }
            for pattern in patterns {
                if pattern.len() > 1024 {
                    return Err(format!("{name} regex exceeds 1024 bytes"));
                }
                regex::RegexBuilder::new(pattern)
                    .size_limit(1 << 20)
                    .build()
                    .map_err(|err| format!("invalid {name} regex: {err}"))?;
            }
        }
        for seed in &self.start_urls {
            valid_url(seed)?;
        }
        Ok(())
    }
}

/// Compiled once per crawl, never once per HTML link.
struct LinkRules {
    allow: Vec<regex::Regex>,
    deny: Vec<regex::Regex>,
}

impl LinkRules {
    fn compile(spec: &CrawlSpec) -> Result<Self, String> {
        let compile = |patterns: &[String]| {
            patterns
                .iter()
                .map(|s| {
                    regex::RegexBuilder::new(s)
                        .size_limit(1 << 20)
                        .build()
                        .map_err(|err| format!("invalid crawl link regex: {err}"))
                })
                .collect::<Result<Vec<_>, String>>()
        };
        Ok(Self {
            allow: compile(&spec.link_allow)?,
            deny: compile(&spec.link_deny)?,
        })
    }

    fn permitted(&self, url: &Url) -> bool {
        let raw = url.as_str();
        (self.allow.is_empty() || self.allow.iter().any(|rule| rule.is_match(raw)))
            && !self.deny.iter().any(|rule| rule.is_match(raw))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CrawlItem {
    pub url: String,
    pub depth: usize,
    pub status: Option<u16>,
    pub links: Vec<String>,
    pub error: Option<String>,
    pub body: Option<String>,
    /// Name of the parser callback requested by the originating
    /// [`SpiderRequest`] (upstream `Request.callback`); `None` means `parse`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback: Option<String>,
    /// Metadata carried from the originating request (upstream `meta`).
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub meta: serde_json::Map<String, serde_json::Value>,
}

/// A follow-up URL yielded from a custom spider parser. Relative URLs are
/// resolved against the final fetched URL; all normal crawl limits apply.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpiderRequest {
    pub url: String,
    #[serde(default = "default_request_method")]
    pub method: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "is_zero_priority")]
    pub priority: i32,
    /// Parser callback name for the response (upstream `callback`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback: Option<String>,
    /// Arbitrary JSON metadata handed to the response (upstream `meta`).
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub meta: serde_json::Map<String, serde_json::Value>,
    /// Bypass duplicate filtering (upstream `dont_filter`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub dont_filter: bool,
    /// Request cookies sent as a `Cookie` header (upstream `cookies`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cookies: BTreeMap<String, String>,
}

impl std::fmt::Debug for SpiderRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpiderRequest")
            .field("url", &"<redacted>")
            .field("method", &self.method)
            .field("header_names", &self.headers.keys().collect::<Vec<_>>())
            .field("body_bytes", &self.body.as_ref().map(Vec::len))
            .field("priority", &self.priority)
            .field("callback", &self.callback)
            .field("meta_keys", &self.meta.keys().collect::<Vec<_>>())
            .field("dont_filter", &self.dont_filter)
            .field("cookie_names", &self.cookies.keys().collect::<Vec<_>>())
            .finish()
    }
}

fn default_request_method() -> String {
    "GET".into()
}

fn is_zero_priority(priority: &i32) -> bool {
    *priority == 0
}

impl SpiderRequest {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            method: default_request_method(),
            headers: BTreeMap::new(),
            body: None,
            priority: 0,
            callback: None,
            meta: serde_json::Map::new(),
            dont_filter: false,
            cookies: BTreeMap::new(),
        }
    }

    /// Parser callback to run for the response.
    pub fn callback(mut self, name: impl Into<String>) -> Self {
        self.callback = Some(name.into());
        self
    }

    pub fn meta(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.meta.insert(key.into(), value);
        self
    }

    pub fn dont_filter(mut self, dont_filter: bool) -> Self {
        self.dont_filter = dont_filter;
        self
    }

    pub fn cookie(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.cookies.insert(name.into(), value.into());
        self
    }

    pub fn method(mut self, method: impl Into<String>) -> Self {
        self.method = method.into();
        self
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }

    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = Some(body.into());
        self
    }

    pub fn priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    /// The priority is a scheduler hint; it cannot change HTTP identity.
    fn is_plain_get_transport(&self) -> bool {
        self.method == "GET"
            && self.headers.is_empty()
            && self.body.is_none()
            && self.cookies.is_empty()
    }

    /// Whether the request carries anything beyond the legacy
    /// `[url, depth]` checkpoint shape.
    fn is_legacy_shape(&self) -> bool {
        self.is_plain_get_transport()
            && self.priority == 0
            && self.callback.is_none()
            && self.meta.is_empty()
            && !self.dont_filter
    }

    /// Fold `cookies` into a `Cookie` header for the transport layer.
    fn with_cookie_header(mut self) -> Result<Self, SpiderFetchError> {
        if self.cookies.is_empty() {
            return Ok(self);
        }
        let mut pairs = Vec::new();
        for (name, value) in &self.cookies {
            if name.is_empty()
                || name.bytes().any(|b| {
                    matches!(b, b';' | b'=' | b'\r' | b'\n' | b'\0') || b.is_ascii_whitespace()
                })
                || value
                    .bytes()
                    .any(|b| matches!(b, b';' | b'\r' | b'\n' | b'\0'))
            {
                return Err(SpiderFetchError::InvalidRequest("invalid request cookies"));
            }
            pairs.push(format!("{name}={value}"));
        }
        let existing = self
            .headers
            .keys()
            .find(|key| key.eq_ignore_ascii_case("cookie"))
            .cloned();
        if let Some(key) = existing {
            let prior = self.headers.remove(&key).unwrap_or_default();
            pairs.insert(0, prior);
        }
        self.headers.insert("Cookie".into(), pairs.join("; "));
        self.cookies.clear();
        Ok(self)
    }

    fn validate_fields(&mut self) -> Result<(), SpiderFetchError> {
        if self.method.is_empty()
            || self.method.len() > 32
            || !self
                .method
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
        {
            return Err(SpiderFetchError::InvalidRequest("invalid HTTP method"));
        }
        self.method.make_ascii_uppercase();
        if self
            .body
            .as_ref()
            .is_some_and(|body| body.len() > 8 * 1024 * 1024)
        {
            return Err(SpiderFetchError::InvalidRequest("body exceeds 8 MiB"));
        }
        let mut header_names = HashSet::new();
        if self.headers.len() > 64
            || self.headers.iter().any(|(name, value)| {
                name.is_empty()
                    || name.len() > 256
                    || !name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
                    || value.len() > 8192
                    || value
                        .bytes()
                        .any(|byte| matches!(byte, b'\r' | b'\n' | b'\0'))
                    || !header_names.insert(name.to_ascii_lowercase())
            })
        {
            return Err(SpiderFetchError::InvalidRequest("invalid request headers"));
        }
        Ok(())
    }

    fn ensure_checkpoint_privacy(&self, spec: &CrawlSpec) -> Result<(), SpiderFetchError> {
        if spec.crawldir.is_some()
            && !spec.development_mode
            && !spec.persist_sensitive_requests
            && (self.body.is_some() || !self.headers.is_empty() || !self.cookies.is_empty())
        {
            return Err(SpiderFetchError::SensitivePersistenceDenied);
        }
        Ok(())
    }
}

/// A parser may yield follow-up requests and structured JSON items together.
#[derive(Clone, Debug, PartialEq)]
pub enum SpiderOutput {
    Request(SpiderRequest),
    Item(serde_json::Value),
}

pub type ParseFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<SpiderOutput>, String>> + Send + 'a>>;

/// User-defined asynchronous extraction logic. It runs on successful pages
/// after fetching, and may use `page.body`, `page.url` and `page.depth`.
/// Returned requests enter the existing bounded scheduler, including robots,
/// domain, link-rule and checkpoint handling.
pub trait SpiderParser: Send + Sync {
    /// Handle a successful response. `page.callback` names the callback the
    /// originating request asked for (upstream `Request.callback`); `None`
    /// is the default `parse`. `page.meta` carries the request metadata.
    fn parse<'a>(&'a self, page: &'a CrawlItem) -> ParseFuture<'a>;

    /// Upstream `on_error(request, error)`: called with `page.error` set when
    /// fetching failed or `parse` returned an error. May yield follow-ups.
    fn on_error<'a>(&'a self, page: &'a CrawlItem) -> ParseFuture<'a> {
        let _ = page;
        Box::pin(async { Ok(Vec::new()) })
    }

    /// Upstream `on_scraped_item`: transform an item or drop it (`None`).
    fn on_scraped_item(&self, item: serde_json::Value) -> Option<serde_json::Value> {
        Some(item)
    }
}

/// Events from [`crawl_with_parser`]. `Page` retains the existing crawl
/// result; `Item` streams a JSON value emitted by the custom parser.
#[derive(Debug)]
pub enum SpiderEvent<'a> {
    Page(&'a CrawlItem),
    Item(&'a serde_json::Value),
}

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
struct Progress {
    #[serde(default)]
    spec_hash: String,
    visited_hashes: Vec<String>,
    queue: Vec<StoredRequest>,
}

/// The old JSON checkpoint queue is an array of [url, depth] pairs. Rich
/// entries are JSON objects; GET-only checkpoints keep their exact shape.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum StoredRequest {
    Legacy((String, usize)),
    Rich {
        request: SpiderRequest,
        depth: usize,
    },
}

#[derive(Clone, Debug)]
struct QueuedRequest {
    request: SpiderRequest,
    depth: usize,
}

impl From<StoredRequest> for QueuedRequest {
    fn from(value: StoredRequest) -> Self {
        match value {
            StoredRequest::Legacy((url, depth)) => Self {
                request: SpiderRequest::new(url),
                depth,
            },
            StoredRequest::Rich { request, depth } => Self { request, depth },
        }
    }
}

impl From<QueuedRequest> for StoredRequest {
    fn from(value: QueuedRequest) -> Self {
        if value.request.is_legacy_shape() {
            StoredRequest::Legacy((value.request.url, value.depth))
        } else {
            StoredRequest::Rich {
                request: value.request,
                depth: value.depth,
            }
        }
    }
}

/// Summary excludes response bodies, so it stays bounded for long crawls.
#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct CrawlSummary {
    pub visited: usize,
    pub successful: usize,
    pub failed: usize,
    pub remaining: usize,
    /// Parser callbacks that returned an error (handed to `on_error`).
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub parse_errors: usize,
}

fn is_zero_usize(value: &usize) -> bool {
    *value == 0
}

/// A separate start gate for each host. Never hold it across HTTP I/O.
/// Dynamic updates apply to the next start gate acquisition, so requests
/// already in flight are not cancelled or rescheduled.
struct HostThrottle {
    last_start: Option<Instant>,
    delay: Duration,
    /// Server-supplied Retry-After is measured from the response time,
    /// independently of the inter-request start spacing.
    not_before: Option<Instant>,
}

impl HostThrottle {
    fn new(base_delay: Duration) -> Self {
        Self {
            last_start: None,
            delay: base_delay,
            not_before: None,
        }
    }

    fn observe(
        &mut self,
        status: Option<u16>,
        latency: Duration,
        minimum: Duration,
        retry_after: Option<Duration>,
    ) {
        let maximum = Duration::from_secs(30).max(minimum);
        if matches!(status, None | Some(408 | 429 | 502 | 503 | 504)) {
            self.delay = self.delay.saturating_mul(2).min(maximum);
            if matches!(status, Some(429 | 503)) {
                if let Some(delay) = retry_after {
                    if let Some(deadline) = Instant::now().checked_add(delay) {
                        self.not_before =
                            Some(self.not_before.map_or(deadline, |old| old.max(deadline)));
                    }
                }
            }
        } else if status.is_some_and(|code| (200..400).contains(&code)) {
            // Smooth successful responses; a very fast single request should
            // not instantly undo a server's previous backpressure signal.
            let target = minimum.max(latency / 2).min(maximum);
            let next_ms = (self.delay.as_millis() * 3 + target.as_millis()) / 4;
            self.delay = Duration::from_millis(next_ms.min(u64::MAX as u128) as u64);
        }
    }
}

/// HTTP Retry-After supports seconds and RFC 7231 HTTP-date. Reject values
/// beyond 24 hours so an untrusted header cannot stall a crawl indefinitely.
fn retry_after_value(headers: &BTreeMap<String, String>) -> Option<Duration> {
    let value = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))?
        .1
        .trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return (seconds <= 86_400).then(|| Duration::from_secs(seconds));
    }
    let deadline = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    let now: chrono::DateTime<chrono::Utc> = std::time::SystemTime::now().into();
    let milliseconds = deadline.signed_duration_since(now).num_milliseconds();
    if milliseconds <= 0 {
        return Some(Duration::ZERO);
    }
    (milliseconds <= 86_400_000).then(|| Duration::from_millis(milliseconds as u64))
}

fn valid_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|_| format!("invalid URL {raw}"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("crawler only accepts HTTP and HTTPS URLs".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("crawler URLs cannot contain embedded credentials".into());
    }
    Ok(url)
}

fn key(url: &str) -> String {
    blake3::hash(url.as_bytes()).to_hex().to_string()
}

const MAX_RESERVED_REQUEST_PAYLOAD_BYTES: usize = 32 * 1024 * 1024;

fn request_payload_bytes(request: &SpiderRequest) -> usize {
    let headers = request
        .headers
        .iter()
        .map(|(name, value)| name.len().saturating_add(value.len()))
        .fold(0usize, usize::saturating_add);
    let cookies = request
        .cookies
        .iter()
        .map(|(name, value)| name.len().saturating_add(value.len()))
        .fold(0usize, usize::saturating_add);
    let meta = if request.meta.is_empty() {
        0
    } else {
        serde_json::to_vec(&request.meta).map_or(usize::MAX, |bytes| bytes.len())
    };
    headers
        .saturating_add(request.body.as_ref().map_or(0, Vec::len))
        .saturating_add(cookies)
        .saturating_add(meta)
}

/// Preserve the legacy GET digest while distinguishing requests with a
/// different method, body or headers. Priority is intentionally excluded.
fn request_key(request: &SpiderRequest) -> String {
    if request.method == "GET" && request.headers.is_empty() && request.body.is_none() {
        return key(&request.url);
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rustscrapling-spider-request-v1\0");
    let mut field = |value: &[u8]| {
        hasher.update(&(value.len() as u64).to_le_bytes());
        hasher.update(value);
    };
    field(request.method.to_ascii_uppercase().as_bytes());
    field(request.url.as_bytes());
    field(&(request.headers.len() as u64).to_le_bytes());
    let mut headers: Vec<_> = request
        .headers
        .iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value))
        .collect();
    headers.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    for (name, value) in &headers {
        field(name.as_bytes());
        field(value.as_bytes());
    }
    // Like upstream `Request.update_fingerprint`, cookies (session kwargs)
    // are not part of the duplicate-filter identity.
    // Disambiguate absent body from a present zero-length body.
    field(if request.body.is_some() { b"1" } else { b"0" });
    if let Some(body) = &request.body {
        field(body);
    }
    hasher.finalize().to_hex().to_string()
}

#[derive(Clone, Debug)]
struct RobotsRule {
    pattern: String,
    allow: bool,
}

#[derive(Default, Clone, Debug)]
struct RobotsPolicy {
    rules: Vec<RobotsRule>,
    minimum_delay: Duration,
}

#[derive(Default)]
struct RobotsGroup {
    agents: Vec<String>,
    policy: RobotsPolicy,
}

/// Reject unbounded/non-finite robots pacing values rather than allowing
/// untrusted policy data to panic Duration conversion or stall a crawl.
fn robots_seconds(value: &str) -> Option<Duration> {
    let seconds = value.trim().parse::<f64>().ok()?;
    (seconds.is_finite() && (0.0..=86_400.0).contains(&seconds))
        .then(|| Duration::from_secs_f64(seconds))
}

fn robots_request_rate(value: &str) -> Option<Duration> {
    let (requests, seconds) = value.trim().split_once('/')?;
    let requests = requests.trim().parse::<u32>().ok()?;
    let period = robots_seconds(seconds)?;
    if requests == 0 {
        return None;
    }
    // Round up: rounding down would violate the server's stated rate.
    let nanos = period.as_nanos().div_ceil(u128::from(requests));
    Some(Duration::from_nanos(nanos.min(u64::MAX as u128) as u64))
}

/// Implements the common RFC 9309 path-prefix and `*`/`$` matching syntax.
/// Specific RustScrapling groups win over the generic `*` group.
fn parse_robots_policy(source: &str) -> RobotsPolicy {
    let mut groups = Vec::<RobotsGroup>::new();
    let mut current = RobotsGroup::default();
    let mut agents = Vec::<String>::new();
    let mut group_has_directives = false;
    for raw in source.lines() {
        let line = raw.split('#').next().unwrap_or_default().trim();
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let key = name.trim().to_ascii_lowercase();
        let value = value.trim();
        if key == "user-agent" {
            if group_has_directives {
                current.agents = std::mem::take(&mut agents);
                groups.push(std::mem::take(&mut current));
                group_has_directives = false;
            }
            agents.push(value.to_ascii_lowercase());
        } else if !agents.is_empty() {
            match key.as_str() {
                "allow" | "disallow" => {
                    if !value.is_empty() {
                        current.policy.rules.push(RobotsRule {
                            pattern: value.to_owned(),
                            allow: key == "allow",
                        });
                    }
                }
                "crawl-delay" => {
                    if let Some(delay) = robots_seconds(value) {
                        current.policy.minimum_delay = current.policy.minimum_delay.max(delay);
                    }
                }
                "request-rate" => {
                    if let Some(delay) = robots_request_rate(value) {
                        current.policy.minimum_delay = current.policy.minimum_delay.max(delay);
                    }
                }
                _ => {}
            }
            // Unknown directives still terminate the current user-agent
            // header group before another User-agent declaration.
            group_has_directives = true;
        }
    }
    if !agents.is_empty() {
        current.agents = agents;
        groups.push(current);
    }
    let specific = groups
        .iter()
        .any(|group| group.agents.iter().any(|agent| agent == "rustscrapling"));
    let mut selected = RobotsPolicy::default();
    for group in groups {
        let matches = group.agents.iter().any(|agent| {
            if specific {
                agent == "rustscrapling"
            } else {
                agent == "*"
            }
        });
        if matches {
            selected.rules.extend(group.policy.rules);
            selected.minimum_delay = selected.minimum_delay.max(group.policy.minimum_delay);
        }
    }
    selected
}

#[cfg(test)]
fn parse_robots(source: &str) -> Vec<RobotsRule> {
    parse_robots_policy(source).rules
}

fn robots_rule_matches(pattern: &str, path: &str) -> bool {
    let anchored = pattern.ends_with('$');
    let content = pattern.strip_suffix('$').unwrap_or(pattern);
    let expression = format!(
        "^{}{}",
        regex::escape(content).replace("\\*", ".*"),
        if anchored { "$" } else { "" }
    );
    regex::Regex::new(&expression).is_ok_and(|re| re.is_match(path))
}

fn robots_permitted(rules: &[RobotsRule], path: &str) -> bool {
    let matched = rules
        .iter()
        .filter(|r| robots_rule_matches(&r.pattern, path))
        .max_by_key(|r| (r.pattern.len(), r.allow));
    matched.is_none_or(|rule| rule.allow)
}

async fn robots_allowed(
    url: &Url,
    fetcher: &Arc<dyn Fetcher>,
    cache: &mut HashMap<String, Option<RobotsPolicy>>,
) -> bool {
    let host_key = url.origin().ascii_serialization();
    if !cache.contains_key(&host_key) {
        let mut robots = url.clone();
        robots.set_path("/robots.txt");
        robots.set_query(None);
        robots.set_fragment(None);
        let policy = match fetcher.fetch(robots.as_str()).await {
            Ok(page) if page.status == 404 => Some(RobotsPolicy::default()),
            Ok(page) if (200..300).contains(&page.status) => Some(parse_robots_policy(&page.body)),
            // If robot policy cannot be established while obedience is
            // explicitly requested, fail closed instead of ignoring it.
            _ => None,
        };
        cache.insert(host_key.clone(), policy);
    }
    // RFC 9309 matches the request target, including any query component.
    // Checking only url.path() can accidentally bypass a query-specific rule.
    let mut path = url.path().to_owned();
    if let Some(query) = url.query() {
        path.push('?');
        path.push_str(query);
    }
    cache
        .get(&host_key)
        .and_then(Option::as_ref)
        .is_some_and(|policy| robots_permitted(&policy.rules, &path))
}

fn host_allowed(spec: &CrawlSpec, url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    if spec.allowed_domains.is_empty() {
        return spec
            .start_urls
            .iter()
            .filter_map(|s| Url::parse(s).ok())
            .any(|seed| seed.host_str().is_some_and(|h| h == host));
    }
    spec.allowed_domains.iter().any(|domain| {
        let domain = domain.trim_start_matches('.');
        !domain.is_empty()
            && (host.eq_ignore_ascii_case(domain)
                || host
                    .to_ascii_lowercase()
                    .ends_with(&format!(".{}", domain.to_ascii_lowercase())))
    })
}

/// Extract HTTP(S) links using the same HTML5 DOM as the selector crate.
pub fn page_links(base: &str, html: &str) -> Vec<String> {
    page_links_limited(base, html, usize::MAX)
}

/// Limit links before allocating result strings, not just when adding them to
/// the crawl queue. A large sitemap-like HTML page can otherwise exhaust
/// memory even when the crawl specification requests only a few pages.
fn page_links_limited(base: &str, html: &str, limit: usize) -> Vec<String> {
    page_links_filtered(base, html, limit, |_| true)
}

/// Count only eligible new links towards the frontier budget. Applying the
/// limit before allowed-domain and visited checks can silently discard useful
/// links when a page starts with navigation to other domains or old pages.
fn page_links_filtered<F>(base: &str, html: &str, limit: usize, mut eligible: F) -> Vec<String>
where
    F: FnMut(&Url) -> bool,
{
    if limit == 0 {
        return Vec::new();
    }
    let Ok(base) = valid_url(base) else {
        return Vec::new();
    };
    let Ok(dom) = parse_html(html, &Html5Options::default()) else {
        return Vec::new();
    };
    // HTML's first <base href> inside <head> sets relative-link resolution.
    // Keep the same safety checks as ordinary fetched and discovered URLs.
    let base = dom
        .descendants_or_self(dom.root)
        .into_iter()
        .find(|&id| {
            dom.node(id).tag == "base"
                && dom.attr(id, "href").is_some()
                && dom.ancestors(id).iter().any(|&a| dom.node(a).tag == "head")
        })
        .and_then(|id| base.join(dom.attr(id, "href")?).ok())
        .filter(|url| valid_url(url.as_str()).is_ok())
        .unwrap_or(base);
    let mut seen = HashSet::new();
    let mut links = Vec::new();
    for node in dom.descendants_or_self(dom.root) {
        if !matches!(dom.node(node).tag.as_str(), "a" | "area") {
            continue;
        }
        let Some(href) = dom.attr(node, "href") else {
            continue;
        };
        let Ok(mut url) = base.join(href.trim()) else {
            continue;
        };
        if valid_url(url.as_str()).is_err() {
            continue;
        }
        url.set_fragment(None);
        if !eligible(&url) {
            continue;
        }
        let value = url.to_string();
        if seen.insert(value.clone()) {
            links.push(value);
            if links.len() == limit {
                break;
            }
        }
    }
    links
}

fn state_path(spec: &CrawlSpec) -> Option<PathBuf> {
    spec.crawldir
        .as_ref()
        .map(|dir| dir.join("crawl-state.json"))
}

fn spec_hash(spec: &CrawlSpec) -> Result<String, String> {
    let bytes = serde_json::to_vec(spec).map_err(|e| format!("encoding crawl spec: {e}"))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn restore(spec: &CrawlSpec) -> Result<Option<Progress>, String> {
    let Some(path) = state_path(spec) else {
        return Ok(None);
    };
    if let Some(parent) = path.parent() {
        match std::fs::symlink_metadata(parent) {
            Ok(metadata) if !metadata.file_type().is_dir() => {
                return Err("crawldir must be a real directory, not a symlink".into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("checking crawldir: {error}")),
        }
    }
    // Recover the last intact checkpoint if Windows had to move the
    // previous file aside before an interrupted replacement.
    let backup = path.with_extension("json.bak");
    let mut last_error = None;
    for candidate in [&path, &backup] {
        let metadata = match std::fs::symlink_metadata(candidate) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("checking crawl state: {error}")),
        };
        // Check symlink_metadata rather than following a malicious state-file
        // symlink. This also prevents accidentally reading unrelated files.
        if !metadata.file_type().is_file() {
            return Err("crawl state must be a regular file (not a symlink)".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Migrate older state files written with a permissive umask before
            // reading URLs that can contain access tokens in their query.
            if metadata.permissions().mode() & 0o077 != 0 {
                std::fs::set_permissions(candidate, std::fs::Permissions::from_mode(0o600))
                    .map_err(|e| format!("protecting crawl state: {e}"))?;
            }
        }
        match std::fs::read(candidate)
            .map_err(|e| e.to_string())
            .and_then(|bytes| serde_json::from_slice::<Progress>(&bytes).map_err(|e| e.to_string()))
        {
            Ok(progress) => {
                if !progress.spec_hash.is_empty() && progress.spec_hash != spec_hash(spec)? {
                    return Err("checkpoint belongs to a different crawl specification".into());
                }
                return Ok(Some(progress));
            }
            Err(reason) => last_error = Some(reason),
        }
    }
    match last_error {
        Some(reason) => Err(format!("invalid crawl state and backup: {reason}")),
        None => Ok(None),
    }
}

fn persist(spec: &CrawlSpec, progress: &Progress) -> Result<(), String> {
    let Some(path) = state_path(spec) else {
        return Ok(());
    };
    for entry in &progress.queue {
        if let StoredRequest::Rich { request, .. } = entry {
            request
                .ensure_checkpoint_privacy(spec)
                .map_err(|err| err.to_string())?;
        }
    }
    let parent = path.parent().ok_or("state has no parent directory")?;
    let parent_existed = parent.exists();
    std::fs::create_dir_all(parent).map_err(|e| format!("creating crawldir: {e}"))?;
    if !std::fs::symlink_metadata(parent)
        .map_err(|e| format!("checking crawldir: {e}"))?
        .file_type()
        .is_dir()
    {
        return Err("crawldir must be a real directory, not a symlink".into());
    }
    #[cfg(unix)]
    if !parent_existed {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("protecting crawldir: {e}"))?;
    }
    // A unique, exclusively created temporary file cannot overwrite an
    // attacker-supplied symlink or a stale temp file from a prior crash.
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pending = path.with_extension(format!("json.{}.{nonce}.tmp", std::process::id()));
    let mut data = progress.clone();
    data.spec_hash = spec_hash(spec)?;
    let bytes = serde_json::to_vec(&data).map_err(|e| e.to_string())?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&pending)
        .map_err(|e| format!("creating state: {e}"))?;
    let write_result = (|| -> Result<(), String> {
        file.write_all(&bytes)
            .map_err(|e| format!("writing state: {e}"))?;
        file.sync_all().map_err(|e| format!("syncing state: {e}"))
    })();
    drop(file);
    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&pending);
        return Err(error);
    }
    #[cfg(windows)]
    {
        let backup = path.with_extension("json.bak");
        if path.exists() {
            if backup.exists() {
                std::fs::remove_file(&backup).map_err(|e| format!("removing old backup: {e}"))?;
            }
            std::fs::rename(&path, &backup).map_err(|e| format!("backing up state: {e}"))?;
        }
        if let Err(err) = std::fs::rename(&pending, &path) {
            if !path.exists() && backup.exists() {
                let _ = std::fs::rename(&backup, &path);
            }
            return Err(format!("saving state: {err}"));
        }
        if backup.exists() {
            std::fs::remove_file(backup).map_err(|e| format!("cleaning state backup: {e}"))?;
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(pending, path).map_err(|e| format!("saving state: {e}"))
    }
}

/// A parser request follows the same frontier controls as discovered links.
/// Robots checks run when the request is actually dispatched.
struct Frontier<'a> {
    visited: &'a HashSet<String>,
    queued: &'a mut HashSet<String>,
    queue: &'a mut VecDeque<QueuedRequest>,
    reserved_payload_bytes: &'a mut usize,
    nonzero_priority_count: &'a mut usize,
}

fn enqueue_spider_request(
    mut request: SpiderRequest,
    base: &str,
    depth: usize,
    spec: &CrawlSpec,
    rules: &LinkRules,
    frontier: Frontier<'_>,
) -> Result<(), String> {
    if depth >= spec.max_depth
        || frontier.visited.len().saturating_add(frontier.queue.len()) >= spec.max_pages
    {
        return Ok(());
    }
    let Ok(base) = valid_url(base) else {
        return Ok(());
    };
    let Ok(mut url) = base.join(request.url.trim()) else {
        return Ok(());
    };
    url.set_fragment(None);
    if valid_url(url.as_str()).is_err() || !host_allowed(spec, &url) || !rules.permitted(&url) {
        return Ok(());
    }
    request.url = url.to_string();
    request.validate_fields().map_err(|err| err.to_string())?;
    request
        .ensure_checkpoint_privacy(spec)
        .map_err(|err| err.to_string())?;
    let fingerprint = request_key(&request);
    if request.dont_filter
        || (!frontier.visited.contains(&fingerprint) && !frontier.queued.contains(&fingerprint))
    {
        let total = frontier
            .reserved_payload_bytes
            .checked_add(request_payload_bytes(&request))
            .filter(|total| *total <= MAX_RESERVED_REQUEST_PAYLOAD_BYTES)
            .ok_or_else(|| SpiderFetchError::FrontierCapacityExceeded.to_string())?;
        *frontier.reserved_payload_bytes = total;
        frontier.queued.insert(fingerprint);
        if request.priority != 0 {
            *frontier.nonzero_priority_count += 1;
        }
        frontier.queue.push_back(QueuedRequest {
            request,
            depth: depth + 1,
        });
    }
    Ok(())
}

/// Stable ordering among equal priorities retains the previous FIFO behavior.
fn pop_next_request(
    queue: &mut VecDeque<QueuedRequest>,
    nonzero_priority_count: &mut usize,
) -> Option<QueuedRequest> {
    // The common URL-only crawl remains O(1) FIFO, even with a huge queue.
    if *nonzero_priority_count == 0 {
        return queue.pop_front();
    }
    let priority = queue.iter().map(|entry| entry.request.priority).max()?;
    let index = queue
        .iter()
        .position(|entry| entry.request.priority == priority)?;
    let next = queue.remove(index)?;
    if next.request.priority != 0 {
        *nonzero_priority_count -= 1;
    }
    Some(next)
}

/// Crawl using bounded batches of concurrent fetches. The callback is invoked
/// for each completed response and should stream NDJSON or similar output.
pub async fn crawl<F>(
    spec: &CrawlSpec,
    fetcher: Arc<dyn Fetcher>,
    mut emit: F,
) -> Result<CrawlSummary, String>
where
    F: FnMut(&CrawlItem) -> Result<(), String>,
{
    crawl_inner(spec, fetcher, None, |event| match event {
        SpiderEvent::Page(page) => emit(page),
        SpiderEvent::Item(_) => Ok(()),
    })
    .await
}

/// Crawl with a user-supplied asynchronous parser. Successful pages are passed
/// to `parser`, which yields JSON items and explicit follow-up requests.
/// The custom parser controls link discovery; default HTML link traversal
/// remains available through [`crawl`] without a parser.
///
/// Follow-up requests share `max_pages`, `max_depth`, domain/link rules,
/// deduplication, robots policy and checkpoints with the normal crawl.
/// `emit` receives pages and yielded items as they are completed, without
/// retaining them for the entire crawl.
pub async fn crawl_with_parser<F>(
    spec: &CrawlSpec,
    fetcher: Arc<dyn Fetcher>,
    parser: &dyn SpiderParser,
    emit: F,
) -> Result<CrawlSummary, String>
where
    F: for<'a> FnMut(SpiderEvent<'a>) -> Result<(), String>,
{
    crawl_inner(spec, fetcher, Some(parser), emit).await
}

async fn crawl_inner<F>(
    spec: &CrawlSpec,
    fetcher: Arc<dyn Fetcher>,
    parser: Option<&dyn SpiderParser>,
    mut emit: F,
) -> Result<CrawlSummary, String>
where
    F: for<'a> FnMut(SpiderEvent<'a>) -> Result<(), String>,
{
    spec.validate()?;
    let link_rules = LinkRules::compile(spec)?;
    let fetcher: Arc<dyn Fetcher> = if spec.development_mode {
        let directory = spec
            .development_cache_dir
            .clone()
            .or_else(|| spec.crawldir.as_ref().map(|dir| dir.join("dev-cache")))
            .unwrap_or_else(|| PathBuf::from(".rustscrapling-dev-cache"));
        Arc::new(cache::CachingFetcher::new(fetcher, directory)?)
    } else {
        fetcher
    };
    // Development runs must reprocess the same cached responses, even when
    // the usual crawldir checkpoint has no remaining requests to visit.
    let state = if !spec.development_mode {
        restore(spec)?
    } else {
        None
    };
    let state = if let Some(saved) = state {
        saved
    } else {
        let seeds = templates::resolve_seeds(spec, &fetcher).await?;
        let mut seen_seeds = HashSet::new();
        Progress {
            queue: seeds
                .into_iter()
                .filter_map(|raw| {
                    let mut url = valid_url(&raw).ok()?;
                    if !host_allowed(spec, &url) {
                        return None;
                    }
                    url.set_fragment(None);
                    let normalized = url.to_string();
                    seen_seeds
                        .insert(key(&normalized))
                        .then_some(StoredRequest::Legacy((normalized, 0)))
                })
                .take(spec.max_pages)
                .collect(),
            ..Progress::default()
        }
    };
    let mut visited: HashSet<String> = state.visited_hashes.into_iter().collect();
    let mut queue: VecDeque<QueuedRequest> = state.queue.into_iter().map(Into::into).collect();
    for pending in &mut queue {
        pending
            .request
            .validate_fields()
            .map_err(|e| e.to_string())?;
        pending
            .request
            .ensure_checkpoint_privacy(spec)
            .map_err(|e| e.to_string())?;
    }
    let mut reserved_payload_bytes = queue.iter().try_fold(0usize, |sum, pending| {
        sum.checked_add(request_payload_bytes(&pending.request))
            .filter(|total| *total <= MAX_RESERVED_REQUEST_PAYLOAD_BYTES)
            .ok_or_else(|| SpiderFetchError::FrontierCapacityExceeded.to_string())
    })?;
    let mut nonzero_priority_count = queue
        .iter()
        .filter(|pending| pending.request.priority != 0)
        .count();
    let mut queued: HashSet<String> = queue
        .iter()
        .map(|entry| request_key(&entry.request))
        .collect();
    let mut summary = CrawlSummary::default();
    // Shared per-host gates must run in the futures themselves. A timestamp
    // computed while filling a batch can be stale before the first poll.
    let mut site_clock = HashMap::<String, Arc<tokio::sync::Mutex<HostThrottle>>>::new();
    let mut robot_cache = HashMap::<String, Option<RobotsPolicy>>::new();
    while !queue.is_empty() && visited.len() < spec.max_pages {
        let mut batch = FuturesUnordered::new();
        while batch.len() < spec.concurrency && !queue.is_empty() && visited.len() < spec.max_pages
        {
            let mut pending = pop_next_request(&mut queue, &mut nonzero_priority_count).unwrap();
            let request_size = request_payload_bytes(&pending.request);
            let raw = pending.request.url.clone();
            let depth = pending.depth;
            queued.remove(&request_key(&pending.request));
            let url = match valid_url(&raw) {
                Ok(u) => u,
                Err(_) => {
                    reserved_payload_bytes = reserved_payload_bytes.saturating_sub(request_size);
                    continue;
                }
            };
            if depth > spec.max_depth || !host_allowed(spec, &url) {
                reserved_payload_bytes = reserved_payload_bytes.saturating_sub(request_size);
                continue;
            }
            // Persisted rich requests have already been normalized. Recheck
            // their transport bounds before dispatching an untrusted file.
            pending
                .request
                .validate_fields()
                .map_err(|e| e.to_string())?;
            let digest = request_key(&pending.request);
            if !visited.insert(digest) && !pending.request.dont_filter {
                reserved_payload_bytes = reserved_payload_bytes.saturating_sub(request_size);
                continue;
            }
            if spec.obey_robots && !robots_allowed(&url, &fetcher, &mut robot_cache).await {
                reserved_payload_bytes = reserved_payload_bytes.saturating_sub(request_size);
                summary.failed += 1;
                emit(SpiderEvent::Page(&CrawlItem {
                    url: raw,
                    depth,
                    status: None,
                    links: Vec::new(),
                    error: Some("blocked by robots.txt policy".into()),
                    body: None,
                    callback: pending.request.callback.clone(),
                    meta: pending.request.meta.clone(),
                }))?;
                continue;
            }
            let robots_minimum = if spec.obey_robots {
                robot_cache
                    .get(&url.origin().ascii_serialization())
                    .and_then(Option::as_ref)
                    .map(|policy| policy.minimum_delay)
                    .unwrap_or_default()
            } else {
                Duration::ZERO
            };
            let host_clock = if spec.delay_ms > 0 || spec.auto_throttle || !robots_minimum.is_zero()
            {
                let host = url.origin().ascii_serialization();
                let initial = Duration::from_millis(if spec.auto_throttle {
                    spec.delay_ms.max(100)
                } else {
                    spec.delay_ms
                })
                .max(robots_minimum);
                Some(Arc::clone(site_clock.entry(host).or_insert_with(|| {
                    Arc::new(tokio::sync::Mutex::new(HostThrottle::new(initial)))
                })))
            } else {
                None
            };
            let minimum = Duration::from_millis(if spec.auto_throttle {
                spec.delay_ms.max(100)
            } else {
                spec.delay_ms
            })
            .max(robots_minimum);
            let auto_throttle = spec.auto_throttle;
            let backend = Arc::clone(&fetcher);
            let request = pending.request;
            let origin = (request.callback.clone(), request.meta.clone());
            batch.push(async move {
                if let Some(ref host_clock) = host_clock {
                    // Do not keep the gate locked during sleep: an earlier
                    // response may arrive with a Retry-After while another
                    // request is already waiting to launch.
                    loop {
                        let mut gate = host_clock.lock().await;
                        let spaced = gate
                            .last_start
                            .and_then(|last| last.checked_add(gate.delay));
                        let deadline = match (spaced, gate.not_before) {
                            (Some(left), Some(right)) => Some(left.max(right)),
                            (left, right) => left.or(right),
                        };
                        if let Some(deadline) = deadline {
                            if deadline > Instant::now() {
                                drop(gate);
                                tokio::time::sleep_until(tokio::time::Instant::from_std(deadline))
                                    .await;
                                continue;
                            }
                        }
                        gate.last_start = Some(Instant::now());
                        gate.not_before = None;
                        break;
                    }
                    // Release the gate before awaiting the network response:
                    // different requests can overlap once their start times
                    // have been spaced apart.
                }
                let started = Instant::now();
                let result = match request.with_cookie_header() {
                    Ok(request) => backend
                        .fetch_request(&request)
                        .await
                        .map_err(|err| err.to_string()),
                    Err(err) => Err(err.to_string()),
                };
                if auto_throttle {
                    if let Some(ref host_clock) = host_clock {
                        let status = result.as_ref().ok().map(|response| response.status);
                        let retry_after = result
                            .as_ref()
                            .ok()
                            .and_then(|response| retry_after_value(&response.headers));
                        host_clock.lock().await.observe(
                            status,
                            started.elapsed(),
                            minimum,
                            retry_after,
                        );
                    }
                }
                (url.to_string(), depth, result, request_size, origin)
            });
        }
        while let Some((url, depth, result, request_size, (callback, meta))) = batch.next().await {
            // An in-flight request retains its reservation until it completes;
            // otherwise high concurrency can exceed the frontier memory cap.
            reserved_payload_bytes = reserved_payload_bytes.saturating_sub(request_size);
            let (item, response_base) = match result {
                Ok(response) => {
                    let response_base = parser.map(|_| response.url.clone());
                    let links = if parser.is_none()
                        && spec.template.follows_links()
                        && response.content_type.contains("html")
                        && depth < spec.max_depth
                    {
                        page_links_filtered(
                            &response.url,
                            &response.body,
                            spec.max_pages
                                .saturating_sub(visited.len())
                                .saturating_sub(queue.len()),
                            |candidate| {
                                host_allowed(spec, candidate)
                                    && link_rules.permitted(candidate)
                                    && !visited.contains(&key(candidate.as_str()))
                                    && !queued.contains(&key(candidate.as_str()))
                            },
                        )
                    } else {
                        Vec::new()
                    };
                    if (200..300).contains(&response.status) {
                        summary.successful += 1;
                    } else {
                        summary.failed += 1;
                    }
                    (
                        CrawlItem {
                            url,
                            depth,
                            status: Some(response.status),
                            links,
                            error: None,
                            body: Some(response.body),
                            callback: callback.clone(),
                            meta: meta.clone(),
                        },
                        response_base,
                    )
                }
                Err(err) => {
                    summary.failed += 1;
                    (
                        CrawlItem {
                            url,
                            depth,
                            status: None,
                            links: Vec::new(),
                            error: Some(err),
                            body: None,
                            callback,
                            meta,
                        },
                        None,
                    )
                }
            };
            for link in &item.links {
                if let Ok(parsed) = valid_url(link) {
                    if host_allowed(spec, &parsed)
                        && !visited.contains(&key(link))
                        && queued.insert(key(link))
                    {
                        queue.push_back(QueuedRequest {
                            request: SpiderRequest::new(link.clone()),
                            depth: depth + 1,
                        });
                    }
                }
            }
            emit(SpiderEvent::Page(&item))?;
            if let Some(parser) = parser {
                // Failure pages do not run application parsing, matching the
                // default successful-response parse behavior of Scrapling.
                let outputs = if item
                    .status
                    .is_some_and(|status| (200..300).contains(&status))
                {
                    // Upstream: an exception in a callback goes to
                    // `on_error(request, error)` and the crawl continues.
                    match parser.parse(&item).await {
                        Ok(outputs) => outputs,
                        Err(error) => {
                            summary.parse_errors += 1;
                            let failed = CrawlItem {
                                error: Some(error),
                                ..item.clone()
                            };
                            parser.on_error(&failed).await?
                        }
                    }
                } else if item.error.is_some() {
                    parser.on_error(&item).await?
                } else {
                    Vec::new()
                };
                {
                    for output in outputs {
                        match output {
                            SpiderOutput::Request(request) => enqueue_spider_request(
                                request,
                                response_base.as_deref().unwrap_or(&item.url),
                                depth,
                                spec,
                                &link_rules,
                                Frontier {
                                    visited: &visited,
                                    queued: &mut queued,
                                    queue: &mut queue,
                                    reserved_payload_bytes: &mut reserved_payload_bytes,
                                    nonzero_priority_count: &mut nonzero_priority_count,
                                },
                            )?,
                            SpiderOutput::Item(value) => {
                                // Upstream `on_scraped_item`: transform or drop.
                                if let Some(value) = parser.on_scraped_item(value) {
                                    emit(SpiderEvent::Item(&value))?
                                }
                            }
                        }
                    }
                }
            }
        }
        let mut fingerprints: Vec<String> = visited.iter().cloned().collect();
        fingerprints.sort();
        if !spec.development_mode {
            persist(
                spec,
                &Progress {
                    spec_hash: spec_hash(spec)?,
                    visited_hashes: fingerprints,
                    queue: queue.iter().cloned().map(Into::into).collect(),
                },
            )?;
        }
    }
    summary.visited = visited.len();
    summary.remaining = queue.len();
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Mock;
    impl Fetcher for Mock {
        fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
            Box::pin(async move {
                Ok(FetchPage {
                    headers: Default::default(),
                    status: 200,
                    url: url.into(),
                    content_type: "text/html".into(),
                    body: if url.ends_with("/a") {
                        "<a href='/b'>b</a><a href='/b#top'>repeat</a>".into()
                    } else {
                        "<p>done</p>".into()
                    },
                })
            })
        }
    }

    #[tokio::test]
    async fn dedupe_and_resume() {
        let temp = tempfile::tempdir().unwrap();
        let spec = CrawlSpec {
            start_urls: vec!["https://example.com/a".into()],
            crawldir: Some(temp.path().to_path_buf()),
            ..CrawlSpec::default()
        };
        let mut pages = Vec::new();
        let first = crawl(&spec, Arc::new(Mock), |item| {
            pages.push(item.url.clone());
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(first.successful, 2);
        assert_eq!(pages.len(), 2);
        let second = crawl(&spec, Arc::new(Mock), |_| panic!("should not refetch"))
            .await
            .unwrap();
        assert_eq!(second.successful, 0);
        assert_eq!(second.visited, 2);
    }

    struct TimedFetcher(Mutex<Vec<Instant>>);

    impl Fetcher for TimedFetcher {
        fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
            Box::pin(async move {
                self.0.lock().unwrap().push(Instant::now());
                Ok(FetchPage {
                    headers: Default::default(),
                    status: 200,
                    url: url.into(),
                    content_type: "text/plain".into(),
                    body: String::new(),
                })
            })
        }
    }

    #[tokio::test]
    async fn per_host_delay_applies_to_fetch_starts_not_batch_construction() {
        let fetcher = Arc::new(TimedFetcher(Mutex::new(Vec::new())));
        let spec = CrawlSpec {
            start_urls: vec![
                "https://example.com/one".into(),
                "https://example.com/two".into(),
                "https://example.com/three".into(),
            ],
            concurrency: 3,
            max_depth: 0,
            delay_ms: 60,
            ..CrawlSpec::default()
        };
        let summary = crawl(&spec, fetcher.clone(), |_| Ok(())).await.unwrap();
        assert_eq!(summary.successful, 3);
        let starts = fetcher.0.lock().unwrap();
        assert_eq!(starts.len(), 3);
        for times in starts.windows(2) {
            assert!(
                times[1].duration_since(times[0]) >= Duration::from_millis(40),
                "requests to one host must not launch together"
            );
        }
    }

    struct SlowRobotFetcher(Mutex<Vec<(String, Instant)>>);

    impl Fetcher for SlowRobotFetcher {
        fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
            Box::pin(async move {
                if url.ends_with("/robots.txt") {
                    if url.contains("other.example") {
                        tokio::time::sleep(Duration::from_millis(180)).await;
                    }
                    return Ok(FetchPage {
                        headers: Default::default(),
                        status: 200,
                        url: url.into(),
                        content_type: "text/plain".into(),
                        body: "User-agent: *\nAllow: /\n".into(),
                    });
                }
                self.0.lock().unwrap().push((url.into(), Instant::now()));
                Ok(FetchPage {
                    headers: Default::default(),
                    status: 200,
                    url: url.into(),
                    content_type: "text/plain".into(),
                    body: String::new(),
                })
            })
        }
    }

    #[tokio::test]
    async fn per_host_delay_survives_slow_batch_preparation() {
        let fetcher = Arc::new(SlowRobotFetcher(Mutex::new(Vec::new())));
        let spec = CrawlSpec {
            start_urls: vec![
                "https://example.com/first".into(),
                "https://other.example/page".into(),
                "https://example.com/second".into(),
            ],
            concurrency: 3,
            max_depth: 0,
            obey_robots: true,
            delay_ms: 90,
            ..CrawlSpec::default()
        };
        assert_eq!(
            crawl(&spec, fetcher.clone(), |_| Ok(()))
                .await
                .unwrap()
                .successful,
            3
        );
        let starts = fetcher.0.lock().unwrap();
        let same_host: Vec<_> = starts
            .iter()
            .filter(|(url, _)| url.contains("example.com"))
            .map(|(_, time)| *time)
            .collect();
        assert_eq!(same_host.len(), 2);
        assert!(same_host[1].duration_since(same_host[0]) >= Duration::from_millis(65));
    }

    #[test]
    fn auto_throttle_backoff_and_recovery_are_bounded() {
        let base = Duration::from_millis(100);
        let mut throttle = HostThrottle::new(base);
        throttle.observe(Some(429), Duration::from_millis(20), base, None);
        assert_eq!(throttle.delay, Duration::from_millis(200));
        throttle.observe(Some(503), Duration::from_millis(10), base, None);
        assert_eq!(throttle.delay, Duration::from_millis(400));
        throttle.observe(Some(200), Duration::from_millis(20), base, None);
        assert_eq!(throttle.delay, Duration::from_millis(325));
        for _ in 0..20 {
            throttle.observe(None, Duration::from_millis(1), base, None);
        }
        assert_eq!(throttle.delay, Duration::from_secs(30));
        for _ in 0..100 {
            throttle.observe(Some(200), Duration::from_millis(2), base, None);
        }
        assert_eq!(throttle.delay, base);
    }

    #[test]
    fn retry_after_seconds_and_http_date_are_bounded() {
        let mut headers = BTreeMap::new();
        headers.insert("rEtRy-AfTeR".into(), " 12 ".into());
        assert_eq!(retry_after_value(&headers), Some(Duration::from_secs(12)));
        for bad in ["NaN", "-1", "86401", "not-a-date"] {
            headers.insert("rEtRy-AfTeR".into(), bad.into());
            assert_eq!(retry_after_value(&headers), None, "{bad}");
        }
        let future = std::time::SystemTime::now() + Duration::from_secs(3);
        let date = chrono::DateTime::<chrono::Utc>::from(future)
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        headers.insert("rEtRy-AfTeR".into(), date);
        let parsed = retry_after_value(&headers).expect("HTTP-date is supported");
        assert!(parsed >= Duration::from_secs(1) && parsed <= Duration::from_secs(3));
        headers.insert("rEtRy-AfTeR".into(), "Wed, 21 Oct 2015 07:28:00 GMT".into());
        assert_eq!(retry_after_value(&headers), Some(Duration::ZERO));
    }

    struct RetryAfterFetcher(Mutex<Vec<Instant>>);

    impl Fetcher for RetryAfterFetcher {
        fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
            Box::pin(async move {
                self.0.lock().unwrap().push(Instant::now());
                let throttled = url.ends_with("/first");
                let mut headers = BTreeMap::new();
                if throttled {
                    headers.insert("Retry-After".into(), "1".into());
                }
                Ok(FetchPage {
                    status: if throttled { 429 } else { 200 },
                    url: url.into(),
                    content_type: "text/plain".into(),
                    body: String::new(),
                    headers,
                })
            })
        }
    }

    #[tokio::test]
    async fn concurrent_waiting_request_rechecks_retry_after_backpressure() {
        let fetcher = Arc::new(RetryAfterFetcher(Mutex::new(Vec::new())));
        let spec = CrawlSpec {
            start_urls: vec![
                "https://example.com/first".into(),
                "https://example.com/second".into(),
            ],
            concurrency: 2,
            max_depth: 0,
            auto_throttle: true,
            ..CrawlSpec::default()
        };
        let summary = crawl(&spec, fetcher.clone(), |_| Ok(())).await.unwrap();
        assert_eq!(summary.successful, 1);
        assert_eq!(summary.failed, 1);
        let starts = fetcher.0.lock().unwrap();
        assert_eq!(starts.len(), 2);
        assert!(
            starts[1].duration_since(starts[0]) >= Duration::from_millis(900),
            "Retry-After must delay even a previously waiting concurrent request"
        );
    }

    #[test]
    fn default_auto_throttle_does_not_change_legacy_spec_hash() {
        let default = CrawlSpec::default();
        let json = serde_json::to_value(&default).unwrap();
        assert!(json.get("auto_throttle").is_none());
        assert!(json.get("development_mode").is_none());
        assert!(json.get("development_cache_dir").is_none());
        assert!(json.get("link_allow").is_none());
        assert!(json.get("link_deny").is_none());
        let enabled = CrawlSpec {
            auto_throttle: true,
            ..default.clone()
        };
        assert_ne!(spec_hash(&default).unwrap(), spec_hash(&enabled).unwrap());
        assert!(
            CrawlSpec::from_toml("start_urls=['https://example.com/']\nauto_throttle=true")
                .unwrap()
                .auto_throttle
        );
    }

    struct OfflineReplaySource(std::sync::atomic::AtomicUsize);

    impl Fetcher for OfflineReplaySource {
        fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
            Box::pin(async move {
                use std::sync::atomic::Ordering;
                let calls = self.0.fetch_add(1, Ordering::SeqCst);
                if calls >= 2 {
                    return Err("offline source must not be fetched again".into());
                }
                Ok(FetchPage {
                    headers: Default::default(),
                    status: 200,
                    url: url.to_owned(),
                    content_type: "text/html".into(),
                    body: if url.ends_with("/start") {
                        "<a href='/second'>second</a>".into()
                    } else {
                        "<h1>replayed</h1>".into()
                    },
                })
            })
        }
    }

    #[tokio::test]
    async fn development_cache_replays_full_crawl_without_checkpoint_short_circuit() {
        use std::sync::atomic::Ordering;
        let temp = tempfile::tempdir().unwrap();
        let source = Arc::new(OfflineReplaySource(std::sync::atomic::AtomicUsize::new(0)));
        let spec = CrawlSpec {
            start_urls: vec!["https://example.org/start".into()],
            max_pages: 2,
            max_depth: 1,
            development_mode: true,
            development_cache_dir: Some(temp.path().join("cached-pages")),
            crawldir: Some(temp.path().join("state")),
            ..CrawlSpec::default()
        };
        let mut original = Vec::new();
        assert_eq!(
            crawl(&spec, source.clone(), |item| {
                original.push(item.body.clone());
                Ok(())
            })
            .await
            .unwrap()
            .successful,
            2
        );
        let mut replayed = Vec::new();
        assert_eq!(
            crawl(&spec, source.clone(), |item| {
                replayed.push(item.body.clone());
                Ok(())
            })
            .await
            .unwrap()
            .successful,
            2
        );
        assert_eq!(original, replayed);
        assert_eq!(source.0.load(Ordering::SeqCst), 2);
        assert!(!temp.path().join("state/crawl-state.json").exists());
    }

    struct ThrottledFetcher(Mutex<Vec<Instant>>);

    impl Fetcher for ThrottledFetcher {
        fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
            Box::pin(async move {
                let mut starts = self.0.lock().unwrap();
                starts.push(Instant::now());
                let status = if url.ends_with("/one") { 429 } else { 200 };
                Ok(FetchPage {
                    headers: Default::default(),
                    status,
                    url: url.into(),
                    content_type: "text/plain".into(),
                    body: String::new(),
                })
            })
        }
    }

    #[tokio::test]
    async fn auto_throttle_respects_server_backpressure_between_batches() {
        let fetcher = Arc::new(ThrottledFetcher(Mutex::new(Vec::new())));
        let spec = CrawlSpec {
            start_urls: vec![
                "https://example.com/one".into(),
                "https://example.com/two".into(),
            ],
            max_depth: 0,
            concurrency: 1,
            auto_throttle: true,
            ..CrawlSpec::default()
        };
        let result = crawl(&spec, fetcher.clone(), |_| Ok(())).await.unwrap();
        assert_eq!(result.failed, 1);
        assert_eq!(result.successful, 1);
        let starts = fetcher.0.lock().unwrap();
        assert_eq!(starts.len(), 2);
        assert!(
            starts[1].duration_since(starts[0]) >= Duration::from_millis(180),
            "429 should increase the next same-host delay to around 200ms"
        );
    }

    #[test]
    fn rejects_unsafe_urls_and_dedupes_fragments() {
        assert!(CrawlSpec::from_toml("start_urls=['file:///etc/passwd']").is_err());
        assert!(CrawlSpec::from_toml("start_urls=['https://name:secret@example.com/']").is_err());
        let links = page_links(
            "https://example.com/a",
            "<a href='/b'>b</a><a href='/b#x'>b2</a><a href='javascript:alert(1)'>no</a>",
        );
        assert_eq!(links, vec!["https://example.com/b"]);
    }

    #[test]
    fn respects_html_base_and_bounds_link_discovery() {
        let html = "<html><head><base href='https://example.com/docs/'></head><body>\
            <a href='one#top'>first</a><a href='one#next'>duplicate</a>\
            <a href='two'>second</a><a href='three'>third</a></body></html>";
        assert_eq!(
            page_links_limited("https://example.com/index", html, 2),
            vec![
                "https://example.com/docs/one".to_owned(),
                "https://example.com/docs/two".to_owned(),
            ]
        );
        assert!(page_links_limited("https://example.com/index", html, 0).is_empty());
        assert_eq!(
            page_links_limited(
                "https://example.com/index",
                "<html><head><base href='javascript:alert(1)'></head><body><a href='/safe'>safe</a></body></html>",
                1,
            ),
            vec!["https://example.com/safe".to_owned()]
        );
    }

    struct LinkFlood;
    impl Fetcher for LinkFlood {
        fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
            Box::pin(async move {
                let body = if url.ends_with("/start") {
                    (0..2000)
                        .map(|id| format!("<a href='/page/{id}'>Page</a>"))
                        .collect::<String>()
                } else {
                    "<p>Done</p>".to_owned()
                };
                Ok(FetchPage {
                    headers: Default::default(),
                    status: 200,
                    url: url.to_owned(),
                    content_type: "text/html".into(),
                    body,
                })
            })
        }
    }

    #[tokio::test]
    async fn large_link_pages_do_not_grow_queue_beyond_max_pages() {
        let spec = CrawlSpec {
            start_urls: vec!["https://example.com/start".into()],
            max_pages: 3,
            max_depth: 1,
            concurrency: 1,
            ..CrawlSpec::default()
        };
        let mut visited = Vec::new();
        let result = crawl(&spec, Arc::new(LinkFlood), |item| {
            visited.push(item.url.clone());
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(result.visited, 3);
        assert_eq!(result.remaining, 0);
        assert_eq!(visited.len(), 3);
        assert!(visited[1].ends_with("/page/0"));
        assert!(visited[2].ends_with("/page/1"));
    }

    struct FrontLoadedNavigation;

    impl Fetcher for FrontLoadedNavigation {
        fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
            Box::pin(async move {
                let body = if url.ends_with("/start") {
                    let mut links = (0..400)
                        .map(|id| format!("<a href='https://outside.example/{id}'>external</a>"))
                        .collect::<String>();
                    links.push_str(&"<a href='/start'>already visited</a>".repeat(400));
                    links.push_str("<a href='/target'>wanted link</a>");
                    links
                } else {
                    "<p>target found</p>".to_owned()
                };
                Ok(FetchPage {
                    headers: Default::default(),
                    status: 200,
                    url: url.to_owned(),
                    content_type: "text/html".into(),
                    body,
                })
            })
        }
    }

    #[tokio::test]
    async fn link_budget_skips_external_and_visited_links_before_counting() {
        let spec = CrawlSpec {
            start_urls: vec!["https://example.com/start".into()],
            max_pages: 2,
            max_depth: 1,
            concurrency: 1,
            ..CrawlSpec::default()
        };
        let mut emitted = Vec::new();
        let result = crawl(&spec, Arc::new(FrontLoadedNavigation), |item| {
            emitted.push(item.clone());
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(result.successful, 2);
        assert_eq!(result.visited, 2);
        assert_eq!(result.remaining, 0);
        assert_eq!(emitted.len(), 2);
        assert_eq!(emitted[0].links, vec!["https://example.com/target"]);
        assert_eq!(emitted[1].url, "https://example.com/target");
    }

    struct FilteredNavigation;

    impl Fetcher for FilteredNavigation {
        fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
            Box::pin(async move {
                let body = if url.ends_with("/start") {
                    let mut body = (0..300)
                        .map(|i| format!("<a href='/blog/{i}'>Unwanted</a>"))
                        .collect::<String>();
                    body.push_str("<a href='/docs/private'>Denied</a>");
                    body.push_str("<a href='/docs/guide'>Allowed</a>");
                    body
                } else {
                    "<p>Selected document</p>".to_owned()
                };
                Ok(FetchPage {
                    status: 200,
                    url: url.to_owned(),
                    content_type: "text/html".into(),
                    body,
                    headers: Default::default(),
                })
            })
        }
    }

    #[tokio::test]
    async fn link_allow_deny_filter_before_frontier_budget_without_filtering_seeds() {
        let spec = CrawlSpec {
            start_urls: vec!["https://example.com/start".into()],
            max_pages: 2,
            max_depth: 1,
            concurrency: 1,
            link_allow: vec![r"/docs/".into()],
            link_deny: vec![r"/docs/private(?:/|$)".into()],
            ..CrawlSpec::default()
        };
        let mut urls = Vec::new();
        let summary = crawl(&spec, Arc::new(FilteredNavigation), |page| {
            urls.push(page.url.clone());
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(summary.visited, 2);
        assert_eq!(summary.remaining, 0);
        assert_eq!(
            urls,
            [
                "https://example.com/start",
                "https://example.com/docs/guide"
            ]
        );
    }

    #[tokio::test]
    async fn unsafe_link_regex_is_rejected_before_fetch() {
        let spec = CrawlSpec {
            start_urls: vec!["https://example.com/start".into()],
            link_allow: vec!["(".into()],
            ..CrawlSpec::default()
        };
        let error = crawl(&spec, Arc::new(Mock), |_| panic!("must not emit"))
            .await
            .unwrap_err();
        assert!(error.contains("invalid link_allow regex"));
        let spec = CrawlSpec {
            link_allow: vec!["a".into(); 65],
            ..spec
        };
        assert!(spec.validate().unwrap_err().contains("at most 64"));
    }

    #[tokio::test]
    async fn seed_limit_counts_unique_eligible_urls_not_duplicate_prefixes() {
        let mut seeds = (0..200)
            .map(|id| format!("https://example.com/first#section{id}"))
            .collect::<Vec<_>>();
        seeds.push("https://example.com/second".into());
        let spec = CrawlSpec {
            start_urls: seeds,
            max_pages: 2,
            max_depth: 0,
            concurrency: 1,
            ..CrawlSpec::default()
        };
        let mut urls = Vec::new();
        let result = crawl(&spec, Arc::new(Mock), |item| {
            urls.push(item.url.clone());
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(result.successful, 2);
        assert_eq!(
            urls,
            ["https://example.com/first", "https://example.com/second"]
        );
    }

    #[test]
    fn robots_specificity_and_wildcards() {
        let policy = parse_robots(
            "User-agent: *\nDisallow: /private\nAllow: /private/public\nDisallow: /*.secret$\nDisallow: /search?private=true\n",
        );
        assert!(!robots_permitted(&policy, "/private/key"));
        assert!(robots_permitted(&policy, "/private/public/a"));
        assert!(!robots_permitted(&policy, "/x.secret"));
        assert!(robots_permitted(&policy, "/public"));
        assert!(!robots_permitted(&policy, "/search?private=true"));
        assert!(robots_permitted(&policy, "/search?private=false"));
    }

    #[test]
    fn robots_pacing_merges_matching_groups_and_ignores_other_agents() {
        let policy = parse_robots_policy(
            "User-agent: *\nDisallow: /everything\nCrawl-delay: 10\n\
             User-agent: RustScrapling\nAllow: /public\nCrawl-delay: 0.15\n\
             User-agent: RustScrapling\nDisallow: /private\nRequest-rate: 2/0.8\n\
             User-agent: OtherBot\nDisallow: /public\nCrawl-delay: 100",
        );
        assert_eq!(policy.minimum_delay, Duration::from_millis(400));
        assert!(!robots_permitted(&policy.rules, "/private/a"));
        assert!(robots_permitted(&policy.rules, "/everything"));
        assert!(robots_permitted(&policy.rules, "/public"));
        let general = parse_robots_policy(
            "User-agent: *\nDisallow: /private\nCrawl-delay: 0.25\nRequest-rate: 2/0.8",
        );
        assert_eq!(general.minimum_delay, Duration::from_millis(400));
        assert!(!robots_permitted(&general.rules, "/private"));
    }

    #[test]
    fn robots_invalid_pacing_values_cannot_panic_or_stall_crawler() {
        for invalid in ["NaN", "inf", "-1", "1e300", "86401", "abcd"] {
            assert_eq!(robots_seconds(invalid), None, "{invalid}");
        }
        for invalid in ["0/5", "2/NaN", "-5/10", "5/999999", "oops"] {
            assert_eq!(robots_request_rate(invalid), None, "{invalid}");
        }
        assert_eq!(
            robots_request_rate("3/1"),
            Some(Duration::from_nanos(333_333_334))
        );
    }

    struct RobotPacingFetcher(Mutex<Vec<Instant>>);

    impl Fetcher for RobotPacingFetcher {
        fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
            Box::pin(async move {
                if url.ends_with("/robots.txt") {
                    return Ok(FetchPage {
                        headers: Default::default(),
                        status: 200,
                        url: url.to_owned(),
                        content_type: "text/plain".into(),
                        body: "User-agent: *\nAllow: /\nCrawl-delay: 0.04\nRequest-rate: 1/0.12"
                            .into(),
                    });
                }
                self.0.lock().unwrap().push(Instant::now());
                Ok(FetchPage {
                    headers: Default::default(),
                    status: 200,
                    url: url.into(),
                    content_type: "text/plain".into(),
                    body: String::new(),
                })
            })
        }
    }

    #[tokio::test]
    async fn robots_request_rate_is_enforced_without_other_delay() {
        let fetcher = Arc::new(RobotPacingFetcher(Mutex::new(Vec::new())));
        let spec = CrawlSpec {
            start_urls: vec![
                "https://example.com/one".into(),
                "https://example.com/two".into(),
                "https://example.com/three".into(),
            ],
            max_depth: 0,
            concurrency: 3,
            obey_robots: true,
            delay_ms: 0,
            auto_throttle: false,
            ..CrawlSpec::default()
        };
        let summary = crawl(&spec, fetcher.clone(), |_| Ok(())).await.unwrap();
        assert_eq!(summary.successful, 3);
        let starts = fetcher.0.lock().unwrap();
        assert_eq!(starts.len(), 3);
        for pair in starts.windows(2) {
            assert!(pair[1].duration_since(pair[0]) >= Duration::from_millis(95));
        }
    }

    struct RobotFetcher;
    impl Fetcher for RobotFetcher {
        fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
            Box::pin(async move {
                Ok(FetchPage {
                    headers: Default::default(),
                    status: 200,
                    url: url.into(),
                    content_type: "text/plain".into(),
                    body: "User-agent: *\nDisallow: /search?private=true\n".into(),
                })
            })
        }
    }

    #[tokio::test]
    async fn robots_rules_cover_query_parameters() {
        let backend: Arc<dyn Fetcher> = Arc::new(RobotFetcher);
        let mut cache = HashMap::new();
        let blocked = Url::parse("https://example.com/search?private=true").unwrap();
        let allowed = Url::parse("https://example.com/search?private=false").unwrap();
        assert!(!robots_allowed(&blocked, &backend, &mut cache).await);
        assert!(robots_allowed(&allowed, &backend, &mut cache).await);
    }

    #[test]
    fn resume_rejects_mismatched_spec_and_recovers_backup() {
        let dir = tempfile::tempdir().unwrap();
        let spec = CrawlSpec {
            start_urls: vec!["https://example.org/one".into()],
            crawldir: Some(dir.path().to_path_buf()),
            ..CrawlSpec::default()
        };
        persist(&spec, &Progress::default()).unwrap();
        let other = CrawlSpec {
            start_urls: vec!["https://example.org/two".into()],
            ..spec.clone()
        };
        assert!(restore(&other)
            .unwrap_err()
            .contains("different crawl specification"));
        let file = state_path(&spec).unwrap();
        std::fs::copy(&file, file.with_extension("json.bak")).unwrap();
        std::fs::write(&file, b"{incomplete").unwrap();
        assert!(restore(&spec).unwrap().is_some());
    }

    #[cfg(unix)]
    #[test]
    fn checkpoint_is_private_and_never_reads_a_state_symlink() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let dir = tempfile::tempdir().unwrap();
        let crawldir = dir.path().join("private-state");
        let spec = CrawlSpec {
            start_urls: vec!["https://example.org/one?token=sensitive".into()],
            crawldir: Some(crawldir.clone()),
            ..CrawlSpec::default()
        };
        persist(&spec, &Progress::default()).unwrap();
        let state = state_path(&spec).unwrap();
        assert_eq!(
            std::fs::metadata(&crawldir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&state).unwrap().permissions().mode() & 0o777,
            0o600
        );

        // Legacy checkpoints with public permissions are secured on read.
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(restore(&spec).unwrap().is_some());
        assert_eq!(
            std::fs::metadata(&state).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let unrelated = dir.path().join("unrelated.json");
        std::fs::write(&unrelated, b"not a crawl checkpoint").unwrap();
        std::fs::remove_file(&state).unwrap();
        symlink(&unrelated, &state).unwrap();
        assert!(restore(&spec).unwrap_err().contains("not a symlink"));
        assert_eq!(
            std::fs::read(&unrelated).unwrap(),
            b"not a crawl checkpoint"
        );
    }

    struct CallbackFixture(Mutex<Vec<String>>);

    impl Fetcher for CallbackFixture {
        fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
            Box::pin(async move {
                self.0.lock().unwrap().push(url.to_owned());
                Ok(FetchPage {
                    headers: Default::default(),
                    status: 200,
                    url: url.to_owned(),
                    content_type: "text/html".into(),
                    body: if url.ends_with("/robots.txt") {
                        "User-agent: *\nDisallow: /blocked\n".into()
                    } else {
                        "<a href='/html-only'>Do not follow automatically</a>".into()
                    },
                })
            })
        }
    }

    struct FollowUpParser;

    impl SpiderParser for FollowUpParser {
        fn parse<'a>(&'a self, page: &'a CrawlItem) -> ParseFuture<'a> {
            Box::pin(async move {
                tokio::task::yield_now().await;
                let mut outputs = vec![SpiderOutput::Item(serde_json::json!({
                    "page": page.url,
                    "depth": page.depth,
                }))];
                if page.url.ends_with("/start") {
                    for next in [
                        "#self",
                        "/next#one",
                        "https://example.com/next#two",
                        "https://example.com/denied",
                        "https://elsewhere.example/escape",
                        "https://name:secret@example.com/private",
                        "file:///local/private",
                        "/blocked",
                        "/overflow",
                    ] {
                        outputs.push(SpiderOutput::Request(SpiderRequest::new(next)));
                    }
                } else {
                    outputs.push(SpiderOutput::Request(SpiderRequest::new("/too-deep")));
                }
                Ok(outputs)
            })
        }
    }

    #[tokio::test]
    async fn custom_parser_streams_items_and_routes_requests_through_frontier_guards() {
        let fetcher = Arc::new(CallbackFixture(Mutex::new(Vec::new())));
        let spec = CrawlSpec {
            start_urls: vec!["https://example.com/start".into()],
            max_pages: 3,
            max_depth: 1,
            concurrency: 1,
            obey_robots: true,
            link_deny: vec!["/denied".into()],
            ..CrawlSpec::default()
        };
        let mut page_urls = Vec::new();
        let mut emitted_items = Vec::new();
        let summary = crawl_with_parser(&spec, fetcher.clone(), &FollowUpParser, |event| {
            match event {
                SpiderEvent::Page(page) => page_urls.push((page.url.clone(), page.error.clone())),
                SpiderEvent::Item(item) => emitted_items.push(item.clone()),
            }
            Ok(())
        })
        .await
        .unwrap();

        assert_eq!(summary.visited, 3);
        assert_eq!(summary.successful, 2);
        assert_eq!(summary.failed, 1); // blocked by robots
        assert_eq!(summary.remaining, 0);
        assert_eq!(page_urls.len(), 3);
        assert_eq!(page_urls[0].0, "https://example.com/start");
        assert_eq!(page_urls[1].0, "https://example.com/next");
        assert_eq!(page_urls[2].0, "https://example.com/blocked");
        assert!(page_urls[2].1.as_ref().unwrap().contains("robots"));
        assert_eq!(
            emitted_items,
            [
                serde_json::json!({"page": "https://example.com/start", "depth": 0}),
                serde_json::json!({"page": "https://example.com/next", "depth": 1}),
            ]
        );
        let fetched = fetcher.0.lock().unwrap();
        assert_eq!(
            fetched
                .iter()
                .filter(|url| url.ends_with("/robots.txt"))
                .count(),
            1
        );
        assert!(!fetched.iter().any(|url| url.ends_with("/blocked")));
        assert!(!fetched.iter().any(|url| url.ends_with("/html-only")));
        assert!(!fetched.iter().any(|url| url.ends_with("/too-deep")));
        assert!(!fetched.iter().any(|url| url.ends_with("/denied")));
        assert!(!fetched.iter().any(|url| url.contains("elsewhere.example")));
    }

    #[tokio::test]
    async fn custom_parser_respects_max_pages_and_legacy_checkpoint_resume() {
        let directory = tempfile::tempdir().unwrap();
        let spec = CrawlSpec {
            start_urls: vec!["https://example.com/start".into()],
            max_pages: 2,
            max_depth: 2,
            crawldir: Some(directory.path().to_path_buf()),
            ..CrawlSpec::default()
        };
        let first = crawl_with_parser(
            &spec,
            Arc::new(CallbackFixture(Mutex::new(Vec::new()))),
            &FollowUpParser,
            |_| Ok(()),
        )
        .await
        .unwrap();
        assert_eq!(first.successful, 2);
        assert_eq!(first.visited, 2);
        assert_eq!(first.remaining, 0);
        let restored = restore(&spec).unwrap().unwrap();
        assert_eq!(restored.visited_hashes.len(), 2);
        assert!(restored.queue.is_empty());
        // Both crawl variants use the original checkpoint format and spec hash.
        let second = crawl(&spec, Arc::new(Mock), |_| panic!("must not refetch"))
            .await
            .unwrap();
        assert_eq!(second.visited, 2);
        assert_eq!(second.successful, 0);
        let third = crawl_with_parser(&spec, Arc::new(Mock), &FollowUpParser, |_| {
            panic!("must not parse or refetch")
        })
        .await
        .unwrap();
        assert_eq!(third.visited, 2);
    }

    struct RedirectFixture;

    impl Fetcher for RedirectFixture {
        fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
            Box::pin(async move {
                Ok(FetchPage {
                    headers: Default::default(),
                    status: 200,
                    url: if url.ends_with("/start") {
                        "https://example.com/final/sub/page".into()
                    } else {
                        url.into()
                    },
                    content_type: "text/plain".into(),
                    body: "ok".into(),
                })
            })
        }
    }

    struct RelativeParser;

    impl SpiderParser for RelativeParser {
        fn parse<'a>(&'a self, page: &'a CrawlItem) -> ParseFuture<'a> {
            Box::pin(async move {
                if page.depth == 0 {
                    Ok(vec![SpiderOutput::Request(SpiderRequest::new("child"))])
                } else {
                    Ok(Vec::new())
                }
            })
        }
    }

    #[tokio::test]
    async fn relative_parser_requests_resolve_from_redirect_destination() {
        let spec = CrawlSpec {
            start_urls: vec!["https://example.com/start".into()],
            max_pages: 2,
            max_depth: 1,
            link_allow: vec!["/final/sub/child".into()],
            ..CrawlSpec::default()
        };
        let mut pages = Vec::new();
        let summary = crawl_with_parser(&spec, Arc::new(RedirectFixture), &RelativeParser, |e| {
            if let SpiderEvent::Page(page) = e {
                pages.push(page.url.clone());
            }
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(summary.visited, 2);
        assert_eq!(
            pages,
            [
                "https://example.com/start",
                "https://example.com/final/sub/child"
            ]
        );
    }

    struct RichRecorder(Mutex<Vec<SpiderRequest>>);

    impl Fetcher for RichRecorder {
        fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
            Box::pin(async move { panic!("rich path must call fetch_request, got URL-only {url}") })
        }

        fn fetch_request<'a>(&'a self, request: &'a SpiderRequest) -> RequestFetchFuture<'a> {
            Box::pin(async move {
                self.0.lock().unwrap().push(request.clone());
                Ok(FetchPage {
                    headers: Default::default(),
                    status: 200,
                    url: request.url.clone(),
                    content_type: "text/plain".into(),
                    body: "ok".into(),
                })
            })
        }
    }

    struct RichParser;

    impl SpiderParser for RichParser {
        fn parse<'a>(&'a self, page: &'a CrawlItem) -> ParseFuture<'a> {
            Box::pin(async move {
                if page.depth != 0 {
                    return Ok(Vec::new());
                }
                let post = SpiderRequest::new("/same#fragment")
                    .method("post")
                    .header("x-test", "first")
                    .body(b"alpha".to_vec())
                    .priority(10);
                Ok(vec![
                    SpiderOutput::Request(SpiderRequest::new("/same")),
                    SpiderOutput::Request(post.clone()),
                    // Changed scheduling priority cannot bypass dedupe.
                    SpiderOutput::Request(post.priority(20)),
                    SpiderOutput::Request(
                        SpiderRequest::new("/same")
                            .method("POST")
                            .header("x-test", "first")
                            .body(b"beta".to_vec())
                            .priority(5),
                    ),
                    SpiderOutput::Request(
                        SpiderRequest::new("/same")
                            .method("POST")
                            .header("x-test", "first")
                            .body(b"gamma".to_vec())
                            .priority(5),
                    ),
                    SpiderOutput::Request(
                        SpiderRequest::new("/same")
                            .method("POST")
                            .header("x-test", "second")
                            .body(b"alpha".to_vec())
                            .priority(7),
                    ),
                    SpiderOutput::Request(
                        SpiderRequest::new("/same")
                            .method("PUT")
                            .header("x-test", "first")
                            .body(b"alpha".to_vec())
                            .priority(1),
                    ),
                ])
            })
        }
    }

    #[tokio::test]
    async fn rich_requests_reach_fetcher_with_headers_body_priority_and_distinct_dedupe() {
        let fetcher = Arc::new(RichRecorder(Mutex::new(Vec::new())));
        let spec = CrawlSpec {
            start_urls: vec!["https://example.org/start".into()],
            max_pages: 7,
            max_depth: 1,
            concurrency: 1,
            ..CrawlSpec::default()
        };
        let summary = crawl_with_parser(&spec, fetcher.clone(), &RichParser, |_| Ok(()))
            .await
            .unwrap();
        assert_eq!(summary.visited, 7);
        assert_eq!(summary.successful, 7);
        let recorded = fetcher.0.lock().unwrap();
        assert_eq!(recorded.len(), 7);
        assert_eq!(recorded[0].url, "https://example.org/start");
        assert_eq!(
            recorded[1..].iter().map(|r| r.priority).collect::<Vec<_>>(),
            [10, 7, 5, 5, 1, 0]
        );
        assert_eq!(recorded[1].method, "POST");
        assert_eq!(recorded[1].body.as_deref(), Some(b"alpha".as_slice()));
        assert_eq!(recorded[1].headers["x-test"], "first");
        assert_eq!(recorded[1].url, "https://example.org/same");
        assert_eq!(recorded[2].headers["x-test"], "second");
        assert_eq!(recorded[3].body.as_deref(), Some(b"beta".as_slice()));
        assert_eq!(recorded[4].body.as_deref(), Some(b"gamma".as_slice()));
        assert_eq!(recorded[5].method, "PUT");
        assert_eq!(recorded[6].method, "GET");
    }

    #[tokio::test]
    async fn legacy_fetcher_does_not_silently_convert_rich_requests_to_get() {
        let error = Mock
            .fetch_request(&SpiderRequest::new("https://example.org/").method("POST"))
            .await
            .unwrap_err();
        assert!(matches!(error, SpiderFetchError::Unsupported(_)));
        assert!(Mock
            .fetch_request(&SpiderRequest::new("https://example.org/"))
            .await
            .is_ok());
        assert!(Mock
            .fetch_request(&SpiderRequest::new("https://example.org/").method("get"))
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn sensitive_requests_require_explicit_checkpoint_opt_in() {
        let dir = tempfile::tempdir().unwrap();
        let spec = CrawlSpec {
            start_urls: vec!["https://example.org/start".into()],
            crawldir: Some(dir.path().to_path_buf()),
            max_pages: 6,
            ..CrawlSpec::default()
        };
        let fetcher = Arc::new(RichRecorder(Mutex::new(Vec::new())));
        let error = crawl_with_parser(&spec, fetcher, &RichParser, |_| Ok(()))
            .await
            .unwrap_err();
        assert!(error.contains("persist_sensitive_requests"));
        assert!(!error.contains("first"));
        assert!(!state_path(&spec).unwrap().exists());
        // Priority/method alone have no sensitive checkpoint payload.
        let request = SpiderRequest::new("https://example.org/next")
            .method("DELETE")
            .priority(4);
        assert!(request.ensure_checkpoint_privacy(&spec).is_ok());
        assert!(matches!(
            request
                .header("Authorization", "secret")
                .ensure_checkpoint_privacy(&spec),
            Err(SpiderFetchError::SensitivePersistenceDenied)
        ));
    }

    #[test]
    fn header_case_identity_and_debug_redaction() {
        let mut valid_extension_method =
            SpiderRequest::new("https://example.org/").method("m-search");
        valid_extension_method.validate_fields().unwrap();
        assert_eq!(valid_extension_method.method, "M-SEARCH");
        let left = SpiderRequest::new("https://example.org/x?token=not-for-logs")
            .method("post")
            .header("X-Z", "private-value")
            .header("x-a", "another-private-value")
            .body(b"private-body".to_vec());
        let right = SpiderRequest::new("https://example.org/x?token=not-for-logs")
            .method("POST")
            .header("x-z", "private-value")
            .header("X-A", "another-private-value")
            .body(b"private-body".to_vec());
        assert_eq!(request_key(&left), request_key(&right));
        let debug = format!("{left:?}");
        for secret in ["not-for-logs", "private-value", "private-body"] {
            assert!(!debug.contains(secret));
        }
        let mut duplicate = SpiderRequest::new("https://example.org/x")
            .header("X-Key", "one")
            .header("x-key", "two");
        assert_eq!(
            duplicate.validate_fields(),
            Err(SpiderFetchError::InvalidRequest("invalid request headers"))
        );
    }

    #[test]
    fn frontier_payload_budget_is_bounded_and_preserves_state_on_failure() {
        let spec = CrawlSpec {
            start_urls: vec!["https://example.org/start".into()],
            max_pages: 10,
            max_depth: 1,
            ..CrawlSpec::default()
        };
        let rules = LinkRules {
            allow: Vec::new(),
            deny: Vec::new(),
        };
        let visited = HashSet::new();
        let mut queued = HashSet::new();
        let mut queue = VecDeque::new();
        let mut payload_bytes = 0;
        let mut nonzero_priority_count = 0;
        for i in 0..4 {
            enqueue_spider_request(
                SpiderRequest::new(format!("/upload-{i}"))
                    .method("POST")
                    .body(vec![42; 8 * 1024 * 1024]),
                "https://example.org/start",
                0,
                &spec,
                &rules,
                Frontier {
                    visited: &visited,
                    queued: &mut queued,
                    queue: &mut queue,
                    reserved_payload_bytes: &mut payload_bytes,
                    nonzero_priority_count: &mut nonzero_priority_count,
                },
            )
            .unwrap();
        }
        assert_eq!(payload_bytes, MAX_RESERVED_REQUEST_PAYLOAD_BYTES);
        let error = enqueue_spider_request(
            SpiderRequest::new("/upload-extra")
                .method("POST")
                .body(vec![42; 1]),
            "https://example.org/start",
            0,
            &spec,
            &rules,
            Frontier {
                visited: &visited,
                queued: &mut queued,
                queue: &mut queue,
                reserved_payload_bytes: &mut payload_bytes,
                nonzero_priority_count: &mut nonzero_priority_count,
            },
        )
        .unwrap_err();
        assert!(error.contains("32 MiB"));
        assert_eq!(queue.len(), 4);
        assert_eq!(queued.len(), 4);
        assert_eq!(payload_bytes, MAX_RESERVED_REQUEST_PAYLOAD_BYTES);
    }

    struct InflightBudgetParser;

    impl SpiderParser for InflightBudgetParser {
        fn parse<'a>(&'a self, page: &'a CrawlItem) -> ParseFuture<'a> {
            Box::pin(async move {
                let count = match page.depth {
                    0 => 4,
                    1 => 2,
                    _ => 0,
                };
                Ok((0..count)
                    .map(|index| {
                        SpiderOutput::Request(
                            SpiderRequest::new(format!(
                                "/depth-{}/child-{index}-{}",
                                page.depth,
                                page.url.len()
                            ))
                            .method("POST")
                            .body(vec![index as u8; 8 * 1024 * 1024]),
                        )
                    })
                    .collect())
            })
        }
    }

    #[tokio::test]
    async fn completed_request_releases_capacity_but_inflight_requests_stay_reserved() {
        let spec = CrawlSpec {
            start_urls: vec!["https://example.org/start".into()],
            max_depth: 2,
            max_pages: 10,
            concurrency: 2,
            ..CrawlSpec::default()
        };
        let error = crawl_with_parser(
            &spec,
            Arc::new(RichRecorder(Mutex::new(Vec::new()))),
            &InflightBudgetParser,
            |_| Ok(()),
        )
        .await
        .unwrap_err();
        assert!(error.contains("32 MiB"));
    }

    #[tokio::test]
    async fn checkpoint_restores_rich_requests_without_changing_legacy_get_shape() {
        let dir = tempfile::tempdir().unwrap();
        let spec = CrawlSpec {
            start_urls: vec!["https://example.org/seed".into()],
            crawldir: Some(dir.path().to_path_buf()),
            persist_sensitive_requests: true,
            max_pages: 3,
            concurrency: 1,
            ..CrawlSpec::default()
        };
        let rich = SpiderRequest::new("https://example.org/seed")
            .method("POST")
            .header("x-test", "secret")
            .body(b"persisted".to_vec())
            .priority(9);
        persist(
            &spec,
            &Progress {
                queue: vec![
                    StoredRequest::Legacy(("https://example.org/seed".into(), 0)),
                    StoredRequest::Rich {
                        request: rich.clone(),
                        depth: 1,
                    },
                ],
                ..Progress::default()
            },
        )
        .unwrap();
        let bytes = std::fs::read(state_path(&spec).unwrap()).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            json["queue"][0],
            serde_json::json!(["https://example.org/seed", 0])
        );
        assert_eq!(
            json["queue"][1]["request"]["body"],
            serde_json::json!([112, 101, 114, 115, 105, 115, 116, 101, 100])
        );
        let fetcher = Arc::new(RichRecorder(Mutex::new(Vec::new())));
        let result = crawl(&spec, fetcher.clone(), |_| Ok(())).await.unwrap();
        assert_eq!(result.visited, 2);
        {
            let calls = fetcher.0.lock().unwrap();
            assert_eq!(
                &calls[..],
                &[rich, SpiderRequest::new("https://example.org/seed")]
            );
        }
        let resumed = restore(&spec).unwrap().unwrap();
        assert!(resumed.queue.is_empty());
        assert_eq!(resumed.visited_hashes.len(), 2);
        let repeated = crawl(&spec, Arc::new(Mock), |_| panic!("unexpected refetch"))
            .await
            .unwrap();
        assert_eq!(repeated.visited, 2);
    }

    #[tokio::test]
    async fn auto_fetcher_sends_method_headers_and_body_over_real_local_http() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/submit", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let size = stream.read(&mut chunk).unwrap();
                if size == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..size]);
                if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&bytes[..end]);
                    let length = header
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nok")
                .unwrap();
            String::from_utf8(bytes).unwrap()
        });
        let http = rustscrapling_http::HttpClient::new(rustscrapling_http::HttpOptions {
            retries: 0,
            direct: true,
            ..rustscrapling_http::HttpOptions::default()
        })
        .unwrap();
        let fetcher = AutoFetcher(rustscrapling_fetch::Fetcher::new(
            http,
            rustscrapling_fetch::FetchOptions::default(),
        ));
        let request = SpiderRequest::new(url)
            .method("POST")
            .header("X-Spider-Test", "present")
            .body(b"real-wire-body".to_vec());
        let page = fetcher.fetch_request(&request).await.unwrap();
        assert_eq!(page.status, 200);
        assert_eq!(page.body, "ok");
        let wire = server.join().unwrap();
        assert!(wire.starts_with("POST /submit HTTP/1.1"), "{wire}");
        assert!(wire.to_ascii_lowercase().contains("x-spider-test: present"));
        assert!(wire.ends_with("real-wire-body"));
    }
    use serde_json::json;

    struct HookRecorder(Mutex<Vec<SpiderRequest>>);

    impl Fetcher for HookRecorder {
        fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
            Box::pin(async move { panic!("unexpected URL-only fetch {url}") })
        }

        fn fetch_request<'a>(&'a self, request: &'a SpiderRequest) -> RequestFetchFuture<'a> {
            Box::pin(async move {
                self.0.lock().unwrap().push(request.clone());
                if request.url.ends_with("/fail") {
                    return Err(SpiderFetchError::Backend("connection refused".into()));
                }
                Ok(FetchPage {
                    headers: Default::default(),
                    status: 200,
                    url: request.url.clone(),
                    content_type: "text/plain".into(),
                    body: "ok".into(),
                })
            })
        }
    }

    struct HookParser(Mutex<Vec<String>>);

    impl SpiderParser for HookParser {
        fn parse<'a>(&'a self, page: &'a CrawlItem) -> ParseFuture<'a> {
            Box::pin(async move {
                self.0.lock().unwrap().push(format!(
                    "parse:{}:{}",
                    page.callback.as_deref().unwrap_or("parse"),
                    page.meta.get("n").map_or(String::new(), |n| n.to_string())
                ));
                match page.callback.as_deref() {
                    None => Ok(vec![
                        SpiderOutput::Request(
                            SpiderRequest::new("/detail")
                                .callback("detail")
                                .meta("n", json!(7))
                                .cookie("sid", "abc"),
                        ),
                        // Same URL again: filtered unless dont_filter.
                        SpiderOutput::Request(SpiderRequest::new("/detail").callback("detail")),
                        SpiderOutput::Request(
                            SpiderRequest::new("/start")
                                .dont_filter(true)
                                .callback("again"),
                        ),
                        SpiderOutput::Request(SpiderRequest::new("/fail").callback("never")),
                        SpiderOutput::Item(json!({"keep": true})),
                        SpiderOutput::Item(json!({"drop": true})),
                    ]),
                    Some("detail") => Err("detail parser failed".into()),
                    _ => Ok(Vec::new()),
                }
            })
        }

        fn on_error<'a>(&'a self, page: &'a CrawlItem) -> ParseFuture<'a> {
            Box::pin(async move {
                self.0.lock().unwrap().push(format!(
                    "error:{}:{}",
                    page.callback.as_deref().unwrap_or("parse"),
                    page.error.as_deref().unwrap_or_default()
                ));
                Ok(vec![SpiderOutput::Item(json!({"recovered": page.url}))])
            })
        }

        fn on_scraped_item(&self, item: serde_json::Value) -> Option<serde_json::Value> {
            (item.get("drop").is_none()).then_some(item)
        }
    }

    #[tokio::test]
    async fn callbacks_meta_dont_filter_cookies_and_error_hooks() {
        let fetcher = Arc::new(HookRecorder(Mutex::new(Vec::new())));
        let parser = HookParser(Mutex::new(Vec::new()));
        let spec = CrawlSpec {
            start_urls: vec!["https://example.org/start".into()],
            max_pages: 10,
            max_depth: 2,
            concurrency: 1,
            ..CrawlSpec::default()
        };
        let mut items = Vec::new();
        let summary = crawl_with_parser(&spec, fetcher.clone(), &parser, |event| {
            if let SpiderEvent::Item(value) = event {
                items.push(value.clone());
            }
            Ok(())
        })
        .await
        .unwrap();
        let log = parser.0.lock().unwrap().clone();
        assert!(log.contains(&"parse:parse:".to_owned()), "{log:?}");
        assert!(log.contains(&"parse:detail:7".to_owned()), "{log:?}");
        assert!(
            log.contains(&"parse:again:".to_owned()),
            "dont_filter refetch: {log:?}"
        );
        assert!(
            log.contains(&"error:detail:detail parser failed".to_owned()),
            "{log:?}"
        );
        assert!(
            log.iter()
                .any(|entry| entry.starts_with("error:never:")
                    && entry.contains("connection refused")),
            "{log:?}"
        );
        assert_eq!(summary.parse_errors, 1);
        assert!(items.contains(&json!({"keep": true})));
        assert!(
            !items.contains(&json!({"drop": true})),
            "on_scraped_item drops items"
        );
        assert!(items.iter().any(|item| item.get("recovered").is_some()));
        let requests = fetcher.0.lock().unwrap().clone();
        let detail: Vec<_> = requests
            .iter()
            .filter(|r| r.url.ends_with("/detail"))
            .collect();
        assert_eq!(detail.len(), 1, "duplicate /detail is filtered");
        assert_eq!(
            detail[0].headers.get("Cookie").map(String::as_str),
            Some("sid=abc")
        );
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.url.ends_with("/start"))
                .count(),
            2
        );
    }
}
