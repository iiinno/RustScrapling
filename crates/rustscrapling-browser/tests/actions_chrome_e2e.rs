//! Real-browser interaction check; opt in with a dedicated Chrome binary.
//! No external HTTP dependency. Runs with a throwaway browser profile.
use rustscrapling_browser::{BrowserClient, LaunchOptions};
use std::{path::PathBuf, time::Duration};

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires an explicitly configured Chrome for Testing binary"]
async fn real_chrome_click_and_unicode_input_without_implicit_runtime() {
    let chrome = PathBuf::from(
        std::env::var_os("RUSTSCRAPLING_CHROME_FOR_TESTING")
            .expect("set RUSTSCRAPLING_CHROME_FOR_TESTING to an installed Chrome binary"),
    );
    assert!(chrome.is_file());
    let mut options = LaunchOptions::new(chrome);
    options.extra_args.push("--no-proxy-server".into());
    // This fixture never opens a persistent user profile or stores credentials.
    options.extra_args.push("--password-store=basic".into());
    if std::env::var_os("RUSTSCRAPLING_CHROME_TEST_NO_SANDBOX").is_some() {
        options.extra_args.push("--no-sandbox".into());
    }
    let browser = BrowserClient::launch(options).await.unwrap();
    let page = browser.open_page(false).await.unwrap();
    let html = r#"<html><head><title>before</title></head><body>
        <input id="entry"><button id="push" onclick="document.title='clicked'">Go</button>
        </body></html>"#;
    let target = format!(
        "data:text/html,{}",
        // Form URL encoding renders spaces as '+', which a data: URL treats
        // literally. Use %20 so HTML attributes remain valid.
        url::form_urlencoded::byte_serialize(html.as_bytes())
            .collect::<String>()
            .replace('+', "%20")
    );
    page.navigate(&target, Duration::from_secs(15))
        .await
        .unwrap();
    let source = page.source().await.unwrap();
    assert!(
        source.contains("id=\"push\""),
        "fixture HTML not loaded: {source}"
    );
    page.click_css("#push").await.unwrap();
    page.type_text_css("#entry", "日本語テキスト")
        .await
        .unwrap();
    page.press_key("ArrowLeft").await.unwrap();
    assert!(!browser
        .commands()
        .await
        .iter()
        .any(|method| method == "Runtime.enable"));
    assert_eq!(
        page.evaluate("document.title").await.unwrap().as_str(),
        Some("clicked")
    );
    assert_eq!(
        page.evaluate("document.querySelector('#entry').value")
            .await
            .unwrap()
            .as_str(),
        Some("日本語テキスト")
    );
    page.close().await.unwrap();
    browser.shutdown().await.unwrap();
}
