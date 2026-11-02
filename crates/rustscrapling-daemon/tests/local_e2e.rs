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

struct Private304Backend;

impl FetchBackend for Private304Backend {
    fn fetch<'a>(&'a self, request: FetchRequest) -> FetchFuture<'a> {
        Box::pin(async move {
            let revalidated = request.headers.contains_key("If-None-Match");
            let mut headers = vec![("ETag".into(), "\"v1\"".into())];
            if revalidated {
                headers.push(("Cache-Control".into(), "private, no-store".into()));
            }
            Ok(FetchResult {
                status: if revalidated { 304 } else { 200 },
                final_url: request.url,
                headers,
                body: if revalidated {
                    Vec::new()
                } else {
                    b"initial".to_vec()
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
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrong_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
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

#[tokio::test]
async fn cache_and_learning_survive_restart_but_credentials_do_not() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "rustscrapling-persistent-{}-{stamp}",
        std::process::id()
    ));
    let config = DaemonConfig {
        state_file: dir.join("state.json"),
        site_interval: Duration::ZERO,
        learning_ttl: Duration::from_secs(5),
        request_timeout: Duration::from_secs(5),
    };
    let url = "http://example.test/article";
    let first_backend = Arc::new(FixtureBackend::default());
    let first = DaemonServer::start(config.clone(), first_backend)
        .await
        .unwrap();
    let client = DaemonClient::from_state_file(&config.state_file).unwrap();
    assert_eq!(
        client.fetch(FetchRequest::get(url)).await.unwrap().status,
        200
    );
    let mut private = FetchRequest::get("http://example.test/private");
    private.headers.insert(
        "Authorization".into(),
        "Bearer private-test-token-123".into(),
    );
    assert_eq!(client.fetch(private).await.unwrap().status, 200);
    assert_eq!(
        client
            .fetch(FetchRequest::get(
                "http://example.test/query?token=secret-456"
            ))
            .await
            .unwrap()
            .status,
        200
    );
    client.stop().await.unwrap();
    first.wait().await.unwrap();

    let database_path = dir.join("state.json.sqlite3");
    assert!(database_path.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&database_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let db_bytes = std::fs::read(&database_path).unwrap();
    assert!(!db_bytes
        .windows(22)
        .any(|bytes| bytes == b"private-test-token-123"));
    assert!(!db_bytes.windows(10).any(|bytes| bytes == b"secret-456"));

    let second_backend = Arc::new(FixtureBackend::default());
    let second = DaemonServer::start(config.clone(), second_backend.clone())
        .await
        .unwrap();
    let client = DaemonClient::from_state_file(&config.state_file).unwrap();
    let status = client.status().await.unwrap();
    assert_eq!(status.cache_entries, 1);
    assert_eq!(status.learned_sites, 1);
    let restored = client.fetch(FetchRequest::get(url)).await.unwrap();
    assert_eq!(restored.status, 200);
    assert_eq!(restored.body, b"<main>fixture content</main>");
    let request = second_backend.seen.lock().unwrap()[0].clone();
    assert_eq!(request.tier, FetchTier::Http);
    assert_eq!(
        request.headers.get("If-None-Match").map(String::as_str),
        Some("\"v1\"")
    );
    assert!(request.headers.contains_key("If-Modified-Since"));
    client.stop().await.unwrap();
    second.wait().await.unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn expired_and_corrupted_persistent_rows_are_not_restored() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "rustscrapling-expired-{}-{stamp}",
        std::process::id()
    ));
    let config = DaemonConfig {
        state_file: dir.join("state.json"),
        site_interval: Duration::ZERO,
        learning_ttl: Duration::from_millis(150),
        request_timeout: Duration::from_secs(5),
    };
    let first = DaemonServer::start(config.clone(), Arc::new(FixtureBackend::default()))
        .await
        .unwrap();
    let client = DaemonClient::from_state_file(&config.state_file).unwrap();
    client
        .fetch(FetchRequest::get("http://example.test/expire"))
        .await
        .unwrap();
    client.stop().await.unwrap();
    first.wait().await.unwrap();
    let db_path = dir.join("state.json.sqlite3");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "INSERT INTO daemon_cache(request_key,response,created_ms) VALUES (?1,?2,?3)",
        rusqlite::params!["{}", b"not json", i64::MAX],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO daemon_learning(host,tier,expires_ms) VALUES (?1,?2,?3)",
        rusqlite::params!["example.invalid", "bogus", i64::MAX],
    )
    .unwrap();
    drop(conn);
    tokio::time::sleep(Duration::from_millis(180)).await;

    let backend = Arc::new(FixtureBackend::default());
    let second = DaemonServer::start(config.clone(), backend.clone())
        .await
        .unwrap();
    let client = DaemonClient::from_state_file(&config.state_file).unwrap();
    let status = client.status().await.unwrap();
    assert_eq!(status.cache_entries, 0);
    assert_eq!(status.learned_sites, 0);
    client
        .fetch(FetchRequest::get("http://example.test/expire"))
        .await
        .unwrap();
    assert_eq!(backend.seen.lock().unwrap()[0].tier, FetchTier::Auto);
    assert!(!backend.seen.lock().unwrap()[0]
        .headers
        .contains_key("If-None-Match"));
    client.stop().await.unwrap();
    second.wait().await.unwrap();

    // A damaged SQLite file reports a typed startup error, releases the token
    // state file and never converts arbitrary bytes into a cached response.
    std::fs::write(&db_path, b"not a SQLite database").unwrap();
    let error = DaemonServer::start(config.clone(), Arc::new(FixtureBackend::default()))
        .await
        .err()
        .expect("corrupt DB must fail safely");
    assert_eq!(error.kind, "storage");
    assert!(!config.state_file.exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn revalidation_with_no_store_removes_old_disk_entry() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "rustscrapling-no-store-{}-{stamp}",
        std::process::id()
    ));
    let config = DaemonConfig {
        state_file: dir.join("state.json"),
        site_interval: Duration::ZERO,
        learning_ttl: Duration::from_secs(5),
        request_timeout: Duration::from_secs(5),
    };
    let server = DaemonServer::start(config.clone(), Arc::new(Private304Backend))
        .await
        .unwrap();
    let client = DaemonClient::from_state_file(&config.state_file).unwrap();
    let url = "http://example.test/change";
    assert_eq!(
        client.fetch(FetchRequest::get(url)).await.unwrap().body,
        b"initial"
    );
    assert_eq!(
        client.fetch(FetchRequest::get(url)).await.unwrap().body,
        b"initial"
    );
    assert_eq!(client.status().await.unwrap().cache_entries, 0);
    client.stop().await.unwrap();
    server.wait().await.unwrap();
    let restarted = DaemonServer::start(config.clone(), Arc::new(FixtureBackend::default()))
        .await
        .unwrap();
    let client = DaemonClient::from_state_file(&config.state_file).unwrap();
    assert_eq!(client.status().await.unwrap().cache_entries, 0);
    client.stop().await.unwrap();
    restarted.wait().await.unwrap();
    std::fs::remove_dir_all(dir).unwrap();
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
