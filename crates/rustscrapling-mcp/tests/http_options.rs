//! make_request / session_make_request upstream arguments against a local
//! HTTP server: redirect policies (incl. "safe"), auth, params, cookies,
//! bodies, stealthy referer, retries and session cookie persistence.
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

fn content(result: &rmcp::model::CallToolResult) -> String {
    result.structured_content.as_ref().unwrap()["content"][0]
        .as_str()
        .unwrap()
        .to_owned()
}

/// `/echo` returns the raw request; `/redirect?to=X` answers 302 to X;
/// `/set` sets a cookie.
async fn serve(log: Arc<Mutex<Vec<String>>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let log = Arc::clone(&log);
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 16384];
                let mut size = 0;
                loop {
                    let Ok(read) = stream.read(&mut buffer[size..]).await else {
                        return;
                    };
                    size += read;
                    let text = String::from_utf8_lossy(&buffer[..size]).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let length = text
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if size >= end + 4 + length || read == 0 {
                            break;
                        }
                    }
                    if read == 0 {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&buffer[..size]).to_string();
                let target = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
                log.lock().unwrap().push(request.clone());
                let response = if let Some(to) = target.strip_prefix("/redirect?to=") {
                    let to = url::form_urlencoded::parse(to.as_bytes())
                        .next()
                        .map(|(k, _)| k.into_owned())
                        .unwrap_or_default();
                    format!("HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                } else if target.starts_with("/set") {
                    let body = "<p>set</p>";
                    format!("HTTP/1.1 200 OK\r\nSet-Cookie: sid=abc; Path=/\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
                } else {
                    let escaped = request.replace('&', "&amp;").replace('<', "&lt;");
                    let body = format!("<html><body><pre>{escaped}</pre></body></html>");
                    format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
                };
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    base
}

#[tokio::test(flavor = "multi_thread")]
async fn http_tools_honour_upstream_arguments() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let base = serve(Arc::clone(&log)).await;
    let handler = RustScraplingMcp::default();
    let request = |extra: Value| {
        let mut args =
            json!({"url": format!("{base}/echo"), "extraction_type": "text", "retries": 1});
        for (key, value) in extra.as_object().unwrap() {
            args[key] = value.clone();
        }
        args
    };

    // Basic auth, list params, cookies merged with a Cookie header, Google referer.
    let echoed = call(
        &handler,
        "make_request",
        request(json!({"auth":{"username":"user","password":"pass"},
            "params":{"q":["a","b"],"n":1},"headers":{"Cookie":"pre=1","X-Null":null},
            "cookies":{"c":"2"}})),
    )
    .await;
    assert_eq!(echoed.is_error, Some(false), "{echoed:?}");
    let body = content(&echoed);
    assert!(body.contains("GET /echo?q=a&q=b&n=1 "), "{body}");
    assert!(
        body.contains("authorization: Basic dXNlcjpwYXNz")
            || body.contains("Authorization: Basic dXNlcjpwYXNz"),
        "{body}"
    );
    assert!(
        body.to_ascii_lowercase().contains("cookie: pre=1; c=2"),
        "{body}"
    );
    assert!(
        body.to_ascii_lowercase()
            .contains("referer: https://www.google.com/"),
        "{body}"
    );
    let bad_auth = call(
        &handler,
        "make_request",
        request(json!({"auth":{"username":"u"}})),
    )
    .await;
    assert!(text(&bad_auth).contains("must contain both 'username' and 'password'"));

    // stealthy_headers=false sends no synthetic referer.
    let plain = call(
        &handler,
        "make_request",
        request(json!({"stealthy_headers":false})),
    )
    .await;
    assert!(!content(&plain).to_ascii_lowercase().contains("referer:"));

    // Bodies: JSON, form data, raw string; ignored for GET.
    let posted = call(
        &handler,
        "make_request",
        request(json!({"method":"POST","json":{"k":[1,2]}})),
    )
    .await;
    let body = content(&posted);
    assert!(
        body.contains("{\"k\":[1,2]}")
            && body
                .to_ascii_lowercase()
                .contains("content-type: application/json"),
        "{body}"
    );
    let form = call(
        &handler,
        "make_request",
        request(json!({"method":"PUT","data":{"a":"1 2","b":"&"}})),
    )
    .await;
    assert!(content(&form).contains("a=1+2&b=%26"), "{}", content(&form));
    let get_body = call(&handler, "make_request", request(json!({"data":"ignored"}))).await;
    assert!(!content(&get_body).contains("ignored"));

    // Redirects: "safe" refuses loopback targets, True follows, False returns 302.
    let redirect = format!(
        "{base}/redirect?to={}",
        url::form_urlencoded::byte_serialize(format!("{base}/echo").as_bytes()).collect::<String>()
    );
    let safe = call(
        &handler,
        "make_request",
        json!({"url":redirect,"retries":1}),
    )
    .await;
    assert_eq!(safe.is_error, Some(true));
    assert!(
        text(&safe).contains("internal or private"),
        "{}",
        text(&safe)
    );
    let follow = call(
        &handler,
        "make_request",
        json!({"url":redirect,"follow_redirects":true,"extraction_type":"text"}),
    )
    .await;
    assert_eq!(follow.is_error, Some(false), "{follow:?}");
    assert_eq!(follow.structured_content.as_ref().unwrap()["status"], 200);
    assert!(follow.structured_content.as_ref().unwrap()["url"]
        .as_str()
        .unwrap()
        .ends_with("/echo"));
    let manual = call(
        &handler,
        "make_request",
        json!({"url":redirect,"follow_redirects":false}),
    )
    .await;
    assert_eq!(manual.structured_content.as_ref().unwrap()["status"], 302);
    let limited = call(
        &handler,
        "make_request",
        json!({"url":redirect,"follow_redirects":true,"max_redirects":0}),
    )
    .await;
    assert_eq!(limited.is_error, Some(true), "{limited:?}");

    // Session: cookies persist across requests; impersonation=None accepted.
    let opened = call(
        &handler,
        "open_request_session",
        json!({"session_id":"h","impersonate":null}),
    )
    .await;
    assert_eq!(opened.is_error, Some(false), "{opened:?}");
    let set = call(
        &handler,
        "session_make_request",
        json!({"session_id":"h","url":format!("{base}/set")}),
    )
    .await;
    assert_eq!(set.is_error, Some(false), "{set:?}");
    let next = call(
        &handler,
        "session_make_request",
        json!({"session_id":"h","url":format!("{base}/echo"),"extraction_type":"text"}),
    )
    .await;
    assert!(
        content(&next)
            .to_ascii_lowercase()
            .contains("cookie: sid=abc"),
        "{}",
        content(&next)
    );
    let fresh = call(&handler, "make_request", request(json!({}))).await;
    assert!(!content(&fresh).contains("sid=abc"));

    // Transport failures are retried and reported as typed errors.
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead = format!("http://{}/", closed.local_addr().unwrap());
    drop(closed);
    let failed = call(
        &handler,
        "make_request",
        json!({"url":dead,"retries":2,"retry_delay":0}),
    )
    .await;
    assert_eq!(failed.is_error, Some(true));
    let invalid = call(
        &handler,
        "make_request",
        json!({"url":dead,"impersonate":"netscape4"}),
    )
    .await;
    assert!(text(&invalid).contains("impersonate"), "{}", text(&invalid));
}
