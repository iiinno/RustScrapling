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
