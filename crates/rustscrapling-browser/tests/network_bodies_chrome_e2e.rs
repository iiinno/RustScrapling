//! Opt-in Chrome E2E for genuine network body capture (not a CDP mock).
//! Run only with RUSTSCRAPLING_CHROME_FOR_TESTING=/path/to/chrome and --ignored.
use rustscrapling_browser::{BrowserClient, LaunchOptions};
use std::{path::PathBuf, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a dedicated real Chrome binary"]
async fn real_chrome_captures_sanitized_xhr_json_body() {
    let chrome = PathBuf::from(
        std::env::var_os("RUSTSCRAPLING_CHROME_FOR_TESTING")
            .expect("set RUSTSCRAPLING_CHROME_FOR_TESTING to a real Chrome executable"),
    );
    assert!(chrome.is_file());
    let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = server.local_addr().unwrap();
    let server_task = tokio::spawn(async move {
        while let Ok((mut connection, _)) = server.accept().await {
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buffer = [0u8; 2048];
                while request.len() < 8192 && !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    let Ok(Ok(count)) =
                        tokio::time::timeout(Duration::from_secs(3), connection.read(&mut buffer))
                            .await
                    else {
                        return;
                    };
                    if count == 0 {
                        return;
                    }
                    request.extend_from_slice(&buffer[..count]);
                }
                let is_api = request.starts_with(b"GET /api/public?");
                let body = if is_api {
                    r#"{"data":"visible-from-real-cdp","access_token":"very-sensitive"}"#
                } else {
                    r#"<html><body><h1>XHR fixture</h1><script>
                        fetch('/api/public?token=private-query').then(r=>r.json())
                          .then(v=>{document.title=v.data});
                    </script></body></html>"#
                };
                let content_type = if is_api {
                    "application/json"
                } else {
                    "text/html; charset=utf-8"
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = connection.write_all(response.as_bytes()).await;
                let _ = connection.shutdown().await;
            });
        }
    });
    let mut options = LaunchOptions::new(chrome);
    options.extra_args.push("--no-proxy-server".into());
    // This test launches a disposable browser profile. Never use this
    // unencrypted credential-store setting for a persistent user profile.
    options.extra_args.push("--password-store=basic".into());
    if std::env::var_os("RUSTSCRAPLING_CHROME_TEST_NO_SANDBOX").is_some() {
        options.extra_args.push("--no-sandbox".into());
    }
    let client = BrowserClient::launch(options).await.unwrap();
    let page = client.open_page(true).await.unwrap();
    page.navigate(&format!("http://{addr}/"), Duration::from_secs(15))
        .await
        .unwrap();
    let found = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let records = page.search_network_bodies("visible-from-real-cdp").await;
            if let Some(record) = records.into_iter().next() {
                break record;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("real Chrome XHR body not captured");
    assert!(matches!(
        found.resource_type.as_deref(),
        Some("Fetch" | "XHR")
    ));
    assert_eq!(found.url, format!("http://{addr}/api/public"));
    let body = found.body.expect("body must be present");
    assert!(body.contains("visible-from-real-cdp"));
    assert!(body.contains("[REDACTED]"));
    assert!(!body.contains("very-sensitive"));
    assert!(!serde_json::to_string(&page.network_records().await)
        .unwrap()
        .contains("private-query"));
    assert!(!client
        .commands()
        .await
        .iter()
        .any(|cmd| cmd == "Runtime.enable"));
    page.close().await.unwrap();
    client.shutdown().await.unwrap();
    server_task.abort();
}
