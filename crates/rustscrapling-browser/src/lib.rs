//! Browser fetching using Chromium DevTools Protocol (CDP).
//!
//! D-007: the stealth path sends commands directly through the WebSocket
//! transport with chromiumoxide_cdp 0.9 types. It never initializes
//! chromiumoxide's Page/FrameManager, which unconditionally enables Runtime.
//! Only the explicitly requested `evaluate` operation enables Runtime.

mod actions;
pub mod installed;
mod keys;
mod settings;
mod transport;

pub use actions::{
    ActionError, BrowserAction, ElementState, LoadState, MouseButton, PointerTarget,
};
pub use settings::{
    fingerprint_for, merge_feature_switches, upstream_launch_args, Fingerprint, PageSettings,
    DISABLED_RESOURCE_TYPES, FINGERPRINTS, PLAYWRIGHT_HEADLESS_SWITCHES, PLAYWRIGHT_SWITCHES,
    UPSTREAM_LAUNCH_ARGS,
};
pub use transport::{BrowserError, CdpEvent};

use chromiumoxide_cdp::cdp::browser_protocol::{dom, target};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time::sleep;
use transport::Connection;

#[derive(Debug, Clone)]
pub struct LaunchOptions {
    pub chrome_path: PathBuf,
    pub headless: bool,
    /// Chromium command-line proxy URI (host:port or scheme://host:port).
    /// Authenticated proxies require an external proxy adaptor for now.
    pub proxy: Option<String>,
    pub extra_args: Vec<String>,
}

impl LaunchOptions {
    pub fn new(chrome_path: impl Into<PathBuf>) -> Self {
        Self {
            chrome_path: chrome_path.into(),
            headless: true,
            proxy: None,
            extra_args: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BrowserGetOptions {
    pub navigation_timeout: Duration,
    pub capture_network: bool,
    /// Applied once when the reusable default page is created (upstream
    /// session settings such as the headless UA fix and 1920x1080 viewport).
    pub settings: Option<PageSettings>,
    /// Navigation referrer (upstream `google_search` uses Google's).
    pub referrer: Option<String>,
}

impl Default for BrowserGetOptions {
    fn default() -> Self {
        Self {
            navigation_timeout: Duration::from_secs(15),
            capture_network: true,
            settings: None,
            referrer: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NetworkRecord {
    pub url: String,
    /// HTTP method from `Network.requestWillBeSent`, when observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    pub status: Option<u16>,
    pub resource_type: Option<String>,
    pub headers: BTreeMap<String, String>,
    pub failed: Option<String>,
    /// CDP request identifier, scoped to this page's session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// Small textual XHR/Fetch response, with sensitive JSON fields redacted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

const MAX_NETWORK_RECORDS: usize = 128;
const MAX_NETWORK_BODY_BYTES: usize = 64 * 1024;
const MAX_NETWORK_TOTAL_BODY_BYTES: usize = 1024 * 1024;

#[derive(Default)]
struct NetworkCapture {
    records: VecDeque<NetworkRecord>,
    /// Request methods by CDP request id, bounded like the record list.
    methods: VecDeque<(String, String)>,
    finished: HashSet<String>,
    sensitive: HashSet<String>,
    ready: VecDeque<String>,
}

impl NetworkCapture {
    fn clear(&mut self) {
        self.records.clear();
        self.methods.clear();
        self.finished.clear();
        self.sensitive.clear();
        self.ready.clear();
    }

    fn observe(&mut self, event: &CdpEvent) {
        let id = event
            .params
            .get("requestId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.len() <= 128);
        match event.method.as_str() {
            "Network.requestWillBeSent" => {
                let method = event
                    .params
                    .pointer("/request/method")
                    .and_then(Value::as_str)
                    .filter(|method| method.len() <= 32);
                if let (Some(id), Some(method)) = (id, method) {
                    if self.methods.len() >= MAX_NETWORK_RECORDS * 4 {
                        self.methods.pop_front();
                    }
                    self.methods.push_back((id.to_owned(), method.to_owned()));
                }
            }
            "Network.responseReceived" => {
                if let Some(id) = id {
                    self.records
                        .retain(|record| record.request_id.as_deref() != Some(id));
                    self.finished.remove(id);
                    self.sensitive.remove(id);
                    self.ready.retain(|ready| ready != id);
                }
                if let Some(id) = id {
                    if event
                        .params
                        .pointer("/response/headers")
                        .and_then(Value::as_object)
                        .is_some_and(|headers| {
                            headers.keys().any(|name| {
                                [
                                    "set-cookie",
                                    "www-authenticate",
                                    "proxy-authenticate",
                                    "authorization",
                                    "proxy-authorization",
                                    "cookie",
                                ]
                                .iter()
                                .any(|sensitive| name.eq_ignore_ascii_case(sensitive))
                            })
                        })
                    {
                        self.sensitive.insert(id.to_owned());
                    }
                }
                if self.records.len() == MAX_NETWORK_RECORDS {
                    if let Some(old) = self.records.pop_front().and_then(|r| r.request_id) {
                        self.finished.remove(&old);
                        self.sensitive.remove(&old);
                        self.ready.retain(|ready| ready != &old);
                    }
                }
                let mut record = record_network(event);
                record.method = id.and_then(|id| {
                    self.methods
                        .iter()
                        .rev()
                        .find(|(request, _)| request == id)
                        .map(|(_, method)| method.clone())
                });
                self.records.push_back(record);
            }
            "Network.loadingFinished" => {
                let Some(id) = id else {
                    return;
                };
                if !self
                    .records
                    .iter()
                    .any(|record| record.request_id.as_deref() == Some(id))
                {
                    return;
                }
                self.finished.insert(id.to_owned());
                if event
                    .params
                    .get("encodedDataLength")
                    .and_then(Value::as_f64)
                    .is_some_and(|len| !len.is_finite() || len > MAX_NETWORK_BODY_BYTES as f64)
                {
                    return;
                }
                if self.records.iter().any(|record| {
                    record.request_id.as_deref() == Some(id) && eligible_api_body(record)
                }) && !self.sensitive.contains(id)
                    && !self.ready.iter().any(|ready| ready == id)
                {
                    self.ready.push_back(id.to_owned());
                }
            }
            "Network.loadingFailed" => {
                if let Some(id) = id {
                    self.finished.insert(id.to_owned());
                    self.ready.retain(|ready| ready != id);
                    if let Some(record) = self
                        .records
                        .iter_mut()
                        .rev()
                        .find(|record| record.request_id.as_deref() == Some(id))
                    {
                        record.failed = event
                            .params
                            .get("errorText")
                            .and_then(Value::as_str)
                            .map(safe_network_error);
                        return;
                    }
                }
                if self.records.len() == MAX_NETWORK_RECORDS {
                    if let Some(old) = self
                        .records
                        .pop_front()
                        .and_then(|record| record.request_id)
                    {
                        self.finished.remove(&old);
                        self.sensitive.remove(&old);
                        self.ready.retain(|ready| ready != &old);
                    }
                }
                let mut record = record_network(event);
                record.method = id.and_then(|id| {
                    self.methods
                        .iter()
                        .rev()
                        .find(|(request, _)| request == id)
                        .map(|(_, method)| method.clone())
                });
                self.records.push_back(record);
            }
            _ => {}
        }
    }

    fn awaiting_api_completion(&self) -> bool {
        self.records.iter().any(|record| {
            eligible_api_body(record)
                && record
                    .request_id
                    .as_ref()
                    .is_some_and(|id| !self.finished.contains(id) && !self.sensitive.contains(id))
        })
    }

    fn take_ready(&mut self) -> Vec<String> {
        self.ready.drain(..).collect()
    }

    fn store_body(&mut self, id: &str, body: String) {
        if self.sensitive.contains(id) {
            return;
        }
        let used: usize = self
            .records
            .iter()
            .filter_map(|r| r.body.as_ref())
            .map(String::len)
            .sum();
        if body.len() > MAX_NETWORK_BODY_BYTES
            || used.saturating_add(body.len()) > MAX_NETWORK_TOTAL_BODY_BYTES
        {
            return;
        }
        if let Some(record) = self
            .records
            .iter_mut()
            .rev()
            .find(|r| r.request_id.as_deref() == Some(id))
        {
            if record.failed.is_none() {
                record.body = Some(body);
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    CloudflareChallenge,
    Captcha,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserResponse {
    /// File and data URLs have no HTTP status. Network capture may be disabled.
    pub status: Option<u16>,
    pub final_url: String,
    pub body: String,
    pub headers: BTreeMap<String, String>,
    pub blocked: Option<BlockKind>,
    pub network: Vec<NetworkRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BrowserState {
    pub version: u8,
    pub cookies: Vec<Value>,
    /// v2: only the active top-level page origin; no third-party frames or IndexedDB.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub origins: Vec<BrowserOriginState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BrowserOriginState {
    pub origin: String,
    pub local_storage: Vec<LocalStorageItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LocalStorageItem {
    pub name: String,
    pub value: String,
}

const MAX_STATE_BYTES: usize = 4 * 1024 * 1024;
const MAX_SCREENSHOT_BASE64_BYTES: usize = 8 * 1024 * 1024;

/// An independent browser connection with a single reusable default tab.
/// Use `open_page` to create additional tabs in the same browser context.
pub struct BrowserClient {
    connection: Arc<Connection>,
    endpoint: String,
    default_page: Mutex<Option<BrowserPage>>,
    owned_child: Option<Child>,
    owned_profile: Option<PathBuf>,
}

impl BrowserClient {
    /// Launch Chrome/Chrome for Testing with a private ephemeral profile and
    /// a random loopback CDP port. No shared personal browser profiles.
    pub async fn launch(options: LaunchOptions) -> Result<Self, BrowserError> {
        if !options.chrome_path.is_file() {
            return Err(BrowserError::InvalidArgument(format!(
                "Chrome executable not found: {}",
                options.chrome_path.display()
            )));
        }
        if options.proxy.as_ref().is_some_and(|proxy| {
            proxy.contains('@')
                || proxy
                    .chars()
                    .any(|ch| ch.is_control() || ch.is_whitespace())
        }) {
            return Err(BrowserError::InvalidArgument(
                "proxy must be a non-authenticated host:port or proxy URL".into(),
            ));
        }
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let profile =
            std::env::temp_dir().join(format!("rustscrapling-cdp-{}-{nanos}", std::process::id()));
        tokio::fs::create_dir(&profile)
            .await
            .map_err(|e| BrowserError::Io(e.to_string()))?;
        let mut cmd = Command::new(&options.chrome_path);
        cmd.arg(format!("--user-data-dir={}", profile.display()))
            .arg("--remote-debugging-port=0")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--disable-background-networking")
            .arg("--window-size=1280,960")
            .arg("about:blank")
            .kill_on_drop(true);
        if options.headless {
            cmd.arg("--headless=new");
        }
        if let Some(proxy) = &options.proxy {
            cmd.arg(format!("--proxy-server={proxy}"));
        }
        for arg in merge_feature_switches(&options.extra_args) {
            cmd.arg(arg);
        }
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // A spawn that never started leaves an unused profile behind; remove it
        // before returning, or repeated failures fill the temp directory with
        // empty rustscrapling-cdp-* directories.
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(error) => {
                discard_profile(&profile).await;
                return Err(BrowserError::Io(error.to_string()));
            }
        };
        let endpoint = match read_debugging_endpoint(&profile, &mut child).await {
            Ok(endpoint) => endpoint,
            Err(error) => {
                let _ = child.kill().await;
                discard_profile(&profile).await;
                return Err(error);
            }
        };
        let connection = match Connection::connect(&endpoint).await {
            Ok(connection) => connection,
            Err(error) => {
                let _ = child.kill().await;
                discard_profile(&profile).await;
                return Err(error);
            }
        };
        Ok(Self {
            connection,
            endpoint,
            default_page: Mutex::new(None),
            owned_child: Some(child),
            owned_profile: Some(profile),
        })
    }

    /// Attach to a browser-level DevTools WebSocket endpoint. The caller
    /// retains ownership of the externally managed browser.
    /// Attach to an existing browser-level DevTools websocket. Only `ws`/`wss`
    /// endpoints without embedded credentials are accepted, on loopback or on
    /// a host listed in `RUSTSCRAPLING_CDP_ALLOWED_HOSTS` (Plan 7.1: external
    /// CDP connections only to allowed endpoints).
    pub async fn connect(ws_endpoint: &str) -> Result<Self, BrowserError> {
        check_cdp_endpoint(
            ws_endpoint,
            std::env::var("RUSTSCRAPLING_CDP_ALLOWED_HOSTS")
                .ok()
                .as_deref(),
        )?;
        Ok(Self {
            connection: Connection::connect(ws_endpoint).await?,
            endpoint: ws_endpoint.to_owned(),
            default_page: Mutex::new(None),
            owned_child: None,
            owned_profile: None,
        })
    }

    /// Browser-level DevTools WebSocket, suitable for attaching a second client.
    pub fn cdp_endpoint(&self) -> &str {
        &self.endpoint
    }

    pub async fn open_page(&self, capture_network: bool) -> Result<BrowserPage, BrowserError> {
        let created = self
            .connection
            .send_typed(target::CreateTargetParams::new("about:blank"), None)
            .await?;
        let id = created.target_id.inner().clone();
        let mut attach = target::AttachToTargetParams::new(created.target_id);
        attach.flatten = Some(true);
        let attached = self.connection.send_typed(attach, None).await?;
        let session_id = attached.session_id.inner().clone();
        let events = self.connection.subscribe();
        // For page targets the main frame id equals the target id.
        let (monitor, monitor_guard) =
            actions::spawn_monitor(&self.connection, session_id.clone(), id.clone());
        let page = BrowserPage {
            connection: self.connection.clone(),
            main_frame: id.clone(),
            target_id: id,
            session_id,
            events: Mutex::new(events),
            network: Mutex::new(NetworkCapture::default()),
            network_enabled: capture_network,
            monitor,
            _monitor_guard: monitor_guard,
        };
        page.raw("Page.enable", json!({})).await?;
        page.raw("Page.setLifecycleEventsEnabled", json!({"enabled": true}))
            .await?;
        // Like Playwright/patchright: keep document.hasFocus() and focus
        // events consistent even when the tab is in the background.
        page.raw(
            "Emulation.setFocusEmulationEnabled",
            json!({"enabled": true}),
        )
        .await?;
        if capture_network {
            page.raw("Network.enable", json!({})).await?;
        }
        Ok(page)
    }

    /// Fetch through the reusable default page. Calls on the same client are
    /// serialized to avoid concurrent navigations racing in one tab.
    pub async fn get(
        &self,
        url: &str,
        options: BrowserGetOptions,
    ) -> Result<BrowserResponse, BrowserError> {
        let mut page = self.default_page.lock().await;
        if page.is_none() {
            let created = self.open_page(options.capture_network).await?;
            if let Some(settings) = &options.settings {
                created.apply_settings(settings).await?;
            }
            *page = Some(created);
        }
        let tab = page.as_ref().expect("initialized above");
        if options.capture_network && !tab.network_enabled {
            tab.raw("Network.enable", json!({})).await?;
        }
        tab.navigate_with_referrer(url, options.navigation_timeout, options.referrer.as_deref())
            .await?;
        let body = tab.source().await?;
        let final_url = tab.current_url().await.unwrap_or_else(|_| url.to_owned());
        let network = if options.capture_network {
            tab.take_network().await
        } else {
            Vec::new()
        };
        let matching_url = redact_network_url(&final_url);
        let response = network
            .iter()
            .rev()
            .find(|entry| {
                entry.resource_type.as_deref() == Some("Document") && entry.url == matching_url
            })
            .or_else(|| {
                network
                    .iter()
                    .rev()
                    .find(|entry| entry.resource_type.as_deref() == Some("Document"))
            });
        let status = response.and_then(|entry| entry.status);
        let headers = response
            .map(|entry| entry.headers.clone())
            .unwrap_or_default();
        let blocked = detect_block(&body, status, &headers);
        Ok(BrowserResponse {
            status,
            final_url,
            body,
            headers,
            blocked,
            network,
        })
    }

    pub async fn commands(&self) -> Vec<String> {
        self.connection.history().await
    }

    /// Cookie-only state when no page is supplied. For localStorage, use
    /// `save_state_for_page` on the explicit session tab.
    pub async fn save_state(&self) -> Result<BrowserState, BrowserError> {
        let result = self
            .connection
            .send("Storage.getCookies", json!({}), None)
            .await?;
        Ok(BrowserState {
            version: 2,
            cookies: result
                .get("cookies")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            origins: vec![],
        })
    }

    /// Capture cookies and localStorage for the current tab's *top-level* HTTP(S)
    /// origin only. CDP storageKey binds the read to that tab's storage partition.
    pub async fn save_state_for_page(
        &self,
        page: &BrowserPage,
    ) -> Result<BrowserState, BrowserError> {
        self.check_page(page)?;
        let origin = page.storage_context().await?;
        let local_storage = page.get_local_storage(&origin.storage_key).await?;
        let mut state = self.save_state().await?;
        state.origins.push(BrowserOriginState {
            origin: origin.origin,
            local_storage,
        });
        validate_state_size(&state)?;
        Ok(state)
    }

    fn check_page(&self, page: &BrowserPage) -> Result<(), BrowserError> {
        if Arc::ptr_eq(&self.connection, &page.connection) {
            Ok(())
        } else {
            Err(BrowserError::InvalidArgument(
                "page does not belong to this browser connection".into(),
            ))
        }
    }

    pub async fn restore_state(&self, state: &BrowserState) -> Result<(), BrowserError> {
        if !matches!(state.version, 1 | 2) || (state.version == 1 && !state.origins.is_empty()) {
            return Err(BrowserError::InvalidArgument(
                "unsupported browser state version".into(),
            ));
        }
        validate_state_size(state)?;
        if !state.origins.is_empty() {
            return Err(BrowserError::InvalidArgument(
                "localStorage restore requires the matching active page".into(),
            ));
        }
        self.restore_cookies(state).await
    }

    async fn restore_cookies(&self, state: &BrowserState) -> Result<(), BrowserError> {
        if state.cookies.len() > 4096 {
            return Err(BrowserError::InvalidArgument(
                "too many cookies in browser state".into(),
            ));
        }
        let mut cookies = Vec::new();
        for value in &state.cookies {
            let mut cookie = serde_json::Map::new();
            for field in [
                "name",
                "value",
                "domain",
                "path",
                "secure",
                "httpOnly",
                "sameSite",
                "expires",
                "priority",
                "sameParty",
                "sourceScheme",
                "sourcePort",
                "partitionKey",
            ] {
                if let Some(value) = value.get(field) {
                    cookie.insert(field.to_owned(), value.clone());
                }
            }
            if ["name", "value", "domain"]
                .iter()
                .any(|field| !cookie.get(*field).is_some_and(Value::is_string))
            {
                return Err(BrowserError::InvalidArgument(
                    "invalid cookie in browser state".into(),
                ));
            }
            cookies.push(Value::Object(cookie));
        }
        self.connection
            .send("Storage.clearCookies", json!({}), None)
            .await?;
        if !cookies.is_empty() {
            self.connection
                .send("Storage.setCookies", json!({"cookies":cookies}), None)
                .await?;
        }
        Ok(())
    }

    /// Reject an origin mismatch *before* modifying any cookies or localStorage.
    pub async fn restore_state_for_page(
        &self,
        page: &BrowserPage,
        state: &BrowserState,
    ) -> Result<(), BrowserError> {
        self.check_page(page)?;
        if !matches!(state.version, 1 | 2) || (state.version == 1 && !state.origins.is_empty()) {
            return Err(BrowserError::InvalidArgument(
                "unsupported browser state version".into(),
            ));
        }
        validate_state_size(state)?;
        if state.origins.len() > 1 {
            return Err(BrowserError::InvalidArgument(
                "only one active top-level origin can be restored".into(),
            ));
        }
        let context = if let Some(origin) = state.origins.first() {
            let current = page.storage_context().await?;
            if origin.origin != current.origin {
                return Err(BrowserError::InvalidArgument(
                    "saved localStorage origin differs from the current page origin".into(),
                ));
            }
            Some(current)
        } else {
            None
        };
        // Validate storage entries before any destructive CDP operation.
        if let Some(origin) = state.origins.first() {
            if origin.local_storage.len() > 4096
                || origin
                    .local_storage
                    .iter()
                    .any(|entry| entry.name.len() > 65536)
            {
                return Err(BrowserError::InvalidArgument(
                    "localStorage item limit exceeded".into(),
                ));
            }
        }
        // Cookies may contain HttpOnly credentials. Restore only after origin checks.
        let mut cookie_state = state.clone();
        cookie_state.origins.clear();
        self.restore_state(&cookie_state).await?;
        if let (Some(origin), Some(context)) = (state.origins.first(), context) {
            page.replace_local_storage(&context.storage_key, &origin.local_storage)
                .await?;
        }
        Ok(())
    }

    pub async fn save_state_file(&self, path: impl AsRef<Path>) -> Result<(), BrowserError> {
        write_state_file(path.as_ref(), &self.save_state().await?).await
    }

    pub async fn save_state_file_for_page(
        &self,
        page: &BrowserPage,
        path: impl AsRef<Path>,
    ) -> Result<(), BrowserError> {
        write_state_file(path.as_ref(), &self.save_state_for_page(page).await?).await
    }

    pub async fn restore_state_file_for_page(
        &self,
        page: &BrowserPage,
        path: impl AsRef<Path>,
    ) -> Result<(), BrowserError> {
        let state = read_state_file(path.as_ref()).await?;
        self.restore_state_for_page(page, &state).await
    }

    pub async fn restore_state_file(&self, path: impl AsRef<Path>) -> Result<(), BrowserError> {
        let state = read_state_file(path.as_ref()).await?;
        self.restore_state(&state).await
    }

    pub async fn shutdown(mut self) -> Result<(), BrowserError> {
        self.default_page.lock().await.take();
        if let Some(mut child) = self.owned_child.take() {
            let _ = child.kill().await;
            if let Some(profile) = self.owned_profile.take() {
                // Chrome's renderer and utility subprocesses may still be
                // releasing files after the browser parent exits. A single
                // recursive delete can race with their last profile writes.
                let mut cleared = false;
                for attempt in 0..30 {
                    match tokio::fs::remove_dir_all(&profile).await {
                        Ok(()) => {
                            cleared = true;
                            break;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            cleared = true;
                            break;
                        }
                        Err(_) if attempt < 29 => sleep(Duration::from_millis(100)).await,
                        Err(_) => break,
                    }
                }
                if !cleared {
                    // Hand it to the daemon sweep rather than reporting success.
                    let _ = tokio::fs::rename(&profile, abandoned_name(&profile)).await;
                }
            }
        }
        Ok(())
    }
}

fn validate_state_size(state: &BrowserState) -> Result<(), BrowserError> {
    let length = serde_json::to_vec(state)
        .map_err(|_| BrowserError::InvalidArgument("invalid browser state".into()))?
        .len();
    if length > MAX_STATE_BYTES {
        return Err(BrowserError::InvalidArgument(
            "browser state exceeds 4 MiB".into(),
        ));
    }
    Ok(())
}

async fn write_state_file(path: &Path, state: &BrowserState) -> Result<(), BrowserError> {
    let json = serde_json::to_vec_pretty(state).map_err(|e| BrowserError::Io(e.to_string()))?;
    if json.len() > MAX_STATE_BYTES {
        return Err(BrowserError::InvalidArgument(
            "browser state exceeds 4 MiB".into(),
        ));
    }
    // create_new is atomic and does not follow a symlink at the destination.
    // Refuse to overwrite existing state containing session credentials.
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .await
        .map_err(|e| BrowserError::Io(e.to_string()))?;
    let written = async {
        file.write_all(&json).await?;
        file.sync_all().await
    }
    .await;
    if let Err(error) = written {
        let _ = tokio::fs::remove_file(path).await;
        return Err(BrowserError::Io(error.to_string()));
    }
    Ok(())
}

/// Validate a CDP websocket endpoint (see [`BrowserClient::connect`]).
pub fn check_cdp_endpoint(endpoint: &str, allowed_hosts: Option<&str>) -> Result<(), BrowserError> {
    let invalid = |message: &str| Err(BrowserError::InvalidArgument(message.to_owned()));
    if endpoint
        .chars()
        .any(|c| c.is_control() || c.is_whitespace())
    {
        return invalid("cdp_url must not contain whitespace or control characters");
    }
    let Ok(url) = url::Url::parse(endpoint) else {
        return invalid("cdp_url must be a ws:// or wss:// URL");
    };
    if !matches!(url.scheme(), "ws" | "wss") {
        return invalid("cdp_url must be a ws:// or wss:// URL");
    }
    if !url.username().is_empty() || url.password().is_some() {
        return invalid("cdp_url must not embed credentials");
    }
    let host = match url.host() {
        Some(url::Host::Domain(name)) => name.to_ascii_lowercase(),
        Some(url::Host::Ipv4(address)) if address.is_loopback() => return Ok(()),
        Some(url::Host::Ipv6(address)) if address.is_loopback() => return Ok(()),
        Some(url::Host::Ipv4(address)) => address.to_string(),
        Some(url::Host::Ipv6(address)) => address.to_string(),
        None => return invalid("cdp_url needs a host"),
    };
    if host == "localhost" {
        return Ok(());
    }
    let allowed = allowed_hosts.unwrap_or_default().split(',').any(|entry| {
        let entry = entry.trim().trim_start_matches('[').trim_end_matches(']');
        !entry.is_empty() && entry.eq_ignore_ascii_case(&host)
    });
    if allowed {
        Ok(())
    } else {
        invalid(&format!(
            "cdp_url host {host} is not loopback; add it to RUSTSCRAPLING_CDP_ALLOWED_HOSTS to allow it"
        ))
    }
}

/// `O_NOFOLLOW` without a libc dependency (Linux, Android, macOS, BSDs).
#[cfg(unix)]
fn libc_o_nofollow() -> i32 {
    if cfg!(any(target_os = "linux", target_os = "android")) {
        0o400000
    } else {
        0x0100
    }
}

async fn read_state_file(path: &Path) -> Result<BrowserState, BrowserError> {
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|e| BrowserError::Io(e.to_string()))?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_STATE_BYTES as u64 {
        return Err(BrowserError::InvalidArgument(
            "state must be a regular file under 4 MiB".into(),
        ));
    }
    // O_NOFOLLOW closes the window between the check above and the open:
    // a file swapped for a symlink in between is refused, not followed.
    let mut options = tokio::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        options.custom_flags(libc_o_nofollow());
    }
    let file = options
        .open(path)
        .await
        .map_err(|e| BrowserError::Io(e.to_string()))?;
    if !file
        .metadata()
        .await
        .map_err(|e| BrowserError::Io(e.to_string()))?
        .is_file()
    {
        return Err(BrowserError::InvalidArgument(
            "state must be a regular file under 4 MiB".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_STATE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| BrowserError::Io(e.to_string()))?;
    if bytes.len() > MAX_STATE_BYTES {
        return Err(BrowserError::InvalidArgument(
            "browser state exceeds 4 MiB".into(),
        ));
    }
    let data: Value = serde_json::from_slice(&bytes)
        .map_err(|_| BrowserError::InvalidArgument("invalid browser state JSON".into()))?;
    if data.get("indexedDB").is_some()
        || data.get("indexed_db").is_some()
        || data
            .get("origins")
            .and_then(Value::as_array)
            .is_some_and(|origins| {
                origins.iter().any(|origin| {
                    origin.get("indexedDB").is_some() || origin.get("indexed_db").is_some()
                })
            })
    {
        return Err(BrowserError::InvalidArgument(
            "IndexedDB state is unsupported".into(),
        ));
    }
    serde_json::from_value(data)
        .map_err(|_| BrowserError::InvalidArgument("invalid browser state JSON".into()))
}

impl Drop for BrowserClient {
    fn drop(&mut self) {
        if let Some(child) = &mut self.owned_child {
            let _ = child.start_kill();
        }
        // An abandoned client still owns a throwaway profile. Chrome's helper
        // processes release the files shortly after the parent dies, so retry
        // briefly instead of leaking a rustscrapling-cdp-* directory.
        if let Some(profile) = self.owned_profile.take() {
            remove_profile_blocking(&profile);
        }
    }
}

/// A launch that never produced a client has no owner, so it cannot rely on
/// `Drop`; it deletes the profile with the same patience and then renames what
/// is left, which is what stopped failed launches from accumulating.
async fn discard_profile(profile: &Path) {
    for attempt in 0..30 {
        match tokio::fs::remove_dir_all(profile).await {
            Ok(()) => return,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) if attempt < 29 => sleep(Duration::from_millis(100)).await,
            Err(_) => break,
        }
    }
    let _ = tokio::fs::rename(profile, abandoned_name(profile)).await;
}

/// Chrome's renderer and utility processes release profile files well after
/// the browser process is reaped, so a short fixed budget left roughly a
/// quarter of one-shot clients behind. Three seconds of 100 ms attempts covers
/// the observed latency; anything still stuck is renamed so the daemon sweep
/// can collect it later instead of being mistaken for a live profile.
fn remove_profile_blocking(profile: &Path) {
    for attempt in 0..30 {
        match std::fs::remove_dir_all(profile) {
            Ok(()) => return,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) if attempt < 29 => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => break,
        }
    }
    let _ = std::fs::rename(profile, abandoned_name(profile));
}

/// Marks a profile that outlived its owner, so the sweep can remove it without
/// waiting for the six-hour staleness window.
fn abandoned_name(profile: &Path) -> std::path::PathBuf {
    let mut name = profile.file_name().unwrap_or_default().to_os_string();
    name.push(".abandoned");
    profile.with_file_name(name)
}

pub struct BrowserPage {
    connection: Arc<Connection>,
    target_id: String,
    main_frame: String,
    monitor: Arc<actions::PageMonitor>,
    _monitor_guard: actions::MonitorGuard,
    session_id: String,
    events: Mutex<tokio::sync::broadcast::Receiver<CdpEvent>>,
    network: Mutex<NetworkCapture>,
    network_enabled: bool,
}

struct StorageContext {
    origin: String,
    storage_key: String,
}

impl BrowserPage {
    pub fn target_id(&self) -> &str {
        &self.target_id
    }

    async fn raw(&self, method: &str, params: Value) -> Result<Value, BrowserError> {
        self.connection
            .send(method, params, Some(&self.session_id))
            .await
    }

    /// Navigate without injecting JavaScript or enabling Runtime.
    pub async fn navigate(
        &self,
        url: &str,
        navigation_timeout: Duration,
    ) -> Result<(), BrowserError> {
        self.navigate_with_referrer(url, navigation_timeout, None)
            .await
    }

    /// [`Self::navigate`] with an explicit `Referer` (upstream
    /// `google_search=True` uses `https://www.google.com/`).
    pub async fn navigate_with_referrer(
        &self,
        url: &str,
        navigation_timeout: Duration,
        referrer: Option<&str>,
    ) -> Result<(), BrowserError> {
        let parsed =
            url::Url::parse(url).map_err(|e| BrowserError::InvalidArgument(e.to_string()))?;
        if !matches!(
            parsed.scheme(),
            "http" | "https" | "file" | "about" | "data"
        ) {
            return Err(BrowserError::InvalidArgument(
                "unsupported navigation URL scheme".into(),
            ));
        }
        // A navigation timeout is a single deadline for both the CDP command
        // acknowledgment and the subsequent load events. Otherwise a stalled
        // Page.navigate can spend the transport's full fixed timeout before
        // the configured navigation timeout even starts.
        let deadline = tokio::time::Instant::now()
            .checked_add(navigation_timeout)
            .ok_or_else(|| BrowserError::InvalidArgument("navigation timeout too large".into()))?;
        self.network.lock().await.clear();
        let mut receiver = self.events.lock().await;
        while receiver.try_recv().is_ok() {} // discard prior-page events
        let result = tokio::time::timeout_at(deadline, {
            let mut params = json!({ "url": url });
            if let Some(referrer) = referrer {
                // As Playwright does, send the referrer verbatim even on
                // HTTPS -> HTTP navigations.
                params["referrer"] = referrer.into();
                params["referrerPolicy"] = "unsafeUrl".into();
            }
            self.raw("Page.navigate", params)
        })
        .await
        .map_err(|_| BrowserError::Timeout("Page.navigate".into()))??;
        if let Some(error) = result.get("errorText").and_then(Value::as_str) {
            return Err(BrowserError::Protocol(error.into()));
        }
        let wait = async {
            let mut main_frame_navigated = false;
            loop {
                match receiver.recv().await {
                    Ok(event) if event.session_id.as_deref() == Some(&self.session_id) => {
                        if event.method == "Page.frameNavigated"
                            && event.params.pointer("/frame/parentId").is_none()
                        {
                            main_frame_navigated = true;
                        }
                        if event.method == "Page.navigatedWithinDocument" {
                            return Ok(());
                        }
                        if event.method == "Page.loadEventFired" && main_frame_navigated {
                            return Ok(());
                        }
                        self.observe_network(&event).await;
                    }
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => return Err(BrowserError::Disconnected),
                }
            }
        };
        tokio::time::timeout_at(deadline, wait)
            .await
            .map_err(|_| BrowserError::Timeout("Page.loadEventFired".into()))??;
        while let Ok(event) = receiver.try_recv() {
            self.observe_network(&event).await;
        }
        Ok(())
    }

    /// HTML source via DOM.getDocument/getOuterHTML, including open Shadow
    /// DOM where the browser protocol supports includeShadowDOM.
    pub async fn source(&self) -> Result<String, BrowserError> {
        self.source_with_shadow(true).await
    }

    /// Document HTML; `include_shadow` serializes open shadow roots as
    /// declarative `<template shadowrootmode>` (upstream `pierce_shadow`).
    pub async fn source_with_shadow(&self, include_shadow: bool) -> Result<String, BrowserError> {
        let document = self
            .connection
            .send_typed(
                dom::GetDocumentParams {
                    depth: Some(-1),
                    pierce: Some(true),
                },
                Some(&self.session_id),
            )
            .await?;
        let response = self
            .connection
            .send_typed(
                dom::GetOuterHtmlParams::builder()
                    .node_id(document.root.node_id)
                    .include_shadow_dom(include_shadow)
                    .build(),
                Some(&self.session_id),
            )
            .await?;
        Ok(response.outer_html)
    }

    pub async fn current_url(&self) -> Result<String, BrowserError> {
        let result = self.raw("Page.getFrameTree", json!({})).await?;
        result
            .pointer("/frameTree/frame/url")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| BrowserError::Protocol("Page.getFrameTree has no URL".into()))
    }

    /// Locate a CSS element through DOM CDP commands, without Runtime.enable
    /// or JavaScript injection. This currently targets the main document only.
    async fn css_node(&self, selector: &str) -> Result<u64, BrowserError> {
        if selector.trim().is_empty() || selector.len() > 2048 || selector.contains('\0') {
            return Err(BrowserError::InvalidArgument(
                "CSS selector must be 1..=2048 bytes and contain no NUL".into(),
            ));
        }
        let root = self.raw("DOM.getDocument", json!({"depth":0})).await?;
        let root_id = root
            .pointer("/root/nodeId")
            .and_then(Value::as_u64)
            .filter(|id| *id > 0)
            .ok_or_else(|| BrowserError::Protocol("DOM root node missing".into()))?;
        let queried = self
            .raw(
                "DOM.querySelector",
                json!({"nodeId":root_id,"selector":selector}),
            )
            .await?;
        queried
            .get("nodeId")
            .and_then(Value::as_u64)
            .filter(|id| *id > 0)
            .ok_or_else(|| BrowserError::InvalidArgument("CSS selector matched no element".into()))
    }

    /// Click the center of the first matching element using real CDP mouse
    /// events, not a DOM-dispatched synthetic click. Does not enter iframes.
    pub async fn click_css(&self, selector: &str) -> Result<(), BrowserError> {
        let node = self.css_node(selector).await?;
        self.raw("DOM.scrollIntoViewIfNeeded", json!({"nodeId":node}))
            .await?;
        let model = self.raw("DOM.getBoxModel", json!({"nodeId":node})).await?;
        let quad = model
            .pointer("/model/content")
            .and_then(Value::as_array)
            .filter(|points| points.len() == 8)
            .ok_or_else(|| BrowserError::Protocol("element has no usable box model".into()))?;
        let mut position = [0.0_f64; 2];
        for pair in quad.as_chunks::<2>().0 {
            for (index, value) in pair.iter().enumerate() {
                position[index] += value
                    .as_f64()
                    .filter(|v| v.is_finite() && v.abs() <= 65536.0)
                    .ok_or_else(|| BrowserError::Protocol("invalid element coordinates".into()))?
                    / 4.0;
            }
        }
        if position[0] < 0.0 || position[1] < 0.0 {
            return Err(BrowserError::InvalidArgument(
                "element is outside the visible viewport".into(),
            ));
        }
        let [x, y] = position;
        self.raw(
            "Input.dispatchMouseEvent",
            json!({"type":"mousePressed","x":x,"y":y,
                "button":"left","clickCount":1}),
        )
        .await?;
        self.raw(
            "Input.dispatchMouseEvent",
            json!({"type":"mouseReleased","x":x,"y":y,
                "button":"left","clickCount":1}),
        )
        .await?;
        Ok(())
    }

    /// Focus the first CSS match and append UTF-8 text via Chrome's input
    /// domain. Deliberately not called `fill`: it does not clear existing text.
    pub async fn type_text_css(&self, selector: &str, text: &str) -> Result<(), BrowserError> {
        if text.len() > 65536 || text.contains('\0') {
            return Err(BrowserError::InvalidArgument(
                "input text exceeds 64 KiB or contains NUL".into(),
            ));
        }
        let node = self.css_node(selector).await?;
        self.raw("DOM.focus", json!({"nodeId":node})).await?;
        if !text.is_empty() {
            self.raw("Input.insertText", json!({"text":text})).await?;
        }
        Ok(())
    }

    /// Replace the value of a native input/textarea with UTF-8 text.
    /// Uses a focused Ctrl+A / Backspace / insertText sequence on the actual
    /// CDP Input domain, so ordinary input/change handlers can observe input.
    /// Rich contenteditable widgets and platform-specific shortcuts remain
    /// outside this contract.
    pub async fn fill_text_css(&self, selector: &str, text: &str) -> Result<(), BrowserError> {
        if text.len() > 65536 || text.contains('\0') {
            return Err(BrowserError::InvalidArgument(
                "input text exceeds 64 KiB or contains NUL".into(),
            ));
        }
        let node = self.css_node(selector).await?;
        let description = self.raw("DOM.describeNode", json!({"nodeId":node})).await?;
        let node_name = description
            .pointer("/node/nodeName")
            .and_then(Value::as_str)
            .ok_or_else(|| BrowserError::Protocol("element type unavailable".into()))?;
        if !matches!(node_name, "INPUT" | "TEXTAREA") {
            return Err(BrowserError::InvalidArgument(
                "fill_text_css only supports native input and textarea elements".into(),
            ));
        }
        if node_name == "INPUT" {
            let attributes = description
                .pointer("/node/attributes")
                .and_then(Value::as_array);
            let input_type = attributes
                .and_then(|attributes| {
                    attributes
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .find(|pair| {
                            pair[0]
                                .as_str()
                                .is_some_and(|key| key.eq_ignore_ascii_case("type"))
                        })
                        .and_then(|pair| pair[1].as_str())
                })
                .unwrap_or("text");
            if ![
                "text", "search", "email", "password", "url", "tel", "number",
            ]
            .iter()
            .any(|allowed| input_type.eq_ignore_ascii_case(allowed))
            {
                return Err(BrowserError::InvalidArgument(
                    "fill_text_css supports only editable text-like input types".into(),
                ));
            }
        }
        self.raw("DOM.focus", json!({"nodeId":node})).await?;
        for event_type in ["keyDown", "keyUp"] {
            self.raw(
                "Input.dispatchKeyEvent",
                json!({"type":event_type,"modifiers":2,"key":"a","code":"KeyA",
                    "windowsVirtualKeyCode":65,"nativeVirtualKeyCode":65}),
            )
            .await?;
        }
        self.press_key("Backspace").await?;
        if !text.is_empty() {
            self.raw("Input.insertText", json!({"text":text})).await?;
        }
        Ok(())
    }

    /// Press and release a supported physical key in the current page.
    pub async fn press_key(&self, key: &str) -> Result<(), BrowserError> {
        let (code, virtual_key) = match key {
            "Enter" => ("Enter", 13),
            "Tab" => ("Tab", 9),
            "Escape" => ("Escape", 27),
            "Backspace" => ("Backspace", 8),
            "Delete" => ("Delete", 46),
            "ArrowUp" => ("ArrowUp", 38),
            "ArrowDown" => ("ArrowDown", 40),
            "ArrowLeft" => ("ArrowLeft", 37),
            "ArrowRight" => ("ArrowRight", 39),
            _ => {
                return Err(BrowserError::InvalidArgument(
                    "unsupported key; use Enter/Tab/Escape/Backspace/Delete/Arrow keys".into(),
                ))
            }
        };
        for event_type in ["keyDown", "keyUp"] {
            self.raw(
                "Input.dispatchKeyEvent",
                json!({"type":event_type,"key":key,"code":code,
                    "windowsVirtualKeyCode":virtual_key,"nativeVirtualKeyCode":virtual_key}),
            )
            .await?;
        }
        Ok(())
    }

    /// Resolve the first-party storage key from the *current page session*.
    /// The server-supplied state origin is never passed as a CDP storage target.
    async fn storage_context(&self) -> Result<StorageContext, BrowserError> {
        let tree = self.raw("Page.getFrameTree", json!({})).await?;
        let frame = tree
            .pointer("/frameTree/frame")
            .ok_or_else(|| BrowserError::Protocol("missing main frame".into()))?;
        let url = frame
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| BrowserError::Protocol("missing main frame URL".into()))?;
        let parsed = url::Url::parse(url)
            .map_err(|_| BrowserError::InvalidArgument("invalid main frame URL".into()))?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err(BrowserError::InvalidArgument(
                "localStorage requires an HTTP(S) top-level page".into(),
            ));
        }
        let origin = parsed.origin().ascii_serialization();
        if frame
            .get("securityOrigin")
            .and_then(Value::as_str)
            .is_some_and(|security| security != origin)
        {
            return Err(BrowserError::InvalidArgument(
                "main frame security origin mismatch".into(),
            ));
        }
        let frame_id = frame
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| BrowserError::Protocol("missing main frame ID".into()))?;
        let key_response = self
            .raw("Storage.getStorageKeyForFrame", json!({"frameId":frame_id}))
            .await?;
        let storage_key = key_response
            .get("storageKey")
            .and_then(Value::as_str)
            .filter(|key| !key.is_empty() && key.len() <= 4096)
            .ok_or_else(|| BrowserError::Protocol("missing storage key for main frame".into()))?;
        Ok(StorageContext {
            origin,
            storage_key: storage_key.to_owned(),
        })
    }

    async fn get_local_storage(&self, key: &str) -> Result<Vec<LocalStorageItem>, BrowserError> {
        let storage_id = json!({"storageKey":key,"isLocalStorage":true});
        let result = self
            .raw(
                "DOMStorage.getDOMStorageItems",
                json!({"storageId":storage_id}),
            )
            .await?;
        let entries = result
            .get("entries")
            .and_then(Value::as_array)
            .ok_or_else(|| BrowserError::Protocol("DOMStorage returned no entries array".into()))?;
        if entries.len() > 4096 {
            return Err(BrowserError::InvalidArgument(
                "too many localStorage items".into(),
            ));
        }
        let mut items = Vec::with_capacity(entries.len());
        for entry in entries {
            let pair = entry
                .as_array()
                .filter(|pair| pair.len() == 2)
                .ok_or_else(|| BrowserError::Protocol("invalid DOMStorage entry".into()))?;
            let name = pair[0]
                .as_str()
                .ok_or_else(|| BrowserError::Protocol("invalid DOMStorage name".into()))?;
            let value = pair[1]
                .as_str()
                .ok_or_else(|| BrowserError::Protocol("invalid DOMStorage value".into()))?;
            items.push(LocalStorageItem {
                name: name.to_owned(),
                value: value.to_owned(),
            });
        }
        items.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(items)
    }

    async fn replace_local_storage(
        &self,
        key: &str,
        items: &[LocalStorageItem],
    ) -> Result<(), BrowserError> {
        let storage_id = json!({"storageKey":key,"isLocalStorage":true});
        self.raw("DOMStorage.clear", json!({"storageId":storage_id}))
            .await?;
        for item in items {
            self.raw(
                "DOMStorage.setDOMStorageItem",
                json!({
                    "storageId":storage_id,"key":item.name,"value":item.value
                }),
            )
            .await?;
        }
        Ok(())
    }

    async fn observe_network(&self, event: &CdpEvent) {
        if self.network_enabled && event.session_id.as_deref() == Some(self.session_id.as_str()) {
            self.network.lock().await.observe(event);
        }
    }

    /// Consume pending CDP events for this page. A short grace window allows
    /// in-flight API responses to finish after the top-level load event.
    async fn collect_network_events(&self) {
        if !self.network_enabled {
            return;
        }
        let mut receiver = self.events.lock().await;
        while let Ok(event) = receiver.try_recv() {
            self.observe_network(&event).await;
        }
        if !self.network.lock().await.awaiting_api_completion() {
            return;
        }
        let deadline = tokio::time::Instant::now() + Duration::from_millis(120);
        while self.network.lock().await.awaiting_api_completion() {
            match tokio::time::timeout_at(deadline, receiver.recv()).await {
                Ok(Ok(event)) => self.observe_network(&event).await,
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
                _ => break,
            }
        }
    }

    async fn refresh_network(&self) {
        self.collect_network_events().await;
        let ready = self.network.lock().await.take_ready();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        for id in ready {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            let result = self
                .connection
                .send_with_timeout(
                    "Network.getResponseBody",
                    json!({"requestId":id}),
                    Some(&self.session_id),
                    remaining,
                )
                .await;
            let Ok(response) = result else {
                continue;
            };
            if response.get("base64Encoded").and_then(Value::as_bool) != Some(false) {
                continue; // CDP binary or unknown encoding must not be retained.
            }
            let Some(raw) = response
                .get("body")
                .and_then(Value::as_str)
                .filter(|body| body.len() <= MAX_NETWORK_BODY_BYTES)
            else {
                continue;
            };
            let mime = self
                .network
                .lock()
                .await
                .records
                .iter()
                .rev()
                .find(|record| record.request_id.as_deref() == Some(id.as_str()))
                .and_then(|record| {
                    record.mime_type.clone().or_else(|| {
                        record
                            .headers
                            .iter()
                            .find(|(key, _)| key.eq_ignore_ascii_case("content-type"))
                            .map(|(_, value)| value.clone())
                    })
                });
            if let Some(body) = sanitized_api_body(raw, mime.as_deref()) {
                self.network.lock().await.store_body(&id, body);
            }
        }
    }

    /// Snapshot of a page's bounded network history, including captured API bodies.
    pub async fn network_records(&self) -> Vec<NetworkRecord> {
        self.refresh_network().await;
        self.network.lock().await.records.iter().cloned().collect()
    }

    /// Search captured textual API response bodies (case-sensitive).
    pub async fn search_network_bodies(&self, needle: &str) -> Vec<NetworkRecord> {
        if needle.is_empty() || needle.len() > 256 {
            return Vec::new();
        }
        self.network_records()
            .await
            .into_iter()
            .filter(|record| {
                record
                    .body
                    .as_ref()
                    .is_some_and(|body| body.contains(needle))
            })
            .collect()
    }

    /// Preserve the existing take-and-clear contract for BrowserClient and MCP.
    pub async fn take_network(&self) -> Vec<NetworkRecord> {
        self.refresh_network().await;
        let mut state = self.network.lock().await;
        let records = state.records.drain(..).collect();
        state.clear();
        records
    }

    /// Explicit JavaScript evaluation, never called from `get`/`source`.
    /// Enabling Runtime is intentionally visible only in this API.
    pub async fn evaluate(&self, expression: &str) -> Result<Value, BrowserError> {
        self.raw("Runtime.enable", json!({})).await?;
        let value = self
            .raw(
                "Runtime.evaluate",
                json!({
                    "expression": expression, "returnByValue": true, "awaitPromise": true
                }),
            )
            .await?;
        if let Some(error) = value.get("exceptionDetails") {
            return Err(BrowserError::Protocol(error.to_string()));
        }
        Ok(value
            .pointer("/result/value")
            .cloned()
            .unwrap_or(Value::Null))
    }

    /// Returns an encoded PNG/JPEG CDP screenshot; limits page size and response size.
    /// This does not initialize Runtime or execute page JavaScript.
    pub async fn screenshot(
        &self,
        image_type: &str,
        quality: Option<u8>,
        full_page: bool,
    ) -> Result<String, BrowserError> {
        if !matches!(image_type, "png" | "jpeg") || (image_type == "png" && quality.is_some()) {
            return Err(BrowserError::InvalidArgument(
                "invalid screenshot format or quality".into(),
            ));
        }
        let mut args = json!({"format": image_type, "fromSurface": true});
        if let Some(quality) = quality {
            if quality > 100 {
                return Err(BrowserError::InvalidArgument(
                    "JPEG quality must be 0-100".into(),
                ));
            }
            args["quality"] = json!(quality);
        }
        if full_page {
            let layout = self.raw("Page.getLayoutMetrics", json!({})).await?;
            let size = layout
                .get("cssContentSize")
                .or_else(|| layout.get("contentSize"))
                .ok_or_else(|| BrowserError::Protocol("CDP layout metrics missing".into()))?;
            let width = size["width"].as_f64().unwrap_or(0.0);
            let height = size["height"].as_f64().unwrap_or(0.0);
            if !(width.is_finite()
                && height.is_finite()
                && width > 0.0
                && height > 0.0
                && width <= 8192.0
                && height <= 8192.0
                && width * height <= 32_000_000.0)
            {
                return Err(BrowserError::InvalidArgument(
                    "full-page screenshot exceeds 8192px or 32MP limits".into(),
                ));
            }
            args["captureBeyondViewport"] = json!(true);
            args["clip"] = json!({"x":0,"y":0,"width":width,"height":height,"scale":1});
        }
        self.capture_screenshot(args).await
    }

    /// Run `Page.captureScreenshot` and validate the encoded result size.
    pub(crate) async fn capture_screenshot(&self, args: Value) -> Result<String, BrowserError> {
        // Large viewports on a busy host can exceed the transport's 20 s
        // default; callers bound the operation with the tool's own timeout.
        let response = self
            .connection
            .send_with_timeout(
                "Page.captureScreenshot",
                args,
                Some(&self.session_id),
                Duration::from_secs(120),
            )
            .await?;
        let encoded = response
            .get("data")
            .and_then(Value::as_str)
            .ok_or_else(|| BrowserError::Protocol("screenshot data missing".into()))?;
        if encoded.is_empty()
            || encoded.len() > MAX_SCREENSHOT_BASE64_BYTES
            || encoded.len() % 4 != 0
            || !encoded
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
        {
            return Err(BrowserError::Protocol(
                "invalid or oversized screenshot data".into(),
            ));
        }
        Ok(encoded.to_owned())
    }

    pub async fn close(self) -> Result<(), BrowserError> {
        self.connection
            .send(
                "Target.closeTarget",
                json!({"targetId":self.target_id}),
                None,
            )
            .await?;
        Ok(())
    }
}

fn record_network(event: &CdpEvent) -> NetworkRecord {
    let response = event.params.get("response").unwrap_or(&Value::Null);
    let request = event.params.get("request").unwrap_or(&Value::Null);
    let url = response
        .get("url")
        .or_else(|| request.get("url"))
        .and_then(Value::as_str)
        .map(redact_network_url)
        .unwrap_or_default();
    let status = response
        .get("status")
        .and_then(Value::as_f64)
        .filter(|v| (100.0..=599.0).contains(v))
        .map(|v| v as u16);
    let headers = response
        .get("headers")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(key, value)| {
                    // Never store Set-Cookie, Authorization, proxy auth or
                    // arbitrary application headers with credential values.
                    if !["content-type", "content-length", "server"]
                        .iter()
                        .any(|allowed| key.eq_ignore_ascii_case(allowed))
                    {
                        return None;
                    }
                    let value = value.as_str().filter(|value| value.len() <= 256)?;
                    Some((key.clone(), value.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default();
    NetworkRecord {
        method: None,
        url,
        status,
        headers,
        resource_type: event
            .params
            .get("type")
            .and_then(Value::as_str)
            .filter(|kind| kind.len() <= 32)
            .map(str::to_owned),
        failed: event
            .params
            .get("errorText")
            .and_then(Value::as_str)
            .map(safe_network_error),
        request_id: event
            .params
            .get("requestId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.len() <= 128)
            .map(str::to_owned),
        mime_type: response
            .get("mimeType")
            .and_then(Value::as_str)
            .filter(|kind| kind.len() <= 128)
            .map(str::to_owned),
        body: None,
    }
}

fn redact_network_url(raw: &str) -> String {
    let Ok(mut url) = url::Url::parse(raw) else {
        return "[redacted]".into();
    };
    if !matches!(url.scheme(), "http" | "https") {
        return format!("{}:[redacted]", url.scheme());
    }
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    let path = url.path().to_ascii_lowercase();
    if [
        "/auth/",
        "/oauth",
        "/token",
        "/login",
        "/signin",
        "/sign-in",
        "/password",
        "/session",
    ]
    .iter()
    .any(|segment| path.contains(segment))
    {
        url.set_path("/[redacted-auth-path]");
    }
    let value = url.as_str();
    if value.len() > 2048 {
        "[redacted:long-url]".into()
    } else {
        value.to_owned()
    }
}

fn safe_network_error(error: &str) -> String {
    if error.len() <= 128
        && error.starts_with("net::ERR_")
        && error
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b':' | b'_'))
    {
        error.to_owned()
    } else {
        "[redacted error]".into()
    }
}

fn eligible_api_body(record: &NetworkRecord) -> bool {
    if !matches!(record.resource_type.as_deref(), Some("XHR" | "Fetch"))
        || record.failed.is_some()
        || record.url.contains("redacted-auth-path")
    {
        return false;
    }
    let mime = record
        .mime_type
        .as_deref()
        .or_else(|| {
            record
                .headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("content-type"))
                .map(|(_, value)| value.as_str())
        })
        .unwrap_or("")
        .to_ascii_lowercase();
    if !(mime.starts_with("application/json")
        || mime.contains("+json")
        || mime.starts_with("text/json")
        || mime.starts_with("text/plain"))
    {
        return false;
    }
    !record.headers.iter().any(|(key, value)| {
        key.eq_ignore_ascii_case("content-length")
            && value
                .parse::<usize>()
                .is_ok_and(|len| len > MAX_NETWORK_BODY_BYTES)
    })
}

fn is_sensitive_json_key(key: &str) -> bool {
    let compact: String = key
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    [
        "password",
        "passwd",
        "secret",
        "token",
        "apikey",
        "accesskey",
        "authorization",
        "credential",
        "cookie",
        "session",
        "privatekey",
        "jwt",
    ]
    .iter()
    .any(|word| compact.contains(word))
}

fn redact_json_values(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, entry) in map {
                if is_sensitive_json_key(key) {
                    *entry = Value::String("[REDACTED]".into());
                } else {
                    redact_json_values(entry);
                }
            }
        }
        Value::Array(values) => values.iter_mut().for_each(redact_json_values),
        Value::String(text) => {
            let lower = text.to_ascii_lowercase();
            if [
                "bearer ",
                "authorization:",
                "token=",
                "password=",
                "api_key=",
            ]
            .iter()
            .any(|marker| lower.contains(marker))
            {
                *text = "[REDACTED]".into();
            }
        }
        _ => {}
    }
}

fn sanitized_api_body(raw: &str, mime_type: Option<&str>) -> Option<String> {
    if raw.len() > MAX_NETWORK_BODY_BYTES {
        return None;
    }
    let mime = mime_type?.to_ascii_lowercase();
    if mime.contains("json") {
        let mut parsed: Value = serde_json::from_str(raw).ok()?;
        redact_json_values(&mut parsed);
        let serialized = serde_json::to_string(&parsed).ok()?;
        (serialized.len() <= MAX_NETWORK_BODY_BYTES).then_some(serialized)
    } else if mime.starts_with("text/plain") {
        // Plain text cannot be field-redacted safely; drop obvious credentials.
        let lower = raw.to_ascii_lowercase();
        if [
            "bearer ",
            "authorization",
            "set-cookie",
            "password",
            "secret",
            "token=",
            "api_key",
            "session=",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
        {
            None
        } else {
            Some(raw.to_owned())
        }
    } else {
        None
    }
}

pub fn detect_block(
    body: &str,
    status: Option<u16>,
    headers: &BTreeMap<String, String>,
) -> Option<BlockKind> {
    let lower = body.to_ascii_lowercase();
    if lower.contains("cf-chl-")
        || lower.contains("/cdn-cgi/challenge-platform/")
        || lower.contains("checking your browser before accessing")
        || (lower.contains("just a moment") && lower.contains("cloudflare"))
        || (status == Some(403)
            && headers.iter().any(|(key, value)| {
                key.eq_ignore_ascii_case("server")
                    && value.to_ascii_lowercase().contains("cloudflare")
            }))
    {
        return Some(BlockKind::CloudflareChallenge);
    }
    if lower.contains("g-recaptcha")
        || lower.contains("h-captcha")
        || lower.contains("verify you are human")
        || lower.contains("hcaptcha.com/1/api.js")
    {
        return Some(BlockKind::Captcha);
    }
    None
}

async fn read_debugging_endpoint(
    profile: &Path,
    child: &mut Child,
) -> Result<String, BrowserError> {
    let path = profile.join("DevToolsActivePort");
    for _ in 0..120 {
        if let Ok(contents) = tokio::fs::read_to_string(&path).await {
            let mut lines = contents.lines();
            if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                if port.parse::<u16>().is_ok() && path.starts_with("/devtools/browser/") {
                    return Ok(format!("ws://127.0.0.1:{port}{path}"));
                }
            }
        }
        if child
            .try_wait()
            .map_err(|e| BrowserError::Io(e.to_string()))?
            .is_some()
        {
            return Err(BrowserError::Io(
                "Chrome exited before opening a CDP port".into(),
            ));
        }
        sleep(Duration::from_millis(100)).await;
    }
    Err(BrowserError::Timeout("Chrome DevToolsActivePort".into()))
}

#[cfg(test)]
mod cdp_endpoint_tests {
    use super::check_cdp_endpoint;

    #[test]
    fn only_loopback_or_allowlisted_websocket_endpoints() {
        for ok in [
            "ws://127.0.0.1:9222/devtools/browser/x",
            "ws://localhost:9222/devtools/browser/x",
            "ws://[::1]:9222/devtools/browser/x",
            "wss://127.0.0.2:1/devtools/browser/x",
        ] {
            assert!(check_cdp_endpoint(ok, None).is_ok(), "{ok}");
        }
        for bad in [
            "ws://10.0.0.5:9222/devtools/browser/x",
            "ws://browser.example:9222/devtools/browser/x",
            "http://127.0.0.1:9222/json/version",
            "ws://user:pass@127.0.0.1:9222/devtools/browser/x",
            "ws://127.0.0.1:9222/devtools/browser/x\n",
            "file:///tmp/socket",
            "not a url",
        ] {
            assert!(check_cdp_endpoint(bad, None).is_err(), "{bad}");
        }
        let allow = Some("browser.example, 10.0.0.5");
        assert!(check_cdp_endpoint("wss://Browser.Example/devtools/browser/x", allow).is_ok());
        assert!(check_cdp_endpoint("ws://10.0.0.5:9222/devtools/browser/x", allow).is_ok());
        assert!(check_cdp_endpoint("ws://10.0.0.6:9222/devtools/browser/x", allow).is_err());
        assert!(check_cdp_endpoint("ws://evil.browser.example/x", allow).is_err());
    }
}

#[cfg(test)]
mod state_file_tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rs-state-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn invalid(result: Result<BrowserState, BrowserError>) -> String {
        match result {
            Err(BrowserError::InvalidArgument(message)) => message,
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn malicious_state_files_are_refused() {
        let dir = dir("read");
        let valid = dir.join("ok.json");
        std::fs::write(
            &valid,
            r#"{"version":1,"cookies":[{"name":"a","value":"b"}]}"#,
        )
        .unwrap();
        assert_eq!(read_state_file(&valid).await.unwrap().cookies.len(), 1);

        // Oversized (sparse file, no 4 MiB write needed).
        let big = dir.join("big.json");
        std::fs::File::create(&big)
            .unwrap()
            .set_len(MAX_STATE_BYTES as u64 + 1)
            .unwrap();
        assert!(invalid(read_state_file(&big).await).contains("4 MiB"));
        // Deep nesting is a JSON error, not a stack overflow.
        let deep = dir.join("deep.json");
        std::fs::write(
            &deep,
            format!(
                r#"{{"version":1,"cookies":{}{}}}"#,
                "[".repeat(100_000),
                "]".repeat(100_000)
            ),
        )
        .unwrap();
        assert_eq!(
            invalid(read_state_file(&deep).await),
            "invalid browser state JSON"
        );
        // Wrong schema, garbage and IndexedDB payloads.
        for (name, body, message) in [
            (
                "schema.json",
                r#"{"version":"x","cookies":{}}"#,
                "invalid browser state JSON",
            ),
            (
                "garbage.json",
                "\u{0}not json",
                "invalid browser state JSON",
            ),
            (
                "idb.json",
                r#"{"version":2,"cookies":[],"origins":[{"origin":"https://a","indexedDB":[]}]}"#,
                "IndexedDB state is unsupported",
            ),
        ] {
            let path = dir.join(name);
            std::fs::write(&path, body).unwrap();
            assert_eq!(invalid(read_state_file(&path).await), message, "{name}");
        }
        // Directories and symlinks (even to a valid state) are refused.
        assert!(invalid(read_state_file(&dir).await).contains("regular file"));
        #[cfg(unix)]
        {
            let link = dir.join("link.json");
            std::os::unix::fs::symlink(&valid, &link).unwrap();
            assert!(invalid(read_state_file(&link).await).contains("regular file"));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn state_writes_are_private_and_never_overwrite_or_follow_links() {
        let dir = dir("write");
        let state = BrowserState {
            version: 1,
            cookies: vec![serde_json::json!({"name":"sid","value":"secret"})],
            origins: Vec::new(),
        };
        let path = dir.join("s.json");
        write_state_file(&path, &state).await.unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Existing file: refused, content unchanged.
        assert!(write_state_file(&path, &state).await.is_err());
        assert_eq!(read_state_file(&path).await.unwrap(), state);
        #[cfg(unix)]
        {
            // A dangling symlink pointing outside: refused, target not created.
            let target = dir.join("outside-target");
            let link = dir.join("link.json");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            assert!(write_state_file(&link, &state).await.is_err());
            assert!(!target.exists());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_history_is_bounded_and_old_ids_are_not_reused() {
        let mut capture = NetworkCapture::default();
        for number in 0..(MAX_NETWORK_RECORDS + 30) {
            capture.observe(&CdpEvent {
                method: "Network.responseReceived".into(),
                session_id: Some("session-a".into()),
                params: json!({
                    "requestId":format!("id-{number}"),
                    "type":"XHR",
                    "response":{"url":format!("https://example.test/api/{number}"),
                        "status":200,"mimeType":"application/json","headers":{}}
                }),
            });
            capture.observe(&CdpEvent {
                method: "Network.loadingFinished".into(),
                session_id: Some("session-a".into()),
                params: json!({"requestId":format!("id-{number}"),"encodedDataLength":12}),
            });
        }
        assert_eq!(capture.records.len(), MAX_NETWORK_RECORDS);
        assert_eq!(capture.ready.len(), MAX_NETWORK_RECORDS);
        assert!(!capture.finished.contains("id-0"));
        assert_eq!(
            capture.records.front().unwrap().request_id.as_deref(),
            Some("id-30")
        );
        capture.store_body("id-0", "should never attach to a later request".into());
        assert!(capture.records.iter().all(|record| record.body.is_none()));
    }

    #[test]
    fn aggregate_body_size_and_single_body_size_remain_bounded() {
        let mut capture = NetworkCapture::default();
        for number in 0..24 {
            let id = format!("id-{number}");
            capture.observe(&CdpEvent {
                method: "Network.responseReceived".into(),
                session_id: Some("session-a".into()),
                params: json!({"requestId":id.clone(),"type":"XHR","response":{
                    "url":"http://example.test/api","status":200,"mimeType":"text/plain"
                }}),
            });
            capture.store_body(&id, "a".repeat(60 * 1024));
        }
        let used: usize = capture
            .records
            .iter()
            .filter_map(|record| record.body.as_ref())
            .map(String::len)
            .sum();
        assert!(used <= MAX_NETWORK_TOTAL_BODY_BYTES);
        assert!(capture.records.iter().any(|record| record.body.is_none()));
        let id = capture.records.front().unwrap().request_id.clone().unwrap();
        capture.store_body(&id, "b".repeat(MAX_NETWORK_BODY_BYTES + 1));
        assert_eq!(
            capture
                .records
                .front()
                .unwrap()
                .body
                .as_ref()
                .unwrap()
                .len(),
            60 * 1024
        );
    }

    #[test]
    fn credential_values_in_json_and_urls_are_redacted() {
        let parsed = sanitized_api_body(
            r#"{"name":"safe","access_token":"abc","nested":{"password":"pass"},"note":"Bearer a-secret"}"#,
            Some("application/json"),
        ).unwrap();
        assert!(parsed.contains("safe"));
        assert!(!parsed.contains("abc"));
        assert!(!parsed.contains("\"pass\""));
        assert!(!parsed.contains("Bearer a-secret"));
        assert_eq!(
            redact_network_url("https://user:pw@example.test/api?q=secret#secret"),
            "https://example.test/api"
        );
        assert!(sanitized_api_body("Bearer secret", Some("text/plain")).is_none());
    }

    #[test]
    fn block_markers_are_specific() {
        assert_eq!(
            detect_block(
                "This article explains Cloudflare and its CDN",
                None,
                &BTreeMap::new()
            ),
            None
        );
        assert_eq!(
            detect_block("Just a moment... Cloudflare", Some(403), &BTreeMap::new()),
            Some(BlockKind::CloudflareChallenge)
        );
        assert_eq!(
            detect_block(
                "<script src='https://hcaptcha.com/1/api.js'></script>",
                None,
                &BTreeMap::new()
            ),
            Some(BlockKind::Captcha)
        );
    }
}
