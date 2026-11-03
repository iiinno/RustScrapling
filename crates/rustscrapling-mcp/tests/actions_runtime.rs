//! MCP `browser_actions` through the tool executor: upstream validation
//! messages without a browser, and a real-Chrome chain when a Chrome binary
//! is available (same discovery as the other MCP runtime tests).
use rustscrapling_mcp::RustScraplingMcp;
use serde_json::{json, Value};
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

#[tokio::test]
async fn actions_are_validated_like_upstream_before_the_session_is_used() {
    let handler = RustScraplingMcp::default();
    for (actions, expected) in [
        (
            json!([{"type":"click"}]),
            "Action 1 (click) needs 'target' or both 'x' and 'y', not both.",
        ),
        (
            json!([{"type":"wait_time","milliseconds":1},{"type":"move","target":"#a","x":1,"y":2}]),
            "Action 2 (move) needs 'target' or both 'x' and 'y', not both.",
        ),
        (
            json!([{"type":"click","x":1}]),
            "Action 1 (click) needs 'target' or both 'x' and 'y', not both.",
        ),
        (
            json!([{"type":"textbox","value":"x"}]),
            "Action 1 (textbox) needs 'target'.",
        ),
        (
            json!([{"type":"combobox","value":[]}]),
            "Action 1 (combobox) needs 'target'.",
        ),
    ] {
        let result = call(
            &handler,
            "browser_actions",
            json!({"session_id":"missing","actions":actions}),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert!(text(&result).contains(expected), "{}", text(&result));
    }
    // Valid actions reach session lookup and fail there instead.
    let result = call(
        &handler,
        "browser_actions",
        json!({"session_id":"missing","actions":[{"type":"press_key","key":"Enter"}]}),
    )
    .await;
    assert!(
        text(&result).contains("session_not_found"),
        "{}",
        text(&result)
    );
    // Schema-level checks: empty lists, negative waits and unknown types.
    for actions in [
        json!([]),
        json!([{"type":"wait_time","milliseconds":-1}]),
        json!([{"type":"teleport"}]),
        json!([{"type":"radio","target":"#r","value":false}]),
    ] {
        let result = call(
            &handler,
            "browser_actions",
            json!({"session_id":"missing","actions":actions}),
        )
        .await;
        assert_eq!(result.is_error, Some(true), "{actions}");
        assert!(!text(&result).contains("session_not_found"), "{actions}");
    }
}

fn chrome() -> Option<String> {
    let from_env = [
        "SCRAPLING_EXECUTABLE_PATH",
        "RUSTSCRAPLING_CHROME_FOR_TESTING",
        "CHROME_FOR_TESTING",
        "RUSTSCRAPLING_CHROME",
    ]
    .into_iter()
    .filter_map(std::env::var_os)
    .map(|path| path.to_string_lossy().into_owned());
    let defaults = ["/usr/bin/google-chrome", "/usr/bin/chromium"]
        .into_iter()
        .map(str::to_owned);
    from_env
        .chain(defaults)
        .find(|path| std::path::Path::new(path).is_file())
}

const PAGE: &str = r#"<!doctype html><html><body>
<input id="name" value="old"><input type="checkbox" id="agree"><label for="agree">Agree</label>
<select id="color"><option>Red</option><option>Blue</option></select>
<button id="go" onclick="document.title = 'clicked ' + event.detail">Go</button>
<button id="ask" onclick="document.body.dataset.answer = prompt('name?')">Ask</button>
<script>window.__pageVar = 'main';</script>
</body></html>"#;

#[tokio::test(flavor = "multi_thread")]
async fn browser_actions_chain_on_real_chrome_if_available() {
    let Some(chrome) = chrome() else {
        eprintln!("Chrome not available; optional MCP actions E2E skipped");
        return;
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/page", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer = [0u8; 2048];
                let size = stream.read(&mut buffer).await.unwrap_or(0);
                let response = if buffer[..size].starts_with(b"GET /page ") {
                    format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{PAGE}", PAGE.len())
                } else {
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .into()
                };
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    let handler = RustScraplingMcp::default();
    let opened = call(
        &handler,
        "browser_open",
        json!({"session_id":"actions","executable_path":chrome}),
    )
    .await;
    assert_eq!(opened.is_error, Some(false), "{opened:?}");
    let fetched = call(
        &handler,
        "browser_fetch",
        json!({"session_id":"actions","url":url}),
    )
    .await;
    assert_eq!(fetched.is_error, Some(false), "{fetched:?}");
    let done = call(
        &handler,
        "browser_actions",
        json!({"session_id":"actions","actions":[
            {"type":"textbox","target":"#name","value":"日本"},
            {"type":"checkbox","target":"#agree","value":true},
            {"type":"combobox","target":"#color","value":"Blue"},
            {"type":"click","target":"#go","click_count":2},
            {"type":"dialog","accept":true,"prompt_text":"Ada"},
            {"type":"click","target":"text=Ask"},
            {"type":"wait_load","state":"load"},
            {"type":"press_key","key":"Tab"}
        ]}),
    )
    .await;
    assert_eq!(done.is_error, Some(false), "{done:?}");
    assert_eq!(text(&done), "Actions completed.");
    let evaluated = call(
        &handler,
        "browser_evaluate",
        json!({"session_id":"actions","isolated_context":false,
            "expression":"JSON.stringify([document.querySelector('#name').value, document.querySelector('#agree').checked, document.querySelector('#color').value, document.title, document.body.dataset.answer])"}),
    )
    .await;
    assert_eq!(evaluated.is_error, Some(false), "{evaluated:?}");
    let body = text(&evaluated);
    for expected in ["日本", "true", "Blue", "clicked 2", "Ada"] {
        assert!(body.contains(expected), "{expected} missing in {body}");
    }
    let failed = call(
        &handler,
        "browser_actions",
        json!({"session_id":"actions","actions":[
            {"type":"wait_time","milliseconds":0},
            {"type":"click","target":"button","timeout":500}
        ]}),
    )
    .await;
    assert_eq!(failed.is_error, Some(true));
    assert!(
        text(&failed).contains("Action 2 (click) failed: strict mode violation"),
        "{}",
        text(&failed)
    );
    // Playwright evaluate semantics in both worlds, without Runtime.enable.
    for (args, expected) in [
        (json!({"expression":"(a) => a.x * 2","arg":{"x":21}}), "42"),
        (
            json!({"expression":"window.__pageVar","isolated_context":false}),
            "\"main\"",
        ),
        (json!({"expression":"window.__pageVar"}), "null"),
        (
            json!({"expression":"async () => ({b: 'é', a: [1, 2.5]})"}),
            "{\"b\":\"é\",\"a\":[1,2.5]}",
        ),
        (
            json!({"expression":"  function () { return document.title }  ","isolated_context":false}),
            "\"clicked 2\"",
        ),
    ] {
        let mut args = args.as_object().unwrap().clone();
        args.insert("session_id".into(), json!("actions"));
        let result = handler
            .execute_tool("browser_evaluate", args.clone())
            .await
            .unwrap();
        assert_eq!(result.is_error, Some(false), "{args:?}: {result:?}");
        assert_eq!(text(&result), expected, "{args:?}");
    }
    let thrown = call(
        &handler,
        "browser_evaluate",
        json!({"session_id":"actions","expression":"() => { throw new Error('boom') }"}),
    )
    .await;
    assert_eq!(thrown.is_error, Some(true));
    assert!(text(&thrown).contains("boom"), "{}", text(&thrown));
    // ARIA snapshot (default extraction), refs as action and screenshot targets.
    let snapshot = call(&handler, "browser_extract", json!({"session_id":"actions"})).await;
    assert_eq!(snapshot.is_error, Some(false), "{snapshot:?}");
    let snapshot = text(&snapshot);
    assert!(snapshot.contains("- button \"Go\" [ref="), "{snapshot}");
    assert!(snapshot.contains("[box="), "{snapshot}");
    let reference = snapshot
        .lines()
        .find(|line| line.contains("button \"Go\""))
        .and_then(|line| line.split("[ref=").nth(1))
        .and_then(|rest| rest.split(']').next())
        .unwrap()
        .to_owned();
    let clicked = call(
        &handler,
        "browser_actions",
        json!({"session_id":"actions","actions":[{"type":"click","target":format!("aria-ref={reference}")}]}),
    )
    .await;
    assert_eq!(clicked.is_error, Some(false), "{clicked:?}");
    let found = call(
        &handler,
        "browser_extract",
        json!({"session_id":"actions","search":"(?i)agree","boxes":false}),
    )
    .await;
    assert!(
        text(&found).contains("checkbox \"Agree\""),
        "{}",
        text(&found)
    );
    assert!(!text(&found).contains("[box="));
    let bad = call(
        &handler,
        "browser_extract",
        json!({"session_id":"actions","search":"x","extraction_type":"html"}),
    )
    .await;
    assert!(text(&bad).contains("search requires snapshot output."));
    let html = call(
        &handler,
        "browser_extract",
        json!({"session_id":"actions","extraction_type":"html","target":"#color"}),
    )
    .await;
    // Content parity of the HTML/Markdown/text renderer with upstream's
    // Convertor (body sanitising, not article extraction) is tracked
    // separately; here only the strict element target is exercised.
    assert_eq!(html.is_error, Some(false), "{html:?}");
    let shot = call(
        &handler,
        "browser_screenshot",
        json!({"session_id":"actions","ref":reference}),
    )
    .await;
    assert_eq!(shot.is_error, Some(false), "{shot:?}");
    let image = serde_json::to_value(&shot.content[0]).unwrap();
    assert!(image["data"].as_str().unwrap().starts_with("iVBORw0KGgo"));
    assert!(serde_json::to_value(&shot.content[1]).unwrap()["text"]
        .as_str()
        .unwrap()
        .ends_with("/page"));
    for (args, expected) in [
        (
            json!({"selector":"#go","ref":reference}),
            "Provide either 'selector' or 'ref', not both.",
        ),
        (
            json!({"selector":"#go","full_page":true}),
            "'full_page' cannot be combined",
        ),
        (
            json!({"quality":5}),
            "'quality' is only valid when 'image_type' is 'jpeg'.",
        ),
    ] {
        let mut args = args.as_object().unwrap().clone();
        args.insert("session_id".into(), json!("actions"));
        let result = handler
            .execute_tool("browser_screenshot", args)
            .await
            .unwrap();
        assert!(text(&result).contains(expected), "{}", text(&result));
    }
    let snap_fetch = call(
        &handler,
        "browser_fetch",
        json!({"session_id":"actions","url":url,"extraction_type":"snapshot","css_selector":"select"}),
    )
    .await;
    assert_eq!(snap_fetch.is_error, Some(false), "{snap_fetch:?}");
    let content = snap_fetch.structured_content.unwrap()["content"][0]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(content.starts_with("- combobox"), "{content}");
    let closed = call(&handler, "close_session", json!({"session_id":"actions"})).await;
    assert_eq!(closed.is_error, Some(false));
}
