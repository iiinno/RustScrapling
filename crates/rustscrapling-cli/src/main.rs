//! The `rustscrapling` command line tool (Plan section 7.1).
//! Stage 2 adds the local selection commands: select, similar, relocate.
//! Fetching commands arrive with the fetch crates in later stages.

mod browser_adapter;
mod crawler;
mod curl;
mod daemon;
mod extract;
mod install;
mod network;
mod sessions;
mod shell;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};
use rustscrapling_adaptive::{find_similar, relocate, SqliteStorage};
use rustscrapling_dom::html5_backend::{parse_html as parse_html5, Html5Options};
use rustscrapling_dom::libxml_backend::{parse_html as parse_libxml, CompatOptions};
use rustscrapling_dom::{Dom, NodeId};
use rustscrapling_select::css::css;
use rustscrapling_select::generate::{
    generate_css_selector, generate_full_css_selector, generate_full_xpath_selector,
    generate_xpath_selector,
};
use rustscrapling_select::text_search::{
    find_by_regex, find_by_text, FindByRegexOpts, FindByTextOpts,
};
use rustscrapling_select::xpath::xpath;
use rustscrapling_select::Selection;
use serde_json::json;

#[derive(Clone, Copy, ValueEnum)]
enum OutputFormat {
    Json,
    Ndjson,
    Text,
    Md,
    Html,
}

#[derive(Args)]
struct OutputOptions {
    /// Output format; JSON is the default for agents.
    #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
    format: OutputFormat,
    /// Save full results to a file; print a small JSON receipt to stdout.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Maximum number of characters printed to stdout (excluding file output).
    #[arg(long)]
    max_chars: Option<usize>,
}

/// Upstream `scrapling mcp` options.
#[derive(Args)]
struct McpArgs {
    /// Serve the streamable-http transport instead of stdio.
    #[arg(long)]
    http: bool,
    /// Host to bind with --http ('0.0.0.0' accepts network connections).
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    /// Port to bind with --http.
    #[arg(long, default_value_t = 8000)]
    port: u16,
    /// Chromium-compatible browser for the browser tools.
    #[arg(long)]
    executable_path: Option<String>,
    /// Require `Authorization: Bearer <token>` (--http only). SCRAPLING_MCP_AUTH_TOKEN
    /// keeps it out of the process list.
    #[arg(long)]
    auth_token: Option<String>,
    /// Enable DNS-rebinding protection for this host, e.g. 'mcp.example.com:8000' (repeatable).
    #[arg(long = "allowed-host")]
    allowed_host: Vec<String>,
    /// Serve --http without authentication (not recommended).
    #[arg(long)]
    no_auth: bool,
    /// On a non-loopback --host, also let tools fetch loopback/private
    /// addresses (refused by default to prevent SSRF into the local network).
    #[arg(long)]
    allow_private_network: bool,
    /// Internal host name or address that tools may fetch on a non-loopback
    /// --host (repeatable).
    #[arg(long = "allow-internal-host")]
    allow_internal_host: Vec<String>,
}

fn run_mcp(args: McpArgs) -> Result<(), String> {
    let typed = |kind: &str, message: String, hint: &str| {
        json!({"schema_version":1,"error":{"kind":kind,"message":message,"hint":hint}}).to_string()
    };
    if let Some(path) = &args.executable_path {
        // The browser tools read this as the server-wide default executable.
        std::env::set_var("SCRAPLING_EXECUTABLE_PATH", path);
    }
    let token = args.auth_token.clone().or_else(|| {
        std::env::var(rustscrapling_mcp::http_transport::MCP_AUTH_TOKEN_ENV)
            .ok()
            .filter(|t| !t.is_empty())
    });
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|_| {
            typed(
                "runtime",
                "Tokio runtime initialization failed".into(),
                "Retry",
            )
        })?;
    let exposed = args.http
        && !matches!(
            args.host.as_str(),
            "127.0.0.1" | "localhost" | "::1" | "[::1]"
        );
    let policy = rustscrapling_mcp::NetworkPolicy {
        block_private: exposed && !args.allow_private_network,
        allowed_internal_hosts: args.allow_internal_host.clone(),
    };
    let server = rustscrapling_mcp::RustScraplingMcp::with_backend(std::sync::Arc::new(
        rustscrapling_mcp::RuntimeBackend::with_policy(policy),
    ));
    if !args.http {
        if token.is_some() {
            eprintln!("WARNING: The authentication token only applies to the streamable-http transport, so it's ignored with stdio.");
        }
        return runtime
            .block_on(rustscrapling_mcp::serve_stdio(server))
            .map_err(|e| typed("mcp", e.to_string(), "Check the MCP client"));
    }
    let options = rustscrapling_mcp::http_transport::HttpServeOptions {
        host: args.host,
        port: args.port,
        auth_token: token,
        allowed_hosts: args.allowed_host,
        allow_unauthenticated: args.no_auth,
    };
    rustscrapling_mcp::http_transport::check_auth_policy(&options).map_err(|message| {
        typed(
            "invalid_arguments",
            message,
            "Pass --auth-token or --no-auth",
        )
    })?;
    runtime
        .block_on(rustscrapling_mcp::http_transport::serve_http(
            server,
            options,
            |address| {
                eprintln!("MCP streamable-http server listening on http://{address}/mcp");
            },
        ))
        .map_err(|message| typed("mcp", message, "Check the host and port"))
}

#[derive(Parser)]
#[command(
    name = "rustscrapling",
    version,
    about = "RustScrapling: adaptive web scraping, Scrapling-compatible"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage the authenticated local fetch daemon.
    Daemon(daemon::DaemonArgs),
    /// Manage reusable browser/HTTP sessions hosted by the daemon.
    Session(sessions::SessionArgs),
    /// Serve the upstream-compatible MCP server (16 tools) over stdio or streamable HTTP.
    Mcp(McpArgs),
    /// Fetch a page and save HTML/Markdown/text to a file (upstream `scrapling extract`).
    Extract(extract::ExtractArgs),
    /// Install Chrome for Testing with version and SHA-256 verification.
    Install(install::InstallArgs),
    /// Interactive scraping console (upstream `scrapling shell`).
    Shell(shell::ShellArgs),
    /// Search recent daemon-managed upstream requests.
    Network(sessions::NetworkArgs),
    /// HTTP GET and article extraction (no browser).
    Get {
        url: String,
        #[command(flatten)]
        options: network::RequestOptions,
    },
    /// Automatically choose HTTP or a configured CDP browser tier.
    Auto {
        url: String,
        #[command(flatten)]
        options: network::RequestOptions,
    },
    /// Render a URL in the configured browser.
    Fetch {
        url: String,
        #[command(flatten)]
        options: network::RequestOptions,
    },
    /// Fetch using the browser's stealth path (no unconditional Runtime.enable).
    Stealth {
        url: String,
        #[command(flatten)]
        options: network::RequestOptions,
    },
    /// HTTP POST with an optional --data body.
    Post {
        url: String,
        #[command(flatten)]
        options: network::RequestOptions,
    },
    /// HTTP PUT with an optional --data body.
    Put {
        url: String,
        #[command(flatten)]
        options: network::RequestOptions,
    },
    /// HTTP DELETE.
    Delete {
        url: String,
        #[command(flatten)]
        options: network::RequestOptions,
    },
    /// Convert a local HTML file or HTTP URL to a safe article representation.
    Md {
        target: String,
        #[command(flatten)]
        options: network::RequestOptions,
    },
    /// Follow links according to a bounded TOML crawler specification.
    Crawl(crawler::CrawlArgs),
    /// Generate stable CSS and XPath selectors for an element.
    SelectorGen {
        target: String,
        /// Select the element used as the reference.
        #[arg(long, conflicts_with = "xpath_expr")]
        css: Option<String>,
        #[arg(long = "xpath")]
        xpath_expr: Option<String>,
        /// Generate complete paths instead of short ID-rooted paths.
        #[arg(long)]
        full: bool,
        /// Use the original libxml2-compatible HTML parser.
        #[arg(long)]
        compat: bool,
        #[command(flatten)]
        output: OutputOptions,
    },
    /// Select elements in a local HTML file (URLs are fetched from stage 3 on).
    Select {
        /// HTML file path, or "-" for stdin.
        target: String,
        /// CSS selector.
        #[arg(long, conflicts_with_all = ["xpath_expr", "text", "regex"])]
        css: Option<String>,
        /// XPath expression.
        #[arg(long = "xpath", conflicts_with_all = ["text", "regex"])]
        xpath_expr: Option<String>,
        /// Find elements by their own text.
        #[arg(long, conflicts_with = "regex")]
        text: Option<String>,
        /// Find elements whose own text matches a regex.
        #[arg(long)]
        regex: Option<String>,
        /// Partial text match (with --text).
        #[arg(long)]
        partial: bool,
        /// Return all matches instead of the first (with --text/--regex).
        #[arg(long)]
        all: bool,
        /// Case-sensitive text/regex match.
        #[arg(long)]
        case_sensitive: bool,
        /// Do not clean whitespace before text/regex matching.
        #[arg(long)]
        no_clean: bool,
        /// Parser: "html5" (default) or "compat" (libxml2).
        #[arg(long, default_value = "html5")]
        parser: String,
        /// Use Scrapling-compatible libxml2 parsing.
        #[arg(long, conflicts_with = "parser")]
        compat: bool,
        #[command(flatten)]
        output: OutputOptions,
        /// Adaptive identifier (with --auto-save / --adaptive).
        #[arg(long)]
        id: Option<String>,
        /// Save matched elements under --id for later relocation.
        #[arg(long)]
        auto_save: bool,
        /// If nothing matches, relocate the element saved under --id.
        #[arg(long)]
        adaptive: bool,
        /// Minimum relocation score (percent).
        #[arg(long, default_value_t = 40)]
        percentage: i64,
        /// Adaptive storage file.
        #[arg(long, default_value = "rustscrapling_storage.db")]
        storage_file: String,
        /// Page URL (separates adaptive storage by domain).
        #[arg(long)]
        url: Option<String>,
    },
    /// Find elements similar to the one matched by --css/--xpath.
    Similar {
        /// HTML file path, or "-" for stdin.
        target: String,
        /// CSS selector picking the reference element.
        #[arg(long, conflicts_with = "xpath_expr")]
        css: Option<String>,
        /// XPath expression picking the reference element.
        #[arg(long = "xpath")]
        xpath_expr: Option<String>,
        /// Similarity threshold 0..1.
        #[arg(long, default_value_t = 0.2)]
        threshold: f64,
        /// Attributes to ignore when comparing (repeatable).
        #[arg(long = "ignore", default_values_t = ["href".to_string(), "src".to_string()])]
        ignore: Vec<String>,
        /// Also match on element text.
        #[arg(long)]
        match_text: bool,
        /// Parser: "html5" (default) or "compat".
        #[arg(long, default_value = "html5")]
        parser: String,
        /// Use Scrapling-compatible libxml2 parsing.
        #[arg(long, conflicts_with = "parser")]
        compat: bool,
        #[command(flatten)]
        output: OutputOptions,
    },
    /// Relocate an element previously saved under --id.
    Relocate {
        /// HTML file path, or "-" for stdin.
        target: String,
        /// Adaptive identifier.
        #[arg(long)]
        id: String,
        /// Minimum relocation score (percent).
        #[arg(long, default_value_t = 40)]
        percentage: i64,
        /// Adaptive storage file.
        #[arg(long, default_value = "rustscrapling_storage.db")]
        storage_file: String,
        /// Page URL (separates adaptive storage by domain).
        #[arg(long)]
        url: Option<String>,
        /// Parser: "html5" (default) or "compat".
        #[arg(long, default_value = "html5")]
        parser: String,
        /// Use Scrapling-compatible libxml2 parsing.
        #[arg(long, conflicts_with = "parser")]
        compat: bool,
        #[command(flatten)]
        output: OutputOptions,
    },
}

fn load_dom(target: &str, parser: &str) -> Result<Dom, String> {
    let content = if target == "-" {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| format!("reading stdin: {e}"))?;
        buf
    } else if target.starts_with("http://") || target.starts_with("https://") {
        return Err(
            "URL targets arrive with the fetch commands in stage 3; pass a local file for now"
                .into(),
        );
    } else {
        std::fs::read_to_string(target).map_err(|e| format!("reading {target}: {e}"))?
    };
    match parser {
        "compat" => parse_libxml(&content, &CompatOptions::default()),
        "html5" => parse_html5(&content, &Html5Options::default()),
        other => Err(format!("unknown parser '{other}' (compat|html5)")),
    }
}

#[derive(Clone)]
enum ResultItem {
    Element(String),
    Text(String),
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn render_items(
    command: &str,
    parser: &str,
    items: &[ResultItem],
    truncated: bool,
    format: OutputFormat,
) -> String {
    let out = match format {
        OutputFormat::Json => json!({
            "schema_version": 1,
            "command": command,
            "parser": parser,
            "truncated": truncated,
            "results": items.iter().map(|item| match item {
                ResultItem::Element(path) => json!({"kind": "element", "path": path}),
                ResultItem::Text(value) => json!({"kind": "text", "value": value}),
            }).collect::<Vec<_>>(),
        })
        .to_string(),
        OutputFormat::Ndjson => items
            .iter()
            .map(|item| match item {
                ResultItem::Element(path) => {
                    json!({"schema_version": 1, "kind": "element", "path": path}).to_string()
                }
                ResultItem::Text(value) => {
                    json!({"schema_version": 1, "kind": "text", "value": value}).to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        OutputFormat::Text => items
            .iter()
            .map(|item| match item {
                ResultItem::Element(path) | ResultItem::Text(path) => path.as_str(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        OutputFormat::Md => items
            .iter()
            .map(|item| match item {
                ResultItem::Element(path) => format!("- `{}`", path.replace('`', "\\`")),
                ResultItem::Text(value) => format!("- {}", value),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        OutputFormat::Html => {
            let mut html = String::from("<ul>");
            for item in items {
                let value = match item {
                    ResultItem::Element(path) | ResultItem::Text(path) => path,
                };
                html.push_str("<li>");
                html.push_str(&escape_html(value));
                html.push_str("</li>");
            }
            html.push_str("</ul>");
            html
        }
    };
    format!("{out}\n")
}

fn output_results(
    command: &str,
    parser: &str,
    dom: &Dom,
    ids: &[NodeId],
    texts: &[String],
    opts: &OutputOptions,
) -> Result<(), String> {
    let items: Vec<ResultItem> = ids
        .iter()
        .map(|&id| ResultItem::Element(dom.element_path(id)))
        .chain(texts.iter().cloned().map(ResultItem::Text))
        .collect();
    let full = render_items(command, parser, &items, false, opts.format);
    if let Some(file) = &opts.out {
        std::fs::write(file, &full).map_err(|e| format!("writing {}: {e}", file.display()))?;
        println!(
            "{}",
            json!({
                "schema_version": 1,
                "command": command,
                "saved_to": file.to_string_lossy(),
                "chars": full.chars().count(),
                "headings": [],
            })
        );
        return Ok(());
    }

    let content = match opts.max_chars {
        None => full,
        Some(limit) if full.chars().count() <= limit => full,
        Some(limit) => {
            // Keep JSON/NDJSON records intact. Find the longest prefix of
            // results that fits; never slice in the middle of UTF-8 or JSON.
            let mut low = 0;
            let mut high = items.len();
            while low < high {
                let mid = low + (high - low).div_ceil(2);
                if render_items(command, parser, &items[..mid], true, opts.format)
                    .chars()
                    .count()
                    <= limit
                {
                    low = mid;
                } else {
                    high = mid - 1;
                }
            }
            let shortened = render_items(command, parser, &items[..low], true, opts.format);
            if shortened.chars().count() > limit {
                return Err(format!(
                    "--max-chars {limit} is too small for the output envelope"
                ));
            }
            shortened
        }
    };
    print!("{content}");
    Ok(())
}

fn element_ids(selections: Vec<Selection>) -> Vec<NodeId> {
    selections
        .into_iter()
        .filter_map(|s| match s {
            Selection::Element(id) => Some(id),
            Selection::Text(_) => None,
        })
        .collect()
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();
    match cli.command {
        Command::Daemon(args) => daemon::run(args),
        Command::Session(args) => sessions::run_session(args),
        Command::Mcp(args) => run_mcp(args),
        Command::Network(args) => sessions::run_network(args),
        Command::Shell(args) => shell::run(args),
        Command::Install(args) => install::run(args),
        Command::Extract(args) => extract::run(args),
        Command::Get { url, options } => network::run_request("GET", &url, &options),
        Command::Auto { url, options } => {
            network::run_tier(&url, &options, rustscrapling_fetch::FetchTier::Auto)
        }
        Command::Fetch { url, options } => {
            network::run_tier(&url, &options, rustscrapling_fetch::FetchTier::Browser)
        }
        Command::Stealth { url, options } => {
            network::run_tier(&url, &options, rustscrapling_fetch::FetchTier::Browser)
        }
        Command::Post { url, options } => network::run_request("POST", &url, &options),
        Command::Put { url, options } => network::run_request("PUT", &url, &options),
        Command::Delete { url, options } => network::run_request("DELETE", &url, &options),
        Command::Md { target, options } => network::run_markdown(&target, &options),
        Command::Crawl(args) => crawler::run(args),
        Command::SelectorGen {
            target,
            css: css_expr,
            xpath_expr,
            full,
            compat,
            output,
        } => {
            let parser = if compat { "compat" } else { "html5" };
            let dom = load_dom(&target, parser)?;
            let root = dom
                .document_element()
                .ok_or("document has no root element")?;
            let matches = if let Some(expr) = css_expr {
                css(&dom, root, &expr).map_err(|e| e.to_string())?
            } else if let Some(expr) = xpath_expr {
                xpath(&dom, root, &expr, &[]).map_err(|e| e.to_string())?
            } else {
                return Err("selector-gen requires --css or --xpath".into());
            };
            let selected = element_ids(matches)
                .into_iter()
                .next()
                .ok_or("reference element not found")?;
            let css = if full {
                generate_full_css_selector(&dom, selected)
            } else {
                generate_css_selector(&dom, selected)
            }
            .map_err(|e| e.to_string())?;
            let xpath = if full {
                generate_full_xpath_selector(&dom, selected)
            } else {
                generate_xpath_selector(&dom, selected)
            }
            .map_err(|e| e.to_string())?;
            let value = json!({"schema_version":1,"command":"selector-gen", "parser":parser,
                "path":dom.element_path(selected),"css":css,"xpath":xpath});
            let serialized = match output.format {
                OutputFormat::Json | OutputFormat::Ndjson => value.to_string(),
                OutputFormat::Text => format!("CSS: {}\nXPath: {}", css, xpath),
                OutputFormat::Md => format!("- CSS: `{}`\n- XPath: `{}`", css, xpath),
                OutputFormat::Html => format!(
                    "<code>{}</code><code>{}</code>",
                    escape_html(&css),
                    escape_html(&xpath)
                ),
            };
            let data = format!("{serialized}\n");
            if let Some(path) = output.out {
                std::fs::write(&path, &data)
                    .map_err(|e| format!("writing {}: {e}", path.display()))?;
                println!(
                    "{}",
                    json!({"schema_version":1,"saved_to":path.to_string_lossy(),"chars":data.chars().count(),"headings":[]})
                );
            } else if output
                .max_chars
                .is_some_and(|max| data.chars().count() > max)
            {
                return Err(
                    "--max-chars too small for generated selector output; use --out".into(),
                );
            } else {
                print!("{data}");
            }
            Ok(())
        }
        Command::Select {
            target,
            css: css_expr,
            xpath_expr,
            text,
            regex,
            partial,
            all,
            case_sensitive,
            no_clean,
            parser,
            compat,
            output,
            id,
            auto_save,
            adaptive,
            percentage,
            storage_file,
            url,
        } => {
            let parser = if compat { "compat" } else { parser.as_str() };
            let dom = load_dom(&target, parser)?;
            let root = dom
                .document_element()
                .ok_or("document has no root element")?;
            let mut results: Vec<NodeId> = Vec::new();
            let mut texts: Vec<String> = Vec::new();
            if let Some(expr) = &css_expr {
                for sel in css(&dom, root, expr).map_err(|e| e.to_string())? {
                    match sel {
                        Selection::Element(id) => results.push(id),
                        Selection::Text(t) => texts.push(t),
                    }
                }
            } else if let Some(expr) = &xpath_expr {
                for sel in xpath(&dom, root, expr, &[]).map_err(|e| e.to_string())? {
                    match sel {
                        Selection::Element(id) => results.push(id),
                        Selection::Text(t) => texts.push(t),
                    }
                }
            } else if let Some(text) = &text {
                results = find_by_text(
                    &dom,
                    root,
                    text,
                    FindByTextOpts {
                        first_match: !all,
                        partial,
                        case_sensitive,
                        clean_match: !no_clean,
                    },
                );
            } else if let Some(pattern) = &regex {
                results = find_by_regex(
                    &dom,
                    root,
                    pattern,
                    FindByRegexOpts {
                        first_match: !all,
                        case_sensitive,
                        clean_match: !no_clean,
                    },
                )
                .map_err(|e| format!("invalid regex: {e}"))?;
            } else {
                return Err("one of --css, --xpath, --text, --regex is required".into());
            }

            let storage = if id.is_some() && (auto_save || adaptive) {
                Some(
                    SqliteStorage::open(&storage_file, url.as_deref())
                        .map_err(|e| format!("opening storage: {e}"))?,
                )
            } else {
                None
            };
            if let (Some(storage), Some(id)) = (&storage, &id) {
                if adaptive && results.is_empty() {
                    if let Some(dict) = storage
                        .retrieve(id)
                        .map_err(|e| format!("retrieving from storage: {e}"))?
                    {
                        results = relocate(&dom, root, &dict, percentage);
                    }
                }
                if auto_save {
                    for &el in &results {
                        storage
                            .save_element(&dom, el, id)
                            .map_err(|e| format!("saving to storage: {e}"))?;
                    }
                }
            }
            output_results("select", parser, &dom, &results, &texts, &output)?;
            Ok(())
        }
        Command::Similar {
            target,
            css: css_expr,
            xpath_expr,
            threshold,
            ignore,
            match_text,
            parser,
            compat,
            output,
        } => {
            let parser = if compat { "compat" } else { parser.as_str() };
            let dom = load_dom(&target, parser)?;
            let root = dom
                .document_element()
                .ok_or("document has no root element")?;
            let base = if let Some(expr) = &css_expr {
                element_ids(css(&dom, root, expr).map_err(|e| e.to_string())?)
                    .into_iter()
                    .next()
            } else if let Some(expr) = &xpath_expr {
                element_ids(xpath(&dom, root, expr, &[]).map_err(|e| e.to_string())?)
                    .into_iter()
                    .next()
            } else {
                return Err("one of --css or --xpath is required".into());
            }
            .ok_or("reference element not found")?;
            let ignore_refs: Vec<&str> = ignore.iter().map(|s| s.as_str()).collect();
            let found = find_similar(&dom, base, threshold, &ignore_refs, match_text);
            output_results("similar", parser, &dom, &found, &[], &output)?;
            Ok(())
        }
        Command::Relocate {
            target,
            id,
            percentage,
            storage_file,
            url,
            parser,
            compat,
            output,
        } => {
            let parser = if compat { "compat" } else { parser.as_str() };
            let dom = load_dom(&target, parser)?;
            let root = dom
                .document_element()
                .ok_or("document has no root element")?;
            let storage = SqliteStorage::open(&storage_file, url.as_deref())
                .map_err(|e| format!("opening storage: {e}"))?;
            let dict = storage
                .retrieve(&id)
                .map_err(|e| format!("retrieving from storage: {e}"))?
                .ok_or_else(|| format!("no element saved under id '{id}'"))?;
            let found = relocate(&dom, root, &dict, percentage);
            output_results("relocate", parser, &dom, &found, &[], &output)?;
            Ok(())
        }
    }
}

fn error_details(message: &str) -> (&'static str, &'static str) {
    if message.starts_with("reading ") || message.starts_with("writing ") {
        (
            "io",
            "Check the input and output file paths and permissions",
        )
    } else if message.starts_with("opening storage")
        || message.starts_with("retrieving from storage")
        || message.starts_with("saving to storage")
    {
        ("storage", "Check the SQLite file and its permissions")
    } else if message.starts_with("no element saved") || message == "reference element not found" {
        (
            "not_found",
            "Check the selector or save an element with --id and --auto-save",
        )
    } else if message.starts_with("invalid regex")
        || message.starts_with("unknown parser")
        || message.contains("parse")
    {
        (
            "parse",
            "Check the input HTML, selector, and parser setting",
        )
    } else if message.starts_with("URL targets arrive") {
        (
            "unsupported",
            "Use a local HTML file until HTTP fetching is available",
        )
    } else {
        (
            "invalid_arguments",
            "Check arguments and input data; see --help",
        )
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // The acquisition module already returns an error with a typed,
            // versioned envelope. Preserve it rather than double-encoding it.
            if serde_json::from_str::<serde_json::Value>(&e)
                .ok()
                .is_some_and(|value| value.get("error").is_some())
            {
                eprintln!("{e}");
                return ExitCode::FAILURE;
            }
            let (kind, hint) = error_details(&e);
            eprintln!(
                "{}",
                json!({
                    "schema_version": 1,
                    "error": {"kind": kind, "message": e, "hint": hint}
                })
            );
            ExitCode::FAILURE
        }
    }
}
