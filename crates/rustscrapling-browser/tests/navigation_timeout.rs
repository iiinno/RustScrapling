//! Mock the CDP transport to prove navigation cannot exceed the caller's
//! deadline when Chrome never acknowledges Page.navigate.
use futures_util::{SinkExt, StreamExt};
use rustscrapling_browser::{BrowserClient, BrowserError};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio_tungstenite::{accept_async, tungstenite::Message};

#[tokio::test]
async fn navigation_deadline_also_bounds_the_cdp_command_ack() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let mock = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(socket).await.unwrap();
        while let Some(Ok(Message::Text(text))) = ws.next().await {
            let request: Value = serde_json::from_str(&text).unwrap();
            let result = match request["method"].as_str().unwrap() {
                "Target.createTarget" => json!({"targetId":"mock-tab"}),
                "Target.attachToTarget" => json!({"sessionId":"mock-session"}),
                "Page.enable"
                | "Page.setLifecycleEventsEnabled"
                | "Emulation.setFocusEmulationEnabled" => json!({}),
                "Page.navigate" => {
                    // An unresponsive browser never responds to this command.
                    // Keep the WebSocket alive so no disconnect masks timeout.
                    while ws.next().await.is_some() {}
                    return;
                }
                unexpected => panic!("unexpected CDP command: {unexpected}"),
            };
            ws.send(Message::Text(
                json!({"id":request["id"],"result":result})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        }
    });
    let client = BrowserClient::connect(&endpoint).await.unwrap();
    let page = client.open_page(false).await.unwrap();
    let start = Instant::now();
    let error = page
        .navigate("http://example.test/", Duration::from_millis(150))
        .await
        .expect_err("stalled Page.navigate must time out");
    assert_eq!(error, BrowserError::Timeout("Page.navigate".into()));
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "command acknowledgment must honor navigation_timeout, not the 20s transport timeout"
    );
    mock.abort();
}
