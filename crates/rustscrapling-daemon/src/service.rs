use crate::protocol::{
    DaemonError, DaemonRequest, DaemonResponse, FetchRequest, FetchResult, FetchTier, Status,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, Mutex, OnceCell};
use tokio::time::{sleep_until, timeout, Instant};
use url::Url;

const MAX_FRAME: usize = 16 * 1024 * 1024;
const MAX_CACHE_BODY: usize = 1024 * 1024;
const MAX_CACHE_ENTRIES: usize = 32;

pub type FetchFuture<'a> =
    Pin<Box<dyn Future<Output = Result<FetchResult, DaemonError>> + Send + 'a>>;

/// Backend for the daemon's shared scheduler. `FetcherBackend` adapts the
/// real Fetcher when the `fetch-integration` feature is enabled.
pub trait FetchBackend: Send + Sync + 'static {
    fn fetch<'a>(&'a self, request: FetchRequest) -> FetchFuture<'a>;
}

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub state_file: PathBuf,
    pub site_interval: Duration,
    pub learning_ttl: Duration,
    pub request_timeout: Duration,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        let dir = if cfg!(windows) {
            std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
        } else {
            std::env::var_os("HOME").map(PathBuf::from)
        }
        .unwrap_or_else(std::env::temp_dir);
        Self {
            state_file: dir.join("rustscrapling").join("daemon.json"),
            site_interval: Duration::from_millis(200),
            learning_ttl: Duration::from_secs(86400),
            request_timeout: Duration::from_secs(60),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StateFile {
    version: u8,
    address: String,
    token: String,
    pid: u32,
}

#[derive(Clone)]
struct Cached {
    response: FetchResult,
    created: Instant,
}

struct Learned {
    tier: FetchTier,
    expires: Instant,
}

type Inflight = OnceCell<Result<FetchResult, DaemonError>>;

struct Shared {
    backend: Arc<dyn FetchBackend>,
    config: DaemonConfig,
    inflight: Mutex<HashMap<String, Arc<Inflight>>>,
    hosts: Mutex<HashMap<String, Arc<Mutex<Instant>>>>,
    cache: Mutex<HashMap<String, Cached>>,
    learning: Mutex<HashMap<String, Learned>>,
    requests: AtomicU64,
    upstream: AtomicU64,
    coalesced: AtomicU64,
}

impl Shared {
    fn new(backend: Arc<dyn FetchBackend>, config: DaemonConfig) -> Self {
        Self {
            backend,
            config,
            inflight: Mutex::new(HashMap::new()),
            hosts: Mutex::new(HashMap::new()),
            cache: Mutex::new(HashMap::new()),
            learning: Mutex::new(HashMap::new()),
            requests: AtomicU64::new(0),
            upstream: AtomicU64::new(0),
            coalesced: AtomicU64::new(0),
        }
    }

    async fn fetch(&self, request: FetchRequest) -> Result<FetchResult, DaemonError> {
        let url = Url::parse(&request.url)
            .map_err(|e| DaemonError::new("invalid_request", e.to_string()))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(DaemonError::new(
                "invalid_request",
                "only absolute HTTP(S) URLs are allowed",
            ));
        }
        if !matches!(
            request.method.as_str(),
            "GET" | "HEAD" | "POST" | "PUT" | "DELETE"
        ) {
            return Err(DaemonError::new(
                "invalid_request",
                "unsupported HTTP method",
            ));
        }
        let key = serde_json::to_string(&request)
            .map_err(|e| DaemonError::new("parse", e.to_string()))?;
        self.requests.fetch_add(1, Ordering::Relaxed);
        // Never merge mutating requests. Repeating an identical POST may be
        // intentional and must reach the upstream for each invocation.
        if request.method != "GET" && request.method != "HEAD" {
            return self.fetch_once(request, &key, &url).await;
        }
        let cell = {
            let mut flights = self.inflight.lock().await;
            if let Some(cell) = flights.get(&key) {
                self.coalesced.fetch_add(1, Ordering::Relaxed);
                cell.clone()
            } else {
                let cell = Arc::new(Inflight::new());
                flights.insert(key.clone(), cell.clone());
                cell
            }
        };
        let answer = cell
            .get_or_init(|| async { self.fetch_once(request, &key, &url).await })
            .await
            .clone();
        let mut flights = self.inflight.lock().await;
        if flights
            .get(&key)
            .is_some_and(|current| Arc::ptr_eq(current, &cell))
        {
            flights.remove(&key);
        }
        answer
    }

    async fn fetch_once(
        &self,
        mut request: FetchRequest,
        key: &str,
        url: &Url,
    ) -> Result<FetchResult, DaemonError> {
        let host = url
            .host_str()
            .expect("validated in fetch")
            .to_ascii_lowercase();
        let original_tier = request.tier;
        if original_tier == FetchTier::Auto {
            let mut learned = self.learning.lock().await;
            learned.retain(|_, record| record.expires > Instant::now());
            if let Some(tier) = learned.get(&host).map(|record| record.tier) {
                request.tier = tier;
            }
        }
        let cached = if request.method == "GET" {
            self.cache
                .lock()
                .await
                .get(key)
                .filter(|e| e.created.elapsed() < self.config.learning_ttl)
                .cloned()
        } else {
            None
        };
        if let Some(cache) = &cached {
            if !request
                .headers
                .keys()
                .any(|name| name.eq_ignore_ascii_case("if-none-match"))
            {
                if let Some(etag) = header(&cache.response.headers, "etag") {
                    request
                        .headers
                        .insert("If-None-Match".into(), etag.to_owned());
                }
            }
            if !request
                .headers
                .keys()
                .any(|name| name.eq_ignore_ascii_case("if-modified-since"))
            {
                if let Some(modified) = header(&cache.response.headers, "last-modified") {
                    request
                        .headers
                        .insert("If-Modified-Since".into(), modified.to_owned());
                }
            }
        }
        let limiter = {
            let mut map = self.hosts.lock().await;
            map.entry(host.clone())
                .or_insert_with(|| Arc::new(Mutex::new(Instant::now())))
                .clone()
        };
        {
            let mut next = limiter.lock().await;
            sleep_until(*next).await;
            *next = Instant::now() + self.config.site_interval;
        }
        self.upstream.fetch_add(1, Ordering::Relaxed);
        let mut result = timeout(
            self.config.request_timeout,
            self.backend.fetch(request.clone()),
        )
        .await
        .map_err(|_| DaemonError::new("timeout", "upstream fetch exceeded deadline"))?;
        // A learned browser route might have gone stale; let the policy retry
        // from HTTP on an explicit backend error.
        if result.is_err() && request.tier != original_tier && original_tier == FetchTier::Auto {
            request.tier = FetchTier::Auto;
            tokio::time::sleep(self.config.site_interval).await;
            self.upstream.fetch_add(1, Ordering::Relaxed);
            result = timeout(
                self.config.request_timeout,
                self.backend.fetch(request.clone()),
            )
            .await
            .map_err(|_| DaemonError::new("timeout", "fallback fetch exceeded deadline"))?;
        }
        let result = result?;
        if result.status == 304 {
            if let Some(cache) = cached {
                // Keep fresh validators if the server provided replacements.
                let mut restored = cache.response;
                for (name, value) in result.headers {
                    if let Some(entry) = restored
                        .headers
                        .iter_mut()
                        .find(|(key, _)| key.eq_ignore_ascii_case(&name))
                    {
                        *entry = (name, value);
                    } else {
                        restored.headers.push((name, value));
                    }
                }
                self.store_cache(key, restored.clone()).await;
                return Ok(restored);
            }
            // 304 without matching cached body cannot be represented as 200.
            return Err(DaemonError::new(
                "cache_miss",
                "received 304 without a cached response",
            ));
        }
        if (200..300).contains(&result.status) && !challenge(&result.body) {
            self.learning.lock().await.insert(
                host,
                Learned {
                    tier: result.tier,
                    expires: Instant::now() + self.config.learning_ttl,
                },
            );
            if request.method == "GET"
                && (header(&result.headers, "etag").is_some()
                    || header(&result.headers, "last-modified").is_some())
                && !header(&result.headers, "cache-control")
                    .is_some_and(|v| v.to_ascii_lowercase().contains("no-store"))
                && header(&result.headers, "vary") != Some("*")
            {
                self.store_cache(key, result.clone()).await;
            }
        }
        Ok(result)
    }

    async fn store_cache(&self, key: &str, response: FetchResult) {
        if response.body.len() > MAX_CACHE_BODY {
            return;
        }
        let mut cache = self.cache.lock().await;
        if cache.len() >= MAX_CACHE_ENTRIES && !cache.contains_key(key) {
            if let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, value)| value.created)
                .map(|(k, _)| k.clone())
            {
                cache.remove(&oldest);
            }
        }
        cache.insert(
            key.into(),
            Cached {
                response,
                created: Instant::now(),
            },
        );
    }

    async fn status(&self, address: &str) -> Status {
        Status {
            pid: std::process::id(),
            address: address.into(),
            requests: self.requests.load(Ordering::Relaxed),
            upstream_requests: self.upstream.load(Ordering::Relaxed),
            coalesced_requests: self.coalesced.load(Ordering::Relaxed),
            cache_entries: self.cache.lock().await.len(),
            learned_sites: self
                .learning
                .lock()
                .await
                .iter()
                .filter(|(_, r)| r.expires > Instant::now())
                .count(),
        }
    }
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn challenge(body: &[u8]) -> bool {
    let text = String::from_utf8_lossy(&body[..body.len().min(16 * 1024)]).to_ascii_lowercase();
    text.contains("cf-chl-")
        || text.contains("/cdn-cgi/challenge-platform/")
        || (text.contains("<title>just a moment") && text.contains("cloudflare"))
}

#[derive(Serialize, Deserialize)]
struct WireRequest {
    token: String,
    request: DaemonRequest,
}

pub struct DaemonServer {
    state_file: PathBuf,
    address: String,
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl DaemonServer {
    /// Start an authenticated background TCP service bound only to loopback.
    /// The state file is create-new (0600 on Unix); an existing state file
    /// is never overwritten, to avoid hijacking an already running daemon.
    pub async fn start(
        config: DaemonConfig,
        backend: Arc<dyn FetchBackend>,
    ) -> Result<Self, DaemonError> {
        let listener = TcpListener::bind("127.0.0.1:0").await.map_err(io_err)?;
        let address = listener.local_addr().map_err(io_err)?.to_string();
        let mut bytes = [0_u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| DaemonError::new("entropy", e.to_string()))?;
        let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let state = StateFile {
            version: 1,
            address: address.clone(),
            token,
            pid: std::process::id(),
        };
        write_state(&config.state_file, &state)?;
        let (stop, stop_rx) = watch::channel(false);
        let path = config.state_file.clone();
        let shared = Arc::new(Shared::new(backend, config));
        let task = tokio::spawn(serve(
            listener,
            state,
            shared,
            stop.clone(),
            stop_rx,
            path.clone(),
        ));
        Ok(Self {
            state_file: path,
            address,
            stop,
            task,
        })
    }

    pub fn address(&self) -> &str {
        &self.address
    }
    pub fn state_file(&self) -> &Path {
        &self.state_file
    }

    pub fn request_shutdown(&self) {
        let _ = self.stop.send(true);
    }

    pub async fn wait(self) -> Result<(), DaemonError> {
        self.task
            .await
            .map_err(|e| DaemonError::new("task", e.to_string()))
    }
}

async fn serve(
    listener: TcpListener,
    state: StateFile,
    shared: Arc<Shared>,
    shutdown: watch::Sender<bool>,
    mut stop: watch::Receiver<bool>,
    state_file: PathBuf,
) {
    loop {
        tokio::select! {
            incoming = listener.accept() => {
                if let Ok((socket, peer)) = incoming {
                    if !peer.ip().is_loopback() { continue; }
                    let state = state.clone();
                    let shared = shared.clone();
                    let shutdown = shutdown.clone();
                    tokio::spawn(async move { let _ = handle(socket, state, shared, shutdown).await; });
                }
            }
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() { break; }
            }
        }
    }
    let _ = std::fs::remove_file(state_file);
}

async fn handle(
    mut socket: TcpStream,
    state: StateFile,
    shared: Arc<Shared>,
    shutdown: watch::Sender<bool>,
) -> Result<(), DaemonError> {
    let data = timeout(shared.config.request_timeout, read_frame(&mut socket))
        .await
        .map_err(|_| DaemonError::new("timeout", "IPC request timed out"))??;
    let response = match serde_json::from_slice::<WireRequest>(&data) {
        Ok(wire) if constant_time_eq(wire.token.as_bytes(), state.token.as_bytes()) => match wire
            .request
        {
            DaemonRequest::Status => DaemonResponse::Status(shared.status(&state.address).await),
            DaemonRequest::Stop => {
                let _ = shutdown.send(true);
                DaemonResponse::Stopping
            }
            DaemonRequest::Fetch { request } => match shared.fetch(request).await {
                Ok(reply) => DaemonResponse::Fetch(reply),
                Err(err) => DaemonResponse::Error(err),
            },
        },
        Ok(_) => DaemonResponse::Error(DaemonError::new("unauthorized", "invalid local token")),
        Err(_) => DaemonResponse::Error(DaemonError::new("parse", "invalid IPC request")),
    };
    match write_frame(&mut socket, &response).await {
        Err(err) if err.kind == "size" => {
            write_frame(&mut socket, &DaemonResponse::Error(err)).await
        }
        result => result,
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

fn io_err(error: impl std::fmt::Display) -> DaemonError {
    DaemonError::new("io", error.to_string())
}

fn write_state(path: &Path, state: &StateFile) -> Result<(), DaemonError> {
    let parent = path
        .parent()
        .ok_or_else(|| DaemonError::new("state", "state file needs a directory"))?;
    let existed = parent.exists();
    std::fs::create_dir_all(parent).map_err(io_err)?;
    if std::fs::symlink_metadata(parent)
        .map_err(io_err)?
        .file_type()
        .is_symlink()
    {
        return Err(DaemonError::new(
            "permission",
            "daemon state directory cannot be a symlink",
        ));
    }
    if existed {
        check_directory(parent)?;
    } else {
        restrict_directory(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(io_err)?;
    let serialized = serde_json::to_vec(state).map_err(io_err)?;
    file.write_all(&serialized).map_err(io_err)?;
    file.sync_all().map_err(io_err)?;
    Ok(())
}

#[cfg(unix)]
fn restrict_directory(path: &Path) -> Result<(), DaemonError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(io_err)
}

#[cfg(unix)]
fn check_directory(path: &Path) -> Result<(), DaemonError> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path).map_err(io_err)?;
    let mode = meta.permissions().mode();
    // Do not chmod an arbitrary existing directory (e.g. /tmp), even if the
    // caller accidentally supplied it. This can lock other users out.
    if mode & 0o077 != 0 {
        return Err(DaemonError::new(
            "permission",
            "existing state directory must have mode 0700 (or stricter)",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn check_directory(path: &Path) -> Result<(), DaemonError> {
    restrict_directory(path)
}

#[cfg(windows)]
fn restrict_directory(path: &Path) -> Result<(), DaemonError> {
    let username = std::process::Command::new("whoami")
        .output()
        .map_err(io_err)?;
    if !username.status.success() {
        return Err(DaemonError::new(
            "permission",
            "cannot determine Windows account",
        ));
    }
    let account = String::from_utf8_lossy(&username.stdout).trim().to_owned();
    let grant = format!("{account}:(OI)(CI)F");
    let result = std::process::Command::new("icacls")
        .arg(path)
        .arg("/inheritance:r")
        .arg("/grant:r")
        .arg(grant)
        .output()
        .map_err(io_err)?;
    if !result.status.success() {
        return Err(DaemonError::new(
            "permission",
            "could not restrict Windows daemon state directory ACL",
        ));
    }
    Ok(())
}

pub struct DaemonClient {
    state: StateFile,
}

impl DaemonClient {
    pub fn from_state_file(path: impl AsRef<Path>) -> Result<Self, DaemonError> {
        let content = std::fs::read(path).map_err(io_err)?;
        let state: StateFile = serde_json::from_slice(&content).map_err(io_err)?;
        if state.version != 1 || state.token.len() != 64 {
            return Err(DaemonError::new("state", "unsupported daemon state file"));
        }
        let addr = state
            .address
            .parse::<std::net::SocketAddr>()
            .map_err(io_err)?;
        if !addr.ip().is_loopback() {
            return Err(DaemonError::new(
                "state",
                "refusing non-loopback daemon address",
            ));
        }
        Ok(Self { state })
    }

    /// `None` when there is no active daemon; callers may fall back to direct
    /// fetching. Authentication errors remain errors, not silent fallback.
    pub async fn discover(path: impl AsRef<Path>) -> Result<Option<Self>, DaemonError> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(None);
        }
        let client = Self::from_state_file(path)?;
        match client.status().await {
            Ok(_) => Ok(Some(client)),
            Err(e) if e.kind == "io" || e.kind == "timeout" => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn call(&self, request: DaemonRequest) -> Result<DaemonResponse, DaemonError> {
        let mut socket = timeout(
            Duration::from_secs(3),
            TcpStream::connect(&self.state.address),
        )
        .await
        .map_err(|_| DaemonError::new("timeout", "daemon connection timeout"))?
        .map_err(io_err)?;
        write_frame(
            &mut socket,
            &WireRequest {
                token: self.state.token.clone(),
                request,
            },
        )
        .await?;
        let response = timeout(Duration::from_secs(70), read_frame(&mut socket))
            .await
            .map_err(|_| DaemonError::new("timeout", "daemon reply timeout"))??;
        let response: DaemonResponse = serde_json::from_slice(&response).map_err(io_err)?;
        if let DaemonResponse::Error(err) = response {
            return Err(err);
        }
        Ok(response)
    }

    pub async fn status(&self) -> Result<Status, DaemonError> {
        match self.call(DaemonRequest::Status).await? {
            DaemonResponse::Status(s) => Ok(s),
            _ => Err(DaemonError::new("protocol", "expected status")),
        }
    }

    pub async fn stop(&self) -> Result<(), DaemonError> {
        match self.call(DaemonRequest::Stop).await? {
            DaemonResponse::Stopping => Ok(()),
            _ => Err(DaemonError::new("protocol", "expected stopping")),
        }
    }

    pub async fn fetch(&self, request: FetchRequest) -> Result<FetchResult, DaemonError> {
        match self.call(DaemonRequest::Fetch { request }).await? {
            DaemonResponse::Fetch(result) => Ok(result),
            _ => Err(DaemonError::new("protocol", "expected fetch result")),
        }
    }
}

async fn read_frame(stream: &mut TcpStream) -> Result<Vec<u8>, DaemonError> {
    let mut data = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let n = stream.read(&mut buffer).await.map_err(io_err)?;
        if n == 0 {
            return Err(DaemonError::new("protocol", "unexpected end of IPC stream"));
        }
        if let Some(pos) = buffer[..n].iter().position(|&byte| byte == b'\n') {
            if data.len() + pos > MAX_FRAME {
                return Err(DaemonError::new("size", "IPC frame exceeds 16MiB"));
            }
            data.extend_from_slice(&buffer[..pos]);
            return Ok(data);
        }
        if data.len() + n > MAX_FRAME {
            return Err(DaemonError::new("size", "IPC frame exceeds 16MiB"));
        }
        data.extend_from_slice(&buffer[..n]);
    }
}

async fn write_frame<T: Serialize>(stream: &mut TcpStream, object: &T) -> Result<(), DaemonError> {
    let data = serde_json::to_vec(object).map_err(io_err)?;
    if data.len() > MAX_FRAME {
        return Err(DaemonError::new("size", "IPC reply exceeds 16MiB"));
    }
    stream.write_all(&data).await.map_err(io_err)?;
    stream.write_all(b"\n").await.map_err(io_err)
}
