//! Streamable-HTTP transport (upstream `scrapling mcp --http`).
//!
//! Mirrors upstream `ScraplingMCPServer.serve`: the transport refuses to start
//! without a bearer token unless `allow_unauthenticated` is set, compares the
//! token in constant time, and turns on DNS-rebinding protection for the
//! given `allowed_hosts` (Host and Origin checks). Without explicit hosts,
//! loopback binds accept only loopback Host headers (as the MCP SDK does for
//! local servers). All clients share one executor, so sessions opened by one
//! request are visible to the next, like upstream's single server object.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::net::TcpListener;

use crate::RustScraplingMcp;

/// Environment variable read for the token when none is passed explicitly.
pub const MCP_AUTH_TOKEN_ENV: &str = "SCRAPLING_MCP_AUTH_TOKEN";

#[derive(Clone, Debug)]
pub struct HttpServeOptions {
    pub host: String,
    pub port: u16,
    pub auth_token: Option<String>,
    pub allowed_hosts: Vec<String>,
    pub allow_unauthenticated: bool,
}

/// Upstream's refusal / warning logic. `Ok(Some(warning))` means start but
/// print the warning.
pub fn check_auth_policy(options: &HttpServeOptions) -> Result<Option<String>, String> {
    let reach = format!("{}:{}", options.host, options.port);
    match (&options.auth_token, options.allow_unauthenticated) {
        (Some(_), true) => Ok(Some(
            "An authentication token was given, so it takes precedence and the server still requires it.".into(),
        )),
        (Some(_), false) => Ok(None),
        (None, false) => Err(format!(
            "Refusing to serve the MCP server over HTTP without authentication because anyone who can reach \
             {reach} would be able to use every tool, including fetching arbitrary URLs from this machine. \
             Pass `--auth-token` (or set the {MCP_AUTH_TOKEN_ENV} environment variable) to require a bearer \
             token, or `--no-auth` to serve it unauthenticated anyway."
        )),
        (None, true) => Ok(Some(format!(
            "The MCP server is running over HTTP without authentication, so anyone who can reach {reach} \
             can use every tool, including fetching arbitrary URLs from this machine."
        ))),
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

fn unauthorized() -> Response<BoxBody<Bytes, Infallible>> {
    let body = r#"{"error":"invalid_token","error_description":"Authentication required"}"#;
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header("Content-Type", "application/json")
        .header(
            "WWW-Authenticate",
            r#"Bearer error="invalid_token", error_description="Authentication required""#,
        )
        .body(Full::new(Bytes::from_static(body.as_bytes())).boxed())
        .expect("static response")
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]")
}

/// Build the rmcp service configuration for these options.
pub fn service_config(options: &HttpServeOptions) -> StreamableHttpServerConfig {
    let config = StreamableHttpServerConfig::default();
    if !options.allowed_hosts.is_empty() {
        let origins: Vec<String> = options
            .allowed_hosts
            .iter()
            .flat_map(|host| [format!("http://{host}"), format!("https://{host}")])
            .collect();
        config
            .with_allowed_hosts(options.allowed_hosts.clone())
            .with_allowed_origins(origins)
    } else if is_loopback_host(&options.host) {
        config
    } else {
        // Upstream: no protection unless hosts are listed for public binds.
        config.disable_allowed_hosts()
    }
}

/// Serve until the listener fails. Returns the bound address through
/// `on_bound` (useful with port 0).
pub async fn serve_http(
    server: RustScraplingMcp,
    options: HttpServeOptions,
    on_bound: impl FnOnce(SocketAddr),
) -> Result<(), String> {
    if let Some(warning) = check_auth_policy(&options)? {
        eprintln!("WARNING: {warning}");
    }
    let listener = TcpListener::bind((options.host.as_str(), options.port))
        .await
        .map_err(|e| format!("cannot bind {}:{}: {e}", options.host, options.port))?;
    on_bound(listener.local_addr().map_err(|e| e.to_string())?);
    let service = Arc::new(StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(LocalSessionManager::default()),
        service_config(&options),
    ));
    let token: Option<Arc<[u8]>> = options.auth_token.map(|t| Arc::from(t.into_bytes()));
    loop {
        let (stream, _) = listener.accept().await.map_err(|e| e.to_string())?;
        let service = Arc::clone(&service);
        let token = token.clone();
        tokio::spawn(async move {
            let handler = hyper::service::service_fn(move |request: Request<Incoming>| {
                let service = Arc::clone(&service);
                let token = token.clone();
                async move {
                    // Upstream (FastMCP) mounts the transport at /mcp only.
                    if !matches!(request.uri().path(), "/mcp" | "/mcp/") {
                        return Ok::<_, Infallible>(
                            Response::builder()
                                .status(StatusCode::NOT_FOUND)
                                .body(Full::new(Bytes::from_static(b"Not Found")).boxed())
                                .expect("static response"),
                        );
                    }
                    if let Some(token) = &token {
                        let presented = request
                            .headers()
                            .get(hyper::header::AUTHORIZATION)
                            .and_then(|v| v.to_str().ok())
                            .and_then(|v| v.strip_prefix("Bearer "))
                            .map(str::as_bytes);
                        if !presented.is_some_and(|p| constant_time_eq(p, token)) {
                            return Ok::<_, Infallible>(unauthorized());
                        }
                    }
                    Ok(service.handle(request).await)
                }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), handler)
                .await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(token: Option<&str>, no_auth: bool) -> HttpServeOptions {
        HttpServeOptions {
            host: "127.0.0.1".into(),
            port: 8000,
            auth_token: token.map(str::to_owned),
            allowed_hosts: Vec::new(),
            allow_unauthenticated: no_auth,
        }
    }

    #[test]
    fn auth_policy_matches_upstream() {
        assert!(check_auth_policy(&options(None, false))
            .unwrap_err()
            .starts_with("Refusing to serve the MCP server over HTTP without authentication"));
        assert_eq!(check_auth_policy(&options(Some("t"), false)), Ok(None));
        assert!(check_auth_policy(&options(Some("t"), true))
            .unwrap()
            .unwrap()
            .contains("takes precedence"));
        assert!(check_auth_policy(&options(None, true))
            .unwrap()
            .unwrap()
            .contains("without authentication"));
    }

    #[test]
    fn allowed_hosts_turn_on_host_and_origin_checks() {
        let mut opts = options(Some("t"), false);
        assert_eq!(
            service_config(&opts).allowed_hosts,
            vec!["localhost", "127.0.0.1", "::1"]
        );
        opts.host = "0.0.0.0".into();
        assert!(service_config(&opts).allowed_hosts.is_empty());
        opts.allowed_hosts = vec!["mcp.example.com:8000".into()];
        let config = service_config(&opts);
        assert_eq!(config.allowed_hosts, vec!["mcp.example.com:8000"]);
        assert_eq!(
            config.allowed_origins,
            vec![
                "http://mcp.example.com:8000",
                "https://mcp.example.com:8000"
            ]
        );
    }
}
