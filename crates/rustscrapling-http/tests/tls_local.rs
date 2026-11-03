use rustscrapling_http::{HttpClient, HttpOptions, Protocol};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tls_rejects_untrusted_cert_and_accepts_explicit_ca_bundle() {
    let generated = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), generated.cert.pem()).unwrap();
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
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
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = tokio::spawn(async move {
        for _ in 0..4 {
            let (stream, _) = listener.accept().await.unwrap();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Ok(mut stream) = acceptor.accept(stream).await {
                    let mut buffer = [0u8; 4096];
                    let _ = stream.read(&mut buffer).await;
                    let _ = stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nyes",
                        )
                        .await;
                }
            });
        }
    });

    let url = format!("https://localhost:{port}/");
    let options = HttpOptions {
        direct: true,
        impersonate: None,
        retries: 0,
        ..HttpOptions::default()
    };
    assert!(
        HttpClient::new(options.clone())
            .unwrap()
            .get(&url)
            .await
            .is_err(),
        "self-signed TLS certificate must be rejected"
    );
    // Upstream `verify=False`: an explicit opt-in that skips verification.
    let unverified = HttpOptions {
        verify_tls: false,
        ..options.clone()
    };
    let result = HttpClient::new(unverified)
        .unwrap()
        .get(&url)
        .await
        .unwrap();
    assert_eq!(result.body, b"yes");
    let with_ca = HttpOptions {
        ca_bundle: Some(file.path().to_path_buf()),
        ..options
    };
    let result = HttpClient::new(with_ca).unwrap().get(&url).await.unwrap();
    assert_eq!(result.body, b"yes");
    let fallback = HttpOptions {
        direct: true,
        impersonate: None,
        retries: 0,
        ca_bundle: Some(file.path().to_path_buf()),
        protocol: Protocol::Http3Preferred,
        http3_probe_timeout: Duration::from_millis(75),
        ..HttpOptions::default()
    };
    let response = HttpClient::new(fallback).unwrap().get(&url).await.unwrap();
    assert!(response.http3_attempted);
    assert_eq!(response.http_version, "HTTP/1.1");
    assert_eq!(response.body, b"yes");
    handle.await.unwrap();
}
