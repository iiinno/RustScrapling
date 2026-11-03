//! Opt-in real Chrome test, independent from local HTTP and DNS networking.
//! Run with RUSTSCRAPLING_CHROME_FOR_TESTING=/path/to/chrome and --ignored.
use rustscrapling_browser::{BrowserClient, LaunchOptions};
use std::path::PathBuf;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires an explicitly configured Chrome for Testing binary"]
async fn chrome_data_url_dom_screenshot_and_explicit_evaluation() {
    let chrome = PathBuf::from(
        std::env::var_os("RUSTSCRAPLING_CHROME_FOR_TESTING")
            .expect("set RUSTSCRAPLING_CHROME_FOR_TESTING to an existing Chrome binary"),
    );
    assert!(chrome.is_file(), "Chrome binary is missing");
    let mut options = LaunchOptions::new(chrome);
    options.extra_args.push("--no-proxy-server".into());
    if std::env::var_os("RUSTSCRAPLING_CHROME_TEST_NO_SANDBOX").is_some() {
        // Only for explicitly opted-in CI/VPS runs on restricted kernels.
        options.extra_args.push("--no-sandbox".into());
    }

    let browser = BrowserClient::launch(options).await.unwrap();
    let page = browser.open_page(false).await.unwrap();
    page.navigate(
        "data:text/html,%3Chtml%3E%3Cbody%3E%3Ch1%3EReal-CDP-fixture%3C%2Fh1%3E%3C%2Fbody%3E%3C%2Fhtml%3E",
        Duration::from_secs(15),
    )
    .await
    .unwrap();

    let html = page.source().await.unwrap();
    assert!(html.contains("Real-CDP-fixture"));
    let png = page.screenshot("png", None, false).await.unwrap();
    assert!(
        png.starts_with("iVBORw0KGgo"),
        "Chrome should return PNG bytes"
    );
    assert!(
        !browser
            .commands()
            .await
            .iter()
            .any(|cmd| cmd == "Runtime.enable"),
        "ordinary DOM and screenshot operations must not enable Runtime"
    );
    assert_eq!(page.evaluate("2 + 3").await.unwrap().as_i64(), Some(5));
    assert!(browser
        .commands()
        .await
        .iter()
        .any(|cmd| cmd == "Runtime.enable"));
    page.close().await.unwrap();
    browser.shutdown().await.unwrap();
}
