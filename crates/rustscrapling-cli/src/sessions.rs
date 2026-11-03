//! CLI sessions hosted by the daemon (same MCP executor as `rustscrapling mcp`)
//! and daemon-scoped request history.
use clap::{Args, Subcommand};
use rustscrapling_daemon::DaemonClient;
use serde_json::json;
use std::path::PathBuf;

use crate::daemon;

#[derive(Debug, Args)]
pub(crate) struct SessionArgs {
    /// Override the daemon discovery state file.
    #[arg(long, global = true)]
    state_file: Option<PathBuf>,
    #[command(subcommand)]
    action: SessionAction,
}

#[derive(Debug, Subcommand)]
enum SessionAction {
    /// Open a browser session (or `--http` session) hosted by the daemon.
    Open {
        #[arg(long)]
        session_id: Option<String>,
        /// Open an HTTP session (upstream `open_request_session`) instead.
        #[arg(long)]
        http: bool,
        /// Show the browser window.
        #[arg(long)]
        headed: bool,
        #[arg(long)]
        executable_path: Option<String>,
        #[arg(long)]
        proxy: Option<String>,
        #[arg(long)]
        cdp_url: Option<String>,
        #[arg(long)]
        locale: Option<String>,
        #[arg(long)]
        timezone_id: Option<String>,
        #[arg(long)]
        useragent: Option<String>,
    },
    /// Close a session and free its resources.
    Close { session_id: String },
    /// List daemon-hosted sessions.
    List,
    /// Save cookies and storage of a browser session (file name in the
    /// daemon's private state directory).
    SaveState { session_id: String, file: String },
    /// Restore cookies and storage into a browser session.
    LoadState { session_id: String, file: String },
    /// Fetch a URL in a browser session (upstream `browser_fetch`).
    Fetch {
        session_id: String,
        url: String,
        /// markdown, html, text or snapshot.
        #[arg(long, default_value = "markdown")]
        format: String,
        #[arg(long)]
        css_selector: Option<String>,
        #[arg(long)]
        wait_selector: Option<String>,
        #[arg(long)]
        network_idle: bool,
    },
    /// Send a request in an HTTP session (upstream `session_make_request`).
    Request {
        session_id: String,
        url: String,
        #[arg(long, default_value = "GET")]
        method: String,
        #[arg(long, default_value = "markdown")]
        format: String,
    },
    /// Call any of the 16 upstream MCP tools with JSON arguments.
    Tool {
        name: String,
        /// JSON object with the tool arguments.
        #[arg(long, default_value = "{}")]
        args: String,
    },
}

fn cli_error(kind: &str, message: impl Into<String>, hint: &str) -> String {
    json!({"schema_version":1,"error":{"kind":kind,"message":message.into(),"hint":hint}})
        .to_string()
}

/// Map a session subcommand to an upstream MCP tool call.
fn tool_call(
    action: SessionAction,
) -> Result<(String, serde_json::Map<String, serde_json::Value>), String> {
    let mut args = serde_json::Map::new();
    let mut put = |key: &str, value: serde_json::Value| {
        if !value.is_null() {
            args.insert(key.to_owned(), value);
        }
    };
    let name = match action {
        SessionAction::Open {
            session_id,
            http,
            headed,
            executable_path,
            proxy,
            cdp_url,
            locale,
            timezone_id,
            useragent,
        } => {
            put("session_id", json!(session_id));
            put("proxy", json!(proxy));
            if http {
                "open_request_session"
            } else {
                if headed {
                    put("headless", json!(false));
                }
                put("executable_path", json!(executable_path));
                put("cdp_url", json!(cdp_url));
                put("locale", json!(locale));
                put("timezone_id", json!(timezone_id));
                put("useragent", json!(useragent));
                "browser_open"
            }
        }
        SessionAction::Close { session_id } => {
            put("session_id", json!(session_id));
            "close_session"
        }
        SessionAction::List => "list_sessions",
        SessionAction::SaveState { session_id, file } => {
            put("session_id", json!(session_id));
            put("path", json!(file));
            "browser_save_state"
        }
        SessionAction::LoadState { session_id, file } => {
            put("session_id", json!(session_id));
            put("path", json!(file));
            "browser_load_state"
        }
        SessionAction::Fetch {
            session_id,
            url,
            format,
            css_selector,
            wait_selector,
            network_idle,
        } => {
            put("session_id", json!(session_id));
            put("url", json!(url));
            put("extraction_type", json!(format));
            put("css_selector", json!(css_selector));
            put("wait_selector", json!(wait_selector));
            if network_idle {
                put("network_idle", json!(true));
            }
            "browser_fetch"
        }
        SessionAction::Request {
            session_id,
            url,
            method,
            format,
        } => {
            put("session_id", json!(session_id));
            put("url", json!(url));
            put("method", json!(method.to_ascii_uppercase()));
            put("extraction_type", json!(format));
            "session_make_request"
        }
        SessionAction::Tool { name, args: raw } => {
            let parsed: serde_json::Value = serde_json::from_str(&raw).map_err(|error| {
                cli_error(
                    "invalid_arguments",
                    format!("--args is not JSON: {error}"),
                    "Pass a JSON object",
                )
            })?;
            let serde_json::Value::Object(map) = parsed else {
                return Err(cli_error(
                    "invalid_arguments",
                    "--args must be a JSON object",
                    "Pass a JSON object",
                ));
            };
            return Ok((name, map));
        }
    };
    Ok((name.to_owned(), args))
}

pub(crate) fn run_session(args: SessionArgs) -> Result<(), String> {
    let path = args.state_file.clone().unwrap_or_else(daemon::state_file);
    let (name, arguments) = tool_call(args.action)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| cli_error("runtime", "Tokio runtime initialization failed", ""))?;
    let result = runtime.block_on(async {
        let client = DaemonClient::discover(&path)
            .await
            .map_err(|e| daemon::typed_error(&e))?
            .ok_or_else(|| {
                cli_error(
                    "not_running",
                    "Sessions are hosted by the daemon, which is not running",
                    "Run rustscrapling daemon start",
                )
            })?;
        client
            .tool(name.clone(), arguments)
            .await
            .map_err(|e| daemon::typed_error(&e))
    })?;
    // Tool-level failures carry the executor's typed error JSON as text.
    if result.get("isError") == Some(&json!(true)) {
        let text = result
            .pointer("/content/0/text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let error = serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|value| value.get("error").cloned())
            .unwrap_or_else(|| json!({"kind":"tool_error","message":text,"hint":""}));
        return Err(json!({"schema_version":1,"tool":name,"error":error}).to_string());
    }
    let payload = result
        .get("structuredContent")
        .cloned()
        .unwrap_or_else(|| result.get("content").cloned().unwrap_or_default());
    println!(
        "{}",
        json!({"schema_version":1,"tool":name,"result":payload})
    );
    Ok(())
}

#[derive(Debug, Args)]
pub(crate) struct NetworkArgs {
    #[command(subcommand)]
    action: NetworkAction,
}

#[derive(Debug, Subcommand)]
enum NetworkAction {
    /// Search completed daemon-managed upstream fetches (not browser subrequests).
    Search {
        /// Match a substring of the sanitized request URL.
        #[arg(long, default_value = "")]
        url_contains: String,
        /// Maximum number of newest matches to return (1..100).
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Override the daemon discovery state file.
        #[arg(long)]
        state_file: Option<PathBuf>,
    },
}

pub(crate) fn run_network(args: NetworkArgs) -> Result<(), String> {
    match args.action {
        NetworkAction::Search {
            url_contains,
            limit,
            state_file,
        } => {
            if !(1..=100).contains(&limit) || url_contains.len() > 1024 {
                return Err(json!({
                    "schema_version":1,
                    "error":{
                        "kind":"invalid_request",
                        "message":"--limit must be 1..100 and --url-contains at most 1024 bytes",
                        "hint":"Use a smaller limit or URL filter"
                    }
                })
                .to_string());
            }
            let path = state_file.unwrap_or_else(daemon::state_file);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| {
                    json!({
                        "schema_version":1,
                        "error":{"kind":"runtime","message":"Tokio runtime initialization failed"}
                    })
                    .to_string()
                })?;
            let records = runtime.block_on(async {
                let client = DaemonClient::discover(&path)
                    .await
                    .map_err(|e| daemon::typed_error(&e))?
                    .ok_or_else(|| {
                        json!({
                            "schema_version":1,
                            "error":{
                                "kind":"not_running",
                                "message":"Network search requires a running daemon",
                                "hint":"Run rustscrapling daemon start"
                            }
                        })
                        .to_string()
                    })?;
                client
                    .network_search(url_contains, limit)
                    .await
                    .map_err(|e| daemon::typed_error(&e))
            })?;
            println!(
                "{}",
                json!({
                    "schema_version":1,
                    "network":{
                        "source":"daemon_fetch",
                        "in_memory":true,
                        "captures_browser_subrequests":false,
                        "records":records
                    }
                })
            );
            Ok(())
        }
    }
}
