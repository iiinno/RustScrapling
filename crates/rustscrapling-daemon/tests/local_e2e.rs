use rustscrapling_daemon::{
    DaemonClient, DaemonConfig, DaemonError, DaemonServer, FetchBackend, FetchFuture, FetchRequest,
    FetchResult, FetchTier,
};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Default)]
struct FixtureBackend {
    seen: Mutex<Vec<FetchRequest>>,
}

impl FetchBackend for FixtureBackend {
    fn fetch<'a>(&'a self, req: FetchRequest) -> FetchFuture<'a> {
        Box::pin(async move {
            self.seen.lock().unwrap().push(req.clone());
            tokio::time::sleep(Duration::from_millis(70)).await;
            let is_304 = req
                .headers
                .get("If-None-Match")
                .is_some_and(|v| v == "\"v1\"");
            Ok(FetchResult {
                status: if is_304 { 304 } else { 200 },
                final_url: req.url,
                headers: vec![
                    ("ETag".into(), "\"v1\"".into()),
                    (
                        "Last-Modified".into(),
                        "Sun, 01 Nov 2026 09:00:00 GMT".into(),
                    ),
                ],
                body: if is_304 {
                    vec![]
                } else {
                    b"<main>fixture content</main>".to_vec()
                },
                tier: FetchTier::Http,
                protocol: Some("HTTP/1.1".into()),
            })
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn localhost_authenticated_ipc_singleflight_revalidation_rate_and_shutdown() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "rustscrapling-daemon-e2e-{}-{stamp}",
        std::process::id()
    ));
    let config = DaemonConfig {
        state_file: dir.join("state.json"),
        site_interval: Duration::from_millis(80),
        learning_ttl: Duration::from_millis(1000),
        request_timeout: Duration::from_secs(5),
    };
    let backend = Arc::new(FixtureBackend::default());
    let server = DaemonServer::start(config.clone(), backend.clone())
        .await
        .unwrap();
    assert!(server.address().starts_with("127.0.0.1:"));
    let client = DaemonClient::discover(&config.state_file)
        .await
        .unwrap()
        .unwrap();
    // A second daemon must not take over an existing state file.
    assert!(DaemonServer::start(config.clone(), backend.clone())
        .await
        .is_err());
    let state = std::fs::read_to_string(&config.state_file).unwrap();
    let token = serde_json::from_str::<serde_json::Value>(&state).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(token.len(), 64);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&config.state_file)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    let wrong_path = dir.join("wrong.json");
    std::fs::write(&wrong_path, state.replace(&token, &"a".repeat(64))).unwrap();
    let wrong = DaemonClient::from_state_file(&wrong_path).unwrap();
    assert_eq!(wrong.status().await.unwrap_err().kind, "unauthorized");

    let url = "http://127.0.0.1:18888/article";
    let invalid_url = client
        .fetch(FetchRequest::get("file:///etc/passwd"))
        .await
        .unwrap_err();
    assert_eq!(invalid_url.kind, "invalid_request");
    let mut invalid_method = FetchRequest::get(url);
    invalid_method.method = "PATCH".into();
    assert_eq!(
        client.fetch(invalid_method).await.unwrap_err().kind,
        "invalid_request"
    );
    assert_eq!(backend.seen.lock().unwrap().len(), 0);

    let (a, b, c) = tokio::join!(
        client.fetch(FetchRequest::get(url)),
        client.fetch(FetchRequest::get(url)),
        client.fetch(FetchRequest::get(url)),
    );
    for response in [a.unwrap(), b.unwrap(), c.unwrap()] {
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"<main>fixture content</main>");
    }
    assert_eq!(backend.seen.lock().unwrap().len(), 1);

    let response = client.fetch(FetchRequest::get(url)).await.unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"<main>fixture content</main>");
    let recorded = backend.seen.lock().unwrap().clone();
    assert_eq!(recorded.len(), 2);
    assert_eq!(
        recorded[1].headers.get("If-None-Match").map(String::as_str),
        Some("\"v1\"")
    );
    assert!(recorded[1].headers.contains_key("If-Modified-Since"));

    let start = tokio::time::Instant::now();
    let (one, two) = tokio::join!(
        client.fetch(FetchRequest::get("http://127.0.0.1:18888/other")),
        client.fetch(FetchRequest::get("http://127.0.0.1:18888/third")),
    );
    assert_eq!(one.unwrap().status, 200);
    assert_eq!(two.unwrap().status, 200);
    assert!(start.elapsed() >= Duration::from_millis(80));
    let status = client.status().await.unwrap();
    assert_eq!(status.requests, 6);
    assert!(status.coalesced_requests >= 2);
    assert_eq!(status.upstream_requests, 4);
    assert_eq!(status.learned_sites, 1);

    // Identical non-idempotent requests must be sent separately.
    let mut post = FetchRequest::get("http://127.0.0.1:18888/update");
    post.method = "POST".into();
    post.body = Some(b"change".to_vec());
    let (first, second) = tokio::join!(client.fetch(post.clone()), client.fetch(post));
    assert_eq!(first.unwrap().status, 200);
    assert_eq!(second.unwrap().status, 200);
    assert_eq!(backend.seen.lock().unwrap().len(), 6);

    tokio::time::sleep(Duration::from_millis(1050)).await;
    let _ = client.fetch(FetchRequest::get(url)).await.unwrap();
    let latest = backend.seen.lock().unwrap().last().unwrap().clone();
    assert!(!latest.headers.contains_key("If-None-Match"));

    client.stop().await.unwrap();
    server.wait().await.unwrap();
    assert!(!config.state_file.exists());
    assert!(DaemonClient::discover(&config.state_file)
        .await
        .unwrap()
        .is_none());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn stable_wire_request_and_typed_error() {
    let req = FetchRequest::get("https://example.net/");
    let copy: FetchRequest = serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
    assert_eq!(req, copy);
    assert_eq!(copy.tier, FetchTier::Auto);
    assert_eq!(
        DaemonError::new("invalid_request", "broken").kind,
        "invalid_request"
    );
}

#[tokio::test]
async fn discover_returns_none_without_state_file() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "rustscrapling-daemon-missing-{}-{stamp}.json",
        std::process::id()
    ));
    assert!(DaemonClient::discover(&path).await.unwrap().is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn existing_shared_directory_is_not_chmodded() {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::temp_dir();
    let before = std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
    if before & 0o077 == 0 {
        return;
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let state = path.join(format!(
        "rustscrapling-daemon-should-not-create-{stamp}.json"
    ));
    let cfg = DaemonConfig {
        state_file: state.clone(),
        ..DaemonConfig::default()
    };
    let failure = DaemonServer::start(cfg, Arc::new(FixtureBackend::default()))
        .await
        .err()
        .expect("shared temp directory must be rejected");
    assert_eq!(failure.kind, "permission");
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
        before
    );
    assert!(!state.exists());
}
