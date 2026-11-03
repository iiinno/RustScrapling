//! Stage 7 CLI: daemon-scoped request history and honest session capabilities.
use clap::{Args, Subcommand};
use rustscrapling_daemon::DaemonClient;
use serde_json::json;
use std::path::PathBuf;

use crate::daemon;

#[derive(Debug, Args)]
pub(crate) struct SessionArgs {
    #[command(subcommand)]
    action: SessionAction,
}

#[derive(Debug, Subcommand)]
enum SessionAction {
    /// Open a reusable browser session (requires persistent browser session hosting).
    Open {
        #[arg(long)]
        session_id: Option<String>,
    },
    /// Close a previously opened browser session.
    Close { session_id: String },
    /// List hosted browser sessions.
    List,
    /// Save cookies and storage state for an open browser session.
    SaveState { session_id: String, file: PathBuf },
    /// Load cookies and storage state into an open browser session.
    LoadState { session_id: String, file: PathBuf },
}

pub(crate) fn run_session(args: SessionArgs) -> Result<(), String> {
    // Each CLI command has a distinct process lifetime. The existing daemon
    // only hosts fetches; the MCP backend owns browser sessions in its process.
    // Returning an ID here would promise a browser context that no longer exists.
    let command = match args.action {
        SessionAction::Open { session_id } => {
            let _ = session_id;
            "open"
        }
        SessionAction::Close { session_id } => {
            let _ = session_id;
            "close"
        }
        SessionAction::List => "list",
        SessionAction::SaveState { session_id, file } => {
            let _ = (session_id, file);
            "save-state"
        }
        SessionAction::LoadState { session_id, file } => {
            let _ = (session_id, file);
            "load-state"
        }
    };
    Err(json!({
        "schema_version": 1,
        "error": {
            "kind": "unsupported",
            "message": format!("session {command} requires a persistent browser session host"),
            "hint": "Use browser session tools in a persistent MCP server process"
        }
    })
    .to_string())
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
