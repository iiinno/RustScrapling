//! End-to-end MCP protocol test against the real server binary and pipes.
use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

#[test]
fn stdio_initialize_list_and_tool_calls() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rustscrapling-mcp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start MCP server");

    let messages = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-06-18","capabilities":{},
            "clientInfo":{"name":"stage7-test","version":"1.0"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
            "name":"browser_open","arguments":{}}}),
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{
            "name":"make_request","arguments":{"url":"https://example.com"}}}),
        json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{
            "name":"list_sessions","arguments":{}}}),
        json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{
            "name":"browser_network_requests","arguments":{"session_id":"test","limit":201}}}),
        json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{
            "name":"not_a_tool","arguments":{}}}),
        json!({"jsonrpc":"2.0","id":8,"method":"ping","params":{}}),
    ];
    {
        let mut stdin = child.stdin.take().unwrap();
        for message in messages {
            writeln!(stdin, "{message}").expect("write JSON-RPC message");
        }
    }
    let output = child.wait_with_output().expect("server exits on stdin EOF");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let responses: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).expect("stdout is JSON-RPC only"))
        .collect();
    let response = |id: i64| {
        responses
            .iter()
            .find(|msg| msg["id"] == id)
            .unwrap_or_else(|| panic!("missing response {id}: {responses:?}"))
    };
    assert!(response(1)["result"]["protocolVersion"].is_string());
    let catalog = response(2)["result"]["tools"].as_array().unwrap();
    assert_eq!(catalog.len(), 16);
    for tool in catalog {
        assert!(tool["description"].as_str().is_some_and(|s| !s.is_empty()));
        for field in tool["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .values()
        {
            assert!(field["description"].as_str().is_some_and(|s| !s.is_empty()));
        }
    }
    let names: Vec<&str> = catalog
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"browser_screenshot"));
    assert!(names.contains(&"session_make_request"));
    assert_eq!(response(3)["result"]["isError"], true);
    assert!(response(3)["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("backend_unavailable"));
    assert_eq!(response(4)["result"]["isError"], true);
    assert_eq!(response(5)["result"]["isError"], false);
    assert_eq!(response(5)["result"]["content"][0]["text"], "[]");
    assert_eq!(response(6)["result"]["isError"], true);
    assert!(response(6)["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("invalid_arguments"));
    assert!(response(7)["error"]["code"].as_i64().unwrap() < 0);
    assert!(response(8)["result"].is_object());
}
