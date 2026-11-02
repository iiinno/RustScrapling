use bytes::Bytes;
use rustscrapling_http::{HttpClient, HttpOptions, Protocol};
use std::sync::Arc;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http3_is_negotiated_over_local_quic_with_a_trusted_ca() {
    let generated = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let pem = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(pem.path(), generated.cert.pem()).unwrap();

    let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![generated.cert.der().clone()],
        rustls::pki_types::PrivatePkcs8KeyDer::from(generated.key_pair.serialize_der()).into(),
    )
    .unwrap();
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let tls = quinn::crypto::rustls::QuicServerConfig::try_from(tls).unwrap();
    let config = quinn::ServerConfig::with_crypto(Arc::new(tls));
    let endpoint = quinn::Endpoint::server(config, "127.0.0.1:0".parse().unwrap()).unwrap();
    let port = endpoint.local_addr().unwrap().port();

    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        let connection = endpoint.accept().await.unwrap().await.unwrap();
        let mut server = h3::server::Connection::new(h3_quinn::Connection::new(connection))
            .await
            .unwrap();
        if let Some(resolver) = server.accept().await.unwrap() {
            let (request, mut stream) = resolver.resolve_request().await.unwrap();
            assert_eq!(request.uri().path(), "/h3");
            stream
                .send_response(http::Response::builder().status(200).body(()).unwrap())
                .await
                .unwrap();
            stream
                .send_data(Bytes::from_static(b"real-h3"))
                .await
                .unwrap();
            stream.finish().await.unwrap();
        }
        // QUIC sends are asynchronous. Keep the endpoint and HTTP/3
        // connection alive until the client has received the response.
        let _ = tokio::time::timeout(Duration::from_secs(10), done_rx).await;
    });

    let options = HttpOptions {
        direct: true,
        impersonate: None,
        protocol: Protocol::Http3Only,
        ca_bundle: Some(pem.path().to_path_buf()),
        retries: 0,
        http3_probe_timeout: Duration::from_secs(5),
        ..HttpOptions::default()
    };
    let client = HttpClient::new(options).unwrap();
    let response = client
        .get(format!("https://localhost:{port}/h3"))
        .await
        .unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"real-h3");
    assert_eq!(response.http_version, "HTTP/3");
    assert!(response.http3_attempted);
    let _ = done_tx.send(());
    handle.await.unwrap();
}
