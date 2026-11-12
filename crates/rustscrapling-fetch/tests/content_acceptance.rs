use rustscrapling_fetch::{FetchOptions, FetchTier, Fetcher};
use rustscrapling_http::{HttpClient, HttpOptions, HttpRequest};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

struct HttpFixture {
    url: String,
    requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl HttpFixture {
    fn new(status: u16, content_type: &str, body: &[u8]) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/fixture", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&requests);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let mut response = format!(
            "HTTP/1.1 {status} Fixture\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !stopping.load(Ordering::SeqCst) && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        socket
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        socket
                            .set_write_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut request = Vec::new();
                        let mut buffer = [0; 1024];
                        while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                            let size = socket.read(&mut buffer).unwrap();
                            assert!(size > 0, "client closed before sending HTTP headers");
                            request.extend_from_slice(&buffer[..size]);
                            assert!(request.len() <= 16384, "fixture received oversized headers");
                        }
                        count.fetch_add(1, Ordering::SeqCst);
                        socket.write_all(&response).unwrap();
                        return;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("fixture accept failed: {error}"),
                }
            }
        });
        Self {
            url,
            requests,
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let joined = worker.join();
            if !thread::panicking() {
                joined.expect("HTTP fixture thread failed");
            }
        }
    }
}

async fn check_content(status: u16, content_type: &str, body: &[u8], usable: bool) {
    let fixture = HttpFixture::new(status, content_type, body);
    let client = HttpClient::new(HttpOptions {
        direct: true,
        retries: 0,
        timeout: Duration::from_secs(3),
        ..Default::default()
    })
    .unwrap();
    let fetcher = Fetcher::new(client, FetchOptions::default());
    let result = fetcher
        .fetch(HttpRequest::get(&fixture.url), FetchTier::Auto)
        .await;
    let accepted = result.as_ref().is_ok_and(|response| response.success());
    let learned = fetcher.learned_tier(&fixture.url).await;
    let baseline =
        std::env::var("RUSTSCRAPLING_U3_EXPECT_BASELINE").is_ok_and(|value| value == "1");
    assert_eq!(fixture.requests.load(Ordering::SeqCst), 1);
    if usable || baseline {
        assert!(
            accepted,
            "expected usable control or old false success, got {result:?}"
        );
        assert_eq!(learned, Some(FetchTier::Http));
    } else {
        assert!(!accepted, "unusable content was returned as fetch success");
        assert_eq!(learned, None, "unusable content poisoned the learned tier");
    }
}

#[tokio::test]
async fn short_plain_content_is_success_and_learned() {
    check_content(200, "text/plain; charset=utf-8", b"OK", true).await;
}

#[tokio::test]
async fn short_html_content_is_success_and_learned() {
    check_content(
        200,
        "text/html; charset=utf-8",
        b"<html><body><p>OK</p></body></html>",
        true,
    )
    .await;
}

#[tokio::test]
async fn unicode_plain_content_is_success_and_learned() {
    check_content(
        200,
        "text/plain; charset=utf-8",
        "研究結果🦀。".as_bytes(),
        true,
    )
    .await;
}

#[tokio::test]
async fn empty_body_is_not_success_or_learned() {
    check_content(200, "text/plain; charset=utf-8", b"", false).await;
}

#[tokio::test]
async fn unicode_whitespace_is_not_success_or_learned() {
    check_content(
        200,
        "text/plain; charset=utf-8",
        " \t\n\u{3000}\u{a0}".as_bytes(),
        false,
    )
    .await;
}

#[tokio::test]
async fn no_content_status_is_not_success_or_learned() {
    check_content(204, "text/plain; charset=utf-8", b"", false).await;
}

#[tokio::test]
async fn javascript_shell_is_not_success_or_learned() {
    check_content(200, "text/html; charset=utf-8", b"<!doctype html><html><body><div id='app'></div><script src='/app.js'></script></body></html>", false).await;
}

#[tokio::test]
async fn metadata_only_is_not_success_or_learned() {
    check_content(
        200,
        "text/html; charset=utf-8",
        b"<!doctype html><html><head><title>Only metadata</title></head><body></body></html>",
        false,
    )
    .await;
}

#[tokio::test]
async fn invalid_declared_utf8_is_not_success_or_learned() {
    check_content(
        200,
        "text/plain; charset=utf-8",
        b"\x93\xfa\x96\x7b\x8c\xea",
        false,
    )
    .await;
}
