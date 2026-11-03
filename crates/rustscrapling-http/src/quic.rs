//! Optional HTTP/3 first attempt, implemented over verified QUIC/TLS.
//! Proxy endpoints are intentionally never bypassed to send direct UDP traffic.

use super::{ErrorKind, HttpError, HttpOptions, HttpResponse};
use bytes::{Buf, Bytes};
use std::net::SocketAddr;
use std::sync::Arc;
use url::Url;
use wreq::header::HeaderMap;
use wreq::Method;

pub(super) fn can_attempt(options: &HttpOptions, url: &Url) -> bool {
    if url.scheme() != "https" {
        return false;
    }
    if options.direct {
        return true;
    }
    if options.proxy.is_some()
        || options.proxy_rotator.is_some()
        || options.proxies.contains_key("https")
    {
        return false;
    }
    // The QUIC implementation does not tunnel through HTTP/SOCKS proxies.
    // Preserve the environment's proxy policy by letting wreq handle those.
    !["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"]
        .into_iter()
        .any(|key| std::env::var_os(key).is_some())
}

fn roots(options: &HttpOptions) -> Result<rustls::RootCertStore, HttpError> {
    let mut roots = rustls::RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    for cert in native.certs {
        let _ = roots.add(cert);
    }
    let file = options
        .ca_bundle
        .clone()
        .or_else(|| std::env::var_os("SSL_CERT_FILE").map(Into::into));
    if let Some(path) = file {
        let bytes = std::fs::read(path)
            .map_err(|_| HttpError::new(ErrorKind::Tls, "could not read CA bundle"))?;
        let mut found = false;
        for cert in <rustls::pki_types::CertificateDer<'static> as rustls::pki_types::pem::PemObject>::pem_slice_iter(&bytes) {
            let cert = cert.map_err(|_| HttpError::new(ErrorKind::Tls, "invalid CA bundle"))?;
            roots
                .add(cert)
                .map_err(|_| HttpError::new(ErrorKind::Tls, "invalid CA certificate"))?;
            found = true;
        }
        if !found {
            return Err(HttpError::new(
                ErrorKind::Tls,
                "CA bundle contains no certificates",
            ));
        }
    }
    if roots.is_empty() {
        return Err(HttpError::new(
            ErrorKind::Tls,
            "no trusted CA certificates available",
        ));
    }
    Ok(roots)
}

pub(super) async fn fetch(
    options: &HttpOptions,
    method: &Method,
    url: &Url,
    headers: &HeaderMap,
    body: Option<&[u8]>,
) -> Result<HttpResponse, HttpError> {
    let host = url
        .host_str()
        .ok_or_else(|| HttpError::new(ErrorKind::InvalidRequest, "URL has no host"))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| HttpError::new(ErrorKind::InvalidRequest, "URL has no port"))?;
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| HttpError::new(ErrorKind::Dns, "HTTP/3 DNS lookup failed"))?
        .collect();
    let addr = addrs
        .iter()
        .find(|addr| addr.is_ipv4())
        .or_else(|| addrs.first())
        .ok_or_else(|| HttpError::new(ErrorKind::Dns, "HTTP/3 DNS lookup returned no addresses"))?;

    // Both ring and aws-lc can be enabled transitively by the HTTP and QUIC
    // stacks. Never rely on rustls' process-global CryptoProvider inference.
    let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|_| HttpError::new(ErrorKind::Tls, "HTTP/3 TLS protocol configuration failed"))?
    .with_root_certificates(roots(options)?)
    .with_no_client_auth();
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let tls = quinn::crypto::rustls::QuicClientConfig::try_from(tls)
        .map_err(|_| HttpError::new(ErrorKind::Tls, "invalid QUIC TLS configuration"))?;
    let client_config = quinn::ClientConfig::new(Arc::new(tls));
    let bind: SocketAddr = if addr.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    }
    .parse()
    .expect("valid bind address");
    let mut endpoint = quinn::Endpoint::client(bind)
        .map_err(|_| HttpError::new(ErrorKind::Transport, "could not bind UDP socket"))?;
    endpoint.set_default_client_config(client_config);
    let conn = endpoint
        .connect(*addr, host)
        .map_err(|_| HttpError::new(ErrorKind::Transport, "HTTP/3 connection setup failed"))?
        .await
        .map_err(|_| HttpError::new(ErrorKind::Transport, "HTTP/3 handshake failed"))?;

    let (mut driver, mut sender) = h3::client::new(h3_quinn::Connection::new(conn.clone()))
        .await
        .map_err(|_| HttpError::new(ErrorKind::Transport, "HTTP/3 negotiation failed"))?;
    let driver = tokio::spawn(async move {
        std::future::poll_fn(|cx| driver.poll_close(cx)).await;
    });

    let result = async {
        let mut builder = http::Request::builder()
            .method(method.clone())
            .uri(url.as_str());
        for (name, value) in headers {
            builder = builder.header(name, value);
        }
        let request = builder.body(()).map_err(|_| {
            HttpError::new(ErrorKind::InvalidRequest, "invalid HTTP/3 request headers")
        })?;
        let mut stream = sender
            .send_request(request)
            .await
            .map_err(|_| HttpError::new(ErrorKind::Transport, "HTTP/3 request failed"))?;
        if let Some(body) = body {
            stream
                .send_data(Bytes::copy_from_slice(body))
                .await
                .map_err(|_| HttpError::new(ErrorKind::Transport, "HTTP/3 request body failed"))?;
        }
        stream
            .finish()
            .await
            .map_err(|_| HttpError::new(ErrorKind::Transport, "HTTP/3 request finish failed"))?;
        let response = stream
            .recv_response()
            .await
            .map_err(|_| HttpError::new(ErrorKind::Transport, "HTTP/3 response failed"))?;
        let status = response.status().as_u16();
        let mut response_headers = Vec::new();
        for (name, value) in response.headers() {
            response_headers.push((
                name.as_str().to_owned(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            ));
        }
        response_headers.sort();
        if response
            .headers()
            .get(http::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .is_some_and(|v| v > options.max_body_bytes as u64)
        {
            return Err(HttpError::new(
                ErrorKind::BodyTooLarge,
                "HTTP/3 response exceeds body limit",
            ));
        }
        let mut received = Vec::new();
        while let Some(mut chunk) = stream
            .recv_data()
            .await
            .map_err(|_| HttpError::new(ErrorKind::Transport, "HTTP/3 response body failed"))?
        {
            let len = chunk.remaining();
            if len > options.max_body_bytes.saturating_sub(received.len()) {
                return Err(HttpError::new(
                    ErrorKind::BodyTooLarge,
                    "HTTP/3 response exceeded body limit",
                ));
            }
            received.extend_from_slice(&chunk.copy_to_bytes(len));
        }
        Ok(HttpResponse {
            status,
            final_url: url.to_string(),
            headers: response_headers,
            body: received,
            http_version: "HTTP/3".into(),
            attempts: 1,
            http3_attempted: true,
        })
    }
    .await;
    driver.abort();
    conn.close(0u32.into(), b"completed");
    result
}
