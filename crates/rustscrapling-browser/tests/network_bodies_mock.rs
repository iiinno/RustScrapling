//! CDP network response bodies, including request/session correlation and
//! credential redaction. No browser executable or external network is needed.
use futures_util::{SinkExt, StreamExt};
use rustscrapling_browser::BrowserClient;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio_tungstenite::{accept_async, tungstenite::Message};

#[tokio::test]
async fn text_api_bodies_are_bounded_and_scoped_to_the_page_session() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let body_requests = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = body_requests.clone();
    let mock = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        while let Some(Ok(Message::Text(text))) = ws.next().await {
            let request: Value = serde_json::from_str(&text).unwrap();
            let method = request["method"].as_str().unwrap();
            let result = match method {
                "Target.createTarget" => json!({"targetId":"test-tab"}),
                "Target.attachToTarget" => json!({"sessionId":"page-a"}),
                "Page.enable" | "Network.enable" | "Target.closeTarget" => json!({}),
                "Page.navigate" => json!({}),
                "Network.getResponseBody" => {
                    assert_eq!(request["sessionId"], "page-a");
                    let id = request["params"]["requestId"].as_str().unwrap();
                    seen.lock().await.push(id.to_owned());
                    match id {
                        "api-json" => json!({"base64Encoded":false,"body":
                            r#"{"data":"safe-visible","access_token":"super-secret","nested":{"password":"p4ss"},"plain":"Bearer example"}"#}),
                        "api-text" => json!({"base64Encoded":false,"body":"hello from API"}),
                        "api-secret" => {
                            json!({"base64Encoded":false,"body":"Authorization: Bearer private"})
                        }
                        "api-base64" => json!({"base64Encoded":true,"body":"YmluYXJ5"}),
                        _ => panic!("body should never be requested for {id}"),
                    }
                }
                other => panic!("unexpected CDP command: {other}"),
            };
            ws.send(Message::Text(
                json!({"id":request["id"],"result":result})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            if method == "Page.navigate" {
                let make_response = |id: &str, kind: &str, mime: &str, url: &str| {
                    let mut headers =
                        json!({"Content-Type":mime,"Content-Length":"99","Server":"fixture"});
                    if id == "document" {
                        headers["Set-Cookie"] = json!("session=cookie-secret");
                        headers["Authorization"] = json!("Bearer auth-secret");
                    }
                    if id == "auth-header" {
                        headers["WWW-Authenticate"] = json!("Bearer secret");
                    }
                    json!({"method":"Network.responseReceived","sessionId":"page-a","params":{
                        "requestId":id,"type":kind,"response":{"url":url,"status":200,"mimeType":mime,
                        "headers":headers}
                    }})
                };
                let finished = |id: &str, len: u64| {
                    json!({"method":"Network.loadingFinished","sessionId":"page-a","params":{
                        "requestId":id,"encodedDataLength":len
                    }})
                };
                let events = vec![
                    // A valid-looking response in a different CDP session is never ours.
                    json!({"method":"Network.responseReceived","sessionId":"page-b","params":{
                        "requestId":"foreign","type":"XHR","response":{"url":"https://private.test/?secret=other","status":200,"mimeType":"application/json"}
                    }}),
                    json!({"method":"Network.loadingFinished","sessionId":"page-b","params":{"requestId":"foreign","encodedDataLength":20}}),
                    make_response(
                        "document",
                        "Document",
                        "text/html",
                        "http://example.test/home",
                    ),
                    make_response(
                        "api-json",
                        "XHR",
                        "application/json",
                        "http://user:pass@example.test/api/list?token=raw-secret#secret",
                    ),
                    make_response(
                        "api-text",
                        "Fetch",
                        "text/plain",
                        "http://example.test/api/text",
                    ),
                    make_response(
                        "api-secret",
                        "Fetch",
                        "text/plain",
                        "http://example.test/api/plain",
                    ),
                    make_response(
                        "api-base64",
                        "Fetch",
                        "application/json",
                        "http://example.test/api/encoded",
                    ),
                    make_response("binary", "Fetch", "image/png", "http://example.test/img"),
                    make_response(
                        "huge",
                        "XHR",
                        "application/json",
                        "http://example.test/huge",
                    ),
                    make_response(
                        "auth",
                        "XHR",
                        "application/json",
                        "http://example.test/oauth/token",
                    ),
                    make_response(
                        "auth-header",
                        "XHR",
                        "application/json",
                        "http://example.test/api/setcookie",
                    ),
                    make_response(
                        "failed",
                        "XHR",
                        "application/json",
                        "http://example.test/api/fail",
                    ),
                    finished("api-json", 80),
                    finished("api-text", 20),
                    finished("api-secret", 50),
                    finished("api-base64", 8),
                    finished("binary", 10),
                    finished("huge", 65537),
                    finished("auth", 10),
                    finished("auth-header", 10),
                    json!({"method":"Network.loadingFailed","sessionId":"page-a","params":{
                        "requestId":"failed","errorText":"net::ERR_ABORTED"
                    }}),
                    json!({"method":"Page.frameNavigated","sessionId":"page-a","params":{"frame":{"id":"main"}}}),
                    json!({"method":"Page.loadEventFired","sessionId":"page-a","params":{"timestamp":1}}),
                ];
                for event in events {
                    ws.send(Message::Text(event.to_string().into()))
                        .await
                        .unwrap();
                }
            }
        }
    });

    let browser = BrowserClient::connect(&endpoint).await.unwrap();
    let page = browser.open_page(true).await.unwrap();
    page.navigate("http://example.test/home", Duration::from_secs(5))
        .await
        .unwrap();
    let found = page.search_network_bodies("safe-visible").await;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].request_id.as_deref(), Some("api-json"));
    let history = page.network_records().await;
    let json_record = history
        .iter()
        .find(|r| r.request_id.as_deref() == Some("api-json"))
        .unwrap();
    assert_eq!(json_record.url, "http://example.test/api/list");
    assert_eq!(json_record.headers.len(), 3);
    let json_body = json_record.body.as_ref().unwrap();
    assert!(json_body.contains("safe-visible"));
    assert!(json_body.contains("[REDACTED]"));
    assert!(!json_body.contains("super-secret"));
    assert!(!json_body.contains("p4ss"));
    assert!(!json_body.contains("Bearer example"));
    assert_eq!(
        history
            .iter()
            .find(|r| r.request_id.as_deref() == Some("api-text"))
            .and_then(|r| r.body.as_deref()),
        Some("hello from API")
    );
    for id in [
        "binary",
        "huge",
        "auth",
        "auth-header",
        "api-secret",
        "api-base64",
        "failed",
    ] {
        assert!(
            history
                .iter()
                .find(|r| r.request_id.as_deref() == Some(id))
                .unwrap()
                .body
                .is_none(),
            "unexpected body for {id}"
        );
    }
    assert_eq!(
        history
            .iter()
            .find(|r| r.request_id.as_deref() == Some("failed"))
            .and_then(|r| r.failed.as_deref()),
        Some("net::ERR_ABORTED")
    );
    assert!(!history
        .iter()
        .any(|r| r.request_id.as_deref() == Some("foreign")));
    let serialized = serde_json::to_string(&history).unwrap();
    for secret in [
        "raw-secret",
        "cookie-secret",
        "auth-secret",
        "super-secret",
        "p4ss",
        "private.test",
    ] {
        assert!(
            !serialized.contains(secret),
            "network history leaked {secret}"
        );
    }
    let requested = body_requests.lock().await.clone();
    assert_eq!(
        requested.len(),
        4,
        "only eligible API requests get a CDP body call: {requested:?}"
    );
    assert!(requested.contains(&"api-json".into()));
    assert!(requested.contains(&"api-text".into()));
    assert!(requested.contains(&"api-secret".into()));
    assert!(requested.contains(&"api-base64".into()));
    let taken = page.take_network().await;
    assert_eq!(taken.len(), history.len());
    assert!(page.network_records().await.is_empty());
    assert!(!browser
        .commands()
        .await
        .iter()
        .any(|name| name == "Runtime.enable"));
    page.close().await.unwrap();
    mock.abort();
}
