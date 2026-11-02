//! CLI lifecycle for the authenticated, loopback-only shared fetch service.
//! No token is ever placed in command-line arguments or stdout.

use crate::browser_adapter::CdpBackend;
use clap::{Args, Subcommand};
use rustscrapling_daemon::{DaemonClient, DaemonConfig, DaemonError, DaemonServer};
use rustscrapling_fetch::{FetchOptions, Fetcher};
use rustscrapling_http::{HttpClient, HttpOptions};
use serde_json::json;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

pub(crate) const STATE_FILE_ENV: &str = "RUSTSCRAPLING_DAEMON_STATE_FILE";

#[derive(Debug, Args)]
pub struct DaemonArgs {
    /// State file path; defaults to the private rustscrapling directory.
    #[arg(long, global = true)]
    state_file: Option<PathBuf>,
    #[command(subcommand)]
    action: Action,
}

#[derive(Debug, Subcommand)]
enum Action {
    /// Start in the background; print a non-secret status receipt.
    Start {
        /// Ignore proxy environment variables in the daemon fetcher.
        #[arg(long)]
        direct: bool,
    },
    /// Stop a running daemon using its authenticated local IPC.
    Stop,
    /// Show daemon health and shared-fetch counters (never the auth token).
    Status,
    /// Internal worker process; not intended for direct invocation.
    #[command(hide = true)]
    Serve {
        #[arg(long)]
        direct: bool,
    },
}

fn config(path: Option<PathBuf>) -> DaemonConfig {
    let mut cfg = DaemonConfig::default();
    cfg.state_file = path.unwrap_or_else(state_file);
    cfg
}

pub(crate) fn state_file() -> PathBuf {
    std::env::var_os(STATE_FILE_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| DaemonConfig::default().state_file)
}

pub(crate) fn typed_error(err: &DaemonError) -> String {
    // Backend error descriptions can contain URLs/credentials. Preserve the
    // typed kind and fixed hint, but never echo untrusted upstream text.
    let hint = match err.kind.as_str() {
        "unauthorized" => "Check the daemon state file and its access permissions",
        "timeout" | "io" => "Check whether the local daemon is still running",
        "invalid_request" => "Check the requested URL and HTTP method",
        "blocked" => "Try the browser acquisition tier",
        "browser_unavailable" | "browser" => "Configure a compatible Chrome browser",
        "state" | "permission" => "Check the private state file and directory",
        _ => "Check daemon configuration or use daemon status",
    };
    json!({"schema_version":1,"error":{"kind":err.kind,"message":format!("daemon {}",err.kind),"hint":hint}}).to_string()
}

pub fn run(args: DaemonArgs) -> Result<(), String> {
    let cfg = config(args.state_file);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all().build()
        .map_err(|_| json!({"schema_version":1,"error":{"kind":"runtime","message":"Tokio runtime initialization failed"}}).to_string())?;
    match args.action {
        Action::Start { direct } => runtime.block_on(start(cfg, direct)),
        Action::Status => runtime.block_on(async {
            let client = DaemonClient::discover(&cfg.state_file)
                .await
                .map_err(|e| typed_error(&e))?
                .ok_or_else(not_running)?;
            let status = client.status().await.map_err(|e| typed_error(&e))?;
            println!(
                "{}",
                json!({"schema_version":1,"daemon":{"running":true,"status":status}})
            );
            Ok(())
        }),
        Action::Stop => runtime.block_on(async {
            let client = DaemonClient::discover(&cfg.state_file)
                .await
                .map_err(|e| typed_error(&e))?
                .ok_or_else(not_running)?;
            client.stop().await.map_err(|e| typed_error(&e))?;
            println!("{}", json!({"schema_version":1,"daemon":{"stopping":true}}));
            Ok(())
        }),
        Action::Serve { direct } => runtime.block_on(serve(cfg, direct)),
    }
}

fn not_running() -> String {
    json!({"schema_version":1,"error":{"kind":"not_running","message":"No active local daemon","hint":"Run rustscrapling daemon start"}}).to_string()
}

async fn start(cfg: DaemonConfig, direct: bool) -> Result<(), String> {
    if DaemonClient::discover(&cfg.state_file)
        .await
        .map_err(|e| typed_error(&e))?
        .is_some()
    {
        return Err(json!({"schema_version":1,"error":{"kind":"already_running","message":"Daemon is already running","hint":"Use daemon status"}}).to_string());
    }
    if cfg.state_file.exists() {
        // Never delete an unknown stale state file automatically; it could
        // belong to a slow or temporarily inaccessible daemon.
        return Err(json!({"schema_version":1,"error":{"kind":"stale_state","message":"An inactive daemon state file exists","hint":"Inspect it before moving or deleting it"}}).to_string());
    }
    let executable =
        std::env::current_exe().map_err(|_| "Cannot locate daemon executable".to_owned())?;
    let mut command = Command::new(executable);
    command
        .arg("daemon")
        .arg("--state-file")
        .arg(&cfg.state_file)
        .arg("serve");
    if direct {
        command.arg("--direct");
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = command.spawn().map_err(|_| json!({"schema_version":1,"error":{"kind":"spawn","message":"Could not launch daemon process"}}).to_string())?;
    for _ in 0..50 {
        // Child creates the state file before it begins accepting. A read of
        // the partially written state file is a transient startup condition.
        let client = match DaemonClient::discover(&cfg.state_file).await {
            Ok(client) => client,
            Err(err) if matches!(err.kind.as_str(), "io" | "state") => None,
            Err(err) => return Err(typed_error(&err)),
        };
        if let Some(client) = client {
            let status = client.status().await.map_err(|e| typed_error(&e))?;
            // Do not print the state file contents or token.
            println!(
                "{}",
                json!({"schema_version":1,"daemon":{"started":true,"status":status}})
            );
            return Ok(());
        }
        if child
            .try_wait()
            .map_err(|_| "Could not query daemon process".to_owned())?
            .is_some()
        {
            return Err(json!({"schema_version":1,"error":{"kind":"startup","message":"Daemon exited before startup","hint":"Check the state directory permissions and configuration"}}).to_string());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(json!({"schema_version":1,"error":{"kind":"timeout","message":"Daemon did not become ready","hint":"Check daemon status and state directory permissions"}}).to_string())
}

async fn serve(cfg: DaemonConfig, direct: bool) -> Result<(), String> {
    let options = HttpOptions {
        direct,
        ..HttpOptions::default()
    };
    let http = HttpClient::new(options).map_err(|e| json!({"schema_version":1,"error":{"kind":e.kind.as_str(),"message":"Could not initialize daemon HTTP client","hint":"Check proxy and TLS configuration"}}).to_string())?;
    let mut fetcher = Fetcher::new(http, FetchOptions::default());
    if let Some(browser) = CdpBackend::new(None, None, None) {
        fetcher = fetcher.with_browser(browser);
    }
    let daemon = DaemonServer::start_with_fetcher(cfg, fetcher)
        .await
        .map_err(|e| typed_error(&e))?;
    daemon.wait().await.map_err(|e| typed_error(&e))
}
