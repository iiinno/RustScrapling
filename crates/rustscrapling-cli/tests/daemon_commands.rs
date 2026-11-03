//! Actual CLI sub-processes, a local HTTP origin, and the authenticated
//! background daemon. No remote network or browser needed.
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Fixture {
    addr: String,
    total: Arc<AtomicUsize>,
    conditional: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn serve_connection(mut stream: TcpStream, total: Arc<AtomicUsize>, conditional: Arc<AtomicUsize>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let mut received = Vec::new();
    let mut buffer = [0u8; 2048];
    while received.len() < 8192 && !received.windows(4).any(|w| w == b"\r\n\r\n") {
        let Ok(n) = stream.read(&mut buffer) else {
            return;
        };
        if n == 0 {
            return;
        }
        received.extend_from_slice(&buffer[..n]);
    }
    let req = String::from_utf8_lossy(&received);
    if !req.starts_with("GET ") {
        return;
    }
    total.fetch_add(1, Ordering::SeqCst);
    let is_conditional = req.lines().any(|line| {
        line.to_ascii_lowercase().starts_with("if-none-match:") && line.contains("\"v1\"")
    });
    let (status, body) = if is_conditional {
        conditional.fetch_add(1, Ordering::SeqCst);
        ("304 Not Modified", "")
    } else {
        (
            "200 OK",
            "<html><body><main><h1>daemon origin fixture</h1></main></body></html>",
        )
    };
    let reply = format!("HTTP/1.1 {status}\r\nContent-Type: text/html\r\nETag: \"v1\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    let _ = stream.write_all(reply.as_bytes());
}

impl Fixture {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let total = Arc::new(AtomicUsize::new(0));
        let conditional = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (counter, cond, flag) = (total.clone(), conditional.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            while !flag.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let (counter, cond) = (counter.clone(), cond.clone());
                        std::thread::spawn(move || serve_connection(stream, counter, cond));
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            addr,
            total,
            conditional,
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rustscrapling"))
}

fn command(args: &[&str], state: &std::path::Path) -> Output {
    let mut process = Command::new(binary());
    let mut child = process
        .args(args)
        .env("RUSTSCRAPLING_DAEMON_STATE_FILE", state)
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("ALL_PROXY")
        .env_remove("http_proxy")
        .env_remove("https_proxy")
        .env_remove("all_proxy")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn CLI test process");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if child.try_wait().expect("query CLI test process").is_some() {
            return child.wait_with_output().expect("collect CLI test output");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait_with_output();
            panic!("CLI process timed out for arguments: {args:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn json_ok(result: &Output) -> Value {
    assert!(
        result.status.success(),
        "stderr: {} stdout: {}",
        String::from_utf8_lossy(&result.stderr),
        String::from_utf8_lossy(&result.stdout)
    );
    serde_json::from_slice::<Value>(&result.stdout).expect("CLI outputs one JSON document")
}

fn error_kind(result: &Output) -> String {
    assert!(
        !result.status.success(),
        "unexpected successful CLI response: {}",
        String::from_utf8_lossy(&result.stdout)
    );
    let error: Value = serde_json::from_slice(&result.stderr)
        .expect("failed CLI process must return a JSON error on stderr");
    assert_eq!(error["schema_version"], 1);
    error["error"]["kind"]
        .as_str()
        .expect("typed CLI error.kind")
        .to_owned()
}

fn wait_for_stop(state: &std::path::Path) {
    for _ in 0..100 {
        if !state.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    panic!("daemon did not remove the token state file");
}

struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = command(&["daemon", "stop"], &self.0);
    }
}

#[test]
fn daemon_process_lifecycle_auto_ipc_and_fallback() {
    let fixture = Fixture::new();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rustscrapling-cli-daemon-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir(&root).unwrap();
    let state = root.join("private").join("daemon.json");
    let start = json_ok(&command(&["daemon", "start", "--direct"], &state));
    let _cleanup = Cleanup(state.clone());
    assert_eq!(start["daemon"]["started"], true);
    assert!(start["daemon"]["status"]["address"]
        .as_str()
        .unwrap()
        .starts_with("127.0.0.1:"));
    assert!(!start.to_string().contains("token"));
    assert!(state.exists());
    let duplicate = command(&["daemon", "start"], &state);
    assert!(!duplicate.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&duplicate.stderr).unwrap()["error"]["kind"],
        "already_running"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&state).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let url = format!("http://{}/fixture", fixture.addr);
    // A tampered state token must not trigger a direct-network fallback.
    let private = std::fs::read_to_string(&state).unwrap();
    let token = serde_json::from_str::<Value>(&private).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    std::fs::write(&state, private.replace(&token, &"0".repeat(64))).unwrap();
    let refused = command(&["get", &url], &state);
    // Restore authentication before any assertion can panic so the guard can
    // always shut down the child process.
    std::fs::write(&state, private).unwrap();
    assert!(!refused.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&refused.stderr).unwrap()["error"]["kind"],
        "unauthorized"
    );
    assert!(!String::from_utf8_lossy(&refused.stderr).contains(&token));
    assert_eq!(fixture.total.load(Ordering::SeqCst), 0);
    let first = json_ok(&command(&["get", &url], &state));
    assert_eq!(first["status"], 200);
    assert!(first["content"]["text"]
        .as_str()
        .unwrap()
        .contains("daemon origin fixture"));
    let second = json_ok(&command(&["get", &url], &state));
    assert_eq!(second["status"], 200);
    assert_eq!(second["content"], first["content"]);
    assert!(
        fixture.conditional.load(Ordering::SeqCst) >= 1,
        "conditional revalidation must pass through daemon"
    );
    let auto = json_ok(&command(&["auto", &url], &state));
    assert_eq!(auto["status"], 200);
    let status = json_ok(&command(&["daemon", "status"], &state));
    assert_eq!(status["daemon"]["running"], true);
    assert!(status["daemon"]["status"]["requests"].as_u64().unwrap() >= 3);
    // An explicit request override must not be ignored by the daemon.
    let before = status["daemon"]["status"]["requests"].as_u64().unwrap();
    let override_get = json_ok(&command(&["get", &url, "--direct"], &state));
    assert_eq!(override_get["status"], 200);
    let after = json_ok(&command(&["daemon", "status"], &state));
    assert_eq!(after["daemon"]["status"]["requests"], before);

    assert_eq!(
        json_ok(&command(&["daemon", "stop"], &state))["daemon"]["stopping"],
        true
    );
    for _ in 0..50 {
        if !state.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    assert!(!state.exists(), "daemon must remove state on stop");
    let stopped = command(&["daemon", "status"], &state);
    assert!(!stopped.status.success());
    let err: Value = serde_json::from_slice(&stopped.stderr).unwrap();
    assert_eq!(err["error"]["kind"], "not_running");
    let fallback = json_ok(&command(&["get", &url], &state));
    assert_eq!(fallback["status"], 200);
    assert!(fixture.total.load(Ordering::SeqCst) >= 4);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn daemon_rejects_non_loopback_state_without_echoing_token() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rustscrapling-cli-bad-state-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir(&root).unwrap();
    let state = root.join("state.json");
    let secret = "secret-token-value".repeat(4);
    std::fs::write(
        &state,
        serde_json::json!({
            "version":1, "address":"203.0.113.9:9222", "token":secret, "pid":1234,
        })
        .to_string(),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o644)).unwrap();
        let exposed = command(&["daemon", "status"], &state);
        assert!(!exposed.status.success());
        assert_eq!(
            serde_json::from_slice::<Value>(&exposed.stderr).unwrap()["error"]["kind"],
            "permission"
        );
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let outcome = command(&["daemon", "status"], &state);
    assert!(!outcome.status.success());
    let error: Value = serde_json::from_slice(&outcome.stderr).unwrap();
    assert_eq!(error["error"]["kind"], "state");
    assert!(!String::from_utf8_lossy(&outcome.stderr).contains(&secret));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn daemon_cli_restart_restores_disk_cache_and_learned_tier() {
    let fixture = Fixture::new();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rustscrapling-daemon-restart-{}-{stamp}",
        std::process::id()
    ));
    let state = root.join("private").join("daemon.json");
    let _cleanup = Cleanup(state.clone());
    let url = format!("http://{}/fixture", fixture.addr);

    let started = json_ok(&command(&["daemon", "start", "--direct"], &state));
    let first_pid = started["daemon"]["status"]["pid"].as_u64().unwrap();
    assert_eq!(started["daemon"]["started"], true);
    assert_eq!(json_ok(&command(&["get", &url], &state))["status"], 200);
    assert_eq!(json_ok(&command(&["auto", &url], &state))["status"], 200);
    let before = json_ok(&command(&["daemon", "status"], &state));
    assert!(
        before["daemon"]["status"]["learned_sites"]
            .as_u64()
            .unwrap()
            >= 1
    );
    assert!(
        before["daemon"]["status"]["cache_entries"]
            .as_u64()
            .unwrap()
            >= 1
    );
    let secret: Value = serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    let token = secret["token"].as_str().unwrap().to_owned();
    assert_eq!(token.len(), 64);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&state).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(
            std::fs::metadata(state.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    assert_eq!(
        json_ok(&command(&["daemon", "stop"], &state))["daemon"]["stopping"],
        true
    );
    wait_for_stop(&state);
    let db = root.join("private").join("daemon.json.sqlite3");
    assert!(db.exists(), "cache database survives daemon shutdown");
    let disk = std::fs::read(&db).unwrap();
    assert!(
        !disk
            .windows(token.len())
            .any(|bytes| bytes == token.as_bytes()),
        "daemon auth token must never be persisted in SQLite"
    );

    let restarted = json_ok(&command(&["daemon", "start", "--direct"], &state));
    assert_eq!(restarted["daemon"]["started"], true);
    assert_ne!(
        restarted["daemon"]["status"]["pid"].as_u64().unwrap(),
        first_pid
    );
    assert!(
        restarted["daemon"]["status"]["cache_entries"]
            .as_u64()
            .unwrap()
            >= 1
    );
    assert!(
        restarted["daemon"]["status"]["learned_sites"]
            .as_u64()
            .unwrap()
            >= 1
    );
    let second = json_ok(&command(&["get", &url], &state));
    assert_eq!(second["status"], 200);
    assert!(second["content"]["text"]
        .as_str()
        .unwrap()
        .contains("daemon origin fixture"));
    assert_eq!(json_ok(&command(&["auto", &url], &state))["status"], 200);
    assert!(
        fixture.conditional.load(Ordering::SeqCst) >= 2,
        "different CLI processes after daemon restart must send validators to origin"
    );
    let now = json_ok(&command(&["daemon", "status"], &state));
    assert!(now["daemon"]["status"]["requests"].as_u64().unwrap() >= 2);
    assert!(!started.to_string().contains(&token));
    assert!(!restarted.to_string().contains(&token));
    assert!(!now.to_string().contains(&token));
    assert_eq!(
        json_ok(&command(&["daemon", "stop"], &state))["daemon"]["stopping"],
        true
    );
    wait_for_stop(&state);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn daemon_cli_startup_failure_is_typed_and_never_claims_success() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rustscrapling-daemon-start-failure-{}-{stamp}",
        std::process::id()
    ));
    let state = root.join("private").join("daemon.json");
    let _cleanup = Cleanup(state.clone());
    json_ok(&command(&["daemon", "start", "--direct"], &state));
    assert_eq!(
        json_ok(&command(&["daemon", "stop"], &state))["daemon"]["stopping"],
        true
    );
    wait_for_stop(&state);
    let db = root.join("private").join("daemon.json.sqlite3");
    let sentinel = b"invalid sqlite file - do not erase - secret is sentinel-789";
    std::fs::write(&db, sentinel).unwrap();
    let failure = command(&["daemon", "start", "--direct"], &state);
    assert_eq!(error_kind(&failure), "startup");
    assert!(!String::from_utf8_lossy(&failure.stderr).contains("sentinel-789"));
    assert!(!String::from_utf8_lossy(&failure.stdout).contains("started"));
    assert_eq!(std::fs::read(&db).unwrap().as_slice(), sentinel);
    assert!(
        !state.exists(),
        "failed child must remove its temporary token state"
    );
    assert_eq!(
        error_kind(&command(&["daemon", "status"], &state)),
        "not_running"
    );

    // A stale state file is never overwritten merely because it is malformed.
    std::fs::write(&state, b"invalid state - do not overwrite").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let stale = command(&["daemon", "start"], &state);
    assert_eq!(error_kind(&stale), "io");
    assert_eq!(
        std::fs::read(&state).unwrap().as_slice(),
        b"invalid state - do not overwrite"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn network_search_cli_queries_daemon_across_processes() {
    let fixture = Fixture::new();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rustscrapling-daemon-search-{}-{stamp}",
        std::process::id()
    ));
    let state = root.join("private").join("daemon.json");
    let _cleanup = Cleanup(state.clone());
    assert_eq!(
        error_kind(&command(&["network", "search"], &state)),
        "not_running"
    );
    json_ok(&command(&["daemon", "start", "--direct"], &state));
    let url = format!("http://{}/trace?key=not-shared", fixture.addr);
    assert_eq!(json_ok(&command(&["get", &url], &state))["status"], 200);
    let found = json_ok(&command(
        &[
            "network",
            "search",
            "--url-contains",
            "/trace",
            "--limit",
            "1",
        ],
        &state,
    ));
    assert_eq!(found["network"]["source"], "daemon_fetch");
    assert_eq!(found["network"]["in_memory"], true);
    assert_eq!(found["network"]["captures_browser_subrequests"], false);
    let records = found["network"]["records"].as_array().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["url"], format!("http://{}/trace", fixture.addr));
    assert_eq!(records[0]["method"], "GET");
    assert_eq!(records[0]["status"], 200);
    assert!(!found.to_string().contains("not-shared"));
    let absent = json_ok(&command(
        &["network", "search", "--url-contains", "/no-match"],
        &state,
    ));
    assert!(absent["network"]["records"].as_array().unwrap().is_empty());
    assert_eq!(
        error_kind(&command(&["network", "search", "--limit", "0"], &state)),
        "invalid_request"
    );
    let private_state = std::fs::read_to_string(&state).unwrap();
    let token = serde_json::from_str::<Value>(&private_state).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    std::fs::write(&state, private_state.replace(&token, &"0".repeat(64))).unwrap();
    let denied = command(&["network", "search"], &state);
    std::fs::write(&state, private_state).unwrap();
    assert_eq!(error_kind(&denied), "unauthorized");
    assert!(!String::from_utf8_lossy(&denied.stderr).contains(&token));
    assert_eq!(
        json_ok(&command(&["daemon", "stop"], &state))["daemon"]["stopping"],
        true
    );
    wait_for_stop(&state);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn session_cli_never_reports_a_disposable_browser_as_persistent() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let state = std::env::temp_dir().join(format!(
        "rustscrapling-session-unused-{}-{stamp}.json",
        std::process::id()
    ));
    for args in [
        vec!["session", "open"],
        vec!["session", "close", "demo"],
        vec!["session", "list"],
        vec!["session", "save-state", "demo", "snapshot.json"],
        vec!["session", "load-state", "demo", "snapshot.json"],
    ] {
        let result = command(&args, &state);
        assert_eq!(error_kind(&result), "unsupported", "{args:?}");
        assert!(
            result.stdout.is_empty(),
            "unsupported operations cannot return success"
        );
    }
    assert!(!state.exists());
}
