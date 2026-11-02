use chromiumoxide::browser::{Browser, BrowserConfig};
use futures::StreamExt;

#[tokio::main]
async fn main() {
    // ---- wreq through the egress proxy ----
    let proxy_url = std::env::var("https_proxy").expect("https_proxy env");
    let client = wreq::Client::builder()
        .proxy(wreq::Proxy::all(&proxy_url).expect("proxy parse"))
        .build()
        .expect("wreq client");
    match client.get("https://example.com/").send().await {
        Ok(resp) => {
            let status = resp.status();
            let ver = resp.version();
            let body = resp.text().await.unwrap_or_default();
            println!("WREQ status={} version={:?} body_len={} has_example={}",
                status, ver, body.len(), body.contains("Example Domain"));
        }
        Err(e) => println!("WREQ ERROR: {:?}", e),
    }

    // ---- chromiumoxide against Chrome for Testing (local file, no network) ----
    let (mut browser, mut handler) = Browser::launch(
        BrowserConfig::builder()
            .chrome_executable("/home/hatch/chrome-for-testing/chrome-linux64/chrome")
            .no_sandbox()
            .build()
            .expect("browser config"),
    )
    .await
    .expect("browser launch");
    let handler_task = tokio::spawn(async move {
        while let Some(h) = handler.next().await {
            if h.is_err() { break; }
        }
    });
    let page = browser.new_page("file:///home/hatch/tmp/testpage.html").await.expect("new page");
    let title = page.get_title().await.expect("title");
    let content = page.content().await.expect("content");
    println!("CDP title={:?} content_has_h1={}", title, content.contains("xvfb-headed-ok"));
    browser.close().await.expect("close");
    handler_task.await.ok();
    println!("TRIAL-NET DONE");
}
