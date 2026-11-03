//! Full MCP-to-CDP transport contract test without depending on a Chrome binary.
use futures_util::{SinkExt, StreamExt};
use rustscrapling_mcp::RustScraplingMcp;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::{net::TcpListener, sync::Mutex};
use tokio_tungstenite::{accept_async, tungstenite::Message};

async fn call(server: &RustScraplingMcp, name: &str, args: Value) -> rmcp::model::CallToolResult {
    server
        .execute_tool(name, args.as_object().unwrap().clone())
        .await
        .unwrap()
}

#[tokio::test]
async fn state_roundtrip_and_screenshots_via_mock_cdp() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let cookies = Arc::new(Mutex::new(vec![
        json!({"name":"sid","value":"initial","domain":"example.test","path":"/"}),
    ]));
    let local_storage = Arc::new(Mutex::new(vec![
        ("token".to_owned(), "sensitive-initial".to_owned()),
        ("name".to_owned(), "test".to_owned()),
    ]));
    let page_origin = Arc::new(Mutex::new("http://example.test".to_owned()));
    let captures = Arc::new(Mutex::new(Vec::<Value>::new()));
    let input_actions = Arc::new(Mutex::new(Vec::<Value>::new()));
    let server_cookies = cookies.clone();
    let server_captures = captures.clone();
    let server_input_actions = input_actions.clone();
    let server_local_storage = local_storage.clone();
    let server_origin = page_origin.clone();
    let mock = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        while let Some(Ok(Message::Text(text))) = ws.next().await {
            let message: Value = serde_json::from_str(&text).unwrap();
            let method = message["method"].as_str().unwrap();
            let params = &message["params"];
            let result = match method {
                "Target.createTarget" => json!({"targetId":"mock-tab"}),
                "Target.attachToTarget" => json!({"sessionId":"mock-session"}),
                "Page.navigate" => json!({}),
                "DOM.getDocument" => json!({"root":{
                    "nodeId":1,"backendNodeId":1,"nodeType":9,"nodeName":"#document", "localName":"", "nodeValue":""
                }}),
                "DOM.getOuterHTML" => {
                    json!({"outerHTML":"<html><body><h1>Mock CDP page</h1></body></html>"})
                }
                "DOM.querySelector" => {
                    assert_eq!(message["sessionId"], "mock-session");
                    assert_eq!(params["selector"], "#test");
                    json!({"nodeId":7})
                }
                "DOM.getBoxModel" => {
                    assert_eq!(params["nodeId"], 7);
                    json!({"model":{"content":[10,20,90,20,90,60,10,60]}})
                }
                "DOM.scrollIntoViewIfNeeded"
                | "DOM.focus"
                | "Input.dispatchMouseEvent"
                | "Input.insertText"
                | "Input.dispatchKeyEvent" => {
                    assert_eq!(message["sessionId"], "mock-session");
                    server_input_actions.lock().await.push(message.clone());
                    json!({})
                }
                "Page.getFrameTree" => {
                    let origin = server_origin.lock().await.clone();
                    json!({"frameTree":{"frame":{"id":"mock-frame","url":format!("{origin}/"),"securityOrigin":origin}}})
                }
                "Storage.getStorageKeyForFrame" => {
                    assert_eq!(params["frameId"], "mock-frame");
                    json!({"storageKey":"http://example.test/"})
                }
                "DOMStorage.getDOMStorageItems" => {
                    assert_eq!(message["sessionId"], "mock-session");
                    assert_eq!(
                        params["storageId"],
                        json!({"storageKey":"http://example.test/","isLocalStorage":true})
                    );
                    json!({"entries":*server_local_storage.lock().await})
                }
                "DOMStorage.clear" => {
                    assert_eq!(message["sessionId"], "mock-session");
                    assert_eq!(params["storageId"]["storageKey"], "http://example.test/");
                    server_local_storage.lock().await.clear();
                    json!({})
                }
                "DOMStorage.setDOMStorageItem" => {
                    assert_eq!(message["sessionId"], "mock-session");
                    assert_eq!(params["storageId"]["storageKey"], "http://example.test/");
                    server_local_storage.lock().await.push((
                        params["key"].as_str().unwrap().into(),
                        params["value"].as_str().unwrap().into(),
                    ));
                    json!({})
                }
                "Page.getLayoutMetrics" => json!({"cssContentSize":{"width":300,"height":450}}),
                "Page.captureScreenshot" => {
                    server_captures.lock().await.push(params.clone());
                    if params["format"] == "jpeg" {
                        json!({"data":"/9j/"})
                    } else {
                        json!({"data":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9zliMAAAAASUVORK5CYII="})
                    }
                }
                "Storage.getCookies" => json!({"cookies":*server_cookies.lock().await}),
                "Storage.clearCookies" => {
                    server_cookies.lock().await.clear();
                    json!({})
                }
                "Storage.setCookies" => {
                    *server_cookies.lock().await = params["cookies"].as_array().unwrap().clone();
                    json!({})
                }
                "Page.enable" | "Network.enable" | "Target.closeTarget" => json!({}),
                other => panic!("unexpected CDP command: {other}"),
            };
            ws.send(Message::Text(
                json!({"id":message["id"],"result":result})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            if method == "Page.navigate" {
                for event in [
                    json!({"method":"Page.frameNavigated","sessionId":"mock-session","params":{"frame":{"id":"mock-frame","url":"http://example.test/"}}}),
                    json!({"method":"Page.loadEventFired","sessionId":"mock-session","params":{"timestamp":0}}),
                ] {
                    ws.send(Message::Text(event.to_string().into()))
                        .await
                        .unwrap();
                }
            }
        }
    });

    let handler = RustScraplingMcp::default();
    let opened = call(
        &handler,
        "browser_open",
        json!({"session_id":"mock","cdp_url":endpoint}),
    )
    .await;
    assert_eq!(opened.is_error, Some(false), "{opened:?}");
    let fetched = call(
        &handler,
        "browser_fetch",
        json!({"session_id":"mock","url":"http://example.test/"}),
    )
    .await;
    assert_eq!(fetched.is_error, Some(false), "{fetched:?}");
    let interacted = call(
        &handler,
        "browser_actions",
        json!({"session_id":"mock","actions":[
            {"type":"click","target":"#test"},
            {"type":"textbox","target":"#test","value":"日本語","clear":false},
            {"type":"press_key","key":"Enter"},
            {"type":"wait_time","milliseconds":0}
        ]}),
    )
    .await;
    assert_eq!(interacted.is_error, Some(false), "{interacted:?}");
    let commands = input_actions.lock().await;
    assert!(commands.iter().any(|command| {
        command["method"] == "Input.insertText" && command["params"]["text"] == "日本語"
    }));
    assert_eq!(
        commands
            .iter()
            .filter(|command| command["method"] == "Input.dispatchMouseEvent")
            .count(),
        2
    );
    drop(commands);
    for invalid in [
        json!({"type":"textbox","target":"#test","value":"replace-default-clear"}),
        json!({"type":"click","target":"#test","x":22}),
        json!({"type":"press_key","key":"Ctrl+L"}),
        json!({"type":"wait_time","milliseconds":-1}),
        json!({"type":"dialog","accept":true}),
    ] {
        let result = call(
            &handler,
            "browser_actions",
            json!({"session_id":"mock","actions":[invalid]}),
        )
        .await;
        assert_eq!(result.is_error, Some(true), "{result:?}");
    }
    let png = call(&handler, "browser_screenshot", json!({"session_id":"mock"})).await;
    assert_eq!(png.is_error, Some(false), "{png:?}");
    let png_value = serde_json::to_value(&png.content[0]).unwrap();
    assert_eq!(png_value["mimeType"], "image/png");
    assert!(png_value["data"]
        .as_str()
        .unwrap()
        .starts_with("iVBORw0KGgo"));
    let jpeg = call(
        &handler,
        "browser_screenshot",
        json!({"session_id":"mock","image_type":"jpeg","quality":70,"full_page":true}),
    )
    .await;
    assert_eq!(jpeg.is_error, Some(false), "{jpeg:?}");
    assert_eq!(
        serde_json::to_value(&jpeg.content[0]).unwrap()["mimeType"],
        "image/jpeg"
    );
    assert_eq!(
        captures.lock().await[1]["clip"]["height"].as_f64(),
        Some(450.0)
    );
    assert_eq!(captures.lock().await[1]["quality"], 70);

    let dir = std::env::temp_dir().join(format!("rustscrapling-mock-state-{}", std::process::id()));
    std::env::set_var("RUSTSCRAPLING_STATE_DIR", &dir);
    let saved = call(
        &handler,
        "browser_save_state",
        json!({"session_id":"mock","path":"first.json"}),
    )
    .await;
    assert_eq!(saved.is_error, Some(false), "{saved:?}");
    assert_eq!(
        saved.structured_content.as_ref().unwrap()["contents"],
        "cookies_and_active_origin_local_storage"
    );
    assert!(std::fs::read_to_string(dir.join("first.json"))
        .unwrap()
        .contains("initial"));
    let saved_file = std::fs::read_to_string(dir.join("first.json")).unwrap();
    assert!(saved_file.contains("sensitive-initial"));
    assert!(saved_file.contains("http://example.test"));
    assert_eq!(
        serde_json::from_str::<Value>(&saved_file).unwrap()["version"],
        2
    );
    *local_storage.lock().await = vec![("token".into(), "changed".into())];
    let restored_v2 = call(
        &handler,
        "browser_load_state",
        json!({"session_id":"mock","path":"first.json"}),
    )
    .await;
    assert_eq!(restored_v2.is_error, Some(false), "{restored_v2:?}");
    assert_eq!(local_storage.lock().await.len(), 2);
    assert!(local_storage
        .lock()
        .await
        .iter()
        .any(|(key, value)| key == "token" && value == "sensitive-initial"));
    // A different origin must fail before clearing either cookies or storage.
    *page_origin.lock().await = "https://different.test".to_owned();
    let denied = call(
        &handler,
        "browser_load_state",
        json!({"session_id":"mock","path":"first.json"}),
    )
    .await;
    assert_eq!(denied.is_error, Some(true));
    assert!(serde_json::to_string(&denied.content)
        .unwrap()
        .contains("invalid_arguments"));
    assert_eq!(local_storage.lock().await.len(), 2);
    assert_eq!(cookies.lock().await[0]["value"], "initial");
    *page_origin.lock().await = "http://example.test".to_owned();
    std::fs::write(
        dir.join("restore.json"),
        json!({"version":1,"cookies":[{
            "name":"sid","value":"restored","domain":"example.test","path":"/"
        }]})
        .to_string(),
    )
    .unwrap();
    let loaded = call(
        &handler,
        "browser_load_state",
        json!({"session_id":"mock","path":"restore.json"}),
    )
    .await;
    assert_eq!(loaded.is_error, Some(false), "{loaded:?}");
    assert_eq!(cookies.lock().await[0]["value"], "restored");
    assert_eq!(
        local_storage.lock().await.len(),
        2,
        "v1 must not clear localStorage"
    );
    let resaved = call(
        &handler,
        "browser_save_state",
        json!({"session_id":"mock","path":"second.json"}),
    )
    .await;
    assert_eq!(resaved.is_error, Some(false), "{resaved:?}");
    assert!(std::fs::read_to_string(dir.join("second.json"))
        .unwrap()
        .contains("restored"));
    std::fs::write(
        dir.join("indexed.json"),
        json!({"version":2,"cookies":[],"indexedDB":[]}).to_string(),
    )
    .unwrap();
    let denied_indexed = call(
        &handler,
        "browser_load_state",
        json!({"session_id":"mock","path":"indexed.json"}),
    )
    .await;
    assert_eq!(
        denied_indexed.is_error,
        Some(true),
        "unknown IndexedDB must not be silently ignored"
    );
    let closed = call(&handler, "close_session", json!({"session_id":"mock"})).await;
    assert_eq!(closed.is_error, Some(false), "{closed:?}");
    std::env::remove_var("RUSTSCRAPLING_STATE_DIR");
    std::fs::remove_dir_all(dir).unwrap();
    mock.abort();
}
