use rustscrapling_browser::{BrowserClient, BrowserGetOptions, LaunchOptions};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn chrome_binary() -> Option<PathBuf> {
    for var in [
        "RUSTSCRAPLING_CHROME_FOR_TESTING",
        "CHROME_FOR_TESTING",
        "RUSTSCRAPLING_CHROME",
    ] {
        if let Some(path) = std::env::var_os(var)
            .map(PathBuf::from)
            .filter(|p| p.is_file())
        {
            return Some(path);
        }
    }
    #[cfg(windows)]
    for path in [
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
    ] {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    #[cfg(unix)]
    for path in [
        "/home/hatch/chrome-for-testing/chrome-linux64/chrome",
        "/usr/bin/google-chrome",
        "/usr/bin/chromium",
    ] {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

#[tokio::test(flavor = "multi_thread")]
async fn local_browser_navigation_reuse_external_connection_and_shadow_dom() {
    let Some(chrome) = chrome_binary() else {
        eprintln!("Chrome executable unavailable; set RUSTSCRAPLING_CHROME_FOR_TESTING for E2E");
        return;
    };
    eprintln!("CDP integration browser: {}", chrome.display());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut bytes = vec![0u8; 8192];
                let Ok(count) = stream.read(&mut bytes).await else {
                    return;
                };
                let req = String::from_utf8_lossy(&bytes[..count]);
                let first = req.starts_with("GET /first ");
                let second = req.starts_with("GET /second ");
                let body = if first {
                    include_str!("fixtures/local.html").to_owned()
                } else if second {
                    "<html><body><article><h1>Reused tab second page</h1></article></body></html>"
                        .into()
                } else {
                    "Not found".into()
                };
                let status = if first || second {
                    "200 OK"
                } else {
                    "404 Not Found"
                };
                let header = format!("HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n", body.len(), if first { "Set-Cookie: local_test=yes; Path=/; SameSite=Lax\r\n" } else { "" });
                let _ = stream.write_all(header.as_bytes()).await;
                let _ = stream.write_all(body.as_bytes()).await;
                let _ = stream.flush().await;
            });
        }
    });

    let mut launch = LaunchOptions::new(chrome);
    // On locked-down CI/VPS kernels, Chrome's user-namespace sandbox may be
    // unavailable. This opt-in is exclusively for the localhost E2E fixture;
    // production browser sessions must retain Chrome's sandbox by default.
    if std::env::var_os("RUSTSCRAPLING_CHROME_TEST_NO_SANDBOX").is_some() {
        launch.extra_args.push("--no-sandbox".into());
    }
    let browser = BrowserClient::launch(launch).await.unwrap();
    let endpoint = browser.cdp_endpoint().to_owned();
    let first = browser
        .get(
            &format!("http://127.0.0.1:{port}/first"),
            BrowserGetOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        first.status,
        Some(200),
        "first network capture: {:?}",
        first.network
    );
    assert!(first.body.contains("RustScrapling browser"));
    assert!(
        first.body.contains("shadow-dom-visible"),
        "Shadow DOM must be present in DOM.getOuterHTML includeShadowDOM output: {}",
        first.body
    );
    assert!(first.blocked.is_none());
    assert_eq!(first.final_url, format!("http://127.0.0.1:{port}/first"));

    let state = browser.save_state().await.unwrap();
    assert!(
        state
            .cookies
            .iter()
            .any(|cookie| cookie.get("name").and_then(|v| v.as_str()) == Some("local_test")),
        "cookie state missing: {:?}",
        state.cookies
    );
    browser.restore_state(&state).await.unwrap();

    let second = browser
        .get(
            &format!("http://127.0.0.1:{port}/second"),
            BrowserGetOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(second.status, Some(200));
    assert!(second.body.contains("Reused tab second page"));
    assert!(!second.body.contains("RustScrapling browser"));
    let commands = browser.commands().await;
    assert_eq!(
        commands
            .iter()
            .filter(|c| *c == "Target.createTarget")
            .count(),
        1,
        "get() should reuse the existing page"
    );
    assert!(
        !commands.iter().any(|c| c == "Runtime.enable"),
        "D-007 violated: {commands:?}"
    );

    let external = BrowserClient::connect(&endpoint).await.unwrap();
    let page = external.open_page(true).await.unwrap();
    page.navigate(
        &format!("http://127.0.0.1:{port}/first"),
        Duration::from_secs(15),
    )
    .await
    .unwrap();
    assert!(page
        .source()
        .await
        .unwrap()
        .contains("RustScrapling browser"));
    assert!(!external
        .commands()
        .await
        .iter()
        .any(|c| c == "Runtime.enable"));
    assert_eq!(page.evaluate("2 + 3").await.unwrap().as_i64(), Some(5));
    assert!(external
        .commands()
        .await
        .iter()
        .any(|c| c == "Runtime.enable"));
    page.close().await.unwrap();
    external.shutdown().await.unwrap(); // must leave external Chrome alive
    assert!(browser
        .get(
            &format!("http://127.0.0.1:{port}/second"),
            BrowserGetOptions::default()
        )
        .await
        .is_ok());
    browser.shutdown().await.unwrap();
    server.abort();
}
