//! Plan G-11 / 7.1(e): negative tests for the daemon IPC. Unauthenticated
//! callers cannot reach the hosted tool executor or fetch backend; malformed,
//! oversized and stalled frames get typed errors without killing the daemon
//! or echoing the token.
use rustscrapling_daemon::{
    DaemonClient, DaemonConfig, DaemonError, DaemonServer, FetchBackend, FetchFuture, FetchRequest,
    ToolFuture, ToolHost,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

#[derive(Default)]
struct CountingBackend(AtomicUsize);

impl FetchBackend for CountingBackend {
    fn fetch<'a>(&'a self, _request: FetchRequest) -> FetchFuture<'a> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(DaemonError::new("internal", "not used")) })
    }
}

#[derive(Default)]
struct CountingTools(AtomicUsize);

impl ToolHost for CountingTools {
    fn call<'a>(
        &'a self,
        name: &'a str,
        _arguments: serde_json::Map<String, Value>,
    ) -> ToolFuture<'a> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok(json!({"called": name})) })
    }
}

async fn exchange(address: &str, frame: &[u8]) -> Value {
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream.write_all(frame).await.unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).await.unwrap();
    serde_json::from_str(&line).unwrap_or_else(|_| panic!("reply: {line:?}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unauthenticated_malformed_oversized_and_stalled_requests_are_refused() {
    let dir = std::env::temp_dir().join(format!("rs-daemon-sec-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let config = DaemonConfig {
        state_file: dir.join("state.json"),
        site_interval: Duration::from_millis(10),
        learning_ttl: Duration::from_secs(60),
        request_timeout: Duration::from_millis(500),
    };
    let backend = Arc::new(CountingBackend::default());
    let tools = Arc::new(CountingTools::default());
    let server =
        DaemonServer::start_with_tools(config.clone(), backend.clone(), Some(tools.clone()))
            .await
            .unwrap();
    let address = server.address().to_owned();
    let state: Value =
        serde_json::from_str(&std::fs::read_to_string(&config.state_file).unwrap()).unwrap();
    let token = state["token"].as_str().unwrap().to_owned();

    // Wrong, empty, missing and prefix tokens never reach the tool host.
    for wire in [
        json!({"token": "0".repeat(64), "request": {"command":"tool","name":"open_session","arguments":{}}}),
        json!({"token": "", "request": {"command":"tool","name":"open_session","arguments":{}}}),
        json!({"token": &token[..63], "request": {"command":"tool","name":"open_session","arguments":{}}}),
        json!({"token": "0".repeat(64), "request": {"command":"fetch","request":{"url":"http://127.0.0.1/","method":"GET","headers":{},"tier":"http"}}}),
        json!({"token": "0".repeat(64), "request": {"command":"stop"}}),
    ] {
        let mut frame = serde_json::to_vec(&wire).unwrap();
        frame.push(b'\n');
        let reply = exchange(&address, &frame).await;
        assert_eq!(reply["kind"], "error", "{reply}");
        assert!(
            ["unauthorized", "parse"].contains(&reply["data"]["kind"].as_str().unwrap()),
            "{reply}"
        );
        assert!(!reply.to_string().contains(&token));
    }
    let missing = exchange(&address, b"{\"request\":{\"command\":\"status\"}}\n").await;
    assert_eq!(missing["data"]["kind"], "parse");

    // Malformed JSON and deep nesting: typed parse errors.
    let reply = exchange(&address, b"not json\n").await;
    assert_eq!(reply["data"]["kind"], "parse");
    let mut deep = format!(
        "{{\"token\":\"{token}\",\"request\":{{\"command\":\"tool\",\"name\":\"x\",\"arguments\":{{\"a\":"
    );
    deep.push_str(&"[".repeat(50_000));
    deep.push_str(&"]".repeat(50_000));
    deep.push_str("}}}\n");
    let reply = exchange(&address, deep.as_bytes()).await;
    assert_eq!(reply["data"]["kind"], "parse", "{reply}");

    // A frame over 16 MiB is refused (connection may close instead of replying).
    {
        let mut stream = TcpStream::connect(&address).await.unwrap();
        let chunk = vec![b'a'; 1 << 20];
        for _ in 0..17 {
            if stream.write_all(&chunk).await.is_err() {
                break;
            }
        }
        let mut line = String::new();
        let _ = tokio::time::timeout(
            Duration::from_secs(5),
            BufReader::new(stream).read_line(&mut line),
        )
        .await;
        if !line.is_empty() {
            let reply: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(reply["data"]["kind"], "size", "{reply}");
        }
    }

    // A client that never finishes its frame is cut off by request_timeout.
    {
        let mut stream = TcpStream::connect(&address).await.unwrap();
        stream.write_all(b"{\"token\":").await.unwrap();
        let mut line = String::new();
        let read = tokio::time::timeout(
            Duration::from_secs(5),
            BufReader::new(stream).read_line(&mut line),
        )
        .await
        .expect("stalled connection was not closed");
        assert!(read.is_ok());
    }

    assert_eq!(tools.0.load(Ordering::SeqCst), 0);
    assert_eq!(backend.0.load(Ordering::SeqCst), 0);

    // The daemon is still alive and serves the real token.
    let client = DaemonClient::discover(&config.state_file)
        .await
        .unwrap()
        .unwrap();
    client.status().await.unwrap();
    let called = client
        .tool("list_sessions", serde_json::Map::new())
        .await
        .unwrap();
    assert_eq!(called, json!({"called": "list_sessions"}));
    assert_eq!(tools.0.load(Ordering::SeqCst), 1);
    client.stop().await.unwrap();
    drop(server);
    let _ = std::fs::remove_dir_all(dir);
}
