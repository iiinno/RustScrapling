//! Exercises first-party CDP input methods without a Chrome executable.
use futures_util::{SinkExt, StreamExt};
use rustscrapling_browser::{BrowserClient, BrowserError};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::{net::TcpListener, sync::Mutex};
use tokio_tungstenite::{accept_async, tungstenite::Message};

#[tokio::test]
async fn css_click_type_and_keys_use_page_session_without_runtime() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let recorded = Arc::new(Mutex::new(Vec::<Value>::new()));
    let sent = recorded.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        while let Some(Ok(Message::Text(raw))) = socket.next().await {
            let cmd: Value = serde_json::from_str(&raw).unwrap();
            let method = cmd["method"].as_str().unwrap();
            let result = match method {
                "Target.createTarget" => json!({"targetId":"actions-tab"}),
                "Target.attachToTarget" => json!({"sessionId":"actions-page"}),
                "Page.enable"
                | "Page.setLifecycleEventsEnabled"
                | "Emulation.setFocusEmulationEnabled"
                | "DOM.scrollIntoViewIfNeeded"
                | "DOM.focus"
                | "Input.dispatchMouseEvent"
                | "Input.insertText"
                | "Input.dispatchKeyEvent"
                | "Target.closeTarget" => json!({}),
                "DOM.getDocument" => json!({"root":{"nodeId":1}}),
                "DOM.querySelector" => {
                    if cmd["params"]["selector"] == "#missing" {
                        json!({"nodeId":0})
                    } else {
                        json!({"nodeId":7})
                    }
                }
                "DOM.describeNode" => json!({"node":{"nodeName":"INPUT"}}),
                "DOM.getBoxModel" => {
                    json!({"model":{"content":[10.0,20.0,110.0,20.0,110.0,60.0,10.0,60.0]}})
                }
                other => panic!("unexpected CDP method: {other}"),
            };
            if !method.starts_with("Target.") {
                assert_eq!(cmd["sessionId"], "actions-page");
            }
            sent.lock().await.push(cmd.clone());
            socket
                .send(Message::Text(
                    json!({"id":cmd["id"],"result":result}).to_string().into(),
                ))
                .await
                .unwrap();
        }
    });
    let client = BrowserClient::connect(&endpoint).await.unwrap();
    let page = client.open_page(false).await.unwrap();
    page.click_css("#button").await.unwrap();
    page.type_text_css("#input", "日本語").await.unwrap();
    page.fill_text_css("#input", "新しい値").await.unwrap();
    page.press_key("Enter").await.unwrap();
    assert!(matches!(
        page.click_css("#missing").await,
        Err(BrowserError::InvalidArgument(_))
    ));
    assert!(matches!(
        page.click_css("").await,
        Err(BrowserError::InvalidArgument(_))
    ));
    assert!(matches!(
        page.type_text_css("#input", &"a".repeat(65537)).await,
        Err(BrowserError::InvalidArgument(_))
    ));
    assert!(matches!(
        page.press_key("Ctrl+L").await,
        Err(BrowserError::InvalidArgument(_))
    ));
    let commands = recorded.lock().await.clone();
    let clicked: Vec<&Value> = commands
        .iter()
        .filter(|command| command["method"] == "Input.dispatchMouseEvent")
        .collect();
    assert_eq!(clicked.len(), 2);
    assert_eq!(clicked[0]["params"]["type"], "mousePressed");
    assert_eq!(clicked[1]["params"]["type"], "mouseReleased");
    assert_eq!(clicked[0]["params"]["x"], 60.0);
    assert_eq!(clicked[0]["params"]["y"], 40.0);
    assert!(commands
        .iter()
        .any(|command| command["method"] == "Input.insertText"
            && command["params"]["text"] == "日本語"));
    assert!(commands
        .iter()
        .any(|command| command["method"] == "Input.insertText"
            && command["params"]["text"] == "新しい値"));
    assert_eq!(
        commands
            .iter()
            .filter(|command| command["method"] == "Input.dispatchKeyEvent")
            .count(),
        6
    );
    assert!(!commands
        .iter()
        .any(|command| command["method"] == "Runtime.enable"));
    page.close().await.unwrap();
    server.abort();
}
