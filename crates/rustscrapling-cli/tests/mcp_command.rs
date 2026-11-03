//! `rustscrapling mcp` serves the same 16-tool MCP server over stdio.
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

#[test]
fn mcp_subcommand_lists_the_upstream_tools() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rustscrapling"))
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for message in [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"cli-test","version":"1"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    ] {
        writeln!(stdin, "{message}").unwrap();
    }
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut tools = None;
    while let Some(Ok(line)) = lines.next() {
        let value: Value = serde_json::from_str(&line).unwrap();
        if value["id"] == 2 {
            tools = Some(value["result"]["tools"].as_array().unwrap().len());
            break;
        }
    }
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(tools, Some(16));
}

/// One HTTP/1.1 exchange (Connection: close); returns (status, headers, body).
fn http(
    address: &str,
    host: &str,
    token: Option<&str>,
    path: &str,
    session: Option<&str>,
    body: &Value,
) -> (u16, String, String) {
    use std::io::Read;
    let payload = body.to_string();
    let mut stream = std::net::TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(20)))
        .unwrap();
    let mut request = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n",
        payload.len()
    );
    if let Some(token) = token {
        request.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    if let Some(session) = session {
        request.push_str(&format!(
            "Mcp-Session-Id: {session}\r\nMcp-Protocol-Version: 2025-06-18\r\n"
        ));
    }
    request.push_str("\r\n");
    request.push_str(&payload);
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .unwrap_or("0")
        .parse()
        .unwrap();
    (status, head.to_ascii_lowercase(), body.to_owned())
}

/// The JSON-RPC message with `id` from an SSE or JSON body.
fn rpc(body: &str, id: i64) -> Value {
    body.lines()
        .filter_map(|line| {
            line.strip_prefix("data:")
                .map(str::trim)
                .or(Some(line.trim()))
        })
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|value| value["id"] == id)
        .unwrap_or_else(|| panic!("no id {id} in {body}"))
}

#[test]
fn mcp_streamable_http_requires_token_and_checks_hosts() {
    // No token and no --no-auth: refuse to start (upstream message).
    let refused = Command::new(env!("CARGO_BIN_EXE_rustscrapling"))
        .args(["mcp", "--http", "--port", "0"])
        .env_remove("SCRAPLING_MCP_AUTH_TOKEN")
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&refused.stderr)
        .contains("Refusing to serve the MCP server over HTTP without authentication"));

    let mut child = Command::new(env!("CARGO_BIN_EXE_rustscrapling"))
        .args(["mcp", "--http", "--port", "0"])
        .env("SCRAPLING_MCP_AUTH_TOKEN", "test-token-123")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    stderr.read_line(&mut line).unwrap();
    let url = line.trim().rsplit(' ').next().unwrap().to_owned();
    let address = url
        .trim_start_matches("http://")
        .trim_end_matches("/mcp")
        .to_owned();
    let initialize = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"http-test","version":"1"}}});

    // Missing/wrong token, wrong path, rebinding Host.
    assert_eq!(
        http(&address, &address, None, "/mcp", None, &initialize).0,
        401
    );
    let (status, head, _) = http(&address, &address, Some("wrong"), "/mcp", None, &initialize);
    assert_eq!(status, 401);
    assert!(head.contains("www-authenticate: bearer"));
    assert_eq!(
        http(
            &address,
            &address,
            Some("test-token-123"),
            "/other",
            None,
            &initialize
        )
        .0,
        404
    );
    assert_eq!(
        http(
            &address,
            "evil.example",
            Some("test-token-123"),
            "/mcp",
            None,
            &initialize
        )
        .0,
        403
    );

    // Authenticated MCP session: initialize, list tools, call a tool.
    let (status, head, body) = http(
        &address,
        &address,
        Some("test-token-123"),
        "/mcp",
        None,
        &initialize,
    );
    assert_eq!(status, 200, "{body}");
    assert!(rpc(&body, 1)["result"]["capabilities"]["tools"].is_object());
    let session = head
        .lines()
        .find_map(|l| {
            l.strip_prefix("mcp-session-id:")
                .map(|v| v.trim().to_owned())
        })
        .expect("session id");
    let (status, _, _) = http(
        &address,
        &address,
        Some("test-token-123"),
        "/mcp",
        Some(&session),
        &json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}),
    );
    assert_eq!(status, 202);
    let (_, _, body) = http(
        &address,
        &address,
        Some("test-token-123"),
        "/mcp",
        Some(&session),
        &json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    );
    assert_eq!(
        rpc(&body, 2)["result"]["tools"].as_array().unwrap().len(),
        16
    );
    let (_, _, body) = http(
        &address,
        &address,
        Some("test-token-123"),
        "/mcp",
        Some(&session),
        &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_sessions","arguments":{}}}),
    );
    assert_eq!(
        rpc(&body, 3)["result"]["structuredContent"]["result"],
        json!([])
    );
    let _ = child.kill();
    let _ = child.wait();
}
