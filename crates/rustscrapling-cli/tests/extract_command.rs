//! `rustscrapling extract` versus the pinned upstream `scrapling extract`
//! (fixtures/oracle/extract_cli, recorded by oracle/extract_cli_oracle.py
//! against the same pages and echo endpoint served here).
use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn header(head: &str, name: &str) -> String {
    head.lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name)
                .then(|| value.trim().to_owned())
        })
        .unwrap_or_default()
}

/// Same behaviour as the oracle's Python handler.
fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let pages = root().join("fixtures/extraction");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let pages = pages.clone();
            std::thread::spawn(move || {
                let mut data = Vec::new();
                let mut buffer = [0u8; 8192];
                let (head, body) = loop {
                    let n = stream.read(&mut buffer).unwrap_or(0);
                    data.extend_from_slice(&buffer[..n]);
                    let text = String::from_utf8_lossy(&data).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let length: usize =
                            header(&text[..end], "content-length").parse().unwrap_or(0);
                        if data.len() >= end + 4 + length || n == 0 {
                            break (text[..end].to_owned(), text[end + 4..].to_owned());
                        }
                    }
                    if n == 0 {
                        break (text, String::new());
                    }
                };
                let mut request_line = head.lines().next().unwrap_or_default().split_whitespace();
                let method = request_line.next().unwrap_or_default().to_owned();
                let path = request_line.next().unwrap_or("/").to_owned();
                let page = if path.starts_with("/echo") {
                    Some(format!(
                        "<html><body><main><h1>Echo</h1><p id=m>{method}</p><p id=p>{path}</p>\
                         <p id=t>{}</p><p id=b>{body}</p><p id=c>{}</p><p id=x>{}</p></main></body></html>",
                        header(&head, "content-type"),
                        header(&head, "cookie"),
                        header(&head, "x-test")
                    ))
                } else {
                    let nohidden = path.starts_with("/nohidden/");
                    let name = path.trim_start_matches("/nohidden").trim_start_matches('/');
                    std::fs::read_to_string(pages.join(name)).ok().map(|html| {
                        if nohidden {
                            let start = html.find("<p hidden>").unwrap();
                            let end = start + html[start..].find("</p>").unwrap() + 4;
                            format!("{}{}", &html[..start], &html[end..])
                        } else {
                            html
                        }
                    })
                };
                let response = match page {
                    Some(page) => format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
                        page.len()
                    ),
                    None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
                };
                let _ = stream.write_all(response.as_bytes());
            });
        }
    });
    base
}

#[test]
fn extract_matches_pinned_upstream_cli_outputs() {
    let expected: Value = serde_json::from_str(
        &std::fs::read_to_string(root().join("fixtures/oracle/extract_cli/expected.json")).unwrap(),
    )
    .unwrap();
    let base = serve();
    let dir = std::env::temp_dir().join(format!("rs-extract-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut mismatches = Vec::new();
    for (name, case) in expected.as_object().unwrap() {
        let out = dir.join(name).to_string_lossy().into_owned();
        let args: Vec<String> = case["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| {
                a.as_str()
                    .unwrap()
                    .replace("{base}", &base)
                    .replace("{out}", &out)
            })
            .collect();
        let output = Command::new(env!("CARGO_BIN_EXE_rustscrapling"))
            .arg("extract")
            .args(&args)
            .env("NO_PROXY", "127.0.0.1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
        let written = std::fs::read_to_string(&args[2]).unwrap();
        assert_eq!(receipt["saved_to"].as_str().unwrap(), args[2]);
        if written != case["content"].as_str().unwrap() {
            mismatches.push(format!(
                "{name}:\n--- upstream\n{}\n--- rust\n{written}",
                case["content"].as_str().unwrap()
            ));
        }
    }
    std::fs::remove_dir_all(&dir).ok();
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n\n"));
}

#[test]
fn extract_rejects_unknown_extensions_and_bodies_on_get_without_writing() {
    let dir = std::env::temp_dir().join(format!("rs-extract-neg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (args, kind) in [
        (
            vec!["get", "http://127.0.0.1:9/", "out.pdf"],
            "invalid_arguments",
        ),
        (
            vec!["get", "http://127.0.0.1:9/", "out.md", "--json", "{}"],
            "invalid_arguments",
        ),
        (
            vec!["fetch", "http://127.0.0.1:9/", "out.md", "--block-ads"],
            "unsupported",
        ),
        (
            vec!["fetch", "http://127.0.0.1:9/", "out.md", "--dns-over-https"],
            "unsupported",
        ),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_rustscrapling"))
            .arg("extract")
            .args(&args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["kind"], kind, "{args:?}");
    }
    assert_eq!(
        std::fs::read_dir(&dir).unwrap().count(),
        0,
        "no file written"
    );
    std::fs::remove_dir_all(dir).ok();
}

/// Real Chrome (opt-in): `extract fetch` renders JavaScript and honours
/// --css-selector/--ai-targeted like upstream StealthyFetcher + Convertor.
#[test]
#[ignore = "requires Chrome; set RUSTSCRAPLING_CHROME_FOR_TESTING"]
fn extract_fetch_renders_javascript_with_real_chrome() {
    let chrome = std::env::var("RUSTSCRAPLING_CHROME_FOR_TESTING")
        .expect("set RUSTSCRAPLING_CHROME_FOR_TESTING");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer);
            let page = "<html><body><main><ul id=list></ul></main><script>for (const n of ['alpha','beta']) { const li = document.createElement('li'); li.textContent = n; document.getElementById('list').appendChild(li); }</script></body></html>";
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}", page.len());
        }
    });
    let dir = std::env::temp_dir().join(format!("rs-extract-fetch-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("items.txt");
    let output = Command::new(env!("CARGO_BIN_EXE_rustscrapling"))
        .args([
            "extract",
            "fetch",
            &format!("http://127.0.0.1:{port}/"),
            out.to_str().unwrap(),
            "-s",
            "li",
            "--ai-targeted",
            "--executable-path",
            &chrome,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "alphabeta");
    std::fs::remove_dir_all(dir).ok();
}
