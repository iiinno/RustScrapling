//! MCP runtime using the integrated HTTP, extraction and CDP libraries.
//! Every unsupported upstream option is rejected instead of silently ignored.
use std::{
    collections::{BTreeMap, HashMap},
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
enum Engine {
    Http(HttpClient),
    Browser {
        client: BrowserClient,
        page: BrowserPage,
        last_url: Option<String>,
        network: Vec<NetworkRecord>,
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
                let session = self.session(required(&args,"session_id")?).await?;
                let guard = session.lock().await;
                match &guard.engine {
                    Engine::Http(client) => self.http_request(&args, Some(client)).await,
                    _ => Err(BackendError::new("session_type", "Expected an HTTP session")),
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
            "browser_save_state" | "browser_load_state" => Err(BackendError::new("unsupported_operation",
                "Browser state currently supports cookies only; upstream requires cookies, localStorage and IndexedDB")),
            "browser_screenshot" => Err(BackendError::new("unsupported_operation",
                "The integrated CDP backend does not expose screenshot capture")),
            _ => Err(BackendError::new("unknown_tool", format!("Unknown tool '{name}'"))),
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
            let closed = page.close().await;
            client.shutdown().await?;
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
                client,
                page,
                last_url: None,
                network: vec![],
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
        network.extend(captured);
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
        if actions.iter().any(|action| action["type"] != "wait_time") {
            return Err(BackendError::new(
                "unsupported_operation",
                "This CDP backend has no mouse/keyboard/dialog action API",
            ));
        }
        let session = self.session(required(args, "session_id")?).await?;
        let s = session.lock().await;
        let Engine::Browser { last_url, .. } = &s.engine else {
            return Err(BackendError::new(
                "session_type",
                "Expected browser session",
            ));
        };
        if last_url.is_none() {
            return Err(BackendError::new("no_page", "Call browser_fetch first"));
        }
        for action in actions {
            let millis = action["milliseconds"].as_f64().unwrap_or(0.0);
            if millis > 60000.0 {
                return Err(BackendError::new(
                    "invalid_arguments",
                    "wait_time must be <= 60000ms",
                ));
            }
            sleep(Duration::from_millis(millis as u64)).await;
        }
        Ok(text(format!("Completed {} wait action(s)", actions.len())))
    }
    async fn network_list(&self, args: &Map<String, Value>) -> Outcome {
        let pattern = opt_str(args, "url_pattern")?
            .map(regex::Regex::new)
            .transpose()
            .map_err(|error| {
                BackendError::new("invalid_arguments", format!("Invalid url_pattern: {error}"))
            })?;
        let include_static = opt_bool(args, "include_static", false)?;
        let session = self.session(required(args, "session_id")?).await?;
        let s = session.lock().await;
        let Engine::Browser { network, .. } = &s.engine else {
            return Err(BackendError::new(
                "session_type",
                "Expected browser session",
            ));
        };
        let after = number(args, "after_id", 0.0)? as usize;
        let limit = number(args, "limit", 50.0)? as usize;
        let matches = |r: &NetworkRecord| {
            pattern.as_ref().is_none_or(|p| p.is_match(&r.url))
                && (include_static
                    || r.status.is_some_and(|status| status >= 400)
                    || r.resource_type.as_deref() == Some("XHR")
                    || r.resource_type.as_deref() == Some("Fetch"))
        };
        let filtered:Vec<_>=network.iter().enumerate().skip(after).filter(|(_,r)| matches(r))
            .take(limit).map(|(i,r)|json!({"id":i+1,"url":r.url,"status":r.status,"resource_type":r.resource_type})).collect();
        let next = filtered
            .last()
            .and_then(|v| v["id"].as_u64())
            .unwrap_or(after as u64);
        Ok(structured(
            json!({"requests":filtered,"next_cursor":next,"has_more":network.iter().skip(next as usize).any(matches),"dropped_count":0}),
        ))
    }
    async fn network_request(&self, args: &Map<String, Value>) -> Outcome {
        let session = self.session(required(args, "session_id")?).await?;
        let s = session.lock().await;
        let Engine::Browser { network, .. } = &s.engine else {
            return Err(BackendError::new(
                "session_type",
                "Expected browser session",
            ));
        };
        let id = number(args, "request_id", 0.0)? as usize;
        let record = network
            .get(id.checked_sub(1).ok_or_else(|| {
                BackendError::new("invalid_arguments", "request_id must be positive")
            })?)
            .ok_or_else(|| BackendError::new("not_found", "Network record not found"))?;
        let part = args
            .get("part")
            .and_then(Value::as_str)
            .unwrap_or("summary");
        match part {
            "summary" => Ok(structured(
                json!({"id":id,"url":record.url,"status":record.status,"resource_type":record.resource_type,"failed":record.failed}),
            )),
            "response_headers" => Ok(structured(
                json!({"id":id,"part":part,"headers":record.headers}),
            )),
            _ => Err(BackendError::new(
                "unsupported_operation",
                format!("'{part}' is not recorded by the CDP backend"),
            )),
        }
    }
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
    let body = std::str::from_utf8(&response.body)
        .map_err(|_| BackendError::new("decode", "Body is not UTF-8"))?;
    let format = args
        .get("extraction_type")
        .and_then(Value::as_str)
        .unwrap_or("markdown");
    let content = render_content(body, &response.final_url, format)?;
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
