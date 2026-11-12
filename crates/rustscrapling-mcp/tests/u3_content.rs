use rustscrapling_mcp::RustScraplingMcp;
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

#[tokio::test]
async fn http_callers_share_strict_decoding_and_content_acceptance() {
    type Case<'a> = (u16, &'a str, &'a [u8], Result<&'a str, &'a str>);
    let cases: &[Case<'_>] = &[
        (
            200,
            "text/plain; charset=Shift_JIS",
            b"\x93\xfa\x96\x7b\x8c\xea",
            Ok("日本語"),
        ),
        (200, "text/plain; charset=utf-8", b"OK", Ok("OK")),
        (
            200,
            "text/plain; charset=utf-8",
            b"\x93\xfa\x96\x7b\x8c\xea",
            Err("decode_failed"),
        ),
        (200, "text/plain", b"", Err("empty")),
        (204, "text/plain", b"", Err("empty")),
        (
            200,
            "text/html",
            b"<html><body><div id='app'></div><script src='/app.js'></script></body></html>",
            Err("needs_rendering"),
        ),
        (403, "text/plain", b"refused", Err("source_refused")),
    ];
    for &(status, content_type, body, expected) in cases {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/content", listener.local_addr().unwrap());
        let mut response = format!(
            "HTTP/1.1 {status} Fixture\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        let worker = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut served = 0;
            while served < 2 && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        socket
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        socket
                            .set_write_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut request = Vec::new();
                        let mut buffer = [0; 1024];
                        while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                            let size = socket.read(&mut buffer).unwrap();
                            assert!(size > 0 && request.len() + size <= 16384);
                            request.extend_from_slice(&buffer[..size]);
                        }
                        socket.write_all(&response).unwrap();
                        served += 1;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("fixture accept failed: {error}"),
                }
            }
            assert_eq!(served, 2);
        });
        let server = RustScraplingMcp::default();
        let opened = server
            .execute_tool(
                "open_request_session",
                json!({"session_id": "u3-content"})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .await
            .unwrap();
        assert_eq!(opened.is_error, Some(false));
        for tool in ["make_request", "session_make_request"] {
            let mut args = json!({"url": url});
            if tool == "session_make_request" {
                args["session_id"] = json!("u3-content");
            }
            let result = server
                .execute_tool(tool, args.as_object().unwrap().clone())
                .await
                .unwrap();
            match expected {
                Ok(text) => {
                    assert_eq!(result.is_error, Some(false), "{result:?}");
                    let output = result.structured_content.as_ref().unwrap();
                    assert_eq!(output["status"], status);
                    assert_eq!(output["content"][0].as_str().unwrap().trim(), text);
                }
                Err(kind) => {
                    assert_eq!(result.is_error, Some(true), "{result:?}");
                    assert!(serde_json::to_string(&result.content)
                        .unwrap()
                        .contains(kind));
                }
            }
        }
        worker.join().unwrap();
    }
}
