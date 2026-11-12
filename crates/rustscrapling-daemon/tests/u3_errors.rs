use rustscrapling_daemon::{
    DaemonClient, DaemonConfig, DaemonError, DaemonServer, FetchBackend, FetchFuture, FetchRequest,
    FetchResult, FetchTier,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct FixtureDirectory(PathBuf);
impl Drop for FixtureDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Backend {
    seen: Mutex<Vec<FetchTier>>,
    error: DaemonError,
}

impl FetchBackend for Backend {
    fn fetch<'a>(&'a self, request: FetchRequest) -> FetchFuture<'a> {
        Box::pin(async move {
            let mut seen = self.seen.lock().unwrap();
            seen.push(request.tier);
            if seen.len() == 2 {
                return Err(self.error.clone());
            }
            Ok(FetchResult {
                status: 200,
                final_url: request.url,
                headers: vec![],
                body: b"synthetic usable content".to_vec(),
                tier: if seen.len() == 1 {
                    FetchTier::Browser
                } else {
                    FetchTier::Http
                },
                protocol: None,
            })
        })
    }
}

async fn after_learning(
    kind: &str,
    status: Option<u16>,
) -> (Result<FetchResult, DaemonError>, Vec<FetchTier>) {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "rustscrapling-u3-daemon-{}-{stamp}",
        std::process::id()
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
    }
    #[cfg(not(unix))]
    std::fs::create_dir(&path).unwrap();
    let directory = FixtureDirectory(path);
    let config = DaemonConfig {
        state_file: directory.0.join("state.json"),
        site_interval: Duration::ZERO,
        learning_ttl: Duration::from_secs(60),
        request_timeout: Duration::from_secs(3),
    };
    let mut error = DaemonError::new(kind, "synthetic failure");
    error.status = status;
    let backend = Arc::new(Backend {
        seen: Mutex::new(Vec::new()),
        error,
    });
    let server = DaemonServer::start(config.clone(), backend.clone())
        .await
        .unwrap();
    let client = DaemonClient::discover(&config.state_file)
        .await
        .unwrap()
        .unwrap();
    let first = client
        .fetch(FetchRequest::get("http://127.0.0.1:9/one"))
        .await;
    let second = client
        .fetch(FetchRequest::get("http://127.0.0.1:9/two"))
        .await;
    client.stop().await.unwrap();
    server.wait().await.unwrap();
    assert!(first.is_ok());
    assert!(!config.state_file.exists());
    let seen = backend.seen.lock().unwrap().clone();
    (second, seen)
}

#[tokio::test]
async fn refusal_and_unknown_errors_never_retry_a_learned_route() {
    for (kind, status) in [
        ("source_refused", Some(403)),
        ("transport", Some(403)),
        ("blocked", None),
        ("unknown_provider_failure", None),
    ] {
        let (result, seen) = after_learning(kind, status).await;
        assert_eq!(seen, vec![FetchTier::Auto, FetchTier::Browser]);
        let error = result.unwrap_err();
        assert_eq!(error.kind, kind);
        assert_eq!(error.status, status);
    }
}

#[tokio::test]
async fn unavailable_learned_route_can_recover_through_auto() {
    let (result, seen) = after_learning("unavailable", None).await;
    assert_eq!(
        seen,
        vec![FetchTier::Auto, FetchTier::Browser, FetchTier::Auto]
    );
    assert_eq!(result.unwrap().body, b"synthetic usable content");
}
