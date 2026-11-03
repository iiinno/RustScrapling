use rustscrapling_http::{decode_body, HttpClient, HttpOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const EXPECTED: &str = "日本語のテスト";

async fn serve(response: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        if let Ok((mut socket, _)) = listener.accept().await {
            let mut buffer = [0u8; 4096];
            let _ = socket.read(&mut buffer).await;
            let _ = socket.write_all(&response).await;
            let _ = socket.shutdown().await;
        }
    });
    format!("http://{addr}")
}

fn reply(content_type: Option<&str>, body: &[u8]) -> Vec<u8> {
    let mut head = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(value) = content_type {
        head.push_str(&format!("Content-Type: {value}\r\n"));
    }
    head.push_str("\r\n");
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

fn direct_options() -> HttpOptions {
    HttpOptions {
        direct: true,
        impersonate: None,
        retries: 0,
        ..HttpOptions::default()
    }
}

async fn fetch(url: &str) -> (Vec<(String, String)>, Vec<u8>, String) {
    let response = HttpClient::new(direct_options())
        .unwrap()
        .get(url)
        .await
        .unwrap();
    (response.headers, response.body, response.final_url)
}

#[tokio::test]
async fn content_type_charset_shift_jis_is_decoded() {
    let (bytes, _, _) = encoding_rs::SHIFT_JIS.encode(EXPECTED);
    let url = serve(reply(Some("text/html; charset=Shift_JIS"), &bytes)).await;
    let (headers, body, final_url) = fetch(&url).await;
    let (text, encoding) = decode_body(&headers, &body, &final_url);
    assert_eq!(text, EXPECTED);
    assert_eq!(encoding, "Shift_JIS");
}

#[tokio::test]
async fn meta_charset_euc_jp_without_header_is_decoded() {
    let page = format!(
        "<html><head><meta charset=\"EUC-JP\"></head><body><p>{EXPECTED}</p></body></html>"
    );
    let (bytes, _, _) = encoding_rs::EUC_JP.encode(&page);
    let url = serve(reply(None, &bytes)).await;
    let (headers, body, final_url) = fetch(&url).await;
    let (text, encoding) = decode_body(&headers, &body, &final_url);
    assert!(
        text.contains(EXPECTED),
        "decoded text lost the Japanese body"
    );
    assert_eq!(encoding, "EUC-JP");
}

#[tokio::test]
async fn utf8_bom_is_decoded_as_utf8() {
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(EXPECTED.as_bytes());
    let url = serve(reply(None, &bytes)).await;
    let (headers, body, final_url) = fetch(&url).await;
    let (text, encoding) = decode_body(&headers, &body, &final_url);
    assert_eq!(
        text, EXPECTED,
        "the byte-order mark must not survive decoding"
    );
    assert_eq!(encoding, "UTF-8");
}

#[tokio::test]
async fn detected_shift_jis_without_any_declaration_is_decoded() {
    let page = format!(
        "<html><head><title>{EXPECTED}</title></head><body><h1>{EXPECTED}</h1>\
         <p>{EXPECTED}。文字コードの推定は本文の統計的な特徴から行われる。</p>\
         <p>{EXPECTED}。宣言が無い場合でも読み手が読める本文を返す必要がある。</p>\
         <p>{EXPECTED}。推定を誤ると本文全体が読めなくなるため、確からしさを記録する。</p>\
         </body></html>"
    );
    let (bytes, _, _) = encoding_rs::SHIFT_JIS.encode(&page);
    let url = serve(reply(None, &bytes)).await;
    let (headers, body, final_url) = fetch(&url).await;
    let (text, encoding) = decode_body(&headers, &body, &final_url);
    assert!(
        text.contains(EXPECTED),
        "guessed encoding {encoding} produced no readable text"
    );
    assert!(
        !text.contains('\u{FFFD}'),
        "guessed encoding {encoding} produced replacement characters"
    );
}
