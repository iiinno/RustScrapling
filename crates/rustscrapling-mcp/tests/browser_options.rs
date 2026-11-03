//! Real-Chrome E2E for upstream browser fetch/session options through the
//! MCP executor (runs when a Chrome binary is available, like runtime.rs).
use rustscrapling_mcp::RustScraplingMcp;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn call(server: &RustScraplingMcp, tool: &str, args: Value) -> rmcp::model::CallToolResult {
    server
        .execute_tool(tool, args.as_object().unwrap().clone())
        .await
        .unwrap()
}

fn text(result: &rmcp::model::CallToolResult) -> String {
    serde_json::to_value(&result.content[0]).unwrap()["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn chrome() -> Option<String> {
    [
        "SCRAPLING_EXECUTABLE_PATH",
        "RUSTSCRAPLING_CHROME_FOR_TESTING",
        "CHROME_FOR_TESTING",
        "RUSTSCRAPLING_CHROME",
    ]
    .into_iter()
    .filter_map(std::env::var_os)
    .map(|path| path.to_string_lossy().into_owned())
    .chain(["/usr/bin/google-chrome", "/usr/bin/chromium"].map(str::to_owned))
    .find(|path| std::path::Path::new(path).is_file())
}

/// Serves `/page` echoing selected request headers, records every path.
async fn serve(log: Arc<Mutex<Vec<String>>>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let log = Arc::clone(&log);
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 8192];
                let size = stream.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..size]).to_string();
                let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
                log.lock().unwrap().push(path.clone());
                let header = |name: &str| {
                    request
                        .lines()
                        .find(|line| line.to_ascii_lowercase().starts_with(&format!("{name}:")))
                        .map(|line| line[name.len() + 1..].trim().to_owned())
                        .unwrap_or_default()
                };
                let (kind, body) = if path.starts_with("/page") {
                    let html = format!(
                        r#"<!doctype html><html><head><link rel="stylesheet" href="/style.css"></head><body>
<p id="referer">{}</p><p id="xtest">{}</p><p id="lang">{}</p>
<img src="/pixel.png"><img src="http://localhost:{port}/blocked.png">
<x-shadow></x-shadow>
<script>customElements.define('x-shadow', class extends HTMLElement {{ constructor() {{ super(); this.attachShadow({{mode:'open'}}).innerHTML = '<p>shadow text</p>'; }} }});</script>
</body></html>"#,
                        header("referer"),
                        header("x-test"),
                        header("accept-language")
                    );
                    ("text/html", html)
                } else if path.ends_with(".css") {
                    ("text/css", "p{color:red}".to_owned())
                } else {
                    ("image/png", String::new())
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    port
}

#[tokio::test(flavor = "multi_thread")]
async fn browser_session_and_fetch_options_take_effect() {
    let Some(chrome) = chrome() else {
        eprintln!("Chrome not available; optional browser options E2E skipped");
        return;
    };
    let log = Arc::new(Mutex::new(Vec::new()));
    let port = serve(Arc::clone(&log)).await;
    let base = format!("http://127.0.0.1:{port}");
    let handler = RustScraplingMcp::default();
    let opened = call(
        &handler,
        "browser_open",
        json!({"session_id":"opts","executable_path":chrome,"locale":"de-DE",
            "timezone_id":"Asia/Tokyo",
            "cookies":[{"name":"seed","value":"cookie-1","url":base}]}),
    )
    .await;
    assert_eq!(opened.is_error, Some(false), "{opened:?}");

    // Defaults: Google referer, de-DE Accept-Language, all resources load.
    let fetched = call(
        &handler,
        "browser_fetch",
        json!({"session_id":"opts","url":format!("{base}/page?1"),"extraction_type":"text"}),
    )
    .await;
    assert_eq!(fetched.is_error, Some(false), "{fetched:?}");
    let content = fetched.structured_content.unwrap()["content"][0]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(content.contains("https://www.google.com/"), "{content}");
    assert!(content.contains("de-DE"), "{content}");
    let evaluate = |expression: &str| json!({"session_id":"opts","expression":expression,"isolated_context":false});
    let state = call(
        &handler,
        "browser_evaluate",
        evaluate("() => [navigator.language, Intl.DateTimeFormat().resolvedOptions().timeZone, document.cookie, navigator.userAgent.includes('HeadlessChrome'), innerWidth, innerHeight, navigator.webdriver]"),
    )
    .await;
    assert_eq!(
        text(&state),
        "[\"de-DE\",\"Asia/Tokyo\",\"seed=cookie-1\",false,1920,1080,false]",
        "{state:?}"
    );
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    {
        let seen = log.lock().unwrap();
        assert!(seen.iter().any(|p| p == "/pixel.png"), "{seen:?}");
        assert!(seen.iter().any(|p| p == "/style.css"), "{seen:?}");
        assert!(seen.iter().any(|p| p == "/blocked.png"), "{seen:?}");
    }

    // Per-request options: own referer + header, blocked domain, disabled resources.
    log.lock().unwrap().clear();
    let fetched = call(
        &handler,
        "browser_fetch",
        json!({"session_id":"opts","url":format!("{base}/page?2"),"extraction_type":"text",
            "google_search":false,"extra_headers":{"X-Test":"yes","Referer":"https://ref.example/"},
            "blocked_domains":["localhost"],"disable_resources":true}),
    )
    .await;
    assert_eq!(fetched.is_error, Some(false), "{fetched:?}");
    let content = fetched.structured_content.unwrap()["content"][0]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(content.contains("https://ref.example/"), "{content}");
    assert!(content.contains("yes"), "{content}");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    {
        let seen = log.lock().unwrap();
        assert!(seen.iter().any(|p| p == "/page?2"), "{seen:?}");
        for blocked in ["/pixel.png", "/style.css", "/blocked.png"] {
            assert!(
                !seen.iter().any(|p| p == blocked),
                "{blocked} loaded: {seen:?}"
            );
        }
    }

    // Request-scoped settings are reset for the next request.
    log.lock().unwrap().clear();
    let fetched = call(
        &handler,
        "browser_fetch",
        json!({"session_id":"opts","url":format!("{base}/page?3"),"extraction_type":"html",
            "main_content_only":false,"pierce_shadow":true}),
    )
    .await;
    let content = fetched.structured_content.unwrap()["content"][0]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!content.contains(">yes<"), "{content}");
    assert!(
        content.contains("<shadow-root><p>shadow text</p></shadow-root>"),
        "{content}"
    );
    let without = call(
        &handler,
        "browser_fetch",
        json!({"session_id":"opts","url":format!("{base}/page?4"),"extraction_type":"html",
            "main_content_only":false}),
    )
    .await;
    let without = without.structured_content.unwrap()["content"][0]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!without.contains("<shadow-root>"), "{without}");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(log.lock().unwrap().iter().any(|p| p == "/pixel.png"));

    let listed = call(&handler, "list_sessions", json!({})).await;
    let settings = &listed.structured_content.unwrap()["result"][0]["settings"];
    assert_eq!(settings["locale"], "de-DE");
    assert_eq!(settings["timezone_id"], "Asia/Tokyo");
    assert_eq!(
        call(&handler, "close_session", json!({"session_id":"opts"}))
            .await
            .is_error,
        Some(false)
    );

    // One-shot fetch with a custom user agent and invalid inputs.
    let once = call(
        &handler,
        "browser_fetch_once",
        json!({"url":format!("{base}/page?5"),"executable_path":chrome,
            "useragent":"RustScraplingTest/1.0","extraction_type":"text","google_search":false}),
    )
    .await;
    assert_eq!(once.is_error, Some(false), "{once:?}");
    for (args, kind) in [
        (
            json!({"session_id":"bad","executable_path":chrome,"timezone_id":"Mars/Base"}),
            "invalid",
        ),
        (
            json!({"session_id":"bad2","executable_path":chrome,"cookies":[{"name":"x","value":"y"}]}),
            "invalid",
        ),
        (
            json!({"session_id":"bad3","executable_path":chrome,"additional_args":{"x":1}}),
            "unsupported",
        ),
    ] {
        let result = call(&handler, "browser_open", args).await;
        assert_eq!(result.is_error, Some(true));
        assert!(text(&result).contains(kind), "{}", text(&result));
    }
    let listed = call(&handler, "list_sessions", json!({})).await;
    assert_eq!(listed.structured_content.unwrap()["result"], json!([]));
}
