//! Differential test: Rust ARIA snapshots versus snapshots produced by the
//! pinned Scrapling v0.5 MCP server on the same HTML fixtures
//! (`oracle/aria_snapshot_oracle.py`, boxes disabled). Also checks that
//! snapshot refs are usable `aria-ref=` action and screenshot targets.
//!
//! Opt in with `RUSTSCRAPLING_CHROME_FOR_TESTING` and `--ignored`.
use rustscrapling_browser::{
    BrowserAction, BrowserClient, LaunchOptions, MouseButton, PointerTarget,
};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/oracle/aria_snapshot")
}

async fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer = [0u8; 4096];
                let size = stream.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..size]);
                let path = request
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .trim_start_matches('/')
                    .to_owned();
                let file = (!path.contains("..") && path.ends_with(".html"))
                    .then(|| std::fs::read(fixtures().join(&path)).ok())
                    .flatten();
                let response = match file {
                    Some(body) => {
                        let mut head = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
                        head.extend(body);
                        head
                    }
                    None => {
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_vec()
                    }
                };
                let _ = stream.write_all(&response).await;
            });
        }
    });
    format!("http://{address}")
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires an explicitly configured Chrome for Testing binary"]
async fn aria_snapshots_match_pinned_upstream_fixtures() {
    let chrome = PathBuf::from(
        std::env::var_os("RUSTSCRAPLING_CHROME_FOR_TESTING")
            .expect("set RUSTSCRAPLING_CHROME_FOR_TESTING"),
    );
    let base = serve().await;
    let mut options = LaunchOptions::new(chrome);
    options.extra_args.push("--no-proxy-server".into());
    options.extra_args.push("--password-store=basic".into());
    if std::env::var_os("RUSTSCRAPLING_CHROME_TEST_NO_SANDBOX").is_some() {
        options.extra_args.push("--no-sandbox".into());
    }
    let browser = BrowserClient::launch(options).await.unwrap();
    let page = browser.open_page(false).await.unwrap();
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixtures().join("MANIFEST.json")).unwrap()).unwrap();
    // Same order as the oracle: frame sequence numbers (ref prefixes) depend on it.
    for name in manifest["fixtures"].as_array().unwrap() {
        let name = name.as_str().unwrap();
        page.navigate(&format!("{base}/{name}.html"), Duration::from_secs(20))
            .await
            .unwrap();
        let expected =
            std::fs::read_to_string(fixtures().join(format!("{name}.boxes-false.yaml"))).unwrap();
        let actual = page
            .aria_snapshot(None, None, false, 10_000.0)
            .await
            .unwrap();
        assert_eq!(
            actual,
            expected.trim_end_matches('\n'),
            "{name} full snapshot"
        );
        let target = manifest["targets"][name].as_str().unwrap();
        let expected =
            std::fs::read_to_string(fixtures().join(format!("{name}.target.yaml"))).unwrap();
        let actual = page
            .aria_snapshot(Some(target), None, false, 10_000.0)
            .await
            .unwrap();
        assert_eq!(
            actual,
            expected.trim_end_matches('\n'),
            "{name} target snapshot"
        );
    }

    // Boxes, depth and refs as action/screenshot targets on the last page.
    let boxed = page
        .aria_snapshot(None, Some(1), true, 10_000.0)
        .await
        .unwrap();
    assert!(boxed.contains("[box="), "{boxed}");
    assert!(
        !boxed.contains("Card title"),
        "depth=1 must stop early: {boxed}"
    );
    let snapshot = page
        .aria_snapshot(None, None, false, 10_000.0)
        .await
        .unwrap();
    let line = snapshot
        .lines()
        .find(|line| line.contains("button \"OK\""))
        .unwrap();
    let reference = line
        .split("[ref=")
        .nth(1)
        .unwrap()
        .split(']')
        .next()
        .unwrap()
        .to_owned();
    page.run_actions(
        &[BrowserAction::Click {
            target: PointerTarget::Selector(format!("aria-ref={reference}")),
            timeout_ms: 5000.0,
            button: MouseButton::Left,
            click_count: 1,
            delay_ms: 0.0,
        }],
        false,
    )
    .await
    .unwrap();
    let image = page
        .element_screenshot(&format!("aria-ref={reference}"), "png", None, 5000.0)
        .await
        .unwrap();
    assert!(image.starts_with("iVBORw0KGgo"));
    let missing = page
        .aria_snapshot(Some("#does-not-exist"), None, false, 1000.0)
        .await
        .unwrap_err();
    assert!(
        missing.to_string().contains("does not match any element"),
        "{missing}"
    );
    browser.shutdown().await.unwrap();
}
