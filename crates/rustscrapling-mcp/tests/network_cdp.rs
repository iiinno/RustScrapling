//! MCP network pagination and response bodies through a real CDP WebSocket
//! transport, using a deterministic mock instead of a Chrome executable.
use futures_util::{SinkExt, StreamExt};
use rmcp::model::CallToolResult;
use rustscrapling_mcp::RustScraplingMcp;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::{net::TcpListener, sync::Mutex};
use tokio_tungstenite::{accept_async, tungstenite::Message};

async fn call(server: &RustScraplingMcp, name: &str, args: Value) -> CallToolResult {
    server
        .execute_tool(name, args.as_object().unwrap().clone())
        .await
        .unwrap()
}

fn content(result: &CallToolResult) -> &Value {
    result
        .structured_content
        .as_ref()
        .expect("structured MCP result")
}

#[tokio::test]
async fn response_bodies_are_redacted_and_ids_survive_evictions() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let body_calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let server_body_calls = body_calls.clone();
    let mock = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        let mut last_url = "about:blank".to_owned();
        let mut navigation = 0_usize;
        while let Some(Ok(Message::Text(text))) = ws.next().await {
            let command: Value = serde_json::from_str(&text).unwrap();
            let method = command["method"].as_str().unwrap();
            let result = match method {
                "Target.createTarget" => json!({"targetId":"mock-tab"}),
                "Target.attachToTarget" => json!({"sessionId":"mock-network-session"}),
                "Page.enable"
                | "Page.setLifecycleEventsEnabled"
                | "Emulation.setFocusEmulationEnabled"
                | "Network.enable"
                | "Target.closeTarget" => json!({}),
                "Page.navigate" => {
                    navigation += 1;
                    last_url = command["params"]["url"].as_str().unwrap().to_owned();
                    json!({})
                }
                "Page.getFrameTree" => json!({"frameTree":{"frame":{"url":last_url}}}),
                "DOM.getDocument" => json!({"root":{
                    "nodeId":1,"backendNodeId":1,"nodeType":9,"nodeName":"#document",
                    "localName":"","nodeValue":""
                }}),
                "DOM.getOuterHTML" => {
                    json!({"outerHTML":"<html><body><main>Network mock</main></body></html>"})
                }
                "Network.getResponseBody" => {
                    assert_eq!(command["sessionId"], "mock-network-session");
                    let request_id = command["params"]["requestId"].as_str().unwrap();
                    assert!(request_id.starts_with("xhr-"));
                    server_body_calls.lock().await.push(request_id.to_owned());
                    json!({"base64Encoded":false,"body":json!({
                        "data":format!("visible-{request_id}"),
                        "access_token":"sensitive-token",
                        "nested":{"password":"sensitive-password"}
                    }).to_string()})
                }
                other => panic!("unexpected CDP method {other}"),
            };
            ws.send(Message::Text(
                json!({"id":command["id"],"result":result})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            if method == "Page.navigate" {
                let events = [
                    // Events for another Chrome tab's session must not appear.
                    json!({"method":"Network.responseReceived","sessionId":"foreign-session","params":{
                        "requestId":"foreign","type":"XHR","response":{
                            "url":"http://private.test/?secret=foreign","status":200,
                            "mimeType":"application/json"}}}),
                    json!({"method":"Network.responseReceived","sessionId":"mock-network-session","params":{
                        "requestId":format!("doc-{navigation}"),"type":"Document","response":{
                            "url":last_url,"status":200,"mimeType":"text/html",
                            "headers":{"Set-Cookie":"sid=private-cookie","Content-Type":"text/html"}}}}),
                    json!({"method":"Network.responseReceived","sessionId":"mock-network-session","params":{
                        "requestId":format!("xhr-{navigation}"),"type":"XHR","response":{
                            "url":format!("http://user:password@example.test/api/{navigation}?token=raw-secret"),
                            "status":200,"mimeType":"application/json",
                            "headers":{"Content-Type":"application/json","X-Api-Key":"private-auth"}}}}),
                    json!({"method":"Network.loadingFinished","sessionId":"mock-network-session","params":{
                        "requestId":format!("xhr-{navigation}"),"encodedDataLength":100}}),
                    json!({"method":"Page.frameNavigated","sessionId":"mock-network-session","params":{
                        "frame":{"id":"main","url":last_url}}}),
                    json!({"method":"Page.loadEventFired","sessionId":"mock-network-session","params":{"timestamp":1}}),
                ];
                for event in events {
                    ws.send(Message::Text(event.to_string().into()))
                        .await
                        .unwrap();
                }
            }
        }
    });

    let mcp = RustScraplingMcp::default();
    let opened = call(
        &mcp,
        "browser_open",
        json!({"session_id":"network","cdp_url":endpoint}),
    )
    .await;
    assert_eq!(opened.is_error, Some(false), "{opened:?}");
    let empty = call(
        &mcp,
        "browser_network_requests",
        json!({"session_id":"network"}),
    )
    .await;
    assert_eq!(content(&empty)["requests"].as_array().unwrap().len(), 0);

    for number in 1..=2 {
        let fetched = call(&mcp, "browser_fetch", json!({
            "session_id":"network","url":format!("http://example.test/page/{number}?sid=private")
        })).await;
        assert_eq!(fetched.is_error, Some(false), "{fetched:?}");
    }
    let first = call(
        &mcp,
        "browser_network_requests",
        json!({
            "session_id":"network","limit":1
        }),
    )
    .await;
    assert_eq!(first.is_error, Some(false), "{first:?}");
    let first_id = content(&first)["requests"][0]["id"].as_u64().unwrap();
    assert!(content(&first)["has_more"].as_bool().unwrap());
    assert_eq!(content(&first)["next_cursor"], first_id);
    assert_eq!(content(&first)["dropped_count"], 0);
    let all_types = call(
        &mcp,
        "browser_network_requests",
        json!({
            "session_id":"network","include_static":true,"limit":1
        }),
    )
    .await;
    let document_id = content(&all_types)["requests"][0]["id"].as_u64().unwrap();
    assert!(document_id < first_id);
    let document_body = call(
        &mcp,
        "browser_network_request",
        json!({
            "session_id":"network","request_id":document_id,"part":"response_body"
        }),
    )
    .await;
    // Upstream NetworkRequestModel shape: data=null plus an availability note.
    assert_eq!(content(&document_body)["part"], "response_body");
    assert!(content(&document_body)["data"].is_null());
    assert!(content(&document_body)["note"].is_string());
    let second = call(
        &mcp,
        "browser_network_requests",
        json!({
            "session_id":"network","after_id":first_id,"limit":1
        }),
    )
    .await;
    let second_id = content(&second)["requests"][0]["id"].as_u64().unwrap();
    assert!(second_id > first_id);
    assert_eq!(content(&second)["has_more"], false);
    let replay = call(
        &mcp,
        "browser_network_requests",
        json!({"session_id":"network"}),
    )
    .await;
    assert_eq!(content(&replay)["requests"][0]["id"], first_id);

    let response = call(
        &mcp,
        "browser_network_request",
        json!({
            "session_id":"network","request_id":first_id,"part":"response_body"
        }),
    )
    .await;
    assert_eq!(response.is_error, Some(false), "{response:?}");
    assert_eq!(content(&response)["request_id"], first_id);
    let body = content(&response)["data"].as_str().unwrap();
    assert!(body.contains("visible-xhr-1"));
    assert!(body.contains("[REDACTED]"));
    assert!(!body.contains("sensitive-token"));
    assert!(!body.contains("sensitive-password"));
    let summary = call(
        &mcp,
        "browser_network_request",
        json!({
            "session_id":"network","request_id":first_id,"part":"summary"
        }),
    )
    .await;
    assert_eq!(
        content(&summary)["data"]["url"],
        "http://example.test/api/1"
    );
    assert_eq!(content(&summary)["data"]["resource_type"], "xhr");
    assert_eq!(content(&summary)["data"]["id"], first_id);
    let headers = call(
        &mcp,
        "browser_network_request",
        json!({
            "session_id":"network","request_id":first_id,"part":"response_headers"
        }),
    )
    .await;
    let serialized = serde_json::to_string(content(&headers)).unwrap();
    assert!(!serialized.contains("private-auth"));
    assert!(!serialized.contains("private-cookie"));

    for (key, bad) in [
        ("limit", json!(0)),
        ("limit", json!(201)),
        ("limit", json!(-1)),
        ("limit", json!(1.5)),
        ("limit", json!(1e100)),
        ("after_id", json!(-1)),
        ("after_id", json!(9007199254740992_u64)),
        ("after_id", json!("NaN")),
    ] {
        let mut args = json!({"session_id":"network"});
        args[key] = bad;
        let invalid = call(&mcp, "browser_network_requests", args).await;
        assert_eq!(invalid.is_error, Some(true), "{key} must be rejected");
    }
    for bad in [json!(0), json!(-1), json!(1.5), json!(9007199254740992_u64)] {
        let invalid = call(
            &mcp,
            "browser_network_request",
            json!({
                "session_id":"network","request_id":bad
            }),
        )
        .await;
        assert_eq!(invalid.is_error, Some(true));
    }

    // Seventy document + XHR pairs exceed the 128-entry per-session cap.
    for number in 3..=70 {
        let fetched = call(
            &mcp,
            "browser_fetch",
            json!({
                "session_id":"network","url":format!("http://example.test/page/{number}")
            }),
        )
        .await;
        assert_eq!(fetched.is_error, Some(false), "fetch {number}: {fetched:?}");
    }
    let after_eviction = call(
        &mcp,
        "browser_network_requests",
        json!({
            "session_id":"network","limit":1
        }),
    )
    .await;
    assert_eq!(content(&after_eviction)["dropped_count"], 12);
    let retained_id = content(&after_eviction)["requests"][0]["id"]
        .as_u64()
        .unwrap();
    assert!(
        retained_id > second_id,
        "evicted ID must never be reassigned"
    );
    assert_eq!(content(&after_eviction)["has_more"], true);
    let evicted = call(
        &mcp,
        "browser_network_request",
        json!({
            "session_id":"network","request_id":first_id
        }),
    )
    .await;
    assert_eq!(
        evicted.is_error,
        Some(true),
        "evicted request should be unavailable"
    );
    let last_page = call(
        &mcp,
        "browser_network_requests",
        json!({
            "session_id":"network","limit":200
        }),
    )
    .await;
    assert_eq!(
        content(&last_page)["requests"].as_array().unwrap().len(),
        64
    );
    assert_eq!(content(&last_page)["has_more"], false);
    assert_eq!(content(&last_page)["dropped_count"], 12);
    let beyond_last = call(
        &mcp,
        "browser_network_requests",
        json!({
            "session_id":"network","after_id":10000,"limit":10
        }),
    )
    .await;
    assert_eq!(
        content(&beyond_last)["requests"].as_array().unwrap().len(),
        0
    );
    assert_eq!(content(&beyond_last)["next_cursor"], 10000);
    assert_eq!(content(&beyond_last)["has_more"], false);
    let only_last = call(
        &mcp,
        "browser_network_requests",
        json!({
            "session_id":"network","url_pattern":"/api/70$","limit":1
        }),
    )
    .await;
    assert_eq!(content(&only_last)["requests"].as_array().unwrap().len(), 1);
    assert_eq!(content(&only_last)["has_more"], false);
    let all = serde_json::to_string(content(&last_page)).unwrap();
    assert!(!all.contains("raw-secret"));
    assert!(!all.contains("private-cookie"));
    assert!(!all.contains("private-auth"));
    assert!(!all.contains("foreign"));
    assert_eq!(body_calls.lock().await.len(), 70);
    let closed = call(&mcp, "close_session", json!({"session_id":"network"})).await;
    assert_eq!(closed.is_error, Some(false), "{closed:?}");
    mock.abort();
}
