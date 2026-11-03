//! Plan 7.1 / G-11: a network-exposed MCP server (NetworkPolicy with
//! block_private) must not let remote clients read the local network.
use rustscrapling_mcp::{NetworkPolicy, RuntimeBackend, RustScraplingMcp};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn server(allowed: &[&str]) -> RustScraplingMcp {
    RustScraplingMcp::with_backend(Arc::new(RuntimeBackend::with_policy(NetworkPolicy {
        block_private: true,
        allowed_internal_hosts: allowed.iter().map(|h| (*h).to_owned()).collect(),
    })))
}

async fn call(server: &RustScraplingMcp, tool: &str, args: Value) -> (bool, String) {
    let result = server
        .execute_tool(tool, args.as_object().unwrap().clone())
        .await
        .unwrap();
    let text = serde_json::to_value(&result.content[0]).unwrap()["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    (result.is_error == Some(true), text)
}

/// `/ok` answers 200; `/redirect` redirects to localhost (another internal name).
async fn serve() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 4096];
                let n = stream.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..n]).to_string();
                let response = if request.starts_with("GET /redirect") {
                    format!("HTTP/1.1 302 Found\r\nLocation: http://localhost:{port}/ok\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                } else {
                    let body = "<html><body><p>internal secret</p></body></html>";
                    format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
                };
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    port
}

#[tokio::test(flavor = "multi_thread")]
async fn exposed_servers_refuse_internal_destinations() {
    let port = serve().await;
    let blocked = server(&[]);
    for url in [
        format!("http://127.0.0.1:{port}/ok"),
        format!("http://localhost:{port}/ok"),
        format!("http://[::1]:{port}/ok"),
        "http://169.254.169.254/latest/meta-data/".to_owned(),
        "http://10.0.0.1/".to_owned(),
    ] {
        let (error, text) = call(&blocked, "make_request", json!({"url": url, "retries": 1})).await;
        assert!(error, "{url}: {text}");
        assert!(text.contains("blocked_by_policy"), "{url}: {text}");
        assert!(!text.contains("internal secret"));
        let (error, text) = call(&blocked, "browser_fetch_once", json!({"url": url})).await;
        assert!(error && text.contains("blocked_by_policy"), "{url}: {text}");
    }
    // Per-call browser launch overrides are refused.
    for args in [
        json!({"session_id": "x", "cdp_url": "ws://127.0.0.1:9222/devtools/browser/x"}),
        json!({"session_id": "x", "executable_path": "/bin/sh"}),
    ] {
        let (error, text) = call(&blocked, "browser_open", args).await;
        assert!(error && text.contains("blocked_by_policy"), "{text}");
    }

    // An explicitly allowed internal host works, but its redirect to another
    // internal name is still refused (redirects are forced to "safe").
    let allowed = server(&["127.0.0.1"]);
    let (error, text) = call(
        &allowed,
        "make_request",
        json!({"url": format!("http://127.0.0.1:{port}/ok"), "retries": 1}),
    )
    .await;
    assert!(!error, "{text}");
    let (error, text) = call(
        &allowed,
        "make_request",
        json!({"url": format!("http://127.0.0.1:{port}/redirect"), "follow_redirects": true, "retries": 1}),
    )
    .await;
    assert!(error, "{text}");
    assert!(!text.contains("internal secret"));

    // The default policy (stdio / loopback HTTP) still allows local targets.
    let (error, text) = call(
        &RustScraplingMcp::default(),
        "make_request",
        json!({"url": format!("http://127.0.0.1:{port}/ok"), "retries": 1}),
    )
    .await;
    assert!(!error, "{text}");
}
