use rustscrapling_http::{ErrorKind, HttpClient, HttpOptions, HttpRequest, Protocol, ProxyRotator};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Local server reads a complete request (headers and Content-Length body) and
/// supplies independent responses for repeated requests and proxy tests.
async fn server<F>(respond: F) -> String
where
    F: Fn(&[u8]) -> Vec<u8> + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let respond = Arc::new(respond);
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let respond = respond.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let n = match socket.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    request.extend_from_slice(&chunk[..n]);
                    if let Some(pos) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&request[..pos]);
                        let len = head
                            .lines()
                            .find_map(|line| {
                                let (key, value) = line.split_once(':')?;
                                key.eq_ignore_ascii_case("Content-Length")
                                    .then(|| value.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if request.len() >= pos + 4 + len {
                            break;
                        }
                    }
                    if request.len() > 1024 * 1024 {
                        return;
                    }
                }
                let bytes = respond(&request);
                let _ = socket.write_all(&bytes).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    format!("http://{addr}")
}

fn reply(status: &str, body: &[u8], extra: &str) -> Vec<u8> {
    let mut bytes = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
        body.len()
    )
    .into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

fn direct_options() -> HttpOptions {
    HttpOptions {
        direct: true,
        impersonate: None,
        retries: 0,
        ..HttpOptions::default()
    }
}

#[tokio::test]
async fn embedded_url_credentials_are_rejected_without_leaking_secret_values() {
    let client = HttpClient::new(direct_options()).unwrap();
    let error = client
        .get("http://alice:very-secret@127.0.0.1:1/private")
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidRequest);
    assert!(!error.to_string().contains("very-secret"));
    assert!(!error.to_string().contains("alice"));
}

#[test]
fn excessive_retry_count_is_rejected_before_requests() {
    let options = HttpOptions {
        retries: u32::MAX,
        ..direct_options()
    };
    let error = HttpClient::new(options).err().expect("must reject");
    assert_eq!(error.kind, ErrorKind::InvalidRequest);
}

#[tokio::test]
async fn headers_methods_binary_body_and_response_headers() {
    let requests = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let captured = requests.clone();
    let base = server(move |r| {
        captured.lock().unwrap().push(r.to_vec());
        reply(
            "201 Created",
            &[0, 255, 1],
            "X-Test: first\r\nX-Test: second\r\n",
        )
    })
    .await;
    let client = HttpClient::new(direct_options()).unwrap();
    for method in ["GET", "POST", "PUT", "DELETE"] {
        let request = HttpRequest::new(method, &base)
            .header("X-API-Key", "one")
            .body([0, 1, 255].to_vec());
        let response = client.request(request).await.unwrap();
        assert_eq!(response.status, 201);
        assert_eq!(response.body, [0, 255, 1]);
        assert_eq!(response.http_version, "HTTP/1.1");
        assert_eq!(
            response
                .headers
                .iter()
                .filter(|(k, _)| k == "x-test")
                .count(),
            2
        );
        assert_eq!(response.attempts, 1);
    }
    let all = requests.lock().unwrap();
    assert_eq!(all.len(), 4);
    for (request, method) in all.iter().zip(["GET", "POST", "PUT", "DELETE"]) {
        let header = String::from_utf8_lossy(request);
        assert!(header.starts_with(method));
        assert!(header.to_ascii_lowercase().contains("x-api-key: one"));
        assert!(request.ends_with(&[0, 1, 255]));
    }
}

#[tokio::test]
async fn redirects_are_optional_and_statuses_are_classified() {
    let base = server(|r| {
        if r.starts_with(b"GET /from") {
            reply("302 Found", b"", "Location: /end\r\n")
        } else {
            reply("403 Forbidden", b"blocked", "")
        }
    })
    .await;
    let client = HttpClient::new(direct_options()).unwrap();
    let followed = client.get(format!("{base}/from")).await.unwrap();
    assert_eq!(followed.status, 403);
    assert!(followed.final_url.ends_with("/end"));
    assert_eq!(
        followed.require_success().unwrap_err().kind,
        ErrorKind::Blocked
    );

    let options = HttpOptions {
        follow_redirects: false,
        ..direct_options()
    };
    let raw = HttpClient::new(options)
        .unwrap()
        .get(format!("{base}/from"))
        .await
        .unwrap();
    assert_eq!(raw.status, 302);
    assert!(raw.final_url.ends_with("/from"));
}

#[tokio::test]
async fn retries_transient_get_but_not_post_by_default() {
    let count = Arc::new(AtomicUsize::new(0));
    let captured = count.clone();
    let base = server(move |_| {
        let n = captured.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            reply("503 Service Unavailable", b"retry", "")
        } else {
            reply("200 OK", b"pass", "")
        }
    })
    .await;
    let client = HttpClient::new(HttpOptions {
        retries: 2,
        ..direct_options()
    })
    .unwrap();
    let response = client.get(base).await.unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"pass");
    assert_eq!(response.attempts, 2);
    assert_eq!(count.load(Ordering::SeqCst), 2);

    let post_count = Arc::new(AtomicUsize::new(0));
    let other = post_count.clone();
    let url = server(move |_| {
        other.fetch_add(1, Ordering::SeqCst);
        reply("503 Service Unavailable", b"", "")
    })
    .await;
    let response = client.request(HttpRequest::new("POST", url)).await.unwrap();
    assert_eq!(response.status, 503);
    assert_eq!(response.attempts, 1);
    assert_eq!(post_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn limits_work_with_content_length_and_streamed_body() {
    let too_large_known = server(|_| reply("200 OK", b"0123456789", "")).await;
    let client = HttpClient::new(HttpOptions {
        max_body_bytes: 5,
        ..direct_options()
    })
    .unwrap();
    assert_eq!(
        client.get(too_large_known).await.unwrap_err().kind,
        ErrorKind::BodyTooLarge
    );

    let streamed = server(|_| b"HTTP/1.1 200 OK\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n\r\n6\r\n123456\r\n0\r\n\r\n".to_vec()).await;
    assert_eq!(
        client.get(streamed).await.unwrap_err().kind,
        ErrorKind::BodyTooLarge
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timeout_and_configuration_errors_are_typed() {
    let base = server(|_| {
        std::thread::sleep(Duration::from_millis(200));
        reply("200 OK", b"late", "")
    })
    .await;
    let options = HttpOptions {
        timeout: Duration::from_millis(40),
        read_timeout: Duration::from_millis(40),
        retries: 0,
        ..direct_options()
    };
    assert_eq!(
        HttpClient::new(options)
            .unwrap()
            .get(base)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Timeout
    );
    assert_eq!(
        HttpClient::new(direct_options())
            .unwrap()
            .get("file:///etc/passwd")
            .await
            .unwrap_err()
            .kind,
        ErrorKind::InvalidRequest
    );
    let options = HttpOptions {
        protocol: Protocol::Auto,
        max_body_bytes: 0,
        ..direct_options()
    };
    assert_eq!(
        HttpClient::new(options).err().unwrap().kind,
        ErrorKind::InvalidRequest
    );
    let options = HttpOptions {
        impersonate: Some("nonsense_999".into()),
        ..direct_options()
    };
    assert_eq!(
        HttpClient::new(options).err().unwrap().kind,
        ErrorKind::InvalidRequest
    );
}

#[tokio::test]
async fn authenticated_proxy_rotation_uses_both_upstreams() {
    let captured = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let a = captured.clone();
    let proxy_a = server(move |r| {
        a.lock().unwrap().push(r.to_vec());
        reply("200 OK", b"a", "")
    })
    .await;
    let b = captured.clone();
    let proxy_b = server(move |r| {
        b.lock().unwrap().push(r.to_vec());
        reply("200 OK", b"b", "")
    })
    .await;
    let proxies = vec![
        proxy_a.replacen("http://", "http://alice:secret@", 1),
        proxy_b,
    ];
    let options = HttpOptions {
        impersonate: None,
        proxy_rotator: Some(ProxyRotator::new(proxies).unwrap()),
        retries: 0,
        ..HttpOptions::default()
    };
    let client = HttpClient::new(options).unwrap();
    let first = client.get("http://example.invalid/check").await.unwrap();
    let second = client.get("http://example.invalid/check").await.unwrap();
    assert_eq!(first.body, b"a");
    assert_eq!(second.body, b"b");
    let all = captured.lock().unwrap();
    assert!(String::from_utf8_lossy(&all[0]).starts_with("GET http://example.invalid/check"));
    assert!(String::from_utf8_lossy(&all[0])
        .to_ascii_lowercase()
        .contains(
            "proxy-authorization: basic ywxpY2u6c2vjcmv0"
                .to_ascii_lowercase()
                .as_str()
        ));
}
