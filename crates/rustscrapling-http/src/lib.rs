//! HTTP acquisition for RustScrapling. TLS verification remains enabled for every request.
//!
//! `HttpClient` retains connection pools across requests. `HttpOptions` configures the
//! client's network identity and policies; `HttpRequest` carries each request's data.

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use url::Url;
use wreq::header::{HeaderMap, HeaderName, HeaderValue};
use wreq::redirect::Policy;
use wreq::{Client, Method, Proxy};
use wreq_util::{Emulation, Profile};

mod quic;

pub const DEFAULT_BODY_LIMIT: usize = 32 * 1024 * 1024;

/// Explicit protocol policy. `Http3Preferred` may use a verified HTTP/2 or HTTP/1.1
/// connection when the separate QUIC transport is not available.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    #[default]
    Auto,
    Http1,
    Http2,
    Http3Preferred,
    /// Require a verified QUIC connection. Never silently use TCP fallback.
    /// Useful for protocol tests and callers that demand HTTP/3.
    Http3Only,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    InvalidRequest,
    Timeout,
    Dns,
    Tls,
    Proxy,
    HttpStatus,
    Blocked,
    BodyTooLarge,
    UnsupportedProtocol,
    Transport,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::Timeout => "timeout",
            Self::Dns => "dns",
            Self::Tls => "tls",
            Self::Proxy => "proxy",
            Self::HttpStatus => "http_status",
            Self::Blocked => "blocked",
            Self::BodyTooLarge => "body_too_large",
            Self::UnsupportedProtocol => "unsupported_protocol",
            Self::Transport => "transport",
        }
    }
}

#[derive(Debug)]
pub struct HttpError {
    pub kind: ErrorKind,
    pub message: String,
    pub status: Option<u16>,
}

impl HttpError {
    fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            status: None,
        }
    }

    fn from_wreq(error: wreq::Error) -> Self {
        let description = error.to_string();
        let lower = description.to_ascii_lowercase();
        let kind = if error.is_timeout() {
            ErrorKind::Timeout
        } else if error.is_proxy_connect() || lower.contains("proxy") {
            ErrorKind::Proxy
        } else if lower.contains("certificate") || lower.contains("tls") || lower.contains("ssl") {
            ErrorKind::Tls
        } else if lower.contains("dns") || lower.contains("lookup") || lower.contains("resolve") {
            ErrorKind::Dns
        } else {
            ErrorKind::Transport
        };
        // Avoid accidentally exposing credentials in proxy/request URLs from library errors.
        Self::new(
            kind,
            format!("HTTP transport failed ({})", error_class_name(kind)),
        )
    }

    /// Recovery guidance for machine-readable `error.hint` output.
    pub fn hint(&self) -> &'static str {
        match self.kind {
            ErrorKind::InvalidRequest => "Fix the URL, method, headers, or request options",
            ErrorKind::Timeout => "Increase timeout or retry later",
            ErrorKind::Dns => "Check DNS resolution and proxy settings",
            ErrorKind::Tls => "Check the server certificate and trusted CA bundle",
            ErrorKind::Proxy => "Verify proxy URL, credentials, and connectivity",
            ErrorKind::HttpStatus => "Inspect the HTTP status and response body",
            ErrorKind::Blocked => "Retry with a different browser profile or --tier browser",
            ErrorKind::BodyTooLarge => "Increase max_body_bytes or select a smaller resource",
            ErrorKind::UnsupportedProtocol => "Try HTTP/2 or HTTP/1.1",
            ErrorKind::Transport => "Check connectivity or retry with another proxy",
        }
    }
}

fn error_class_name(kind: ErrorKind) -> &'static str {
    kind.as_str()
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind.as_str(), self.message)
    }
}

impl std::error::Error for HttpError {}

/// Deterministic round-robin proxy selection shared across clones of a client.
#[derive(Clone)]
pub struct ProxyRotator {
    urls: Arc<Vec<String>>,
    cursor: Arc<AtomicUsize>,
}

impl ProxyRotator {
    pub fn new(urls: Vec<String>) -> Result<Self, HttpError> {
        if urls.is_empty() {
            return Err(HttpError::new(
                ErrorKind::InvalidRequest,
                "proxy list is empty",
            ));
        }
        for url in &urls {
            validate_proxy_url(url)?;
        }
        Ok(Self {
            urls: Arc::new(urls),
            cursor: Arc::new(AtomicUsize::new(0)),
        })
    }

    pub fn next_index(&self) -> usize {
        self.cursor.fetch_add(1, Ordering::Relaxed) % self.urls.len()
    }

    pub fn len(&self) -> usize {
        self.urls.len()
    }
    pub fn is_empty(&self) -> bool {
        self.urls.is_empty()
    }
    fn urls(&self) -> &[String] {
        &self.urls
    }
}

#[derive(Clone)]
pub struct HttpOptions {
    pub impersonate: Option<String>,
    pub stealthy_headers: bool,
    pub proxy: Option<String>,
    /// Scheme-specific proxies; keys `http` and `https` only.
    pub proxies: BTreeMap<String, String>,
    pub proxy_rotator: Option<ProxyRotator>,
    pub retries: u32,
    pub timeout: Duration,
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
    /// Maximum time spent attempting UDP/QUIC before falling back to wreq.
    pub http3_probe_timeout: Duration,
    pub follow_redirects: bool,
    pub protocol: Protocol,
    pub max_body_bytes: usize,
    pub retry_non_idempotent: bool,
    /// PEM CA bundle: system CA trust is used when unset (also respects SSL_CERT_FILE).
    pub ca_bundle: Option<PathBuf>,
    /// Disable all proxies, including those from the environment.
    pub direct: bool,
}

impl Default for HttpOptions {
    fn default() -> Self {
        Self {
            impersonate: Some("chrome_147".into()),
            stealthy_headers: true,
            proxy: None,
            proxies: BTreeMap::new(),
            proxy_rotator: None,
            retries: 2,
            timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(10),
            read_timeout: Duration::from_secs(20),
            http3_probe_timeout: Duration::from_secs(2),
            follow_redirects: true,
            protocol: Protocol::Auto,
            max_body_bytes: DEFAULT_BODY_LIMIT,
            retry_non_idempotent: false,
            ca_bundle: None,
            direct: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Option<Vec<u8>>,
}

impl HttpRequest {
    pub fn get(url: impl Into<String>) -> Self {
        Self::new("GET", url)
    }
    pub fn new(method: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            url: url.into(),
            headers: BTreeMap::new(),
            body: None,
        }
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }

    pub fn body(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.body = Some(bytes.into());
        self
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HttpResponse {
    pub status: u16,
    pub final_url: String,
    /// Header pairs retain duplicate names (including Set-Cookie).
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Actual negotiated protocol, e.g. `HTTP/1.1` or `HTTP/2.0`.
    pub http_version: String,
    pub attempts: u32,
    /// True only when a genuine HTTP/3 connection was attempted.
    pub http3_attempted: bool,
}

impl HttpResponse {
    pub fn status_error_kind(&self) -> Option<ErrorKind> {
        match self.status {
            403 | 429 => Some(ErrorKind::Blocked),
            400..=599 => Some(ErrorKind::HttpStatus),
            _ => None,
        }
    }

    pub fn require_success(&self) -> Result<(), HttpError> {
        if let Some(kind) = self.status_error_kind() {
            Err(HttpError {
                kind,
                status: Some(self.status),
                message: format!("HTTP {}", self.status),
            })
        } else {
            Ok(())
        }
    }
}

/// Reuses pooled HTTP/1.1 and HTTP/2 connections. Proxy rotation selects one pool
/// for each attempt, including retries; HTTP/3 is never falsely reported as negotiated.
#[derive(Clone)]
pub struct HttpClient {
    options: HttpOptions,
    clients: Arc<Vec<Client>>,
}

impl HttpClient {
    pub fn new(options: HttpOptions) -> Result<Self, HttpError> {
        if options.timeout.is_zero()
            || options.connect_timeout.is_zero()
            || options.read_timeout.is_zero()
            || options.http3_probe_timeout.is_zero()
        {
            return Err(HttpError::new(
                ErrorKind::InvalidRequest,
                "timeouts must be positive",
            ));
        }
        if options.max_body_bytes == 0 {
            return Err(HttpError::new(
                ErrorKind::InvalidRequest,
                "body limit must be positive",
            ));
        }
        if options.direct
            && (options.proxy.is_some()
                || !options.proxies.is_empty()
                || options.proxy_rotator.is_some())
        {
            return Err(HttpError::new(
                ErrorKind::InvalidRequest,
                "direct and explicit proxy settings conflict",
            ));
        }
        if options.proxy.is_some()
            && (!options.proxies.is_empty() || options.proxy_rotator.is_some())
            || !options.proxies.is_empty() && options.proxy_rotator.is_some()
        {
            return Err(HttpError::new(
                ErrorKind::InvalidRequest,
                "choose one explicit proxy mechanism",
            ));
        }
        for key in options.proxies.keys() {
            if key != "http" && key != "https" {
                return Err(HttpError::new(
                    ErrorKind::InvalidRequest,
                    "proxy map keys must be http or https",
                ));
            }
        }

        let proxy_urls = options.proxy_rotator.as_ref().map(|r| r.urls().to_vec());
        let urls: Vec<Option<&str>> = match &proxy_urls {
            Some(urls) => urls.iter().map(|s| Some(s.as_str())).collect(),
            None => vec![options.proxy.as_deref()],
        };
        let mut clients = Vec::with_capacity(urls.len());
        let env_ca_bundle = std::env::var_os("SSL_CERT_FILE").map(PathBuf::from);
        for url in urls {
            let mut builder = Client::builder()
                .timeout(options.timeout)
                .connect_timeout(options.connect_timeout)
                .read_timeout(options.read_timeout)
                .redirect(if options.follow_redirects {
                    Policy::limited(10)
                } else {
                    Policy::none()
                });

            if let Some(profile) = &options.impersonate {
                builder = builder.emulation(emulation(profile, options.stealthy_headers)?);
            }
            if let Some(bundle) = options.ca_bundle.as_ref().or(env_ca_bundle.as_ref()) {
                let pem = std::fs::read(bundle)
                    .map_err(|_| HttpError::new(ErrorKind::Tls, "could not read CA bundle"))?;
                let store = wreq::tls::trust::CertStore::builder()
                    .set_default_paths()
                    .add_stack_pem_certs(&pem)
                    .build()
                    .map_err(|_| HttpError::new(ErrorKind::Tls, "invalid CA bundle"))?;
                builder = builder.tls_cert_store(store);
            }
            if options.direct {
                builder = builder.no_proxy();
            } else if let Some(url) = url {
                builder = builder.proxy(proxy_from_url(url, ProxyScope::All)?);
            } else if !options.proxies.is_empty() {
                for (scheme, url) in &options.proxies {
                    let scope = if scheme == "http" {
                        ProxyScope::Http
                    } else {
                        ProxyScope::Https
                    };
                    builder = builder.proxy(proxy_from_url(url, scope)?);
                }
            }
            if options.protocol == Protocol::Http1 {
                builder = builder.http1_only();
            }
            if options.protocol == Protocol::Http2 {
                builder = builder.http2_only();
            }
            clients.push(builder.build().map_err(HttpError::from_wreq)?);
        }
        Ok(Self {
            options,
            clients: Arc::new(clients),
        })
    }

    pub async fn get(&self, url: impl Into<String>) -> Result<HttpResponse, HttpError> {
        self.request(HttpRequest::get(url)).await
    }

    pub async fn request(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        let url = Url::parse(&request.url)
            .map_err(|_| HttpError::new(ErrorKind::InvalidRequest, "invalid URL"))?;
        if url.scheme() != "http" && url.scheme() != "https" || url.host().is_none() {
            return Err(HttpError::new(
                ErrorKind::InvalidRequest,
                "only HTTP and HTTPS URLs are accepted",
            ));
        }
        let method = Method::from_bytes(request.method.as_bytes())
            .map_err(|_| HttpError::new(ErrorKind::InvalidRequest, "invalid HTTP method"))?;
        let mut headers = HeaderMap::new();
        for (name, value) in &request.headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| HttpError::new(ErrorKind::InvalidRequest, "invalid header name"))?;
            let value = HeaderValue::from_str(value)
                .map_err(|_| HttpError::new(ErrorKind::InvalidRequest, "invalid header value"))?;
            headers.insert(name, value);
        }

        let retry_allowed = self.options.retry_non_idempotent
            || matches!(
                method,
                Method::GET
                    | Method::HEAD
                    | Method::OPTIONS
                    | Method::TRACE
                    | Method::PUT
                    | Method::DELETE
            );
        let max_attempts = if retry_allowed {
            self.options.retries.saturating_add(1)
        } else {
            1
        };
        // A genuine QUIC handshake runs only if no proxy must be honored. For
        // HTTP/3 failure, wreq negotiates HTTP/2 or HTTP/1.1 and the actual
        // negotiated version is always reflected in the response.
        let mut http3_attempted = false;
        if self.options.protocol == Protocol::Http3Only && !quic::can_attempt(&self.options, &url) {
            return Err(HttpError::new(
                ErrorKind::UnsupportedProtocol,
                "HTTP/3 only requires HTTPS and a direct, non-proxied connection",
            ));
        }
        if matches!(
            self.options.protocol,
            Protocol::Http3Preferred | Protocol::Http3Only
        ) && quic::can_attempt(&self.options, &url)
        {
            http3_attempted = true;
            let timeout = self.options.http3_probe_timeout.min(self.options.timeout);
            let result = tokio::time::timeout(
                timeout,
                quic::fetch(
                    &self.options,
                    &method,
                    &url,
                    &headers,
                    request.body.as_deref(),
                ),
            )
            .await;
            match result {
                Ok(Ok(response)) => {
                    if self.options.protocol == Protocol::Http3Only
                        || !self.options.follow_redirects
                        || !(300..=399).contains(&response.status)
                    {
                        if self.options.protocol == Protocol::Http3Only
                            || !retry_allowed
                            || !matches!(response.status, 408 | 429 | 500 | 502 | 503 | 504)
                        {
                            return Ok(response);
                        }
                    }
                }
                Ok(Err(error)) if self.options.protocol == Protocol::Http3Only => {
                    return Err(error);
                }
                Err(_) if self.options.protocol == Protocol::Http3Only => {
                    return Err(HttpError::new(ErrorKind::Timeout, "HTTP/3 probe timed out"));
                }
                _ => {}
            }
        }
        let mut last_error = None;
        for attempt in 1..=max_attempts {
            let index = self
                .options
                .proxy_rotator
                .as_ref()
                .map_or(0, ProxyRotator::next_index);
            let mut builder = self.clients[index]
                .request(method.clone(), url.as_str())
                .headers(headers.clone());
            if let Some(ref body) = request.body {
                builder = builder.body(body.clone());
            }

            match builder.send().await {
                Ok(response) => {
                    let status = response.status().as_u16();
                    if attempt < max_attempts && matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
                    {
                        tokio::time::sleep(retry_delay(attempt)).await;
                        continue;
                    }
                    let final_url = response.uri().to_string();
                    let http_version = format!("{:?}", response.version());
                    let content_length = response.content_length();
                    let mut response_headers = Vec::new();
                    for (name, value) in response.headers().iter() {
                        response_headers.push((
                            name.as_str().to_owned(),
                            String::from_utf8_lossy(value.as_bytes()).into_owned(),
                        ));
                    }
                    response_headers.sort();
                    if content_length.is_some_and(|len| len > self.options.max_body_bytes as u64) {
                        return Err(HttpError::new(
                            ErrorKind::BodyTooLarge,
                            "response Content-Length exceeds configured limit",
                        ));
                    }
                    let mut body = Vec::new();
                    let mut stream = response.bytes_stream();
                    while let Some(chunk) = stream.next().await {
                        let chunk = chunk.map_err(HttpError::from_wreq)?;
                        if chunk.len() > self.options.max_body_bytes.saturating_sub(body.len()) {
                            return Err(HttpError::new(
                                ErrorKind::BodyTooLarge,
                                "response exceeded configured body limit",
                            ));
                        }
                        body.extend_from_slice(&chunk);
                    }
                    return Ok(HttpResponse {
                        status,
                        final_url,
                        headers: response_headers,
                        body,
                        http_version,
                        attempts: attempt,
                        http3_attempted,
                    });
                }
                Err(err) => {
                    let error = HttpError::from_wreq(err);
                    let retry = matches!(
                        error.kind,
                        ErrorKind::Timeout
                            | ErrorKind::Dns
                            | ErrorKind::Proxy
                            | ErrorKind::Transport
                    );
                    if attempt == max_attempts || !retry {
                        return Err(error);
                    }
                    last_error = Some(error);
                    tokio::time::sleep(retry_delay(attempt)).await;
                }
            }
        }
        Err(last_error
            .unwrap_or_else(|| HttpError::new(ErrorKind::Transport, "no HTTP attempts ran")))
    }
}

fn retry_delay(attempt: u32) -> Duration {
    Duration::from_millis(50u64.saturating_mul(1u64 << attempt.min(5)))
}

fn emulation(profile: &str, headers: bool) -> Result<Emulation, HttpError> {
    let normalized = profile.trim().to_ascii_lowercase().replace(['_', '-'], "");
    let normalized = match normalized.as_str() {
        "chrome" => "chrome149",
        "firefox" => "firefox151",
        "safari" => "safari26",
        "edge" => "edge148",
        "opera" => "opera130",
        other => other,
    };
    let selected = Profile::VARIANTS
        .iter()
        .find(|variant| format!("{variant:?}").to_ascii_lowercase() == normalized);
    match selected {
        Some(profile) => Ok(Emulation::builder()
            .profile(*profile)
            .headers(headers)
            .build()),
        None => Err(HttpError::new(
            ErrorKind::InvalidRequest,
            format!("unsupported impersonate profile: {normalized}"),
        )),
    }
}

#[derive(Clone, Copy)]
enum ProxyScope {
    All,
    Http,
    Https,
}

fn validate_proxy_url(raw: &str) -> Result<(), HttpError> {
    let parsed = Url::parse(raw)
        .map_err(|_| HttpError::new(ErrorKind::InvalidRequest, "invalid proxy URL"))?;
    if !matches!(
        parsed.scheme(),
        "http" | "https" | "socks4" | "socks4a" | "socks5" | "socks5h"
    ) || parsed.host().is_none()
    {
        return Err(HttpError::new(
            ErrorKind::InvalidRequest,
            "unsupported proxy URL",
        ));
    }
    Ok(())
}

fn proxy_from_url(raw: &str, scope: ProxyScope) -> Result<Proxy, HttpError> {
    validate_proxy_url(raw)?;
    let mut parsed = Url::parse(raw)
        .map_err(|_| HttpError::new(ErrorKind::InvalidRequest, "invalid proxy URL"))?;
    let username = parsed.username().to_owned();
    let password = parsed.password().unwrap_or("").to_owned();
    if !username.is_empty() || parsed.password().is_some() {
        parsed
            .set_username("")
            .map_err(|_| HttpError::new(ErrorKind::InvalidRequest, "invalid proxy credentials"))?;
        parsed
            .set_password(None)
            .map_err(|_| HttpError::new(ErrorKind::InvalidRequest, "invalid proxy credentials"))?;
    }
    let proxy = match scope {
        ProxyScope::All => Proxy::all(parsed.as_str()),
        ProxyScope::Http => Proxy::http(parsed.as_str()),
        ProxyScope::Https => Proxy::https(parsed.as_str()),
    }
    .map_err(|_| HttpError::new(ErrorKind::InvalidRequest, "could not configure proxy"))?;
    let proxy = if !username.is_empty() {
        proxy.basic_auth(&username, &password)
    } else {
        proxy
    };
    Ok(proxy.no_proxy(wreq::NoProxy::from_env()))
}
