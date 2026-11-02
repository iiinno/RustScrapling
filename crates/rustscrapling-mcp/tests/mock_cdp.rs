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
    let captures = Arc::new(Mutex::new(Vec::<Value>::new()));
    let server_cookies = cookies.clone();
    let server_captures = captures.clone();
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
                "Page.getFrameTree" => {
                    json!({"frameTree":{"frame":{"url":"http://example.test/"}}})
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
    assert!(std::fs::read_to_string(dir.join("first.json"))
        .unwrap()
        .contains("initial"));
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
    let closed = call(&handler, "close_session", json!({"session_id":"mock"})).await;
    assert_eq!(closed.is_error, Some(false), "{closed:?}");
    std::env::remove_var("RUSTSCRAPLING_STATE_DIR");
    std::fs::remove_dir_all(dir).unwrap();
    mock.abort();
}
