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
    detect_block, BrowserClient, BrowserPage, LaunchOptions, NetworkRecord,
};
use rustscrapling_extract::{extract, ExtractOptions};
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
            BackendError::new("session_not_found", format!("Session '{id}' is not open"))
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
                format!("Session '{id}' already exists"),
            ));
        }
        sessions.insert(
            id.clone(),
            Arc::new(Mutex::new(Session {
                created_at: chrono::Utc::now().to_rfc3339(),
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
        results.sort_by(|a, b| a["session_id"].as_str().cmp(&b["session_id"].as_str()));
        Ok(structured(json!(results)))
    }
    async fn close_session(&self, args: &Map<String, Value>) -> Outcome {
        let id = required(args, "session_id")?;
        let mut sessions = self.sessions.lock().await;
        let entry = sessions
            .get(id)
            .ok_or_else(|| BackendError::new("session_not_found", "No session with this ID"))?;
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
            json!({"session_id":id,"message":"Session closed"}),
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
                ("main_content_only", json!(true)),
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
        for key in ["css_selector", "auth", "proxy_auth"] {
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
        let path = opt_str(args, "executable_path")?
            .map(PathBuf::from)
            .or_else(default_chrome);
        if let Some(cdp) = opt_str(args, "cdp_url")? {
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
        if let Some(proxy) = opt_str(args, "proxy")? {
            launch.proxy = Some(proxy.into());
        }
        Ok(BrowserClient::launch(launch).await?)
    }
    async fn browser_open(&self, args: &Map<String, Value>) -> Outcome {
        for key in [
            "timezone_id",
            "locale",
            "useragent",
            "cookies",
            "additional_args",
        ] {
            reject_some(args, key)?;
        }
        allow_defaults(
            args,
            &[
                ("real_chrome", json!(false)),
                ("hide_canvas", json!(false)),
                ("block_webrtc", json!(false)),
                ("allow_webgl", json!(true)),
            ],
        )?;
        let client = Self::browser_client(args).await?;
        let page = client.open_page(true).await?;
        let settings =
            json!({"headless":opt_bool(args,"headless",true)?,"cdp_url":opt_str(args,"cdp_url")?});
        self.add_session(
            args,
            Engine::Browser {
                client: Box::new(client),
                page: Box::new(page),
                last_url: None,
                network: NetworkHistory::default(),
            },
            settings,
            "stealthy",
        )
        .await
    }
    async fn browser_once(&self, args: &Map<String, Value>) -> Outcome {
        browser_options(args, true)?;
        let client = Self::browser_client(args).await?;
        let page = client.open_page(true).await?;
        page.navigate(
            required(args, "url")?,
            Duration::from_millis(number(args, "timeout", 30000.0)? as u64),
        )
        .await?;
        let wait = number(args, "wait", 0.0)?;
        if wait > 0.0 {
            sleep(Duration::from_millis(wait as u64)).await;
        }
        let body = page.source().await?;
        let final_url = page.current_url().await?;
        let events = page.take_network().await;
        let status = events
            .iter()
            .rev()
            .find(|record| {
                record.url == final_url && record.resource_type.as_deref() == Some("Document")
            })
            .or_else(|| {
                events
                    .iter()
                    .rev()
                    .find(|record| record.resource_type.as_deref() == Some("Document"))
            })
            .and_then(|record| record.status);
        let result = check_challenge(&body, status)
            .and_then(|_| render_browser(&body, &final_url, status, args));
        page.close().await?;
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
        let url = required(args, "url")?;
        page.navigate(
            url,
            Duration::from_millis(number(args, "timeout", 30000.0)? as u64),
        )
        .await?;
        let wait = number(args, "wait", 0.0)?;
        if wait > 0.0 {
            sleep(Duration::from_millis(wait as u64)).await;
        }
        let body = page.source().await?;
        let final_url = page.current_url().await?;
        *last_url = Some(final_url.clone());
        let captured = page.take_network().await;
        let status = captured
            .iter()
            .rev()
            .find(|record| {
                record.url == final_url && record.resource_type.as_deref() == Some("Document")
            })
            .or_else(|| {
                captured
                    .iter()
                    .rev()
                    .find(|record| record.resource_type.as_deref() == Some("Document"))
            })
            .and_then(|record| record.status);
        network.append(captured)?;
        check_challenge(&body, status)?;
        render_browser(&body, &final_url, status, args)
    }
    async fn browser_extract(&self, args: &Map<String, Value>) -> Outcome {
        for key in ["target", "search", "depth"] {
            reject_some(args, key)?;
        }
        allow_defaults(
            args,
            &[("boxes", json!(true)), ("main_content_only", json!(true))],
        )?;
        let format = args
            .get("extraction_type")
            .and_then(Value::as_str)
            .unwrap_or("snapshot");
        if format == "snapshot" {
            return Err(BackendError::new(
                "unsupported_operation",
                "ARIA snapshots and refs are not exposed by the current CDP backend",
            ));
        }
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
        let html = page.source().await?;
        let out = render_content(&html, last_url.as_deref().unwrap_or(""), format)?;
        Ok(text(out))
    }
    async fn browser_evaluate(&self, args: &Map<String, Value>) -> Outcome {
        reject_some(args, "arg")?;
        if opt_bool(args, "isolated_context", true)? {
            return Err(BackendError::unsupported("isolated_context=true"));
        }
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
        let value = page.evaluate(required(args, "expression")?).await?;
        Ok(text(value.to_string()))
    }
    async fn browser_actions(&self, args: &Map<String, Value>) -> Outcome {
        if opt_bool(args, "slowly", false)? {
            return Err(BackendError::unsupported("slowly=true"));
        }
        let actions = args
            .get("actions")
            .and_then(Value::as_array)
            .ok_or_else(|| BackendError::new("invalid_arguments", "actions must be an array"))?;
        if actions.len() > 128 {
            return Err(BackendError::new(
                "invalid_arguments",
                "browser_actions supports at most 128 sequential actions",
            ));
        }
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
        for (index, action) in actions.iter().enumerate() {
            let action = action.as_object().ok_or_else(|| {
                BackendError::new(
                    "invalid_arguments",
                    format!("action {} must be an object", index + 1),
                )
            })?;
            let action_type = action.get("type").and_then(Value::as_str).ok_or_else(|| {
                BackendError::new(
                    "invalid_arguments",
                    format!("action {} has no valid type", index + 1),
                )
            })?;
            let outcome: Result<(), BackendError> = async {
                match action_type {
                    "wait_time" => {
                        let millis = action_duration(action, "milliseconds", None, 60_000)?;
                        sleep(millis).await;
                        Ok(())
                    }
                    "click" => {
                        // Only CSS element targeting and one left click are
                        // implemented. Do not pretend that coordinate clicks,
                        // refs, right clicks, repeated clicks or delayed clicks
                        // were honored.
                        for key in ["x", "y"] {
                            reject_some(action, key)?;
                        }
                        if action.get("button").is_some_and(|v| v != "left")
                            || action
                                .get("click_count")
                                .is_some_and(|v| v.as_u64() != Some(1))
                            || action.get("delay").is_some_and(|v| v.as_f64() != Some(0.0))
                        {
                            return Err(BackendError::unsupported(
                                "click button/click_count/delay",
                            ));
                        }
                        let timeout = action_duration(action, "timeout", Some(30_000), 300_000)?;
                        let target = action_target(action)?;
                        tokio::time::timeout(timeout, page.click_css(target))
                            .await
                            .map_err(|_| BackendError::new("timeout", "click timed out"))??;
                        Ok(())
                    }
                    "textbox" => {
                        let clear = opt_bool(action, "clear", true)?;
                        let timeout = action_duration(action, "timeout", Some(30_000), 300_000)?;
                        let target = action_target(action)?;
                        let value =
                            action.get("value").and_then(Value::as_str).ok_or_else(|| {
                                BackendError::new(
                                    "invalid_arguments",
                                    "textbox value must be a string",
                                )
                            })?;
                        tokio::time::timeout(timeout, async {
                            if clear {
                                page.fill_text_css(target, value).await
                            } else {
                                page.type_text_css(target, value).await
                            }
                        })
                        .await
                        .map_err(|_| BackendError::new("timeout", "textbox timed out"))??;
                        Ok(())
                    }
                    "press_key" => {
                        let key = action.get("key").and_then(Value::as_str).ok_or_else(|| {
                            BackendError::new(
                                "invalid_arguments",
                                "press_key requires a key string",
                            )
                        })?;
                        page.press_key(key).await?;
                        Ok(())
                    }
                    _ => Err(BackendError::new(
                        "unsupported_operation",
                        format!("browser action '{action_type}' is not implemented"),
                    )),
                }
            }
            .await;
            if let Err(mut error) = outcome {
                error.message = format!("action {} ({action_type}): {}", index + 1, error.message);
                return Err(error);
            }
        }
        Ok(text(format!(
            "Completed {} browser action(s)",
            actions.len()
        )))
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
        reject_some(args, "selector")?;
        reject_some(args, "ref")?;
        let format = opt_str(args, "image_type")?.unwrap_or("png");
        if !matches!(format, "png" | "jpeg") {
            return Err(BackendError::new(
                "invalid_arguments",
                "image_type must be png or jpeg",
            ));
        }
        let quality = if args.get("quality").is_some_and(|v| !v.is_null()) {
            let value = number(args, "quality", 0.0)?;
            if format != "jpeg" || value > 100.0 || value.fract() != 0.0 {
                return Err(BackendError::new(
                    "invalid_arguments",
                    "quality is valid only for JPEG (integer 0-100)",
                ));
            }
            Some(value as u8)
        } else {
            None
        };
        let full_page = opt_bool(args, "full_page", false)?;
        let timeout_ms = number(args, "timeout", 30000.0)?;
        if timeout_ms > 300_000.0 {
            return Err(BackendError::new(
                "invalid_arguments",
                "Screenshot timeout cannot exceed 300000ms",
            ));
        }
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
        let capture = page.screenshot(format, quality, full_page);
        let encoded = if timeout_ms == 0.0 {
            capture.await?
        } else {
            tokio::time::timeout(Duration::from_millis(timeout_ms as u64), capture)
                .await
                .map_err(|_| BackendError::new("timeout", "Screenshot timed out"))??
        };
        Ok(CallToolResult::success(vec![
            ContentBlock::image(encoded, format!("image/{format}")),
            ContentBlock::text(
                json!({"url":last_url,"format":format,"full_page":full_page}).to_string(),
            ),
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
            .map(|entry| {
                json!({
                    "id":entry.id,"url":entry.record.url,"status":entry.record.status,
                    "resource_type":entry.record.resource_type
                })
            })
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
        match part {
            "summary" => Ok(structured(
                json!({"id":id,"url":record.url,"status":record.status,
                    "resource_type":record.resource_type,"failed":record.failed,
                    "mime_type":record.mime_type,"body_available":record.body.is_some()}),
            )),
            "response_headers" => Ok(structured(
                json!({"id":id,"part":part,"headers":record.headers}),
            )),
            "response_body" => Ok(structured(json!({
                "id":id,"part":part,"available":record.body.is_some(),
                "body":record.body,"mime_type":record.mime_type,
                "size_bytes":record.body.as_ref().map(String::len),
                "note":if record.body.is_some() {
                    "Text body captured by CDP with conservative credential redaction"
                } else {
                    "Body not captured (unsupported, binary, oversized, sensitive or unavailable)"
                }
            }))),
            _ => unreachable!("part validated above"),
        }
    }
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

fn action_target(args: &Map<String, Value>) -> Result<&str, BackendError> {
    args.get("target")
        .and_then(Value::as_str)
        .filter(|selector| !selector.trim().is_empty())
        .ok_or_else(|| {
            BackendError::new(
                "unsupported_operation",
                "only explicit nonempty CSS selector targets are supported",
            )
        })
}

fn action_duration(
    args: &Map<String, Value>,
    key: &str,
    default: Option<u64>,
    max_ms: u64,
) -> Result<Duration, BackendError> {
    let value = match args.get(key) {
        Some(value) => value.as_f64().ok_or_else(|| {
            BackendError::new(
                "invalid_arguments",
                format!("'{key}' must be a finite number"),
            )
        })?,
        None => default
            .map(|ms| ms as f64)
            .ok_or_else(|| BackendError::new("invalid_arguments", format!("missing '{key}'")))?,
    };
    if !value.is_finite() || value < 0.0 || value > max_ms as f64 {
        return Err(BackendError::new(
            "invalid_arguments",
            format!("'{key}' must be a finite number from 0 to {max_ms} milliseconds"),
        ));
    }
    Ok(Duration::from_secs_f64(value / 1000.0))
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
    let decoded = rustscrapling_fetch::ContentInput::from(&response)
        .extract(None)
        .map_err(|error| BackendError::new(error.kind.as_str(), error.message))?;
    let format = args
        .get("extraction_type")
        .and_then(Value::as_str)
        .unwrap_or("markdown");
    let content = render_content(&decoded.source, &response.final_url, format)?;
    Ok(structured(
        json!({"status":response.status,"url":response.final_url,"content":[content]}),
    ))
}
fn render_browser(
    html: &str,
    url: &str,
    status: Option<u16>,
    args: &Map<String, Value>,
) -> Outcome {
    let format = args
        .get("extraction_type")
        .and_then(Value::as_str)
        .unwrap_or("markdown");
    if format == "snapshot" {
        return Err(BackendError::new(
            "unsupported_operation",
            "ARIA snapshots are not exposed by CDP backend",
        ));
    }
    let content = render_content(html, url, format)?;
    Ok(structured(
        json!({"status":status.unwrap_or(0),"url":url,"content":[content]}),
    ))
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
fn render_content(html: &str, url: &str, format: &str) -> Result<String, BackendError> {
    let page = extract(
        html,
        ExtractOptions {
            page_url: Some(url),
            query: None,
        },
    )
    .map_err(|e| BackendError::new("parse", e))?;
    match format {
        "markdown" => Ok(page.markdown),
        "text" => Ok(page.text),
        "html" => Ok(page.html),
        _ => Err(BackendError::new(
            "invalid_arguments",
            "Unsupported extraction_type",
        )),
    }
}
fn browser_options(args: &Map<String, Value>, once: bool) -> Result<(), BackendError> {
    for key in [
        "css_selector",
        "extra_headers",
        "wait_selector",
        "cookies",
        "additional_args",
        "timezone_id",
        "locale",
        "useragent",
        "blocked_domains",
    ] {
        reject_some(args, key)?;
    }
    let mut defaults = vec![
        ("google_search", json!(true)),
        ("main_content_only", json!(true)),
        ("network_idle", json!(false)),
        ("disable_resources", json!(false)),
        ("solve_cloudflare", json!(false)),
        ("pierce_shadow", json!(false)),
        ("wait_selector_state", json!("attached")),
    ];
    if once {
        defaults.extend([
            ("real_chrome", json!(false)),
            ("hide_canvas", json!(false)),
            ("block_webrtc", json!(false)),
            ("allow_webgl", json!(true)),
        ]);
    } else {
        defaults.push(("load_dom", json!(true)));
    }
    allow_defaults(args, &defaults)
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
