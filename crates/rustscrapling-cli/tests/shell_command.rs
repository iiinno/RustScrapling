//! `rustscrapling shell` (Plan C-06): curl conversion parity with the pinned
//! upstream `CurlParser` oracle, and the fetch → page/pages → select →
//! extract loop against a loopback server.
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};

fn shell(code: &str) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_rustscrapling"))
        .args(["shell", "-c", code])
        .env_remove("BROWSER")
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn fixture(name: &str) -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/oracle/curl/");
    serde_json::from_str(&std::fs::read_to_string(format!("{path}{name}")).unwrap()).unwrap()
}

#[test]
fn uncurl_matches_upstream_curl_parser_oracle() {
    let cases = fixture("cases.json");
    let expected = fixture("expected.json");
    let cases = cases.as_array().unwrap();
    assert_eq!(cases.len(), expected.as_array().unwrap().len());
    for (case, expected) in cases.iter().zip(expected.as_array().unwrap()) {
        let command = case.as_str().unwrap();
        let (code, stdout, stderr) = shell(&format!("uncurl {command}"));
        if let Some(kind) = expected.get("error") {
            assert_ne!(code, 0, "{command}: {stdout}");
            if kind == "AttributeError" {
                let error: Value = serde_json::from_str(stderr.lines().next().unwrap()).unwrap();
                assert_eq!(error["error"]["message"], expected["message"], "{command}");
            }
            continue;
        }
        assert_eq!(code, 0, "{command}: {stderr}");
        let actual: Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(&actual, expected, "{command}");
    }
}

/// Echo server: responds with an HTML page describing the request.
fn serve() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            std::thread::spawn(move || {
                let mut data = Vec::new();
                let mut buffer = [0u8; 8192];
                loop {
                    let n = stream.read(&mut buffer).unwrap_or(0);
                    data.extend_from_slice(&buffer[..n]);
                    let text = String::from_utf8_lossy(&data).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let length = text[..end]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if data.len() >= end + 4 + length || n == 0 {
                            break;
                        }
                    }
                    if n == 0 {
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&data).to_string();
                let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
                let mut lines = head.lines();
                let request_line = lines.next().unwrap_or_default().to_owned();
                let header = |name: &str| {
                    head.lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case(name).then(|| v.trim().to_owned())
                        })
                        .unwrap_or_default()
                };
                let escape = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;");
                let page = format!(
                    "<html><head><title>Echo</title></head><body><article><h1>Echo</h1>\
                     <p id=\"line\">{}</p><p id=\"cookie\">{}</p><p id=\"x\">{}</p>\
                     <p id=\"type\">{}</p><p id=\"body\">{}</p></article></body></html>",
                    escape(&request_line),
                    escape(&header("cookie")),
                    escape(&header("x-test")),
                    escape(&header("content-type")),
                    escape(body)
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nX-Echo: yes\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
                    page.len()
                );
                let _ = stream.write_all(response.as_bytes());
            });
        }
    });
    port
}

#[test]
fn fetch_select_extract_and_history_loop() {
    let base = format!("http://127.0.0.1:{}", serve());
    let script = format!(
        "get {base}/one --param q='a b' -H 'X-Test: hi' --cookie 'sid=1'
css '#line::text'
css '#x::text, #cookie::text'
headers
post {base}/two --json '{{\"k\":[1,2]}}'
css '#body::text' --first
css '#type::text'
curl2fetcher curl '{base}/three' -H 'cookie: c=3' --data-urlencode 'f=x y'
css '#line::text, #cookie::text, #body::text'
get {base}/4
get {base}/5
delete {base}/6
pages
use 1
page
md
xpath '//p[@id=\"line\"]/text()'
"
    );
    let (code, stdout, stderr) = shell(&script);
    assert_eq!(code, 0, "{stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    let value = |index: usize| -> Value { serde_json::from_str(lines[index]).unwrap() };
    assert_eq!(value(0)["status"], 200);
    assert_eq!(value(0)["source"], "http");
    assert_eq!(value(1), json!(["GET /one?q=a+b HTTP/1.1"]));
    assert_eq!(value(2), json!(["sid=1", "hi"]));
    assert!(value(3)
        .as_array()
        .unwrap()
        .contains(&json!(["x-echo", "yes"])));
    assert_eq!(value(4)["method"], "POST");
    assert_eq!(value(5), json!(["{\"k\":[1,2]}"]));
    assert_eq!(value(6), json!(["application/json"]));
    assert_eq!(value(7)["method"], "POST");
    assert_eq!(value(8), json!(["POST /three HTTP/1.1", "c=3", "f=x+y"]));
    // Five-page history: /one fell out, the current page is /6.
    let pages = value(12);
    let urls: Vec<&str> = pages
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["url"].as_str().unwrap())
        .collect();
    assert_eq!(urls.len(), 5);
    assert!(
        urls[0].ends_with("/two") && urls[4].ends_with("/6"),
        "{urls:?}"
    );
    assert_eq!(pages[4]["current"], true);
    assert_eq!(pages[4]["method"], "DELETE");
    assert!(value(13)["url"].as_str().unwrap().ends_with("/three"));
    assert!(value(14)["url"].as_str().unwrap().ends_with("/three"));
    let rest = lines[15..].join("\n");
    assert!(rest.starts_with("Echo\n===="), "{rest}");
    assert!(rest.contains("POST /three HTTP/1.1"), "{rest}");
    assert!(rest.ends_with("[\"POST /three HTTP/1.1\"]"), "{rest}");
}

#[test]
fn errors_are_typed_and_scripts_from_stdin_work() {
    // No page yet, unknown command, bad option, GET with a body.
    for (code, kind) in [
        ("css h1", "no_page"),
        ("frobnicate", "invalid_arguments"),
        ("get http://127.0.0.1:9/ --bogus", "invalid_arguments"),
        ("get http://127.0.0.1:9/ --data x", "invalid_arguments"),
        ("curl2fetcher curl https://e.test/ -X PATCH", "unsupported"),
    ] {
        let (status, _, stderr) = shell(code);
        assert_eq!(status, 1, "{code}");
        let error: Value = serde_json::from_str(stderr.lines().next().unwrap()).unwrap();
        assert_eq!(error["error"]["kind"], kind, "{code}: {stderr}");
        assert!(error["error"]["hint"].is_string());
    }
    // `exit` stops a piped script; help goes to stdout.
    let dir = std::env::temp_dir().join(format!("rs-shell-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("page.html");
    std::fs::write(
        &file,
        "<html><body><div><p class=x>one</p><p class=x>two</p></div></body></html>",
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_rustscrapling"))
        .arg("shell")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    write!(
        child.stdin.take().unwrap(),
        "load {} --url https://local.test/\ncss p.x::text\nselector-gen 'p.x'\nexit\ncss nope\n",
        file.display()
    )
    .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 3, "{stdout}");
    assert_eq!(
        serde_json::from_str::<Value>(lines[0]).unwrap()["url"],
        "https://local.test/"
    );
    assert_eq!(lines[1], "[\"one\",\"two\"]");
    assert!(lines[2].contains("\"css\""));
    let (status, viewed, _) = shell(&format!("load {}\nview", file.display()));
    assert_eq!(status, 0);
    let saved = serde_json::from_str::<Value>(viewed.lines().nth(1).unwrap()).unwrap();
    let saved = std::path::PathBuf::from(saved["saved_to"].as_str().unwrap());
    assert!(std::fs::read_to_string(&saved).unwrap().contains("two"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&saved).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    std::fs::remove_file(saved).ok();
    let (status, help, _) = shell("help");
    assert_eq!(status, 0);
    assert!(help.contains("curl2fetcher"));
    std::fs::remove_dir_all(dir).ok();
}

fn chrome() -> Option<String> {
    [
        "RUSTSCRAPLING_CHROME_FOR_TESTING",
        "RUSTSCRAPLING_CHROME_PATH",
    ]
    .into_iter()
    .filter_map(std::env::var_os)
    .map(|p| p.to_string_lossy().into_owned())
    .find(|p| std::path::Path::new(p).is_file())
}

/// Real Chrome: opt-in via RUSTSCRAPLING_CHROME_FOR_TESTING (live browser
/// E2E stays separate from the deterministic tests above).
#[test]
#[ignore = "requires Chrome; run with --ignored and RUSTSCRAPLING_CHROME_FOR_TESTING"]
fn fetch_renders_javascript_in_chrome() {
    let chrome = chrome().expect("RUSTSCRAPLING_CHROME_FOR_TESTING not set");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer);
            let page = "<html><body><div id=out></div><script>document.getElementById('out').textContent='rendered by js'</script></body></html>";
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
                page.len()
            );
        }
    });
    let output = Command::new(env!("CARGO_BIN_EXE_rustscrapling"))
        .args([
            "shell",
            "--chrome-path",
            &chrome,
            "-c",
            &format!("fetch http://127.0.0.1:{port}/\ncss '#out::text'"),
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        serde_json::from_str::<Value>(lines[0]).unwrap()["source"],
        "browser"
    );
    assert_eq!(lines[1], "[\"rendered by js\"]");
}
