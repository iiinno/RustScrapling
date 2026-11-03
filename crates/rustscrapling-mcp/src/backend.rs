//! MCP runtime using the integrated HTTP, extraction and CDP libraries.
//! Every unsupported upstream option is rejected instead of silently ignored.
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use rmcp::model::{CallToolResult, ContentBlock};
use rustscrapling_browser::{
    detect_block, BrowserAction, BrowserClient, BrowserPage, ElementState, LaunchOptions,
    LoadState, MouseButton, NetworkRecord, PageSettings, PointerTarget, UPSTREAM_LAUNCH_ARGS,
};
use rustscrapling_extract::compat::{convert_content, ContentFormat};
use rustscrapling_http::{HttpClient, HttpOptions, HttpRequest, HttpResponse};
use serde_json::{json, Map, Value};
use tokio::{sync::Mutex, time::sleep};
use uuid::Uuid;

#[derive(Debug)]
pub struct BackendError {
    pub kind: String,
    pub message: String,
    pub hint: String,
}
type Outcome = Result<CallToolResult, BackendError>;

impl BackendError {
    fn new(kind: &str, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
            hint: "Check tool arguments and available backends".into(),
        }
    }
    fn unsupported(option: &str) -> Self {
        Self::new(
            "unsupported_option",
            format!("'{option}' cannot yet be honored by the Rust backend"),
        )
    }
}
impl From<rustscrapling_http::HttpError> for BackendError {
    fn from(error: rustscrapling_http::HttpError) -> Self {
        Self {
            kind: error.kind.as_str().into(),
            hint: error.hint().into(),
            message: error.message,
        }
    }
}
impl From<rustscrapling_browser::BrowserError> for BackendError {
    fn from(error: rustscrapling_browser::BrowserError) -> Self {
        let kind = match error {
            rustscrapling_browser::BrowserError::Timeout(_) => "timeout",
            rustscrapling_browser::BrowserError::InvalidArgument(_) => "invalid_arguments",
            rustscrapling_browser::BrowserError::Disconnected => "browser_disconnected",
            rustscrapling_browser::BrowserError::Io(_) => "browser_io",
            rustscrapling_browser::BrowserError::Protocol(_) => "browser_protocol",
        };
        Self::new(kind, error.to_string())
    }
}

struct Session {
    created_at: String,
    settings: Value,
    engine: Engine,
}

const MAX_NETWORK_RECORDS: usize = 128;
const MAX_NETWORK_BODY_BYTES: usize = 64 * 1024;
const MAX_NETWORK_TOTAL_BODY_BYTES: usize = 1024 * 1024;
// MCP JSON numbers must round-trip safely through JavaScript-based clients.
const MAX_SAFE_NETWORK_ID: u64 = (1_u64 << 53) - 1;

struct NetworkEntry {
    id: u64,
    record: NetworkRecord,
}

struct NetworkHistory {
    entries: VecDeque<NetworkEntry>,
    next_id: u64,
    body_bytes: usize,
    dropped_count: u64,
}

impl Default for NetworkHistory {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
            next_id: 1,
            body_bytes: 0,
            dropped_count: 0,
        }
    }
}

impl NetworkHistory {
    fn append(
        &mut self,
        records: impl IntoIterator<Item = NetworkRecord>,
    ) -> Result<(), BackendError> {
        for mut record in records {
            let id = self.next_id;
            if id > MAX_SAFE_NETWORK_ID {
                return Err(BackendError::new(
                    "network_id_exhausted",
                    "Network request IDs exhausted",
                ));
            }
            self.next_id += 1;
            // BrowserPage is the redaction boundary. Do not persist larger bodies
            // even if a future producer relaxes its own per-record limit.
            if record
                .body
                .as_ref()
                .is_some_and(|body| body.len() > MAX_NETWORK_BODY_BYTES)
            {
                record.body = None;
            }
            let bytes = record.body.as_ref().map_or(0, String::len);
            while self.entries.len() >= MAX_NETWORK_RECORDS
                || self.body_bytes.saturating_add(bytes) > MAX_NETWORK_TOTAL_BODY_BYTES
            {
                self.evict_oldest();
            }
            self.body_bytes += bytes;
            self.entries.push_back(NetworkEntry { id, record });
        }
        Ok(())
    }

    fn evict_oldest(&mut self) {
        if let Some(entry) = self.entries.pop_front() {
            self.body_bytes -= entry.record.body.as_ref().map_or(0, String::len);
            self.dropped_count += 1;
        }
    }

    fn get(&self, id: u64) -> Option<&NetworkRecord> {
        self.entries
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| &entry.record)
    }
}

enum Engine {
    Http(HttpClient),
    Browser {
        client: Box<BrowserClient>,
        page: Box<BrowserPage>,
        last_url: Option<String>,
        network: NetworkHistory,
    },
}

#[derive(Default)]
pub struct RuntimeBackend {
    sessions: Mutex<HashMap<String, Arc<Mutex<Session>>>>,
}
impl RuntimeBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn call(&self, name: &str, args: Map<String, Value>) -> Outcome {
        match name {
            "make_request" => self.http_request(&args, None).await,
            "open_request_session" => self.open_http(&args).await,
            "session_make_request" => {
                let session = self.session(required(&args, "session_id")?).await?;
                let guard = session.lock().await;
                match &guard.engine {
                    Engine::Http(client) => self.http_request(&args, Some(client)).await,
                    _ => Err(BackendError::new(
                        "session_type",
                        "Expected an HTTP session",
                    )),
                }
            }
            "browser_fetch_once" => self.browser_once(&args).await,
            "browser_open" => self.browser_open(&args).await,
            "browser_fetch" => self.browser_fetch(&args).await,
            "browser_extract" => self.browser_extract(&args).await,
            "browser_evaluate" => self.browser_evaluate(&args).await,
            "browser_actions" => self.browser_actions(&args).await,
            "browser_network_requests" => self.network_list(&args).await,
            "browser_network_request" => self.network_request(&args).await,
            "list_sessions" => self.list_sessions().await,
            "close_session" => self.close_session(&args).await,
            "browser_save_state" => self.browser_save_state(&args).await,
            "browser_load_state" => self.browser_load_state(&args).await,
            "browser_screenshot" => self.browser_screenshot(&args).await,
            _ => Err(BackendError::new(
                "unknown_tool",
                format!("Unknown tool '{name}'"),
            )),
        }
    }

    async fn session(&self, id: &str) -> Result<Arc<Mutex<Session>>, BackendError> {
        self.sessions.lock().await.get(id).cloned().ok_or_else(|| {
            BackendError::new(
                "session_not_found",
                format!("Session '{id}' not found. Use list_sessions to see active sessions."),
            )
        })
    }
    async fn add_session(
        &self,
        args: &Map<String, Value>,
        engine: Engine,
        settings: Value,
        ty: &str,
    ) -> Outcome {
        let id = args
            .get("session_id")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| Uuid::new_v4().simple().to_string()[..12].to_owned());
        let mut sessions = self.sessions.lock().await;
        if sessions.contains_key(&id) {
            return Err(BackendError::new(
                "duplicate_session",
                format!("Session '{id}' already exists. Use a different ID or close the existing session first."),
            ));
        }
        sessions.insert(
            id.clone(),
            Arc::new(Mutex::new(Session {
                created_at: chrono::Utc::now()
                    .to_rfc3339_opts(chrono::SecondsFormat::Micros, false),
                settings,
                engine,
            })),
        );
        Ok(structured(json!({"session_id":id,"session_type":ty})))
    }
    async fn list_sessions(&self) -> Outcome {
        let entries: Vec<(String, Arc<Mutex<Session>>)> = self
            .sessions
            .lock()
            .await
            .iter()
            .map(|(name, s)| (name.clone(), s.clone()))
            .collect();
        let mut results = Vec::with_capacity(entries.len());
        for (id, entry) in entries {
            let s = entry.lock().await;
            results.push(json!({"session_id":id,"session_type":match s.engine {Engine::Http(_) => "static", _ => "stealthy"},
                "created_at":s.created_at,"is_alive":true,"settings":s.settings}));
        }
        // Upstream lists sessions in creation order and wraps the list as
        // `{"result": [...]}` (its declared outputSchema).
        results.sort_by(|a, b| {
            (a["created_at"].as_str(), a["session_id"].as_str())
                .cmp(&(b["created_at"].as_str(), b["session_id"].as_str()))
        });
        Ok(structured(json!({ "result": results })))
    }
    async fn close_session(&self, args: &Map<String, Value>) -> Outcome {
        let id = required(args, "session_id")?;
        let mut sessions = self.sessions.lock().await;
        let entry = sessions.get(id).ok_or_else(|| {
            BackendError::new(
                "session_not_found",
                format!("Session '{id}' not found. Use list_sessions to see active sessions."),
            )
        })?;
        if Arc::strong_count(entry) > 1 {
            return Err(BackendError::new(
                "session_busy",
                "Session has active operations; retry after they finish",
            ));
        }
        let entry = sessions.remove(id).expect("session checked above");
        drop(sessions);
        let session = Arc::try_unwrap(entry)
            .map_err(|_| BackendError::new("session_busy", "Session has active operations"))?
            .into_inner();
        if let Engine::Browser { client, page, .. } = session.engine {
            let closed = (*page).close().await;
            (*client).shutdown().await?;
            closed?;
        }
        Ok(structured(
            json!({"session_id":id,"message":format!("Session '{id}' closed successfully.")}),
        ))
    }

    async fn open_http(&self, args: &Map<String, Value>) -> Outcome {
        allow_defaults(args, &[("impersonate", json!("chrome"))])?;
        let proxy = opt_str(args, "proxy")?.map(str::to_owned);
        let options = HttpOptions {
            follow_redirects: false,
            proxy: proxy.clone(),
            ..HttpOptions::default()
        };
        let client = HttpClient::new(options)?;
        self.add_session(
            args,
            Engine::Http(client),
            json!({"impersonate":"chrome","proxy":proxy}),
            "static",
        )
        .await
    }

    async fn http_request(
        &self,
        args: &Map<String, Value>,
        session_client: Option<&HttpClient>,
    ) -> Outcome {
        allow_defaults(
            args,
            &[
                ("impersonate", json!("chrome")),
                ("stealthy_headers", json!(true)),
                ("timeout", json!(30)),
                ("max_redirects", json!(30)),
                ("retries", json!(3)),
                ("retry_delay", json!(1)),
                ("verify", json!(true)),
                ("http3", json!(false)),
                ("follow_redirects", json!("safe")),
            ],
        )?;
        for key in ["auth", "proxy_auth"] {
            reject_some(args, key)?;
        }
        if session_client.is_some() {
            reject_some(args, "proxy")?;
        }
        let url = url::Url::parse(required(args, "url")?)
            .map_err(|_| BackendError::new("invalid_arguments", "Invalid URL"))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(BackendError::new(
                "invalid_arguments",
                "Only HTTP/HTTPS URLs are supported",
            ));
        }
        let mut url = url;
        if let Some(params) = args.get("params").filter(|v| !v.is_null()) {
            let map = params.as_object().ok_or_else(|| {
                BackendError::new("invalid_arguments", "params must be an object")
            })?;
            for (key, value) in map {
                let v = scalar(value)
                    .ok_or_else(|| BackendError::unsupported("params with nested values"))?;
                url.query_pairs_mut().append_pair(key, &v);
            }
        }
        let method = args.get("method").and_then(Value::as_str).unwrap_or("GET");
        let mut req = HttpRequest::new(method, url.to_string());
        if let Some(headers) = args.get("headers").filter(|v| !v.is_null()) {
            for (key, val) in headers.as_object().ok_or_else(|| {
                BackendError::new("invalid_arguments", "headers must be an object")
            })? {
                if let Some(v) = val.as_str() {
                    req = req.header(key, v);
                } else if !val.is_null() {
                    return Err(BackendError::new(
                        "invalid_arguments",
                        "header values must be strings or null",
                    ));
                }
            }
        }
        if let Some(cookies) = args.get("cookies").filter(|v| !v.is_null()) {
            if req.headers.keys().any(|k| k.eq_ignore_ascii_case("cookie")) {
                return Err(BackendError::unsupported("cookies with Cookie header"));
            }
            let mut pairs = Vec::new();
            for (key, val) in cookies
                .as_object()
                .ok_or_else(|| BackendError::new("invalid_arguments", "cookies must be object"))?
            {
                let v = val.as_str().ok_or_else(|| {
                    BackendError::new("invalid_arguments", "cookies must be strings")
                })?;
                if key.chars().any(|ch| matches!(ch, ';' | '\r' | '\n'))
                    || v.chars().any(|ch| matches!(ch, ';' | '\r' | '\n'))
                {
                    return Err(BackendError::new(
                        "invalid_arguments",
                        "cookie contains control delimiter",
                    ));
                }
                pairs.push(format!("{key}={v}"));
            }
            if !pairs.is_empty() {
                req = req.header("Cookie", pairs.join("; "));
            }
        }
        if method != "GET" {
            if let Some(json_body) = args.get("json").filter(|v| !v.is_null()) {
                if args.get("data").is_some_and(|v| !v.is_null()) {
                    return Err(BackendError::unsupported("json and data together"));
                }
                req = req
                    .header("Content-Type", "application/json")
                    .body(json_body.to_string().into_bytes());
            } else if let Some(data) = args.get("data").filter(|v| !v.is_null()) {
                if let Some(s) = data.as_str() {
                    req = req.body(s.as_bytes().to_vec());
                } else if let Some(obj) = data.as_object() {
                    let mut form = url::form_urlencoded::Serializer::new(String::new());
                    for (k, v) in obj {
                        form.append_pair(
                            k,
                            v.as_str()
                                .ok_or_else(|| BackendError::unsupported("form data values"))?,
                        );
                    }
                    req = req
                        .header("Content-Type", "application/x-www-form-urlencoded")
                        .body(form.finish().into_bytes());
                }
            }
        }
        let one_off;
        let client = if let Some(client) = session_client {
            client
        } else {
            let settings = HttpOptions {
                follow_redirects: false,
                proxy: opt_str(args, "proxy")?.map(str::to_owned),
                ..HttpOptions::default()
            };
            one_off = HttpClient::new(settings)?;
            &one_off
        };
        let response = client.request(req).await?;
        if (300..400).contains(&response.status)
            && args.get("follow_redirects") != Some(&json!(false))
        {
            return Err(BackendError::new(
                "redirect_policy_unavailable",
                format!(
                    "HTTP {} redirect: safe redirect verification is not integrated",
                    response.status
                ),
            ));
        }
        render_http(response, args)
    }

    async fn browser_client(args: &Map<String, Value>) -> Result<BrowserClient, BackendError> {
        reject_nonempty_object(args, "additional_args")?;
        let path = opt_str(args, "executable_path")?
            .map(PathBuf::from)
            .or_else(default_chrome);
        if let Some(cdp) = opt_str(args, "cdp_url")? {
            // Upstream: launch settings do not apply to an existing browser.
            return Ok(BrowserClient::connect(cdp).await?);
        }
        let path = path.ok_or_else(|| {
            BackendError::new(
                "browser_unavailable",
                "Chrome executable not found; set SCRAPLING_EXECUTABLE_PATH",
            )
        })?;
        let mut launch = LaunchOptions::new(path);
        launch.headless = opt_bool(args, "headless", true)?;
        match args.get("proxy").filter(|value| !value.is_null()) {
            None => {}
            Some(Value::String(proxy)) => launch.proxy = Some(proxy.clone()),
            Some(Value::Object(proxy)) => {
                if proxy.get("username").is_some() || proxy.get("password").is_some() {
                    return Err(BackendError::unsupported("authenticated browser proxy"));
                }
                let server = proxy.get("server").and_then(Value::as_str).ok_or_else(|| {
                    BackendError::new("invalid_arguments", "proxy dictionary needs 'server'")
                })?;
                launch.proxy = Some(server.to_owned());
            }
            Some(_) => {
                return Err(BackendError::new(
                    "invalid_arguments",
                    "proxy must be a URL or a dictionary",
                ))
            }
        }
        launch
            .extra_args
            .extend(UPSTREAM_LAUNCH_ARGS.iter().map(|arg| (*arg).to_owned()));
        if opt_bool(args, "block_webrtc", false)? {
            launch.extra_args.extend([
                "--webrtc-ip-handling-policy=disable_non_proxied_udp".to_owned(),
                "--force-webrtc-ip-handling-policy".to_owned(),
            ]);
        }
        if !opt_bool(args, "allow_webgl", true)? {
            launch.extra_args.extend([
                "--disable-webgl".to_owned(),
                "--disable-webgl-image-chromium".to_owned(),
                "--disable-webgl2".to_owned(),
            ]);
        }
        if opt_bool(args, "hide_canvas", false)? {
            launch
                .extra_args
                .push("--fingerprinting-canvas-image-data-noise".to_owned());
        }
        Ok(BrowserClient::launch(launch).await?)
    }

    /// Session-level page settings shared by `browser_open` and
    /// `browser_fetch_once` (upstream `StealthySession` options).
    async fn open_configured_page(
        client: &BrowserClient,
        args: &Map<String, Value>,
    ) -> Result<BrowserPage, BackendError> {
        let page = client.open_page(true).await?;
        let cookies = match args.get("cookies").filter(|value| !value.is_null()) {
            None => Vec::new(),
            Some(Value::Array(cookies)) => cookies.clone(),
            Some(_) => {
                return Err(BackendError::new(
                    "invalid_arguments",
                    "cookies must be a list of cookie objects",
                ))
            }
        };
        let settings = PageSettings {
            locale: opt_str(args, "locale")?.map(str::to_owned),
            timezone_id: opt_str(args, "timezone_id")?.map(str::to_owned),
            user_agent: opt_str(args, "useragent")?.map(str::to_owned),
            // The headless UA fix applies only to browsers launched here;
            // an external CDP browser keeps its own identity.
            headless: opt_bool(args, "headless", true)? && opt_str(args, "cdp_url")?.is_none(),
            cookies,
            viewport: opt_str(args, "cdp_url")?.is_none().then_some((1920, 1080)),
        };
        if let Err(error) = page.apply_settings(&settings).await {
            let _ = page.close().await;
            return Err(error.into());
        }
        Ok(page)
    }

    async fn browser_open(&self, args: &Map<String, Value>) -> Outcome {
        let client = Self::browser_client(args).await?;
        let page = match Self::open_configured_page(&client, args).await {
            Ok(page) => page,
            Err(error) => {
                let _ = client.shutdown().await;
                return Err(error);
            }
        };
        let mut settings = Map::new();
        for key in [
            "headless",
            "real_chrome",
            "timezone_id",
            "locale",
            "useragent",
            "cdp_url",
            "hide_canvas",
            "block_webrtc",
            "allow_webgl",
        ] {
            if let Some(value) = args.get(key).filter(|v| !v.is_object() && !v.is_array()) {
                settings.insert(key.to_owned(), value.clone());
            }
        }
        self.add_session(
            args,
            Engine::Browser {
                client: Box::new(client),
                page: Box::new(page),
                last_url: None,
                network: NetworkHistory::default(),
            },
            Value::Object(settings),
            "stealthy",
        )
        .await
    }
    async fn browser_once(&self, args: &Map<String, Value>) -> Outcome {
        browser_options(args, true)?;
        let client = Self::browser_client(args).await?;
        let result = async {
            let page = Self::open_configured_page(&client, args).await?;
            let fetched = fetch_on_page(&page, args).await;
            let _ = page.close().await;
            let fetched = fetched?;
            check_challenge(&fetched.body, fetched.status)?;
            render_browser(&fetched.body, &fetched.final_url, fetched.status, args)
        }
        .await;
        client.shutdown().await?;
        result
    }
    async fn browser_fetch(&self, args: &Map<String, Value>) -> Outcome {
        browser_options(args, false)?;
        let session = self.session(required(args, "session_id")?).await?;
        let mut session = session.lock().await;
        let Engine::Browser {
            page,
            last_url,
            network,
            ..
        } = &mut session.engine
        else {
            return Err(BackendError::new(
                "session_type",
                "Expected a browser session",
            ));
        };
        let fetched = fetch_on_page(page, args).await?;
        *last_url = Some(fetched.final_url.clone());
        let (body, final_url, status, captured) = (
            fetched.body,
            fetched.final_url,
            fetched.status,
            fetched.network,
        );
        network.append(captured)?;
        check_challenge(&body, status)?;
        if opt_str(args, "extraction_type")? == Some("snapshot") {
            let snapshot = page
                .aria_snapshot(
                    opt_str(args, "css_selector")?,
                    None,
                    true,
                    DEFAULT_PAGE_TIMEOUT_MS,
                )
                .await?;
            return Ok(structured(
                json!({"status":status.unwrap_or(0),"url":final_url,"content":[snapshot]}),
            ));
        }
        render_browser(&body, &final_url, status, args)
    }
    async fn browser_extract(&self, args: &Map<String, Value>) -> Outcome {
        let format = opt_str(args, "extraction_type")?.unwrap_or("snapshot");
        let target = opt_str(args, "target")?;
        let pattern = match opt_str(args, "search")? {
            None => None,
            Some(_) if format != "snapshot" => {
                return Err(BackendError::new(
                    "invalid_arguments",
                    "search requires snapshot output.",
                ))
            }
            Some(search) => Some(regex::Regex::new(search).map_err(|error| {
                BackendError::new(
                    "invalid_arguments",
                    format!("Invalid search regex: {error}"),
                )
            })?),
        };
        let depth = match args.get("depth").filter(|value| !value.is_null()) {
            None => None,
            Some(value) => Some(value.as_i64().ok_or_else(|| {
                BackendError::new("invalid_arguments", "'depth' must be an integer")
            })?),
        };
        let boxes = opt_bool(args, "boxes", true)?;
        let s = self.session(required(args, "session_id")?).await?;
        let s = s.lock().await;
        let Engine::Browser { page, last_url, .. } = &s.engine else {
            return Err(BackendError::new(
                "session_type",
                "Expected browser session",
            ));
        };
        if last_url.is_none() {
            return Err(BackendError::new("no_page", "Call browser_fetch first"));
        }
        if format == "snapshot" {
            let snapshot = page
                .aria_snapshot(target, depth, boxes, DEFAULT_PAGE_TIMEOUT_MS)
                .await?;
            return Ok(text(match pattern {
                Some(pattern) => search_snapshot(&snapshot, &pattern),
                None => snapshot,
            }));
        }
        let html = match target {
            Some(target) => {
                page.element_outer_html(target, DEFAULT_PAGE_TIMEOUT_MS)
                    .await?
            }
            None => page.source().await?,
        };
        let url = page
            .current_url()
            .await
            .unwrap_or_else(|_| last_url.clone().unwrap_or_default());
        let _ = url;
        let chunks = render_chunks(
            &html,
            format,
            None,
            opt_bool(args, "main_content_only", true)?,
        )?;
        Ok(text(chunks.concat()))
    }
    async fn browser_evaluate(&self, args: &Map<String, Value>) -> Outcome {
        let expression = required(args, "expression")?;
        let arg = args.get("arg").filter(|value| !value.is_null());
        let isolated = opt_bool(args, "isolated_context", true)?;
        let s = self.session(required(args, "session_id")?).await?;
        let s = s.lock().await;
        let Engine::Browser { page, last_url, .. } = &s.engine else {
            return Err(BackendError::new(
                "session_type",
                "Expected browser session",
            ));
        };
        if last_url.is_none() {
            return Err(BackendError::new("no_page", "Call browser_fetch first"));
        }
        let value = page.evaluate_expression(expression, arg, isolated).await?;
        // Upstream: json.dumps(..., ensure_ascii=False, separators=(",", ":")).
        Ok(text(value.to_string()))
    }
    async fn browser_actions(&self, args: &Map<String, Value>) -> Outcome {
        let raw = args
            .get("actions")
            .and_then(Value::as_array)
            .ok_or_else(|| BackendError::new("invalid_arguments", "actions must be an array"))?;
        if raw.is_empty() {
            return Err(BackendError::new(
                "invalid_arguments",
                "actions must contain at least one action",
            ));
        }
        if raw.len() > 1000 {
            return Err(BackendError::new(
                "invalid_arguments",
                "browser_actions supports at most 1000 sequential actions",
            ));
        }
        // Upstream validates every action target before reserving the page.
        let actions = raw
            .iter()
            .enumerate()
            .map(|(offset, action)| parse_action(offset + 1, action))
            .collect::<Result<Vec<_>, _>>()?;
        let slowly = opt_bool(args, "slowly", false)?;
        let session = self.session(required(args, "session_id")?).await?;
        let s = session.lock().await;
        let Engine::Browser { page, last_url, .. } = &s.engine else {
            return Err(BackendError::new(
                "session_type",
                "Expected browser session",
            ));
        };
        if last_url.is_none() {
            return Err(BackendError::new("no_page", "Call browser_fetch first"));
        }
        page.run_actions(&actions, slowly).await.map_err(|error| {
            BackendError::new(
                if error.timed_out {
                    "timeout"
                } else {
                    "action_failed"
                },
                error.to_string(),
            )
        })?;
        Ok(text("Actions completed.".into()))
    }
    async fn browser_save_state(&self, args: &Map<String, Value>) -> Outcome {
        let path = state_file_path(required(args, "path")?)?;
        let session = self.session(required(args, "session_id")?).await?;
        let session = session.lock().await;
        let Engine::Browser {
            client,
            page,
            last_url,
            ..
        } = &session.engine
        else {
            return Err(BackendError::new(
                "session_type",
                "Expected browser session",
            ));
        };
        if last_url.is_none() {
            return Err(BackendError::new(
                "no_page",
                "Call browser_fetch before saving localStorage state",
            ));
        }
        client.save_state_file_for_page(page, &path).await?;
        Ok(structured(
            json!({"saved":true,"path":required(args,"path")?,"contents":"cookies_and_active_origin_local_storage","note":"Only the active top-level origin is saved; IndexedDB is unsupported"}),
        ))
    }
    async fn browser_load_state(&self, args: &Map<String, Value>) -> Outcome {
        let path = state_file_path(required(args, "path")?)?;
        let session = self.session(required(args, "session_id")?).await?;
        let session = session.lock().await;
        let Engine::Browser { client, page, .. } = &session.engine else {
            return Err(BackendError::new(
                "session_type",
                "Expected browser session",
            ));
        };
        client.restore_state_file_for_page(page, &path).await?;
        Ok(structured(
            json!({"loaded":true,"path":required(args,"path")?,"contents":"cookies_and_active_origin_local_storage","note":"v1 files restore cookies only; IndexedDB is unsupported; reload page to use restored state"}),
        ))
    }
    async fn browser_screenshot(&self, args: &Map<String, Value>) -> Outcome {
        let format = opt_str(args, "image_type")?.unwrap_or("png");
        if !matches!(format, "png" | "jpeg") {
            return Err(BackendError::new(
                "invalid_arguments",
                "image_type must be png or jpeg",
            ));
        }
        let quality = if args.get("quality").is_some_and(|v| !v.is_null()) {
            if format != "jpeg" {
                return Err(BackendError::new(
                    "invalid_arguments",
                    "'quality' is only valid when 'image_type' is 'jpeg'.",
                ));
            }
            let value = number(args, "quality", 0.0)?;
            if value > 100.0 || value.fract() != 0.0 {
                return Err(BackendError::new(
                    "invalid_arguments",
                    "quality must be an integer 0-100",
                ));
            }
            Some(value as u8)
        } else {
            None
        };
        let selector = opt_str(args, "selector")?;
        let reference = opt_str(args, "ref")?;
        if selector.is_some() && reference.is_some() {
            return Err(BackendError::new(
                "invalid_arguments",
                "Provide either 'selector' or 'ref', not both.",
            ));
        }
        let full_page = opt_bool(args, "full_page", false)?;
        if full_page && (selector.is_some() || reference.is_some()) {
            return Err(BackendError::new(
                "invalid_arguments",
                "'full_page' cannot be combined with 'selector' or 'ref'.",
            ));
        }
        let timeout_ms = number(args, "timeout", 30000.0)?;
        let session = self.session(required(args, "session_id")?).await?;
        let session = session.lock().await;
        let Engine::Browser { page, last_url, .. } = &session.engine else {
            return Err(BackendError::new(
                "session_type",
                "Expected browser session",
            ));
        };
        if last_url.is_none() {
            return Err(BackendError::new("no_page", "Call browser_fetch first"));
        }
        let element = selector
            .map(str::to_owned)
            .or_else(|| reference.map(|reference| format!("aria-ref={reference}")));
        let encoded = match element {
            Some(element) => {
                page.element_screenshot(&element, format, quality, timeout_ms)
                    .await?
            }
            None => {
                let capture = page.screenshot(format, quality, full_page);
                match Duration::try_from_secs_f64(timeout_ms / 1000.0) {
                    Ok(limit) if timeout_ms > 0.0 => tokio::time::timeout(limit, capture)
                        .await
                        .map_err(|_| {
                        BackendError::new("timeout", format!("Timeout {timeout_ms}ms exceeded."))
                    })??,
                    _ => capture.await?,
                }
            }
        };
        let url = page
            .current_url()
            .await
            .unwrap_or_else(|_| last_url.clone().unwrap_or_default());
        Ok(CallToolResult::success(vec![
            ContentBlock::image(encoded, format!("image/{format}")),
            ContentBlock::text(url),
        ]))
    }
    async fn network_list(&self, args: &Map<String, Value>) -> Outcome {
        let after = network_integer(args, "after_id", Some(0), 0, MAX_SAFE_NETWORK_ID)?;
        let limit = network_integer(args, "limit", Some(50), 1, 200)? as usize;
        let url_pattern = opt_str(args, "url_pattern")?;
        if url_pattern.is_some_and(|value| value.len() > 2048) {
            return Err(BackendError::new(
                "invalid_arguments",
                "url_pattern is too long",
            ));
        }
        let pattern = url_pattern
            .map(regex::Regex::new)
            .transpose()
            .map_err(|error| {
                BackendError::new("invalid_arguments", format!("Invalid url_pattern: {error}"))
            })?;
        let include_static = opt_bool(args, "include_static", false)?;
        let session = self.session(required(args, "session_id")?).await?;
        let mut s = session.lock().await;
        let Engine::Browser { page, network, .. } = &mut s.engine else {
            return Err(BackendError::new(
                "session_type",
                "Expected browser session",
            ));
        };
        // take_network() uses the same CDP response-body capture as the page's
        // network_records()/search_network_bodies() APIs, then clears the page
        // buffer so repeated MCP reads never assign new IDs to the same event.
        network.append(page.take_network().await)?;
        let matches = |r: &NetworkRecord| {
            pattern.as_ref().is_none_or(|p| p.is_match(&r.url))
                && (include_static
                    || r.status.is_some_and(|status| status >= 400)
                    || r.resource_type.as_deref() == Some("XHR")
                    || r.resource_type.as_deref() == Some("Fetch"))
        };
        let filtered: Vec<_> = network
            .entries
            .iter()
            .filter(|entry| entry.id > after && matches(&entry.record))
            .take(limit)
            .map(|entry| network_info(entry.id, &entry.record))
            .collect();
        let next = filtered
            .last()
            .and_then(|v| v["id"].as_u64())
            .unwrap_or(after);
        let has_more = network
            .entries
            .iter()
            .any(|entry| entry.id > next && matches(&entry.record));
        Ok(structured(
            json!({"requests":filtered,"next_cursor":next,"has_more":has_more,"dropped_count":network.dropped_count}),
        ))
    }
    async fn network_request(&self, args: &Map<String, Value>) -> Outcome {
        let id = network_integer(args, "request_id", None, 1, MAX_SAFE_NETWORK_ID)?;
        let part = opt_str(args, "part")?.unwrap_or("summary");
        if !matches!(part, "summary" | "response_headers" | "response_body") {
            return Err(BackendError::new(
                "unsupported_operation",
                format!("'{part}' is not recorded by the CDP backend"),
            ));
        }
        let session = self.session(required(args, "session_id")?).await?;
        let mut s = session.lock().await;
        let Engine::Browser { page, network, .. } = &mut s.engine else {
            return Err(BackendError::new(
                "session_type",
                "Expected browser session",
            ));
        };
        network.append(page.take_network().await)?;
        let record = network.get(id).ok_or_else(|| {
            BackendError::new("not_found", "Network record was evicted or does not exist")
        })?;
        // Upstream NetworkRequestModel: {request_id, part, data, note}.
        let (data, note) = match part {
            "summary" => (network_info(id, record), Value::Null),
            "response_headers" => (
                json!(record.headers),
                // Only allow-listed, non-credential headers are recorded.
                json!("Headers are partial; full headers unavailable."),
            ),
            "response_body" => match &record.body {
                Some(body) => (json!(body), Value::Null),
                None => (
                    Value::Null,
                    json!("Body not captured (unsupported, binary, oversized, sensitive or unavailable)."),
                ),
            },
            _ => unreachable!("part validated above"),
        };
        match part {
            "summary" | "response_headers" | "response_body" => Ok(structured(
                json!({"request_id":id,"part":part,"data":data,"note":note}),
            )),
            _ => unreachable!("part validated above"),
        }
    }
}

/// Upstream `NetworkRequestInfo`: Playwright-style lowercase resource type
/// and an integer status (0 when the CDP layer saw no HTTP status).
fn network_info(id: u64, record: &NetworkRecord) -> Value {
    json!({
        "id": id,
        "url": record.url,
        "method": record.method.as_deref().unwrap_or("GET"),
        "resource_type": record.resource_type.as_deref().unwrap_or("other").to_ascii_lowercase(),
        "status": record.status.unwrap_or(0),
    })
}

/// State is a credential-bearing file. MCP callers can address only a safe
/// filename in a dedicated directory, never arbitrary host filesystem paths.
fn state_file_path(name: &str) -> Result<PathBuf, BackendError> {
    let stem = name.strip_suffix(".json").ok_or_else(|| {
        BackendError::new("invalid_state_path", "State filename must end in .json")
    })?;
    if stem.is_empty()
        || stem.len() > 64
        || !stem
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return Err(BackendError::new(
            "invalid_state_path",
            "Use a simple filename such as session.json (letters, digits, _ or -)",
        ));
    }
    let root = std::env::var_os("RUSTSCRAPLING_STATE_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(default_state_dir);
    if std::fs::symlink_metadata(&root).is_err() {
        std::fs::create_dir_all(&root)
            .map_err(|_| BackendError::new("state_io", "Cannot create browser state directory"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).map_err(
                |_| BackendError::new("state_io", "Cannot protect browser state directory"),
            )?;
        }
    }
    let metadata = std::fs::symlink_metadata(&root)
        .map_err(|_| BackendError::new("state_io", "Cannot inspect browser state directory"))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(BackendError::new(
            "invalid_state_path",
            "Browser state directory must not be a symlink",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(BackendError::new(
                "state_permissions",
                "Browser state directory must have private permissions (0700)",
            ));
        }
    }
    Ok(root.join(name))
}

fn default_state_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(data) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(data)
                .join("RustScrapling")
                .join("browser-states");
        }
    }
    #[cfg(unix)]
    {
        if let Some(data) = std::env::var_os("XDG_STATE_HOME") {
            return PathBuf::from(data)
                .join("rustscrapling")
                .join("browser-states");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(".local/state/rustscrapling/browser-states");
        }
    }
    std::env::temp_dir().join("rustscrapling-browser-states")
}

fn required<'a>(args: &'a Map<String, Value>, key: &str) -> Result<&'a str, BackendError> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| BackendError::new("invalid_arguments", format!("Missing '{key}'")))
}
fn opt_str<'a>(args: &'a Map<String, Value>, key: &str) -> Result<Option<&'a str>, BackendError> {
    match args.get(key).filter(|v| !v.is_null()) {
        None => Ok(None),
        Some(v) => v.as_str().map(Some).ok_or_else(|| {
            BackendError::new("invalid_arguments", format!("'{key}' must be string"))
        }),
    }
}
fn opt_bool(args: &Map<String, Value>, key: &str, default: bool) -> Result<bool, BackendError> {
    args.get(key).map_or(Ok(default), |v| {
        v.as_bool().ok_or_else(|| {
            BackendError::new("invalid_arguments", format!("'{key}' must be boolean"))
        })
    })
}

/// `SHADOW_SNAPSHOT_JS` from pinned Scrapling
/// `scrapling/engines/toolbelt/convertor.py` (BSD-3-Clause, see NOTICE).
const SHADOW_SNAPSHOT_JS: &str = r#"() => {
    if (!Array.from(document.querySelectorAll('*')).some(node => node.shadowRoot)) return null;
    const documentCopy = document.implementation.createHTMLDocument('');
    let roots = 0;
    function copy(node, pierce = true) {
        if (node.nodeType !== Node.ELEMENT_NODE) return node.cloneNode(true);
        const name = node.prefix ? `${node.prefix}:${node.localName}` : node.localName;
        const target = documentCopy.createElementNS(node.namespaceURI, name);
        for (const attr of node.attributes) target.setAttributeNS(attr.namespaceURI, attr.name, attr.value);
        if (pierce && node instanceof HTMLSlotElement && node.getRootNode() instanceof ShadowRoot) {
            const assigned = node.assignedNodes();
            for (const child of assigned.length ? assigned : node.childNodes) target.append(copy(child));
            return target;
        }
        if (pierce && node.shadowRoot) {
            roots++;
            const shadow = documentCopy.createElement('shadow-root');
            for (const child of node.shadowRoot.childNodes) shadow.append(copy(child));
            target.append(shadow);
        } else {
            const template = node instanceof HTMLTemplateElement;
            const source = template ? node.content : node;
            const destination = template ? target.content : target;
            for (const child of source.childNodes) destination.append(copy(child, pierce && !template));
        }
        return target;
    }
    const root = copy(document.documentElement);
    if (!roots) return null;
    document.adoptNode(root);
    const doctype = document.doctype ? new XMLSerializer().serializeToString(document.doctype) : '';
    return doctype + root.outerHTML;
}"#;

struct Fetched {
    body: String,
    final_url: String,
    status: Option<u16>,
    network: Vec<NetworkRecord>,
}

/// Navigate a configured page with the per-request upstream fetch options
/// (`google_search`, `extra_headers`, `blocked_domains`,
/// `disable_resources`, `network_idle`, `wait_selector`, `wait`,
/// `pierce_shadow`, `timeout`). Request-scoped settings are reset after.
async fn fetch_on_page(
    page: &BrowserPage,
    args: &Map<String, Value>,
) -> Result<Fetched, BackendError> {
    let url = required(args, "url")?;
    let timeout_ms = number(args, "timeout", 30000.0)?;
    let mut headers = BTreeMap::new();
    if let Some(extra) = args.get("extra_headers").filter(|value| !value.is_null()) {
        for (name, value) in extra.as_object().ok_or_else(|| {
            BackendError::new("invalid_arguments", "extra_headers must be an object")
        })? {
            let value = value.as_str().ok_or_else(|| {
                BackendError::new("invalid_arguments", "extra_headers values must be strings")
            })?;
            headers.insert(name.clone(), value.to_owned());
        }
    }
    // Chrome rejects a Referer in Network.setExtraHTTPHeaders, so — like
    // Playwright — it becomes the navigation referrer instead. Upstream's
    // `google_search` overrides any supplied referer.
    let google = opt_bool(args, "google_search", true)?;
    let supplied_referer = headers
        .keys()
        .find(|name| name.eq_ignore_ascii_case("referer"))
        .cloned()
        .and_then(|name| headers.remove(&name));
    let referrer = if google {
        Some("https://www.google.com/".to_owned())
    } else {
        supplied_referer
    };
    let blocked: Vec<String> = match args.get("blocked_domains").filter(|v| !v.is_null()) {
        None => Vec::new(),
        Some(value) => value
            .as_array()
            .ok_or_else(|| {
                BackendError::new("invalid_arguments", "blocked_domains must be a list")
            })?
            .iter()
            .map(|domain| domain.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                BackendError::new("invalid_arguments", "blocked_domains must contain strings")
            })?,
    };
    let disable_resources = opt_bool(args, "disable_resources", false)?;
    if !headers.is_empty() {
        page.set_extra_headers(&headers).await?;
    }
    if !blocked.is_empty() {
        page.set_blocked_domains(&blocked).await?;
    }
    if disable_resources {
        page.set_resources_disabled(true).await?;
    }
    let outcome = async {
        page.navigate_with_referrer(
            url,
            Duration::try_from_secs_f64(timeout_ms / 1000.0).unwrap_or(Duration::from_secs(30)),
            referrer.as_deref(),
        )
        .await?;
        // Upstream: both waits are best effort ("continue if the wait fails").
        if opt_bool(args, "network_idle", false)? {
            let _ = page
                .run_actions(
                    &[BrowserAction::WaitLoad {
                        state: LoadState::NetworkIdle,
                        timeout_ms,
                    }],
                    false,
                )
                .await;
        }
        if let Some(selector) = opt_str(args, "wait_selector")? {
            let state = match opt_str(args, "wait_selector_state")?.unwrap_or("attached") {
                "attached" => ElementState::Attached,
                "detached" => ElementState::Detached,
                "visible" => ElementState::Visible,
                "hidden" => ElementState::Hidden,
                other => {
                    return Err(BackendError::new(
                        "invalid_arguments",
                        format!("unknown wait_selector_state '{other}'"),
                    ))
                }
            };
            let _ = page
                .run_actions(
                    &[BrowserAction::WaitElement {
                        target: format!("{selector} >> nth=0"),
                        state,
                        timeout_ms,
                    }],
                    false,
                )
                .await;
        }
        let wait = number(args, "wait", 0.0)?;
        if wait > 0.0 {
            sleep(Duration::from_millis(wait as u64)).await;
        }
        // Upstream returns page.content() (no shadow DOM) unless
        // `pierce_shadow`, which serializes open shadow roots as
        // <shadow-root> elements via SHADOW_SNAPSHOT_JS.
        let pierced = if opt_bool(args, "pierce_shadow", false)? {
            page.evaluate_expression(SHADOW_SNAPSHOT_JS, None, true)
                .await
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
        } else {
            None
        };
        let body = match pierced {
            Some(body) => body,
            None => page.source_with_shadow(false).await?,
        };
        let final_url = page.current_url().await?;
        Ok::<_, BackendError>((body, final_url))
    }
    .await;
    // Request-scoped settings never leak into the next session request.
    if !headers.is_empty() {
        let _ = page.set_extra_headers(&BTreeMap::new()).await;
    }
    if !blocked.is_empty() {
        let _ = page.set_blocked_domains(&[]).await;
    }
    if disable_resources {
        let _ = page.set_resources_disabled(false).await;
    }
    let (body, final_url) = outcome?;
    let network = page.take_network().await;
    let status = network
        .iter()
        .rev()
        .find(|record| {
            record.url == final_url && record.resource_type.as_deref() == Some("Document")
        })
        .or_else(|| {
            network
                .iter()
                .rev()
                .find(|record| record.resource_type.as_deref() == Some("Document"))
        })
        .and_then(|record| record.status);
    Ok(Fetched {
        body,
        final_url,
        status,
        network,
    })
}

fn reject_nonempty_object(args: &Map<String, Value>, key: &str) -> Result<(), BackendError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(()),
        Some(Value::Object(map)) if map.is_empty() => Ok(()),
        Some(_) => Err(BackendError::unsupported(key)),
    }
}

/// Playwright's default action/navigation timeout used by upstream when a
/// tool exposes no timeout parameter.
const DEFAULT_PAGE_TIMEOUT_MS: f64 = 30_000.0;

/// Port of upstream `_search_snapshot`: keep matching lines, three lines of
/// context on each side and every ancestor line (by indentation), marking
/// omitted runs with `...`. Refs are never rewritten.
fn search_snapshot(snapshot: &str, pattern: &regex::Regex) -> String {
    let lines: Vec<&str> = if snapshot.is_empty() {
        Vec::new()
    } else {
        snapshot.split('\n').collect()
    };
    let matches: Vec<usize> = (0..lines.len())
        .filter(|&index| pattern.is_match(lines[index]))
        .collect();
    if matches.is_empty() {
        return String::new();
    }
    let mut retained = std::collections::BTreeSet::new();
    for &index in &matches {
        retained.extend(index.saturating_sub(3)..(index + 4).min(lines.len()));
    }
    let mut ancestors: Vec<(usize, usize)> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let indent = line.chars().count() - line.trim_start().chars().count();
        while ancestors.last().is_some_and(|(depth, _)| *depth >= indent) {
            ancestors.pop();
        }
        if retained.contains(&index) {
            retained.extend(ancestors.iter().map(|(_, parent)| *parent));
        }
        ancestors.push((indent, index));
    }
    let mut output: Vec<&str> = Vec::new();
    let mut previous: Option<usize> = None;
    for &index in &retained {
        if index > previous.map_or(0, |p| p + 1) {
            output.push("...");
        }
        output.push(lines[index]);
        previous = Some(index);
    }
    if previous.is_some_and(|p| p + 1 < lines.len()) {
        output.push("...");
    }
    output.join("\n")
}

/// Convert one upstream `BrowserAction` JSON object (already validated
/// against the pinned schema) into the browser engine's action type, with
/// the same cross-field checks and messages as upstream `_validate_actions`.
fn parse_action(index: usize, raw: &Value) -> Result<BrowserAction, BackendError> {
    let invalid = |message: String| BackendError::new("invalid_arguments", message);
    let action = raw
        .as_object()
        .ok_or_else(|| invalid(format!("Action {index} must be an object.")))?;
    let kind = action
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("Action {index} has no valid type.")))?;
    let field = |key: &str| action.get(key).filter(|value| !value.is_null());
    let float = |key: &str, default: f64, min: Option<f64>| -> Result<f64, BackendError> {
        let value = match field(key) {
            None => default,
            Some(value) => value.as_f64().ok_or_else(|| {
                invalid(format!(
                    "Action {index} ({kind}): '{key}' must be a finite number."
                ))
            })?,
        };
        if !value.is_finite() || min.is_some_and(|min| value < min) {
            return Err(invalid(format!(
                "Action {index} ({kind}): '{key}' is out of range."
            )));
        }
        Ok(value)
    };
    let positive = |key: &str| -> Result<u32, BackendError> {
        match field(key) {
            None => Ok(1),
            Some(value) => value
                .as_u64()
                .filter(|count| (1..=u64::from(u32::MAX)).contains(count))
                .map(|count| count as u32)
                .ok_or_else(|| {
                    invalid(format!(
                        "Action {index} ({kind}): '{key}' must be a positive integer."
                    ))
                }),
        }
    };
    let target = field("target").and_then(Value::as_str).map(str::to_owned);
    let timeout_ms = float("timeout", 30_000.0, Some(0.0))?;
    let needs_target = || {
        target
            .clone()
            .ok_or_else(|| invalid(format!("Action {index} ({kind}) needs 'target'.")))
    };
    let pointer = || -> Result<PointerTarget, BackendError> {
        let x = field("x");
        let y = field("y");
        if target.is_none() == x.is_none() || x.is_none() != y.is_none() {
            return Err(invalid(format!(
                "Action {index} ({kind}) needs 'target' or both 'x' and 'y', not both."
            )));
        }
        Ok(match &target {
            Some(selector) => PointerTarget::Selector(selector.clone()),
            None => PointerTarget::Point {
                x: float("x", 0.0, None)?,
                y: float("y", 0.0, None)?,
            },
        })
    };
    let state = |key: &str| field(key).and_then(Value::as_str);
    Ok(match kind {
        "move" => BrowserAction::Move {
            target: pointer()?,
            timeout_ms,
            steps: positive("steps")?,
        },
        "click" => BrowserAction::Click {
            target: pointer()?,
            timeout_ms,
            button: match state("button").unwrap_or("left") {
                "left" => MouseButton::Left,
                "right" => MouseButton::Right,
                "middle" => MouseButton::Middle,
                other => return Err(invalid(format!("Action {index} (click): unknown button '{other}'."))),
            },
            click_count: positive("click_count")?,
            delay_ms: float("delay", 0.0, Some(0.0))?,
        },
        "wheel" => BrowserAction::Wheel {
            delta_x: float("delta_x", 0.0, None)?,
            delta_y: float("delta_y", 0.0, None)?,
        },
        "textbox" => BrowserAction::Textbox {
            target: needs_target()?,
            value: field("value")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid(format!("Action {index} (textbox): 'value' must be a string.")))?
                .to_owned(),
            clear: match field("clear") {
                None => true,
                Some(value) => value.as_bool().ok_or_else(|| {
                    invalid(format!("Action {index} (textbox): 'clear' must be boolean."))
                })?,
            },
            timeout_ms,
        },
        "checkbox" => BrowserAction::Checkbox {
            target: needs_target()?,
            checked: field("value").and_then(Value::as_bool).ok_or_else(|| {
                invalid(format!("Action {index} (checkbox): 'value' must be boolean."))
            })?,
            timeout_ms,
        },
        "radio" => {
            if field("value") != Some(&Value::Bool(true)) {
                return Err(invalid(format!("Action {index} (radio): 'value' must be true.")));
            }
            BrowserAction::Radio {
                target: needs_target()?,
                timeout_ms,
            }
        }
        "combobox" => BrowserAction::Combobox {
            target: needs_target()?,
            labels: match field("value") {
                Some(Value::String(label)) => vec![label.clone()],
                Some(Value::Array(labels)) => labels
                    .iter()
                    .map(|label| label.as_str().map(str::to_owned))
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(|| {
                        invalid(format!("Action {index} (combobox): labels must be strings."))
                    })?,
                _ => {
                    return Err(invalid(format!(
                        "Action {index} (combobox): 'value' must be a label or a list of labels."
                    )))
                }
            },
            timeout_ms,
        },
        "press_key" => BrowserAction::PressKey {
            key: field("key")
                .and_then(Value::as_str)
                .filter(|key| !key.is_empty())
                .ok_or_else(|| invalid(format!("Action {index} (press_key): 'key' is required.")))?
                .to_owned(),
        },
        "dialog" => BrowserAction::Dialog {
            accept: field("accept").and_then(Value::as_bool).ok_or_else(|| {
                invalid(format!("Action {index} (dialog): 'accept' must be boolean."))
            })?,
            prompt_text: field("prompt_text").and_then(Value::as_str).map(str::to_owned),
        },
        "wait_time" => BrowserAction::WaitTime {
            milliseconds: match field("milliseconds") {
                Some(_) => float("milliseconds", 0.0, Some(0.0))?,
                None => {
                    return Err(invalid(format!(
                        "Action {index} (wait_time): 'milliseconds' is required."
                    )))
                }
            },
        },
        "wait_element" => BrowserAction::WaitElement {
            target: needs_target()?,
            state: match state("state").unwrap_or("visible") {
                "attached" => ElementState::Attached,
                "detached" => ElementState::Detached,
                "visible" => ElementState::Visible,
                "hidden" => ElementState::Hidden,
                other => return Err(invalid(format!("Action {index} (wait_element): unknown state '{other}'."))),
            },
            timeout_ms,
        },
        "wait_load" => BrowserAction::WaitLoad {
            state: match state("state") {
                Some("domcontentloaded") => LoadState::DomContentLoaded,
                Some("load") => LoadState::Load,
                Some("networkidle") => LoadState::NetworkIdle,
                _ => {
                    return Err(invalid(format!(
                        "Action {index} (wait_load): 'state' must be domcontentloaded, load or networkidle."
                    )))
                }
            },
            timeout_ms,
        },
        other => return Err(invalid(format!("Action {index} has unknown type '{other}'."))),
    })
}

/// Network cursors and request IDs are integers, never floating-point values.
/// Keep them within the exact JSON integer range of common MCP clients.
fn network_integer(
    args: &Map<String, Value>,
    key: &str,
    default: Option<u64>,
    min: u64,
    max: u64,
) -> Result<u64, BackendError> {
    let number = match args.get(key) {
        Some(value) => value.as_u64(),
        None => default,
    };
    number
        .filter(|number| (min..=max).contains(number))
        .ok_or_else(|| {
            BackendError::new(
                "invalid_arguments",
                format!("'{key}' must be an integer between {min} and {max}"),
            )
        })
}

fn number(args: &Map<String, Value>, key: &str, default: f64) -> Result<f64, BackendError> {
    let n = args
        .get(key)
        .map_or(default, |v| v.as_f64().unwrap_or(f64::NAN));
    if !n.is_finite() || n < 0.0 {
        return Err(BackendError::new(
            "invalid_arguments",
            format!("'{key}' must be a nonnegative finite number"),
        ));
    }
    Ok(n)
}
fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}
fn reject_some(args: &Map<String, Value>, key: &str) -> Result<(), BackendError> {
    if args.get(key).is_some_and(|v| !v.is_null()) {
        Err(BackendError::unsupported(key))
    } else {
        Ok(())
    }
}
fn allow_defaults(
    args: &Map<String, Value>,
    defaults: &[(&str, Value)],
) -> Result<(), BackendError> {
    for (key, expected) in defaults {
        if args
            .get(*key)
            .is_some_and(|v| !v.is_null() && v != expected)
        {
            return Err(BackendError::unsupported(key));
        }
    }
    Ok(())
}
fn structured(value: Value) -> CallToolResult {
    CallToolResult::structured(value)
}
fn text(value: String) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(value)])
}
fn render_http(response: HttpResponse, args: &Map<String, Value>) -> Outcome {
    let body = String::from_utf8_lossy(&response.body);
    let content = render_args(&body, args)?;
    Ok(structured(
        json!({"status":response.status,"url":response.final_url,"content":content}),
    ))
}
fn render_browser(
    html: &str,
    url: &str,
    status: Option<u16>,
    args: &Map<String, Value>,
) -> Outcome {
    let content = render_args(html, args)?;
    Ok(structured(
        json!({"status":status.unwrap_or(0),"url":url,"content":content}),
    ))
}
/// Upstream `_translate_response`: every chunk of `Convertor._extract_content`
/// (including its trailing empty chunk) with C0 control characters removed.
fn render_args(html: &str, args: &Map<String, Value>) -> Result<Vec<String>, BackendError> {
    let format = opt_str(args, "extraction_type")?.unwrap_or("markdown");
    let mut chunks = render_chunks(
        html,
        format,
        opt_str(args, "css_selector")?,
        opt_bool(args, "main_content_only", true)?,
    )?;
    chunks.push(String::new());
    Ok(chunks)
}
fn render_chunks(
    html: &str,
    format: &str,
    css_selector: Option<&str>,
    main_content_only: bool,
) -> Result<Vec<String>, BackendError> {
    let format = ContentFormat::parse(format).ok_or_else(|| {
        BackendError::new(
            "invalid_arguments",
            format!("Unknown extraction type: {format}"),
        )
    })?;
    let chunks = convert_content(html, format, css_selector, main_content_only)
        .map_err(|error| BackendError::new("invalid_arguments", error))?;
    Ok(chunks
        .into_iter()
        .map(|chunk| {
            chunk
                .chars()
                .filter(|&c| !matches!(c as u32, 0x00..=0x08 | 0x0b | 0x0c | 0x0e..=0x1f))
                .collect()
        })
        .collect())
}
fn check_challenge(body: &str, status: Option<u16>) -> Result<(), BackendError> {
    if let Some(kind) = detect_block(body, status, &BTreeMap::new()) {
        return Err(BackendError::new(
            "blocked",
            format!("Browser encountered a challenge: {kind:?}"),
        ));
    }
    Ok(())
}
fn browser_options(args: &Map<String, Value>, once: bool) -> Result<(), BackendError> {
    let _ = once;
    allow_defaults(args, &[("solve_cloudflare", json!(false))])
}
fn default_chrome() -> Option<PathBuf> {
    for var in [
        "SCRAPLING_EXECUTABLE_PATH",
        "RUSTSCRAPLING_CHROME_FOR_TESTING",
        "CHROME_FOR_TESTING",
        "RUSTSCRAPLING_CHROME",
    ] {
        if let Some(path) = std::env::var_os(var)
            .map(PathBuf::from)
            .filter(|p| p.is_file())
        {
            return Some(path);
        }
    }
    #[cfg(windows)]
    for candidate in [
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
    ] {
        let path = PathBuf::from(candidate);
        if path.is_file() {
            return Some(path);
        }
    }
    #[cfg(unix)]
    for candidate in [
        "/usr/bin/google-chrome",
        "/usr/bin/chromium",
        "/home/hatch/chrome-for-testing/chrome-linux64/chrome",
    ] {
        let path = PathBuf::from(candidate);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

#[cfg(test)]
mod network_tests {
    use super::*;

    fn record(index: usize, body: Option<String>) -> NetworkRecord {
        NetworkRecord {
            method: Some("GET".into()),
            url: format!("https://example.test/api/{index}"),
            status: Some(200),
            resource_type: Some("XHR".into()),
            headers: BTreeMap::new(),
            failed: None,
            request_id: Some(format!("cdp-{index}")),
            mime_type: Some("application/json".into()),
            body,
        }
    }

    #[test]
    fn request_ids_never_shift_and_eviction_count_is_exact() {
        let mut history = NetworkHistory::default();
        for index in 0..140 {
            history.append([record(index, None)]).unwrap();
        }
        assert_eq!(history.entries.len(), 128);
        assert_eq!(history.dropped_count, 12);
        assert_eq!(history.entries.front().unwrap().id, 13);
        assert_eq!(history.entries.back().unwrap().id, 140);
        assert!(history.get(1).is_none());
        assert_eq!(
            history.get(13).unwrap().request_id.as_deref(),
            Some("cdp-12")
        );
        history.append([record(140, None)]).unwrap();
        assert_eq!(history.entries.front().unwrap().id, 14);
        assert_eq!(history.entries.back().unwrap().id, 141);
        assert_eq!(history.dropped_count, 13);
    }

    #[test]
    fn total_body_limit_is_enforced_without_reassigning_ids() {
        let mut history = NetworkHistory::default();
        for index in 0..20 {
            history
                .append([record(index, Some("x".repeat(64 * 1024)))])
                .unwrap();
        }
        assert_eq!(history.body_bytes, 1024 * 1024);
        assert_eq!(history.entries.len(), 16);
        assert_eq!(history.dropped_count, 4);
        assert_eq!(history.entries.front().unwrap().id, 5);
        history
            .append([record(20, Some("x".repeat(MAX_NETWORK_BODY_BYTES + 1)))])
            .unwrap();
        assert_eq!(history.body_bytes, 1024 * 1024);
        assert!(history.get(21).unwrap().body.is_none());
    }

    #[test]
    fn snapshot_search_keeps_context_and_ancestors_like_upstream() {
        let snapshot = "- generic [ref=e1]:\n  - list [ref=e2]:\n    - listitem [ref=e3]: a\n    - listitem [ref=e4]: b\n    - listitem [ref=e5]: c\n    - listitem [ref=e6]: d\n    - listitem [ref=e7]: e\n    - listitem [ref=e8]: f\n    - listitem [ref=e9]: Target\n    - listitem [ref=e10]: h\n  - button \"Go\" [ref=e11]\n  - text: tail1\n  - text: tail2\n  - text: tail3\n  - text: tail4";
        let pattern = regex::Regex::new("Target").unwrap();
        assert_eq!(
            super::search_snapshot(snapshot, &pattern),
            "- generic [ref=e1]:\n  - list [ref=e2]:\n...\n    - listitem [ref=e6]: d\n    - listitem [ref=e7]: e\n    - listitem [ref=e8]: f\n    - listitem [ref=e9]: Target\n    - listitem [ref=e10]: h\n  - button \"Go\" [ref=e11]\n  - text: tail1\n..."
        );
        assert_eq!(
            super::search_snapshot(snapshot, &regex::Regex::new(r"e1\]").unwrap()),
            "- generic [ref=e1]:\n  - list [ref=e2]:\n    - listitem [ref=e3]: a\n    - listitem [ref=e4]: b\n..."
        );
        assert_eq!(
            super::search_snapshot(snapshot, &regex::Regex::new("nope").unwrap()),
            ""
        );
        assert_eq!(super::search_snapshot("", &pattern), "");
    }

    #[test]
    fn numeric_network_arguments_require_exact_bounded_integers() {
        let good = json!({"after_id":140,"limit":200,"request_id":MAX_SAFE_NETWORK_ID});
        let args = good.as_object().unwrap();
        assert_eq!(
            network_integer(args, "after_id", Some(0), 0, MAX_SAFE_NETWORK_ID).unwrap(),
            140
        );
        assert_eq!(
            network_integer(args, "limit", Some(50), 1, 200).unwrap(),
            200
        );
        assert_eq!(
            network_integer(args, "request_id", None, 1, MAX_SAFE_NETWORK_ID).unwrap(),
            MAX_SAFE_NETWORK_ID
        );
        for invalid in [
            json!(-1),
            json!(1.5),
            json!(null),
            json!("NaN"),
            json!(9007199254740992_u64),
            json!(1e100),
            json!(f64::NAN),
            json!(f64::INFINITY),
            json!(true),
        ] {
            let args = json!({"after_id":invalid});
            assert!(network_integer(
                args.as_object().unwrap(),
                "after_id",
                Some(0),
                0,
                MAX_SAFE_NETWORK_ID
            )
            .is_err());
        }
        for invalid in [json!(0), json!(-1), json!(201), json!(2.5), json!(u64::MAX)] {
            let args = json!({"limit":invalid});
            assert!(network_integer(args.as_object().unwrap(), "limit", Some(50), 1, 200).is_err());
        }
        assert!(network_integer(&Map::new(), "request_id", None, 1, MAX_SAFE_NETWORK_ID).is_err());
    }
}
