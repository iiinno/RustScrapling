//! Browser fetching using Chromium DevTools Protocol (CDP).
//!
//! D-007: the stealth path sends commands directly through the WebSocket
//! transport with chromiumoxide_cdp 0.9 types. It never initializes
//! chromiumoxide's Page/FrameManager, which unconditionally enables Runtime.
//! Only the explicitly requested `evaluate` operation enables Runtime.

mod transport;

pub use transport::{BrowserError, CdpEvent};

use chromiumoxide_cdp::cdp::browser_protocol::{dom, target};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time::{sleep, timeout};
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

#[derive(Debug, Clone, Copy)]
pub struct BrowserGetOptions {
    pub navigation_timeout: Duration,
    pub capture_network: bool,
}

impl Default for BrowserGetOptions {
    fn default() -> Self {
        Self {
            navigation_timeout: Duration::from_secs(15),
            capture_network: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NetworkRecord {
    pub url: String,
    pub status: Option<u16>,
    pub resource_type: Option<String>,
    pub headers: BTreeMap<String, String>,
    pub failed: Option<String>,
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
        for arg in &options.extra_args {
            cmd.arg(arg);
        }
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut child = cmd.spawn().map_err(|e| BrowserError::Io(e.to_string()))?;
        let endpoint = match read_debugging_endpoint(&profile, &mut child).await {
            Ok(endpoint) => endpoint,
            Err(error) => {
                let _ = child.kill().await;
                let _ = tokio::fs::remove_dir_all(&profile).await;
                return Err(error);
            }
        };
        let connection = match Connection::connect(&endpoint).await {
            Ok(connection) => connection,
            Err(error) => {
                let _ = child.kill().await;
                let _ = tokio::fs::remove_dir_all(&profile).await;
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
    pub async fn connect(ws_endpoint: &str) -> Result<Self, BrowserError> {
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
        let page = BrowserPage {
            connection: self.connection.clone(),
            target_id: id,
            session_id,
            events: Mutex::new(events),
            network: Mutex::new(Vec::new()),
            network_enabled: capture_network,
        };
        page.raw("Page.enable", json!({})).await?;
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
            *page = Some(self.open_page(options.capture_network).await?);
        }
        let tab = page.as_ref().expect("initialized above");
        if options.capture_network && !tab.network_enabled {
            tab.raw("Network.enable", json!({})).await?;
        }
        tab.navigate(url, options.navigation_timeout).await?;
        let body = tab.source().await?;
        let final_url = tab.current_url().await.unwrap_or_else(|_| url.to_owned());
        let network = if options.capture_network {
            tab.take_network().await
        } else {
            Vec::new()
        };
        let response = network
            .iter()
            .rev()
            .find(|entry| {
                entry.resource_type.as_deref() == Some("Document") && entry.url == final_url
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

    /// Save cookies only. Reading localStorage would require Runtime and is
    /// intentionally outside implicit stealth operations.
    pub async fn save_state(&self) -> Result<BrowserState, BrowserError> {
        let result = self
            .connection
            .send("Storage.getCookies", json!({}), None)
            .await?;
        Ok(BrowserState {
            version: 1,
            cookies: result
                .get("cookies")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        })
    }

    pub async fn restore_state(&self, state: &BrowserState) -> Result<(), BrowserError> {
        if state.version != 1 {
            return Err(BrowserError::InvalidArgument(
                "unsupported browser state version".into(),
            ));
        }
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

    pub async fn save_state_file(&self, path: impl AsRef<Path>) -> Result<(), BrowserError> {
        let json = serde_json::to_vec_pretty(&self.save_state().await?)
            .map_err(|e| BrowserError::Io(e.to_string()))?;
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
        let path = path.as_ref();
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

    pub async fn restore_state_file(&self, path: impl AsRef<Path>) -> Result<(), BrowserError> {
        let path = path.as_ref();
        let metadata = tokio::fs::symlink_metadata(path)
            .await
            .map_err(|e| BrowserError::Io(e.to_string()))?;
        if !metadata.file_type().is_file() || metadata.len() > MAX_STATE_BYTES as u64 {
            return Err(BrowserError::InvalidArgument(
                "state must be a regular file under 4 MiB".into(),
            ));
        }
        let file = tokio::fs::File::open(path)
            .await
            .map_err(|e| BrowserError::Io(e.to_string()))?;
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
        let state: BrowserState = serde_json::from_slice(&bytes)
            .map_err(|_| BrowserError::InvalidArgument("invalid browser state JSON".into()))?;
        self.restore_state(&state).await
    }

    pub async fn shutdown(mut self) -> Result<(), BrowserError> {
        self.default_page.lock().await.take();
        if let Some(mut child) = self.owned_child.take() {
            let _ = child.kill().await;
            if let Some(profile) = self.owned_profile.take() {
                tokio::fs::remove_dir_all(profile)
                    .await
                    .map_err(|e| BrowserError::Io(e.to_string()))?;
            }
        }
        Ok(())
    }
}

impl Drop for BrowserClient {
    fn drop(&mut self) {
        if let Some(child) = &mut self.owned_child {
            let _ = child.start_kill();
        }
    }
}

pub struct BrowserPage {
    connection: Arc<Connection>,
    target_id: String,
    session_id: String,
    events: Mutex<tokio::sync::broadcast::Receiver<CdpEvent>>,
    network: Mutex<Vec<NetworkRecord>>,
    network_enabled: bool,
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
        self.network.lock().await.clear();
        let mut receiver = self.events.lock().await;
        while receiver.try_recv().is_ok() {} // discard prior-page events
        let result = self.raw("Page.navigate", json!({"url":url})).await?;
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
                        if event.method == "Network.responseReceived"
                            || event.method == "Network.loadingFailed"
                        {
                            self.network.lock().await.push(record_network(&event));
                        }
                    }
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => return Err(BrowserError::Disconnected),
                }
            }
        };
        timeout(navigation_timeout, wait)
            .await
            .map_err(|_| BrowserError::Timeout("Page.loadEventFired".into()))??;
        while let Ok(event) = receiver.try_recv() {
            if event.session_id.as_deref() == Some(&self.session_id)
                && matches!(
                    event.method.as_str(),
                    "Network.responseReceived" | "Network.loadingFailed"
                )
            {
                self.network.lock().await.push(record_network(&event));
            }
        }
        Ok(())
    }

    /// HTML source via DOM.getDocument/getOuterHTML, including open Shadow
    /// DOM where the browser protocol supports includeShadowDOM.
    pub async fn source(&self) -> Result<String, BrowserError> {
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
                    .include_shadow_dom(true)
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

    pub async fn take_network(&self) -> Vec<NetworkRecord> {
        std::mem::take(&mut *self.network.lock().await)
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
        let response = self.raw("Page.captureScreenshot", args).await?;
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
        .unwrap_or_default()
        .to_owned();
    let status = response
        .get("status")
        .and_then(Value::as_f64)
        .map(|v| v as u16);
    let headers = response
        .get("headers")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        value
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| value.to_string()),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    NetworkRecord {
        url,
        status,
        headers,
        resource_type: event
            .params
            .get("type")
            .and_then(Value::as_str)
            .map(str::to_owned),
        failed: event
            .params
            .get("errorText")
            .and_then(Value::as_str)
            .map(str::to_owned),
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
mod tests {
    use super::*;

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
