//! Plan G-03: long-lived browser session reuse in headless and headed mode
//! through the MCP executor. Opt-in (real Chrome; headed needs a display,
//! e.g. `xvfb-run`):
//!
//!   RUSTSCRAPLING_CHROME_FOR_TESTING=/path/chrome \
//!     cargo test -p rustscrapling-mcp --test session_reuse_chrome_e2e -- --ignored
use rustscrapling_mcp::RustScraplingMcp;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Override with RUSTSCRAPLING_REUSE_ROUNDS for longer soak runs.
fn rounds() -> usize {
    std::env::var("RUSTSCRAPLING_REUSE_ROUNDS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(25)
}

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

async fn serve() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 8192];
                let size = stream.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..size]).to_string();
                let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let round = path.trim_start_matches("/round/");
                let body = format!(
                    r#"<!doctype html><html><body><h1 id="title">Round {round}</h1>
<button id="b" onclick="this.textContent='clicked {round}'">go</button>
<input id="i"><script>localStorage.setItem('visits', String(Number(localStorage.getItem('visits')||0)+1));</script>
</body></html>"#
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    port
}

async fn reuse(headless: bool) {
    let chrome = std::env::var("RUSTSCRAPLING_CHROME_FOR_TESTING")
        .expect("set RUSTSCRAPLING_CHROME_FOR_TESTING");
    let port = serve().await;
    let handler = RustScraplingMcp::default();
    let opened = call(
        &handler,
        "browser_open",
        json!({"session_id":"reuse","executable_path":chrome,"headless":headless}),
    )
    .await;
    assert_eq!(opened.is_error, Some(false), "{opened:?}");
    let evaluate = |expression: &str| json!({"session_id":"reuse","expression":expression,"isolated_context":false});
    let started = std::time::Instant::now();
    for round in 0..rounds() {
        let url = format!("http://127.0.0.1:{port}/round/{round}");
        let fetched = call(
            &handler,
            "browser_fetch",
            json!({"session_id":"reuse","url":url,"extraction_type":"text","main_content_only":false}),
        )
        .await;
        assert_eq!(fetched.is_error, Some(false), "round {round}: {fetched:?}");
        let content = fetched.structured_content.unwrap()["content"][0]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(content.contains(&format!("Round {round}")), "{content}");
        let acted = call(
            &handler,
            "browser_actions",
            json!({"session_id":"reuse","actions":[
                {"type":"click","target":"#b"},
                {"type":"textbox","target":"#i","value":format!("r{round}")}]}),
        )
        .await;
        assert_eq!(acted.is_error, Some(false), "round {round}: {acted:?}");
        let state = call(
            &handler,
            "browser_evaluate",
            evaluate("() => [document.querySelector('#b').textContent, document.querySelector('#i').value, localStorage.getItem('visits')]"),
        )
        .await;
        assert_eq!(
            text(&state),
            format!("[\"clicked {round}\",\"r{round}\",\"{}\"]", round + 1),
            "round {round}"
        );
    }
    let identity = call(
        &handler,
        "browser_evaluate",
        evaluate("() => [navigator.userAgent.includes('HeadlessChrome'), navigator.webdriver, outerWidth > 0]"),
    )
    .await;
    assert_eq!(text(&identity), "[false,false,true]");
    let listed = call(&handler, "list_sessions", json!({})).await;
    let sessions = listed.structured_content.unwrap()["result"].clone();
    assert_eq!(sessions.as_array().unwrap().len(), 1, "{sessions}");
    let closed = call(&handler, "close_session", json!({"session_id":"reuse"})).await;
    assert_eq!(closed.is_error, Some(false));
    eprintln!(
        "headless={headless}: {} fetch+action rounds in one session took {:?}",
        rounds(),
        started.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Chrome (RUSTSCRAPLING_CHROME_FOR_TESTING)"]
async fn headless_session_is_reused_across_rounds() {
    reuse(true).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Chrome and a display (e.g. xvfb-run)"]
async fn headed_session_is_reused_across_rounds() {
    assert!(
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some(),
        "headed mode needs a display; run under xvfb-run"
    );
    reuse(false).await;
}
