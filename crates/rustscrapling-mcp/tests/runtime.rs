use rustscrapling_mcp::RustScraplingMcp;
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::TcpListener,
};

async fn call(server: &RustScraplingMcp, tool: &str, args: Value) -> rmcp::model::CallToolResult {
    server
        .execute_tool(tool, args.as_object().unwrap().clone())
        .await
        .unwrap()
}

#[tokio::test]
async fn real_http_and_http_session_round_trip() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/article", listener.local_addr().unwrap());
    let responder = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0; 2048];
            let size = stream.read(&mut buffer).unwrap();
            assert!(String::from_utf8_lossy(&buffer[..size]).contains("GET /article"));
            let body = "<html><body><article><h1>Local story</h1><p>The MCP HTTP backend works.</p></article></body></html>";
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
    });

    let handler = RustScraplingMcp::default();
    let one = call(&handler, "make_request", json!({"url":url})).await;
    assert_eq!(one.is_error, Some(false), "{one:?}");
    assert_eq!(one.structured_content.as_ref().unwrap()["status"], 200);
    assert!(one.structured_content.as_ref().unwrap()["content"][0]
        .as_str()
        .unwrap()
        .contains("MCP HTTP backend works"));

    let opened = call(
        &handler,
        "open_request_session",
        json!({"session_id":"http-test"}),
    )
    .await;
    assert_eq!(opened.is_error, Some(false), "{opened:?}");
    assert_eq!(
        opened.structured_content.as_ref().unwrap()["session_type"],
        "static"
    );

    let second = call(
        &handler,
        "session_make_request",
        json!({"url":url,"session_id":"http-test"}),
    )
    .await;
    assert_eq!(second.is_error, Some(false), "{second:?}");
    assert_eq!(second.structured_content.as_ref().unwrap()["status"], 200);
    responder.join().unwrap();

    let sessions = call(&handler, "list_sessions", json!({})).await;
    assert_eq!(
        sessions.structured_content.as_ref().unwrap()[0]["session_id"],
        "http-test"
    );
    let ended = call(&handler, "close_session", json!({"session_id":"http-test"})).await;
    assert_eq!(ended.is_error, Some(false));
    let missing = call(
        &handler,
        "session_make_request",
        json!({"url":url,"session_id":"http-test"}),
    )
    .await;
    assert_eq!(missing.is_error, Some(true));
    assert!(serde_json::to_value(&missing.content[0]).unwrap()["text"]
        .as_str()
        .unwrap()
        .contains("session_not_found"));
    let unsupported = call(
        &handler,
        "make_request",
        json!({"url":url,"css_selector":"p"}),
    )
    .await;
    assert_eq!(unsupported.is_error, Some(true));
    assert!(
        serde_json::to_value(&unsupported.content[0]).unwrap()["text"]
            .as_str()
            .unwrap()
            .contains("unsupported_option")
    );
}

#[tokio::test]
async fn browser_session_operations_require_a_real_session() {
    let handler = RustScraplingMcp::default();
    for (name, args) in [
        ("browser_screenshot", json!({"session_id":"missing"})),
        (
            "browser_save_state",
            json!({"session_id":"missing","path":"state.json"}),
        ),
        (
            "browser_load_state",
            json!({"session_id":"missing","path":"state.json"}),
        ),
    ] {
        let result = call(&handler, name, args).await;
        assert_eq!(result.is_error, Some(true), "{name} falsely succeeded");
        let body = serde_json::to_value(&result.content[0]).unwrap()["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(body.contains("session_not_found"), "{name}: {body}");
    }
    for unsafe_path in [
        "../leak.json",
        "..\\leak.json",
        "C:\\leak.json",
        "/tmp/leak.json",
        "other/secret.json",
        "hidden.txt",
    ] {
        let result = call(
            &handler,
            "browser_save_state",
            json!({"session_id":"missing","path":unsafe_path}),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        let output = serde_json::to_string(&result.content).unwrap();
        assert!(
            output.contains("invalid_state_path"),
            "{unsafe_path}: {output}"
        );
    }
}

#[tokio::test]
async fn browser_get_and_session_if_chrome_is_available() {
    let candidates = [
        "SCRAPLING_EXECUTABLE_PATH",
        "RUSTSCRAPLING_CHROME_FOR_TESTING",
        "CHROME_FOR_TESTING",
        "RUSTSCRAPLING_CHROME",
    ];
    let chrome = candidates
        .into_iter()
        .filter_map(std::env::var_os)
        .find(|path| std::path::Path::new(path).is_file());
    #[cfg(windows)]
    let chrome = chrome.or_else(|| {
        Some(std::ffi::OsString::from(
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        ))
        .filter(|p| std::path::Path::new(p).is_file())
    });
    #[cfg(unix)]
    let chrome = chrome.or_else(|| {
        ["/usr/bin/google-chrome", "/usr/bin/chromium"]
            .into_iter()
            .find(|s| std::path::Path::new(s).is_file())
            .map(Into::into)
    });
    let Some(chrome) = chrome else {
        eprintln!("Chrome not available; optional browser E2E skipped");
        return;
    };

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/page", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0; 2048];
            let _ = stream.read(&mut buf).unwrap();
            let body="<html><body><main><h1>Browser MCP</h1><p>CDP session success.</p></main></body></html>";
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/html\r\nSet-Cookie: mcp_session=private; Path=/; SameSite=Lax\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
    });
    let handler = RustScraplingMcp::default();
    let path = chrome.to_string_lossy();
    let once = call(
        &handler,
        "browser_fetch_once",
        json!({"url":url,"executable_path":path}),
    )
    .await;
    assert_eq!(once.is_error, Some(false), "{once:?}");
    assert!(once.structured_content.unwrap()["content"][0]
        .as_str()
        .unwrap()
        .contains("Browser MCP"));
    let opened = call(
        &handler,
        "browser_open",
        json!({"session_id":"browser-test","executable_path":path}),
    )
    .await;
    assert_eq!(opened.is_error, Some(false), "{opened:?}");
    let fetched = call(
        &handler,
        "browser_fetch",
        json!({"session_id":"browser-test","url":url}),
    )
    .await;
    assert_eq!(fetched.is_error, Some(false), "{fetched:?}");
    let extracted = call(
        &handler,
        "browser_extract",
        json!({"session_id":"browser-test","extraction_type":"text"}),
    )
    .await;
    assert_eq!(extracted.is_error, Some(false), "{extracted:?}");
    let evaluated = call(
        &handler,
        "browser_evaluate",
        json!({"session_id":"browser-test","expression":"document.title","isolated_context":false}),
    )
    .await;
    assert_eq!(evaluated.is_error, Some(false), "{evaluated:?}");
    let screenshot = call(
        &handler,
        "browser_screenshot",
        json!({"session_id":"browser-test"}),
    )
    .await;
    assert_eq!(screenshot.is_error, Some(false), "{screenshot:?}");
    let image = serde_json::to_value(&screenshot.content[0]).unwrap();
    assert_eq!(image["type"], "image");
    assert_eq!(image["mimeType"], "image/png");
    assert!(image["data"].as_str().unwrap().len() > 100);
    assert!(image["data"].as_str().unwrap().starts_with("iVBORw0KGgo"));
    let jpeg = call(
        &handler,
        "browser_screenshot",
        json!({"session_id":"browser-test","image_type":"jpeg","quality":40,"full_page":true}),
    )
    .await;
    assert_eq!(jpeg.is_error, Some(false), "{jpeg:?}");
    assert_eq!(
        serde_json::to_value(&jpeg.content[0]).unwrap()["mimeType"],
        "image/jpeg"
    );
    assert!(serde_json::to_value(&jpeg.content[0]).unwrap()["data"]
        .as_str()
        .unwrap()
        .starts_with("/9j/"));
    for invalid in [
        json!({"selector":"h1"}),
        json!({"image_type":"png","quality":50}),
    ] {
        let mut args = invalid.as_object().unwrap().clone();
        args.insert("session_id".into(), json!("browser-test"));
        assert_eq!(
            handler
                .execute_tool("browser_screenshot", args)
                .await
                .unwrap()
                .is_error,
            Some(true)
        );
    }
    let dir = std::env::temp_dir().join(format!(
        "rustscrapling-mcp-state-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::env::set_var("RUSTSCRAPLING_STATE_DIR", &dir);
    let saved = call(
        &handler,
        "browser_save_state",
        json!({"session_id":"browser-test","path":"login.json"}),
    )
    .await;
    assert_eq!(saved.is_error, Some(false), "{saved:?}");
    assert_eq!(
        saved.structured_content.as_ref().unwrap()["contents"],
        "cookies_only"
    );
    let content = std::fs::read_to_string(dir.join("login.json")).unwrap();
    assert!(content.contains("mcp_session"));
    assert!(!content.contains("localStorage"));
    let duplicate = call(
        &handler,
        "browser_save_state",
        json!({"session_id":"browser-test","path":"login.json"}),
    )
    .await;
    assert_eq!(duplicate.is_error, Some(true));
    let changed = call(
        &handler,
        "browser_evaluate",
        json!({"session_id":"browser-test","expression":"document.cookie = 'mcp_session=changed; Path=/'","isolated_context":false}),
    )
    .await;
    assert_eq!(changed.is_error, Some(false));
    let loaded = call(
        &handler,
        "browser_load_state",
        json!({"session_id":"browser-test","path":"login.json"}),
    )
    .await;
    assert_eq!(loaded.is_error, Some(false), "{loaded:?}");
    let cookie = call(
        &handler,
        "browser_evaluate",
        json!({"session_id":"browser-test","expression":"document.cookie","isolated_context":false}),
    )
    .await;
    assert!(serde_json::to_string(&cookie)
        .unwrap()
        .contains("mcp_session=private"));
    std::fs::write(dir.join("bad.json"), "{invalid json").unwrap();
    let malformed = call(
        &handler,
        "browser_load_state",
        json!({"session_id":"browser-test","path":"bad.json"}),
    )
    .await;
    assert_eq!(malformed.is_error, Some(true));
    std::fs::write(dir.join("huge.json"), vec![0u8; 4 * 1024 * 1024 + 1]).unwrap();
    let too_big = call(
        &handler,
        "browser_load_state",
        json!({"session_id":"browser-test","path":"huge.json"}),
    )
    .await;
    assert_eq!(too_big.is_error, Some(true));
    assert_eq!(
        call(
            &handler,
            "close_session",
            json!({"session_id":"browser-test"})
        )
        .await
        .is_error,
        Some(false)
    );
    server.join().unwrap();
    std::env::remove_var("RUSTSCRAPLING_STATE_DIR");
    std::fs::remove_dir_all(dir).unwrap();
}
