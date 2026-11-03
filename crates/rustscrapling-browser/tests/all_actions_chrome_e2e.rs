//! Real-Chrome E2E for every upstream Scrapling v0.5 `browser_actions`
//! variant (move, click, wheel, textbox, checkbox, radio, combobox,
//! press_key, dialog, wait_time, wait_element, wait_load), including
//! Playwright selector engines and actionability failures.
//!
//! Opt in with `RUSTSCRAPLING_CHROME_FOR_TESTING=/path/to/chrome` and
//! `--ignored`. Uses a throwaway profile and a loopback-only HTTP fixture.
use rustscrapling_browser::{
    BrowserAction, BrowserClient, ElementState, LaunchOptions, LoadState, MouseButton,
    PointerTarget,
};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const PAGE: &str = r#"<!doctype html><html><head><title>Actions</title>
<style>
  body { margin: 0; font: 16px sans-serif; }
  #scroller { height: 80px; overflow: auto; border: 1px solid #000; }
  #scroller div { height: 1000px; }
  #covered-wrap { position: relative; width: 120px; height: 40px; }
  #overlay { position: absolute; inset: 0; background: rgba(0,0,0,.2); }
  #late { display: none; }
</style></head><body>
<h1>Action fixture</h1>
<label for="name">Name</label><input id="name" value="old">
<textarea id="bio">x</textarea>
<div id="editor" contenteditable="true">rich</div>
<input type="checkbox" id="agree"><label for="agree">Agree</label>
<label><input type="checkbox" id="news"> Newsletter</label>
<input type="radio" name="c" id="r1" value="a"><input type="radio" name="c" id="r2" value="b">
<select id="color"><option>Red</option><option>Green</option><option>Blue</option></select>
<select id="many" multiple><option>A</option><option selected>B</option><option>C</option></select>
<button id="go" onclick="log('click:' + event.detail + ':' + event.button)" oncontextmenu="log('context');return false">Go</button>
<button id="disabled" disabled>Off</button>
<div id="hover" onmouseover="log('hover')" style="width:80px;height:20px">Hover me</div>
<div id="scroller"><div>tall</div></div>
<div id="covered-wrap"><button id="covered" onclick="log('covered')">Covered</button><div id="overlay"></div></div>
<button id="ask" onclick="log('confirm:' + confirm('sure?'))">Ask</button>
<button id="prompt" onclick="log('prompt:' + prompt('name?'))">Prompt</button>
<button id="later" onclick="setTimeout(() => { document.getElementById('late').style.display = 'block'; }, 300)">Later</button>
<div id="late">Arrived</div>
<host-el id="host"></host-el>
<input id="keys" onkeydown="log('key:' + event.key + ':' + event.ctrlKey)">
<input id="date" type="date">
<input id="prefix" value="old">
<pre id="log"></pre>
<script>
  function log(text) { document.getElementById('log').textContent += text + '\n'; }
  customElements.define('host-el', class extends HTMLElement {
    constructor() { super(); this.attachShadow({mode: 'open'}).innerHTML = '<button id="inner" onclick="this.getRootNode().host.dataset.clicked = 1">Shadow</button>'; }
  });
</script>
</body></html>"#;

fn chrome() -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os("RUSTSCRAPLING_CHROME_FOR_TESTING")
            .expect("set RUSTSCRAPLING_CHROME_FOR_TESTING to an installed Chrome binary"),
    );
    assert!(path.is_file());
    path
}

async fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buffer = [0u8; 4096];
                let Ok(size) = stream.read(&mut buffer).await else {
                    return;
                };
                let request = String::from_utf8_lossy(&buffer[..size]);
                let response = if request.starts_with("GET /page ") {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{PAGE}",
                        PAGE.len()
                    )
                } else {
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_owned()
                };
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    format!("http://{address}/page")
}

fn sel(selector: &str) -> PointerTarget {
    PointerTarget::Selector(selector.into())
}

fn click(selector: &str) -> BrowserAction {
    BrowserAction::Click {
        target: sel(selector),
        timeout_ms: 5000.0,
        button: MouseButton::Left,
        click_count: 1,
        delay_ms: 0.0,
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires an explicitly configured Chrome for Testing binary"]
async fn every_upstream_action_variant_runs_on_real_chrome() {
    let url = serve().await;
    let mut options = LaunchOptions::new(chrome());
    options.extra_args.push("--no-proxy-server".into());
    // Throwaway test profile only; never applied to the production launcher.
    options.extra_args.push("--password-store=basic".into());
    if std::env::var_os("RUSTSCRAPLING_CHROME_TEST_NO_SANDBOX").is_some() {
        options.extra_args.push("--no-sandbox".into());
    }
    let browser = BrowserClient::launch(options).await.unwrap();
    let page = browser.open_page(false).await.unwrap();
    page.navigate(&url, Duration::from_secs(20)).await.unwrap();

    let actions = vec![
        BrowserAction::WaitLoad {
            state: LoadState::Load,
            timeout_ms: 10_000.0,
        },
        BrowserAction::WaitLoad {
            state: LoadState::NetworkIdle,
            timeout_ms: 10_000.0,
        },
        // fill replaces existing content, including non-ASCII text.
        BrowserAction::Textbox {
            target: "#name".into(),
            value: "日本語".into(),
            clear: true,
            timeout_ms: 5000.0,
        },
        // label-based role/text engines and press_sequentially append.
        BrowserAction::Textbox {
            target: "role=textbox[name=\"Name\"]".into(),
            value: "!".into(),
            clear: false,
            timeout_ms: 5000.0,
        },
        BrowserAction::Textbox {
            target: "#bio".into(),
            value: "".into(),
            clear: true,
            timeout_ms: 5000.0,
        },
        BrowserAction::Textbox {
            target: "#editor".into(),
            value: "replaced".into(),
            clear: true,
            timeout_ms: 5000.0,
        },
        BrowserAction::Textbox {
            target: "#date".into(),
            value: "2026-11-03".into(),
            clear: true,
            timeout_ms: 5000.0,
        },
        BrowserAction::Checkbox {
            target: "#agree".into(),
            checked: true,
            timeout_ms: 5000.0,
        },
        // Already-satisfied state is a no-op.
        BrowserAction::Checkbox {
            target: "#agree".into(),
            checked: true,
            timeout_ms: 5000.0,
        },
        // A label wrapping the input is retargeted to the control.
        BrowserAction::Checkbox {
            target: "text=Newsletter".into(),
            checked: true,
            timeout_ms: 5000.0,
        },
        BrowserAction::Radio {
            target: "#r2".into(),
            timeout_ms: 5000.0,
        },
        BrowserAction::Combobox {
            target: "#color".into(),
            labels: vec!["Blue".into()],
            timeout_ms: 5000.0,
        },
        BrowserAction::Combobox {
            target: "#many".into(),
            labels: vec!["A".into(), "C".into()],
            timeout_ms: 5000.0,
        },
        BrowserAction::Click {
            target: sel("#go"),
            timeout_ms: 5000.0,
            button: MouseButton::Left,
            click_count: 2,
            delay_ms: 10.0,
        },
        BrowserAction::Click {
            target: sel("xpath=//button[@id='go']"),
            timeout_ms: 5000.0,
            button: MouseButton::Right,
            click_count: 1,
            delay_ms: 0.0,
        },
        click("host-el >> #inner"),
        BrowserAction::Move {
            target: sel("text=Hover me"),
            timeout_ms: 5000.0,
            steps: 1,
        },
        BrowserAction::Move {
            target: PointerTarget::Point { x: 5.0, y: 5.0 },
            timeout_ms: 5000.0,
            steps: 4,
        },
        BrowserAction::Move {
            target: sel("#scroller"),
            timeout_ms: 5000.0,
            steps: 1,
        },
        BrowserAction::Wheel {
            delta_x: 0.0,
            delta_y: 300.0,
        },
        BrowserAction::Dialog {
            accept: true,
            prompt_text: None,
        },
        click("#ask"),
        BrowserAction::Dialog {
            accept: true,
            prompt_text: Some("Ada".into()),
        },
        click("#prompt"),
        // No queued reply: the dialog is dismissed instead of blocking.
        click("#ask"),
        click("role=button[name=\"Later\"]"),
        BrowserAction::WaitElement {
            target: "#late".into(),
            state: ElementState::Visible,
            timeout_ms: 5000.0,
        },
        BrowserAction::WaitElement {
            target: "#missing".into(),
            state: ElementState::Detached,
            timeout_ms: 1000.0,
        },
        // Unfocused <input>: typing starts at the beginning (Playwright).
        BrowserAction::Textbox {
            target: "#prefix".into(),
            value: "X".into(),
            clear: false,
            timeout_ms: 5000.0,
        },
        click("#keys"),
        BrowserAction::PressKey {
            key: "ControlOrMeta+A".into(),
        },
        BrowserAction::PressKey {
            key: "Shift+KeyB".into(),
        },
        BrowserAction::WaitTime { milliseconds: 50.0 },
    ];
    page.run_actions(&actions, false).await.unwrap();
    assert!(
        !browser
            .commands()
            .await
            .iter()
            .any(|method| method == "Runtime.enable"),
        "actions must not enable the main-world Runtime domain"
    );

    let state = page
        .evaluate(
            r#"JSON.stringify({
                name: document.querySelector('#name').value,
                bio: document.querySelector('#bio').value,
                editor: document.querySelector('#editor').textContent,
                date: document.querySelector('#date').value,
                agree: document.querySelector('#agree').checked,
                news: document.querySelector('#news').checked,
                r2: document.querySelector('#r2').checked,
                color: document.querySelector('#color').value,
                many: Array.from(document.querySelector('#many').selectedOptions).map(o => o.label),
                shadow: document.querySelector('#host').dataset.clicked || null,
                scrolled: document.querySelector('#scroller').scrollTop,
                keys: document.querySelector('#keys').value,
                prefix: document.querySelector('#prefix').value,
                log: document.querySelector('#log').textContent.trim().split('\n')
            })"#,
        )
        .await
        .unwrap();
    let state: Value = serde_json::from_str(state.as_str().unwrap()).unwrap();
    assert_eq!(state["name"], "日本語!", "{state}");
    assert_eq!(state["bio"], "");
    assert_eq!(state["editor"], "replaced");
    assert_eq!(state["date"], "2026-11-03");
    assert_eq!(state["agree"], true);
    assert_eq!(state["news"], true);
    assert_eq!(state["r2"], true);
    assert_eq!(state["color"], "Blue");
    assert_eq!(state["many"], serde_json::json!(["A", "C"]));
    assert_eq!(state["shadow"], "1");
    assert!(state["scrolled"].as_f64().unwrap() > 0.0, "{state}");
    assert_eq!(state["keys"], "B");
    assert_eq!(state["prefix"], "Xold");
    let log: Vec<&str> = state["log"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for expected in [
        "click:1:0",
        "click:2:0",
        "context",
        "hover",
        "confirm:true",
        "prompt:Ada",
        "confirm:false",
        "key:Control:true",
        "key:A:true",
        "key:Shift:false",
        "key:B:false",
    ] {
        assert!(log.contains(&expected), "missing {expected}: {log:?}");
    }

    // Failure semantics: strict mode, non-fillable input, unknown key,
    // missing option timeout, disabled and covered elements.
    let failures = [
        (click("input"), "strict mode violation"),
        (
            BrowserAction::Textbox {
                target: "#agree".into(),
                value: "x".into(),
                clear: true,
                timeout_ms: 1000.0,
            },
            "Input of type \"checkbox\" cannot be filled",
        ),
        (
            BrowserAction::PressKey { key: "Foo".into() },
            "Unknown key: \"Foo\"",
        ),
        (
            BrowserAction::Combobox {
                target: "#color".into(),
                labels: vec!["Nope".into()],
                timeout_ms: 300.0,
            },
            "Timeout 300ms exceeded",
        ),
        (
            BrowserAction::Click {
                target: sel("#disabled"),
                timeout_ms: 300.0,
                button: MouseButton::Left,
                click_count: 1,
                delay_ms: 0.0,
            },
            "not enabled",
        ),
        (
            BrowserAction::Click {
                target: sel("#covered"),
                timeout_ms: 500.0,
                button: MouseButton::Left,
                click_count: 1,
                delay_ms: 0.0,
            },
            "intercepts pointer events",
        ),
        (
            BrowserAction::Click {
                target: sel("bogus=engine"),
                timeout_ms: 300.0,
                button: MouseButton::Left,
                click_count: 1,
                delay_ms: 0.0,
            },
            "Unknown engine",
        ),
    ];
    for (action, expected) in failures {
        let kind = action.kind();
        let error = page
            .run_actions(
                &[BrowserAction::WaitTime { milliseconds: 0.0 }, action],
                false,
            )
            .await
            .unwrap_err();
        assert_eq!(error.index, 2);
        assert_eq!(error.kind, kind);
        let text = error.to_string();
        assert!(
            text.starts_with(&format!("Action 2 ({kind}) failed: ")) && text.contains(expected),
            "{text} should contain {expected}"
        );
    }
    let log = page
        .evaluate("document.querySelector('#log').textContent")
        .await
        .unwrap();
    assert!(!log.as_str().unwrap().contains("covered"));
    // `slowly` adds human-like delays but keeps the same effects.
    let started = std::time::Instant::now();
    page.run_actions(
        &[
            BrowserAction::Textbox {
                target: "#bio".into(),
                value: "ab".into(),
                clear: true,
                timeout_ms: 5000.0,
            },
            BrowserAction::WaitTime { milliseconds: 0.0 },
        ],
        true,
    )
    .await
    .unwrap();
    assert!(started.elapsed() >= Duration::from_millis(200));
    assert_eq!(
        page.evaluate("document.querySelector('#bio').value")
            .await
            .unwrap(),
        "ab"
    );
    page.close().await.unwrap();
    browser.shutdown().await.unwrap();
}
