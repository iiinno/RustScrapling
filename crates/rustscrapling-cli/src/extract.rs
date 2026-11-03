//! `rustscrapling extract get|post|put|delete|fetch URL OUTPUT_FILE`: the
//! upstream `scrapling extract` command group (Scrapling v0.5 `cli.py`).
//!
//! The page is fetched through the same MCP executor as `rustscrapling mcp`
//! (`make_request`, or `browser_open` + `browser_fetch` + `close_session`
//! for `fetch`) and written like upstream `Convertor.write_content_to_file`:
//! the output extension selects HTML/Markdown/text, `--css-selector` keeps all
//! matches, `--ai-targeted` selects main content only, chunks are joined
//! with a blank line for Markdown. The file is written only after success.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use rustscrapling_mcp::RustScraplingMcp;
use serde_json::{json, Map, Value};

use crate::curl::{parse_cookie_string, parse_header_lines, upsert};

/// Resolve upstream's `--x/--no-x` flag pairs: the later one wins (clap
/// `overrides_with`), otherwise the default.
fn pair(on: bool, off: bool, default: bool) -> bool {
    if off {
        false
    } else if on {
        true
    } else {
        default
    }
}

#[derive(Args)]
pub struct ExtractArgs {
    #[command(subcommand)]
    command: ExtractCommand,
}

#[derive(Subcommand)]
enum ExtractCommand {
    /// Perform a GET request and save the content to a file (.html/.md/.txt).
    Get(HttpArgs),
    /// Perform a POST request and save the content to a file (.html/.md/.txt).
    Post(HttpArgs),
    /// Perform a PUT request and save the content to a file (.html/.md/.txt).
    Put(HttpArgs),
    /// Perform a DELETE request and save the content to a file (.html/.md/.txt).
    Delete(HttpArgs),
    /// Fetch with the stealth browser and save the content (.html/.md/.txt).
    Fetch(BrowserArgs),
}

#[derive(Args)]
struct HttpArgs {
    url: String,
    output_file: PathBuf,
    /// Extract only main content and sanitize hidden elements for AI consumption.
    #[arg(long)]
    ai_targeted: bool,
    /// Use stealthy browser headers (default: true).
    #[arg(long, overrides_with = "no_stealthy_headers")]
    stealthy_headers: bool,
    #[arg(long, overrides_with = "stealthy_headers")]
    no_stealthy_headers: bool,
    /// Browser to impersonate; a comma-separated list picks one at random.
    #[arg(long)]
    impersonate: Option<String>,
    /// Verify TLS certificates (default: true).
    #[arg(long, overrides_with = "no_verify")]
    verify: bool,
    #[arg(long, overrides_with = "verify")]
    no_verify: bool,
    /// Follow redirects (default: true).
    #[arg(long, overrides_with = "no_follow_redirects")]
    follow_redirects: bool,
    #[arg(long, overrides_with = "follow_redirects")]
    no_follow_redirects: bool,
    /// Query parameter "key=value" (repeatable).
    #[arg(short = 'p', long = "params")]
    params: Vec<String>,
    /// CSS selector; all matches are kept.
    #[arg(short = 's', long)]
    css_selector: Option<String>,
    /// Proxy URL, e.g. http://user:pass@host:port.
    #[arg(long)]
    proxy: Option<String>,
    /// Request timeout in seconds.
    #[arg(long, default_value_t = 30)]
    timeout: u64,
    /// Cookies "name1=value1; name2=value2".
    #[arg(long)]
    cookies: Option<String>,
    /// HTTP header "Key: Value" (repeatable).
    #[arg(short = 'H', long = "headers")]
    headers: Vec<String>,
    /// JSON request body (POST/PUT only).
    #[arg(short = 'j', long)]
    json: Option<String>,
    /// Form data request body, e.g. "a=1&b=2" (POST/PUT only).
    #[arg(short = 'd', long)]
    data: Option<String>,
}

#[derive(Args)]
struct BrowserArgs {
    url: String,
    output_file: PathBuf,
    #[arg(long)]
    ai_targeted: bool,
    /// Browser executable (falls back to SCRAPLING_EXECUTABLE_PATH).
    #[arg(long)]
    executable_path: Option<String>,
    /// Extra header "Key: Value" (repeatable).
    #[arg(short = 'H', long = "extra-headers")]
    extra_headers: Vec<String>,
    #[arg(long)]
    proxy: Option<String>,
    #[arg(long, overrides_with = "no_real_chrome")]
    real_chrome: bool,
    #[arg(long, overrides_with = "real_chrome")]
    no_real_chrome: bool,
    #[arg(long)]
    locale: Option<String>,
    #[arg(long)]
    wait_selector: Option<String>,
    #[arg(short = 's', long)]
    css_selector: Option<String>,
    /// Extra wait after page load, in milliseconds.
    #[arg(long, default_value_t = 0)]
    wait: u64,
    /// Timeout in milliseconds.
    #[arg(long, default_value_t = 30000)]
    timeout: u64,
    #[arg(long, overrides_with = "no_pierce_shadow")]
    pierce_shadow: bool,
    #[arg(long, overrides_with = "pierce_shadow")]
    no_pierce_shadow: bool,
    #[arg(long, overrides_with = "no_network_idle")]
    network_idle: bool,
    #[arg(long, overrides_with = "network_idle")]
    no_network_idle: bool,
    #[arg(long, overrides_with = "enable_resources")]
    disable_resources: bool,
    #[arg(long, overrides_with = "disable_resources")]
    enable_resources: bool,
    #[arg(long, overrides_with = "no_headless")]
    headless: bool,
    #[arg(long, overrides_with = "headless")]
    no_headless: bool,
    /// Route DNS through DoH (not implemented: typed error when enabled).
    #[arg(long, overrides_with = "no_dns_over_https")]
    dns_over_https: bool,
    #[arg(long, overrides_with = "dns_over_https")]
    no_dns_over_https: bool,
    /// Block known ad domains (not implemented: typed error when enabled).
    #[arg(long, overrides_with = "no_block_ads")]
    block_ads: bool,
    #[arg(long, overrides_with = "block_ads")]
    no_block_ads: bool,
    #[arg(long, overrides_with = "allow_webrtc")]
    block_webrtc: bool,
    #[arg(long, overrides_with = "block_webrtc")]
    allow_webrtc: bool,
    #[arg(long, overrides_with = "no_solve_cloudflare")]
    solve_cloudflare: bool,
    #[arg(long, overrides_with = "solve_cloudflare")]
    no_solve_cloudflare: bool,
    #[arg(long, overrides_with = "block_webgl")]
    allow_webgl: bool,
    #[arg(long, overrides_with = "allow_webgl")]
    block_webgl: bool,
    #[arg(long, overrides_with = "show_canvas")]
    hide_canvas: bool,
    #[arg(long, overrides_with = "hide_canvas")]
    show_canvas: bool,
}

fn error(kind: &str, message: impl Into<String>, hint: &str) -> String {
    json!({"schema_version":1,"error":{"kind":kind,"message":message.into(),"hint":hint}})
        .to_string()
}

/// Upstream `write_content_to_file` extension check and format mapping.
fn format_for(path: &Path) -> Result<(&'static str, &'static str), String> {
    let name = path.to_string_lossy();
    if name.ends_with(".md") {
        Ok(("markdown", "\n\n"))
    } else if name.ends_with(".html") {
        Ok(("html", ""))
    } else if name.ends_with(".txt") {
        Ok(("text", ""))
    } else {
        Err(error(
            "invalid_arguments",
            "Unknown file type: filename must end with '.md', '.html', or '.txt'",
            "Choose an output file ending in .md, .html or .txt",
        ))
    }
}

fn object(pairs: Vec<(String, String)>) -> Value {
    Value::Object(
        pairs
            .into_iter()
            .map(|(k, v)| (k, Value::String(v)))
            .collect::<Map<_, _>>(),
    )
}

fn http_arguments(
    method: &str,
    args: &HttpArgs,
    format: &str,
) -> Result<Map<String, Value>, String> {
    let invalid = |message: String| error("invalid_arguments", message, "See --help");
    let (headers, mut cookies) = parse_header_lines(&args.headers, true).map_err(invalid)?;
    if let Some(raw) = &args.cookies {
        for (key, value) in parse_cookie_string(raw) {
            upsert(&mut cookies, key, value);
        }
    }
    let mut params = Vec::new();
    for param in &args.params {
        if let Some((key, value)) = param.split_once('=') {
            params.push((key.to_owned(), value.to_owned()));
        }
    }
    let mut map = Map::new();
    map.insert("url".into(), json!(args.url));
    map.insert("method".into(), json!(method));
    map.insert("extraction_type".into(), json!(format));
    map.insert("main_content_only".into(), json!(args.ai_targeted));
    if let Some(selector) = &args.css_selector {
        map.insert("css_selector".into(), json!(selector));
    }
    if !headers.is_empty() {
        map.insert("headers".into(), object(headers));
    }
    if !cookies.is_empty() {
        map.insert("cookies".into(), object(cookies));
    }
    if !params.is_empty() {
        map.insert("params".into(), object(params));
    }
    if let Some(raw) = &args.json {
        let value: Value = serde_json::from_str(raw)
            .map_err(|e| invalid(format!("Invalid JSON data '{raw}': {e}")))?;
        map.insert("json".into(), value);
    }
    if let Some(data) = &args.data {
        map.insert("data".into(), json!(data));
    }
    if let Some(impersonate) = &args.impersonate {
        let value = if impersonate.contains(',') {
            json!(impersonate.split(',').map(str::trim).collect::<Vec<_>>())
        } else {
            json!(impersonate)
        };
        map.insert("impersonate".into(), value);
    }
    if let Some(proxy) = &args.proxy {
        map.insert("proxy".into(), json!(proxy));
    }
    map.insert("timeout".into(), json!(args.timeout));
    map.insert(
        "follow_redirects".into(),
        json!(pair(args.follow_redirects, args.no_follow_redirects, true)),
    );
    map.insert(
        "verify".into(),
        json!(pair(args.verify, args.no_verify, true)),
    );
    map.insert(
        "stealthy_headers".into(),
        json!(pair(args.stealthy_headers, args.no_stealthy_headers, true)),
    );
    Ok(map)
}

async fn call(server: &RustScraplingMcp, tool: &str, args: Value) -> Result<Value, String> {
    let Value::Object(arguments) = args else {
        return Err(error(
            "internal",
            "arguments must be an object",
            "Report a bug",
        ));
    };
    let result = server
        .execute_tool(tool, arguments)
        .await
        .map_err(|e| error("internal", e.to_string(), "Report a bug"))?;
    if result.is_error == Some(true) {
        let text = serde_json::to_value(&result.content)
            .ok()
            .and_then(|content| content[0]["text"].as_str().map(str::to_owned))
            .unwrap_or_default();
        // The executor already returns a typed {"error": {...}} document.
        return Err(match serde_json::from_str::<Value>(&text) {
            Ok(mut value) if value.get("error").is_some() => {
                value["schema_version"] = json!(1);
                value.to_string()
            }
            _ => error("fetch", text, "Check the URL and options"),
        });
    }
    Ok(result.structured_content.unwrap_or(Value::Null))
}

async fn fetch_browser(args: &BrowserArgs, format: &str) -> Result<Value, String> {
    let unsupported = |what: &str| {
        error(
            "unsupported",
            format!("{what} is not implemented in RustScrapling yet"),
            "Run without this option",
        )
    };
    if pair(args.dns_over_https, args.no_dns_over_https, false) {
        return Err(unsupported("--dns-over-https"));
    }
    if pair(args.block_ads, args.no_block_ads, false) {
        return Err(unsupported("--block-ads"));
    }
    let (extra_headers, _) = parse_header_lines(&args.extra_headers, false)
        .map_err(|e| error("invalid_arguments", e, "See --help"))?;
    let server = RustScraplingMcp::default();
    let mut open = json!({
        "session_id": "extract",
        "headless": pair(args.headless, args.no_headless, true),
        "real_chrome": pair(args.real_chrome, args.no_real_chrome, false),
        "block_webrtc": pair(args.block_webrtc, args.allow_webrtc, false),
        "allow_webgl": pair(args.allow_webgl, args.block_webgl, true),
        "hide_canvas": pair(args.hide_canvas, args.show_canvas, false),
    });
    if let Some(path) = args.executable_path.clone().or_else(|| {
        std::env::var("SCRAPLING_EXECUTABLE_PATH")
            .ok()
            .filter(|p| !p.is_empty())
    }) {
        open["executable_path"] = json!(path);
    }
    if let Some(locale) = &args.locale {
        open["locale"] = json!(locale);
    }
    if let Some(proxy) = &args.proxy {
        open["proxy"] = json!(proxy);
    }
    call(&server, "browser_open", open).await?;
    let mut fetch = json!({
        "session_id": "extract",
        "url": args.url,
        "extraction_type": format,
        "main_content_only": args.ai_targeted,
        "timeout": args.timeout,
        "network_idle": pair(args.network_idle, args.no_network_idle, false),
        "disable_resources": pair(args.disable_resources, args.enable_resources, false),
        "pierce_shadow": pair(args.pierce_shadow, args.no_pierce_shadow, false),
        "solve_cloudflare": pair(args.solve_cloudflare, args.no_solve_cloudflare, false),
    });
    if args.wait > 0 {
        fetch["wait"] = json!(args.wait);
    }
    if let Some(selector) = &args.wait_selector {
        fetch["wait_selector"] = json!(selector);
    }
    if let Some(selector) = &args.css_selector {
        fetch["css_selector"] = json!(selector);
    }
    if !extra_headers.is_empty() {
        fetch["extra_headers"] = object(extra_headers);
    }
    let result = call(&server, "browser_fetch", fetch).await;
    let _ = call(&server, "close_session", json!({"session_id": "extract"})).await;
    result
}

pub fn run(args: ExtractArgs) -> Result<(), String> {
    let (output, format, separator) = match &args.command {
        ExtractCommand::Get(a)
        | ExtractCommand::Post(a)
        | ExtractCommand::Put(a)
        | ExtractCommand::Delete(a) => {
            let (format, separator) = format_for(&a.output_file)?;
            (a.output_file.clone(), format, separator)
        }
        ExtractCommand::Fetch(a) => {
            let (format, separator) = format_for(&a.output_file)?;
            (a.output_file.clone(), format, separator)
        }
    };
    if let ExtractCommand::Get(a) | ExtractCommand::Delete(a) = &args.command {
        if a.json.is_some() || a.data.is_some() {
            return Err(error(
                "invalid_arguments",
                "--json/--data are only accepted by `extract post` and `extract put`",
                "Use extract post or extract put to send a body",
            ));
        }
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| error("runtime", e.to_string(), "Retry"))?;
    let document = runtime.block_on(async {
        match &args.command {
            ExtractCommand::Get(a) => {
                call(
                    &RustScraplingMcp::default(),
                    "make_request",
                    Value::Object(http_arguments("GET", a, format)?),
                )
                .await
            }
            ExtractCommand::Delete(a) => {
                call(
                    &RustScraplingMcp::default(),
                    "make_request",
                    Value::Object(http_arguments("DELETE", a, format)?),
                )
                .await
            }
            ExtractCommand::Post(a) => {
                call(
                    &RustScraplingMcp::default(),
                    "make_request",
                    Value::Object(http_arguments("POST", a, format)?),
                )
                .await
            }
            ExtractCommand::Put(a) => {
                call(
                    &RustScraplingMcp::default(),
                    "make_request",
                    Value::Object(http_arguments("PUT", a, format)?),
                )
                .await
            }
            ExtractCommand::Fetch(a) => fetch_browser(a, format).await,
        }
    })?;
    let chunks: Vec<&str> = document["content"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .filter(|c| !c.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let text = chunks.join(separator);
    let path = if output.is_absolute() {
        output
    } else {
        std::env::current_dir()
            .map_err(|e| error("io", e.to_string(), "Check the working directory"))?
            .join(output)
    };
    std::fs::write(&path, &text).map_err(|e| {
        error(
            "io",
            format!("writing {}: {e}", path.display()),
            "Check the output path",
        )
    })?;
    println!(
        "{}",
        json!({"schema_version":1,"saved_to":path.to_string_lossy(),"format":format,
            "chars":text.chars().count(),"status":document["status"],"url":document["url"]})
    );
    Ok(())
}
