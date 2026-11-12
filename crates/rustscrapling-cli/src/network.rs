//! CLI HTTP acquisition and structured content output (Plan 6.1, 7.1).
//! Browser/auto commands use the separate fetch policy when it is connected;
//! this module never reports an unattempted browser or HTTP/3 connection.

use std::path::PathBuf;
use std::time::Duration;

use crate::browser_adapter::CdpBackend;
use crate::daemon;
use clap::{Args, ValueEnum};
use rustscrapling_daemon::{
    DaemonClient, FetchRequest as DaemonFetchRequest, FetchResult as DaemonFetchResult,
    FetchTier as DaemonTier,
};
use rustscrapling_extract::{extract, ExtractOptions, ExtractedPage};
use rustscrapling_fetch::{
    ContentInput, FetchError, FetchOptions as FetchPolicy, FetchTier, Fetcher,
};
use rustscrapling_http::{HttpClient, HttpOptions, HttpRequest, Protocol};
use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum OutputFormat {
    Json,
    Ndjson,
    Md,
    Text,
    Html,
}

#[derive(Clone, Debug, Args)]
pub struct RequestOptions {
    /// Response format (JSON by default).
    #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
    pub format: OutputFormat,
    /// Save the entire response to a file, leaving only a JSON receipt on stdout.
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Maximum number of Unicode characters printed to stdout.
    #[arg(long)]
    pub max_chars: Option<usize>,
    /// Maximum UTF-8 bytes in JSON stdout, including its trailing newline.
    #[arg(long)]
    pub max_json_bytes: Option<usize>,
    /// Rank extracted paragraphs for this research query.
    #[arg(long)]
    pub query: Option<String>,
    /// Browser emulation profile, e.g. chrome_147.
    #[arg(long)]
    pub impersonate: Option<String>,
    /// Explicit HTTP(S)/SOCKS proxy URL.
    #[arg(long)]
    pub proxy: Option<String>,
    /// Requests beyond the first try, when safe to retry.
    #[arg(long, default_value_t = 2)]
    pub retries: u32,
    /// Total timeout in seconds.
    #[arg(long, default_value_t = 30)]
    pub timeout: u64,
    /// Try HTTP/3 first, falling back when it fails.
    #[arg(long)]
    pub http3: bool,
    /// Do not follow HTTP redirect responses.
    #[arg(long)]
    pub no_redirects: bool,
    /// Disable environment proxy lookup and all explicit proxies.
    #[arg(long, conflicts_with = "proxy")]
    pub direct: bool,
    /// Custom PEM certificate authority bundle; TLS verification stays on.
    #[arg(long)]
    pub ca_bundle: Option<PathBuf>,
    /// Request body for POST, PUT and other HTTP methods.
    #[arg(long)]
    pub data: Option<String>,
    /// Chrome/Chromium executable. A private temporary profile is created.
    #[arg(long)]
    pub chrome_path: Option<PathBuf>,
    /// Attach to an existing browser-level CDP websocket (ws://.../devtools/browser/...).
    #[arg(long)]
    pub cdp_url: Option<String>,
}

fn transport_options(args: &RequestOptions) -> HttpOptions {
    let mut options = HttpOptions {
        proxy: args.proxy.clone(),
        retries: args.retries,
        timeout: Duration::from_secs(args.timeout),
        follow_redirects: !args.no_redirects,
        protocol: if args.http3 {
            Protocol::Http3Preferred
        } else {
            Protocol::Auto
        },
        ca_bundle: args.ca_bundle.clone(),
        direct: args.direct,
        ..HttpOptions::default()
    };
    if let Some(profile) = &args.impersonate {
        options.impersonate = Some(profile.clone());
    }
    options
}

fn error(kind: &str, description: &str, hint: &str) -> String {
    failure(kind, description, hint, None)
}

fn failure(kind: &str, description: &str, hint: &str, status: Option<u16>) -> String {
    let kind = if matches!(status, Some(401 | 403 | 429)) {
        "source_refused"
    } else {
        kind
    };
    let kind = match kind {
        "blocked" => "source_refused",
        "http_status" | "httpstatus" => "source_http",
        "timeout" => "stage_timeout",
        "dns" | "proxy" | "browser" | "io" => "transport",
        "tls" => "tls_rejected",
        "body_too_large" | "bodytoolarge" => "output_too_large",
        "browser_unavailable" | "browserunavailable" => "unavailable",
        "invalidrequest" => "invalid_request",
        "parse" | "conversion_failed" => "decode_failed",
        "unsupported_protocol" => "unsupported_contract",
        "invalid_request"
        | "unavailable"
        | "transport"
        | "stage_timeout"
        | "saturated"
        | "invalid_payload"
        | "unsupported_contract"
        | "network_policy_unsupported"
        | "destination_denied"
        | "source_refused"
        | "source_http"
        | "rate_limited"
        | "tls_rejected"
        | "empty"
        | "needs_rendering"
        | "decode_failed"
        | "output_too_large"
        | "cancelled"
        | "deadline_exceeded"
        | "cleanup_unconfirmed"
        | "busy"
        | "exhausted"
        | "unknown_provider_failure" => kind,
        _ => "unknown_provider_failure",
    };
    let hint = if kind == "source_refused" {
        "Respect the source access restrictions"
    } else {
        hint
    };
    // JSON escaping expands a byte to at most six bytes. These caps keep the
    // complete error, including framing, below the consumer's 8192-byte limit.
    let mut document = json!({ "schema_version": 1, "error": {
        "kind": kind, "message": bounded_text(description, 512),
        "hint": bounded_text(hint, 256)
    }});
    if let Some(status) = status {
        document["error"]["status"] = json!(status);
    }
    document.to_string()
}

fn bounded_text(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn fetch_error(err: FetchError) -> String {
    failure(err.kind.as_str(), &err.message, &err.hint, err.status)
}

fn daemon_fetch_error(err: &rustscrapling_daemon::DaemonError) -> String {
    failure(
        &err.kind,
        "Daemon acquisition failed",
        "Inspect the local daemon or use --direct",
        err.status,
    )
}

fn validate_options(options: &RequestOptions) -> Result<(), String> {
    if options.timeout == 0 || options.max_json_bytes == Some(0) {
        return Err(error(
            "invalid_request",
            "Timeout and JSON byte budget must be positive",
            "Increase the specified limit",
        ));
    }
    if options.max_json_bytes.is_some()
        && !matches!(options.format, OutputFormat::Json | OutputFormat::Ndjson)
    {
        return Err(error(
            "invalid_request",
            "--max-json-bytes requires JSON output",
            "Use --format json or ndjson",
        ));
    }
    Ok(())
}

/// Do not silently ignore transport/profile options that the daemon cannot
/// represent in its typed request. In those cases use the direct acquisition
/// path, preserving the request's documented semantics.
fn daemon_compatible(options: &RequestOptions) -> bool {
    !options.direct
        && options.proxy.is_none()
        && options.impersonate.is_none()
        && options.retries == 2
        && options.timeout == 30
        && !options.http3
        && !options.no_redirects
        && options.ca_bundle.is_none()
        && options.chrome_path.is_none()
        && options.cdp_url.is_none()
        && options.data.is_none()
}

async fn request_via_daemon(
    url: &str,
    options: &RequestOptions,
    tier: DaemonTier,
) -> Result<Option<DaemonFetchResult>, String> {
    if !daemon_compatible(options) {
        return Ok(None);
    }
    let state = daemon::state_file();
    let Some(client) = DaemonClient::discover(&state)
        .await
        .map_err(|e| daemon_fetch_error(&e))?
    else {
        return Ok(None);
    };
    let mut request = DaemonFetchRequest::get(url);
    request.tier = tier;
    client
        .fetch(request)
        .await
        .map(Some)
        .map_err(|e| daemon_fetch_error(&e))
}

pub fn run_request(method: &str, target: &str, options: &RequestOptions) -> Result<(), String> {
    validate_options(options)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| error("transport", &e.to_string(), "Check the runtime"))?;
    let response = runtime.block_on(async {
        if method == "GET" {
            if let Some(result) = request_via_daemon(target, options, DaemonTier::Http).await? {
                if !matches!(result.tier, DaemonTier::Http) {
                    return Err(error(
                        "unsupported_contract",
                        "Daemon returned a different acquisition tier",
                        "Use --direct with this daemon version",
                    ));
                }
                return Ok(rustscrapling_http::HttpResponse {
                    status: result.status,
                    final_url: result.final_url,
                    headers: result.headers,
                    body: result.body,
                    http_version: result.protocol.unwrap_or_else(|| "CDP".into()),
                    attempts: 1,
                    http3_attempted: false,
                });
            }
        }
        let client = HttpClient::new(transport_options(options)).map_err(|err| {
            error(
                err.kind.as_str(),
                &err.message,
                "Check network/proxy/TLS settings",
            )
        })?;
        let mut request = HttpRequest::new(method, target);
        if let Some(data) = &options.data {
            request = request.body(data.as_bytes().to_vec());
        }
        client.request(request).await.map_err(|err| {
            let hint = match err.kind {
                rustscrapling_http::ErrorKind::Proxy => {
                    "Check the configured proxy and credentials"
                }
                rustscrapling_http::ErrorKind::Dns => "Check DNS and hostname",
                rustscrapling_http::ErrorKind::Timeout => "Increase --timeout or retry later",
                rustscrapling_http::ErrorKind::Tls => {
                    "Check CA trust; do not disable certificate verification"
                }
                _ => "Inspect the URL and retry using another acquisition tier",
            };
            failure(err.kind.as_str(), &err.message, hint, err.status)
        })
    })?;
    let decoded = ContentInput::from(&response)
        .extract(options.query.as_deref())
        .map_err(fetch_error)?;
    let document = response_document(&response, &decoded.source, decoded.extracted.as_ref());
    emit_output(document, options, decoded.extracted.as_ref())
}

/// Runs the shared acquisition policy. Without a configured BrowserBackend,
/// blocked HTTP responses are returned as errors, *not* as fake successes.
pub fn run_tier(target: &str, options: &RequestOptions, tier: FetchTier) -> Result<(), String> {
    validate_options(options)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| error("transport", &e.to_string(), "Check Tokio runtime"))?;
    let response = runtime.block_on(async {
        let ipc_tier = match tier {
            FetchTier::Auto => DaemonTier::Auto,
            FetchTier::Http => DaemonTier::Http,
            FetchTier::Browser => DaemonTier::Browser,
            FetchTier::Headed => DaemonTier::Headed,
        };
        if let Some(result) = request_via_daemon(target, options, ipc_tier).await? {
            return Ok(rustscrapling_fetch::FetchResponse {
                status: result.status,
                final_url: result.final_url,
                headers: result.headers,
                body: result.body,
                tier: match result.tier {
                    DaemonTier::Auto => FetchTier::Auto,
                    DaemonTier::Http => FetchTier::Http,
                    DaemonTier::Browser => FetchTier::Browser,
                    DaemonTier::Headed => FetchTier::Headed,
                },
                protocol: result.protocol,
            });
        }
        let client = HttpClient::new(transport_options(options))
            .map_err(|e| error(e.kind.as_str(), &e.message, "Check network options"))?;
        let mut fetcher = Fetcher::new(client, FetchPolicy::default());
        if let Some(backend) = CdpBackend::new(
            options.chrome_path.clone(),
            options.cdp_url.clone(),
            options.proxy.clone(),
        ) {
            fetcher = fetcher.with_browser(backend);
        }
        fetcher
            .fetch(HttpRequest::get(target), tier)
            .await
            .map_err(fetch_error)
    })?;
    let decoded = ContentInput::from(&response)
        .extract(options.query.as_deref())
        .map_err(fetch_error)?;
    let proxy = rustscrapling_http::HttpResponse {
        status: response.status,
        final_url: response.final_url,
        headers: response.headers,
        body: response.body,
        http_version: response.protocol.unwrap_or_else(|| "CDP".into()),
        attempts: 1,
        http3_attempted: false,
    };
    let mut document = response_document(&proxy, &decoded.source, decoded.extracted.as_ref());
    document["tier"] = serde_json::to_value(response.tier).unwrap_or_else(|_| json!("unknown"));
    emit_output(document, options, decoded.extracted.as_ref())
}

/// Accept a local file (or stdin) without a network round-trip, while
/// keeping `md URL` on the exact same extraction and output contract.
pub fn run_markdown(target: &str, options: &RequestOptions) -> Result<(), String> {
    validate_options(options)?;
    if target.starts_with("http://") || target.starts_with("https://") {
        return run_request("GET", target, options);
    }
    let html = if target == "-" {
        use std::io::Read;
        let mut source = String::new();
        std::io::stdin()
            .take(32 * 1024 * 1024)
            .read_to_string(&mut source)
            .map_err(|e| error("io", &e.to_string(), "Provide UTF-8 HTML on stdin"))?;
        source
    } else {
        std::fs::read_to_string(target)
            .map_err(|e| error("io", &e.to_string(), "Check the source file"))?
    };
    let parsed = extract(
        &html,
        ExtractOptions {
            page_url: None,
            query: options.query.as_deref(),
        },
    )
    .map_err(|msg| error("parse", &msg, "Inspect the input HTML"))?;
    let document = json!({
        "schema_version": 1,
        "url": target,
        "status": null,
        "tier": "local",
        "protocol": null,
        "metadata": parsed.metadata,
        "headings": parsed.headings,
        "paragraphs": parsed.paragraphs,
        "content": {"html":parsed.html,"text":parsed.text,"markdown":parsed.markdown},
        "truncated": false,
    });
    emit_output(document, options, Some(&parsed))
}

fn response_document(
    response: &rustscrapling_http::HttpResponse,
    decoded: &str,
    extracted: Option<&ExtractedPage>,
) -> Value {
    let content = extracted
        .map(|page| {
            json!({
                "text": page.text, "markdown": page.markdown, "html": page.html,
            })
        })
        .unwrap_or_else(|| json!({ "text": decoded, "markdown": decoded, "html": decoded }));
    let metadata = extracted
        .map(|page| json!(page.metadata))
        .unwrap_or_else(|| json!({}));
    let headings = extracted
        .map(|page| json!(page.headings))
        .unwrap_or_else(|| json!([]));
    let paragraphs = extracted
        .map(|page| json!(page.paragraphs))
        .unwrap_or_else(|| json!([]));
    json!({
        "schema_version": 1,
        "url": response.final_url,
        "status": response.status,
        "tier": "http",
        "protocol": response.http_version,
        "http3_attempted": response.http3_attempted,
        "attempts": response.attempts,
        "headers": response.headers,
        "metadata": metadata,
        "headings": headings,
        "paragraphs": paragraphs,
        "content": content,
        "truncated": false,
    })
}

fn format_document(document: &Value, format: OutputFormat) -> Result<String, String> {
    let formatted = match format {
        OutputFormat::Json | OutputFormat::Ndjson => document.to_string(),
        OutputFormat::Md => document
            .pointer("/content/markdown")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        OutputFormat::Text => document
            .pointer("/content/text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        OutputFormat::Html => document
            .pointer("/content/html")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    };
    Ok(format!("{formatted}\n"))
}

fn emit_output(
    document: Value,
    options: &RequestOptions,
    extracted: Option<&ExtractedPage>,
) -> Result<(), String> {
    let full = format_document(&document, options.format)?;
    if let Some(path) = &options.out {
        let receipt = json!({
            "schema_version": 1, "saved_to": path.to_string_lossy(),
            "chars": full.chars().count(),
            "headings": extracted.map(|page| page.headings.clone()).unwrap_or_default()
        });
        let receipt = bounded_json(receipt, options)?;
        std::fs::write(path, &full)
            .map_err(|err| error("io", &err.to_string(), "Check --out permissions and path"))?;
        print!("{receipt}");
    } else if matches!(options.format, OutputFormat::Json | OutputFormat::Ndjson) {
        print!("{}", bounded_json(document, options)?);
    } else if let Some(limit) = options.max_chars {
        print!("{}", full.chars().take(limit).collect::<String>());
    } else {
        print!("{full}");
    }
    Ok(())
}

fn within_limits(output: &str, options: &RequestOptions) -> bool {
    options
        .max_json_bytes
        .is_none_or(|limit| output.len() <= limit)
        && options
            .max_chars
            .is_none_or(|limit| output.chars().count() <= limit)
}

fn bounded_json(mut document: Value, options: &RequestOptions) -> Result<String, String> {
    let full = format_document(&document, OutputFormat::Json)?;
    if within_limits(&full, options) {
        return Ok(full);
    }
    document["truncated"] = json!(true);
    // These fields duplicate or describe the source. Preserve their schema while
    // reserving the remaining budget for the fetched content and its identity.
    if let Some(metadata) = document.get_mut("metadata") {
        if metadata
            .as_object()
            .is_some_and(|fields| !fields.is_empty())
        {
            *metadata = json!(rustscrapling_extract::Metadata::default());
        }
    }
    for field in ["headings", "paragraphs", "headers"] {
        if let Some(value) = document.get_mut(field) {
            *value = json!([]);
        }
    }
    if document.get("content").is_some() {
        let originals: Vec<String> = ["text", "markdown", "html"]
            .iter()
            .map(|field| {
                document
                    .pointer(&format!("/content/{field}"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect();
        let require_markdown = !originals[1].trim().is_empty();
        let mut low = 0usize;
        let mut high = originals
            .iter()
            .map(|text| text.chars().count())
            .max()
            .unwrap_or(0);
        while low < high {
            let mid = low + (high - low).div_ceil(2);
            let mut probe = document.clone();
            shrink_contents(&mut probe, &originals, mid);
            if within_limits(&format_document(&probe, OutputFormat::Json)?, options) {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        shrink_contents(&mut document, &originals, low);
        if require_markdown
            && document["content"]["markdown"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .is_empty()
        {
            return Err(output_budget_error());
        }
    }
    let bounded = format_document(&document, OutputFormat::Json)?;
    if !within_limits(&bounded, options) {
        return Err(output_budget_error());
    }
    Ok(bounded)
}

fn output_budget_error() -> String {
    error(
        "invalid_request",
        "Output budget cannot hold the required JSON fields and content",
        "Increase --max-chars or --max-json-bytes, or use --out",
    )
}

fn shrink_contents(document: &mut Value, contents: &[String], limit: usize) {
    document["truncated"] = Value::Bool(true);
    for (field, content) in ["text", "markdown", "html"].iter().zip(contents) {
        document["content"][*field] = Value::String(content.chars().take(limit).collect());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_json_is_bounded_after_escaping() {
        let output = failure(
            "browser",
            &"\0\n🦀".repeat(10000),
            &"\0".repeat(10000),
            Some(502),
        );
        assert!(output.len() + 1 <= 8192);
        let value: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value["error"]["kind"], "transport");
        assert_eq!(value["error"]["status"], 502);
    }

    #[test]
    fn json_is_never_sliced_mid_record() {
        let mut doc = json!({"schema_version":1,"content":{"text":"漢字とemoji🦀","markdown":"あいう", "html":"<p>こんにちは</p>"},"truncated":false});
        let originals = vec![
            "漢字とemoji🦀".into(),
            "あいう".into(),
            "<p>こんにちは</p>".into(),
        ];
        shrink_contents(&mut doc, &originals, 2);
        let s = format_document(&doc, OutputFormat::Json).unwrap();
        assert!(serde_json::from_str::<Value>(&s).is_ok());
        assert_eq!(doc["content"]["text"], "漢字");
        assert_eq!(doc["truncated"], true);
    }
}
