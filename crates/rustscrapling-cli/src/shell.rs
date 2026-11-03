//! `rustscrapling shell`: interactive scraping console (Plan C-06), the Rust
//! counterpart of upstream `scrapling shell` (IPython + Fetcher shortcuts).
//!
//! Upstream evaluates Python; this console has a small command language
//! covering the same exploration loop: fetch a page (`get`/`post`/`put`/
//! `delete`/`fetch`/`load`), keep it as `page` with the last five in `pages`,
//! try selectors (`css`/`xpath`/`find-text`/`re`/`similar`/`selector-gen`),
//! inspect extraction (`text`/`md`/`html`, upstream Convertor), turn DevTools
//! curl commands into requests (`uncurl`/`curl2fetcher`) and `view` a page.
//! `-c` runs commands non-interactively; piped stdin is read as a script.

use std::collections::VecDeque;
use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Args;
use rustscrapling_adaptive::find_similar;
use rustscrapling_dom::libxml_backend::{parse_html, CompatOptions};
use rustscrapling_dom::{Dom, NodeId};
use rustscrapling_extract::compat::{convert_content, render_elements, ContentFormat};
use rustscrapling_fetch::BrowserBackend;
use rustscrapling_http::{HttpClient, HttpOptions, HttpRequest};
use rustscrapling_select::css::css;
use rustscrapling_select::generate::{generate_css_selector, generate_xpath_selector};
use rustscrapling_select::text_search::{
    find_by_regex, find_by_text, FindByRegexOpts, FindByTextOpts,
};
use rustscrapling_select::xpath::xpath;
use rustscrapling_select::Selection;
use serde_json::{json, Value};

use crate::browser_adapter::CdpBackend;
use crate::curl::{parse_cookie_string, parse_curl, shlex_split, CurlRequest};

const HISTORY: usize = 5;

#[derive(Args)]
pub struct ShellArgs {
    /// Run these shell commands (one per line), print the results and exit.
    #[arg(short = 'c', long = "code")]
    code: Option<String>,
    /// Chrome/Chromium executable for `fetch` (a private temporary profile is used).
    #[arg(long)]
    chrome_path: Option<PathBuf>,
    /// Attach `fetch` to an existing browser-level CDP websocket instead.
    #[arg(long)]
    cdp_url: Option<String>,
}

#[derive(Clone)]
struct Page {
    url: String,
    method: String,
    status: Option<u16>,
    headers: Vec<(String, String)>,
    body: String,
    source: &'static str,
}

impl Page {
    fn summary(&self) -> Value {
        json!({"status": self.status, "method": self.method, "url": self.url,
            "source": self.source, "chars": self.body.chars().count()})
    }
}

struct ShellError {
    kind: &'static str,
    message: String,
    hint: &'static str,
}

fn fail(kind: &'static str, message: impl Into<String>, hint: &'static str) -> ShellError {
    ShellError {
        kind,
        message: message.into(),
        hint,
    }
}

fn usage(message: impl Into<String>) -> ShellError {
    fail(
        "invalid_arguments",
        message,
        "Type `help` for the list of commands",
    )
}

pub struct Shell {
    runtime: tokio::runtime::Runtime,
    pages: VecDeque<Page>,
    current: Option<usize>,
    chrome_path: Option<PathBuf>,
    cdp_url: Option<String>,
    browser: Option<Arc<CdpBackend>>,
}

/// Options shared by the HTTP shortcuts (`get`, `post`, `put`, `delete`).
#[derive(Default)]
struct RequestFlags {
    headers: Vec<(String, String)>,
    cookies: Vec<(String, String)>,
    params: Vec<(String, String)>,
    data: Option<String>,
    json: Option<Value>,
    impersonate: Option<String>,
    proxy: Option<String>,
    timeout: Option<u64>,
    no_redirects: bool,
}

fn take_value(args: &mut std::vec::IntoIter<String>, flag: &str) -> Result<String, ShellError> {
    args.next()
        .ok_or_else(|| usage(format!("{flag} expects a value")))
}

fn parse_request_flags(args: Vec<String>) -> Result<(String, RequestFlags), ShellError> {
    let mut flags = RequestFlags::default();
    let mut url = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-H" | "--header" => {
                let line = take_value(&mut args, &arg)?;
                let (key, value) = line
                    .split_once(':')
                    .ok_or_else(|| usage(format!("header without colon: '{line}'")))?;
                flags
                    .headers
                    .push((key.trim().to_owned(), value.trim().to_owned()));
            }
            "--cookie" | "-b" => {
                flags
                    .cookies
                    .extend(parse_cookie_string(&take_value(&mut args, &arg)?));
            }
            "--param" | "-p" => {
                let pair = take_value(&mut args, &arg)?;
                let (key, value) = pair.split_once('=').unwrap_or((pair.as_str(), ""));
                flags.params.push((key.to_owned(), value.to_owned()));
            }
            "--data" | "-d" => flags.data = Some(take_value(&mut args, &arg)?),
            "--json" => {
                let raw = take_value(&mut args, &arg)?;
                flags.json = Some(
                    serde_json::from_str(&raw)
                        .map_err(|e| usage(format!("--json is not valid JSON: {e}")))?,
                );
            }
            "--impersonate" => flags.impersonate = Some(take_value(&mut args, &arg)?),
            "--proxy" => flags.proxy = Some(take_value(&mut args, &arg)?),
            "--timeout" => {
                let raw = take_value(&mut args, &arg)?;
                let seconds = raw
                    .parse::<u64>()
                    .ok()
                    .filter(|s| *s > 0)
                    .ok_or_else(|| usage("--timeout must be a positive integer (seconds)"))?;
                flags.timeout = Some(seconds);
            }
            "--no-redirects" => flags.no_redirects = true,
            other if other.starts_with('-') => {
                return Err(usage(format!("unknown option '{other}'")))
            }
            _ if url.is_none() => url = Some(arg),
            _ => return Err(usage(format!("unexpected argument '{arg}'"))),
        }
    }
    let url = url.ok_or_else(|| usage("a URL is required"))?;
    Ok((url, flags))
}

fn with_params(url: &str, params: &[(String, String)]) -> String {
    if params.is_empty() {
        return url.to_owned();
    }
    fn encode(text: &str) -> String {
        text.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    (b as char).to_string()
                }
                b' ' => "+".into(),
                _ => format!("%{b:02X}"),
            })
            .collect()
    }
    let query = params
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let (base, fragment) = match url.split_once('#') {
        Some((base, fragment)) => (base, Some(fragment)),
        None => (url, None),
    };
    let joiner = if base.contains('?') { "&" } else { "?" };
    match fragment {
        Some(fragment) => format!("{base}{joiner}{query}#{fragment}"),
        None => format!("{base}{joiner}{query}"),
    }
}

/// Split a command line; `None` asks the caller for a continuation line
/// (trailing backslash or an open quote, as in pasted curl commands).
fn split_line(line: &str) -> Option<Result<Vec<String>, String>> {
    if line.trim_end().ends_with('\\') {
        return None;
    }
    match shlex_split(line) {
        Err(message) if message == "No closing quotation" => None,
        other => Some(other),
    }
}

impl Shell {
    pub fn new(chrome_path: Option<PathBuf>, cdp_url: Option<String>) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| format!("Tokio runtime initialization failed: {e}"))?;
        Ok(Self {
            runtime,
            pages: VecDeque::new(),
            current: None,
            chrome_path,
            cdp_url,
            browser: None,
        })
    }

    fn page(&self) -> Result<&Page, ShellError> {
        self.current
            .and_then(|index| self.pages.get(index))
            .ok_or_else(|| {
                fail(
                    "no_page",
                    "no page loaded yet",
                    "Fetch one with `get URL`, `fetch URL` or `load FILE`",
                )
            })
    }

    fn push_page(&mut self, page: Page) -> Value {
        let summary = page.summary();
        self.pages.push_back(page);
        if self.pages.len() > HISTORY {
            self.pages.pop_front();
        }
        self.current = Some(self.pages.len() - 1);
        summary
    }

    fn dom(&self) -> Result<Dom, ShellError> {
        let page = self.page()?;
        let source = page.body.trim();
        parse_html(
            if source.is_empty() { "<html/>" } else { source },
            &CompatOptions::default(),
        )
        .map_err(|e| fail("parse", e, "The current page is not parseable HTML"))
    }

    fn http(&mut self, method: &str, url: &str, flags: RequestFlags) -> Result<Value, ShellError> {
        let mut options = HttpOptions {
            follow_redirects: !flags.no_redirects,
            safe_redirects: true,
            proxy: flags.proxy.clone(),
            ..HttpOptions::default()
        };
        if let Some(profile) = flags.impersonate {
            options.impersonate = Some(profile);
        }
        if let Some(seconds) = flags.timeout {
            options.timeout = Duration::from_secs(seconds);
        }
        let target = with_params(url, &flags.params);
        let mut request = HttpRequest::new(method, target.clone());
        for (key, value) in &flags.headers {
            request = request.header(key.clone(), value.clone());
        }
        if !flags.cookies.is_empty() {
            let cookie = flags
                .cookies
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; ");
            request = request.header("Cookie", cookie);
        }
        if let Some(value) = &flags.json {
            if !flags
                .headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            {
                request = request.header("Content-Type", "application/json");
            }
            request = request.body(value.to_string().into_bytes());
        } else if let Some(data) = &flags.data {
            request = request.body(data.clone().into_bytes());
        }
        let response = self
            .runtime
            .block_on(async move {
                let client = HttpClient::new(options)?;
                client.request(request).await
            })
            .map_err(|e| fail(e.kind.as_str(), e.message.clone(), e.hint()))?;
        let page = Page {
            url: response.final_url.clone(),
            method: method.to_owned(),
            status: Some(response.status),
            headers: response.headers.clone(),
            body: String::from_utf8_lossy(&response.body).into_owned(),
            source: "http",
        };
        Ok(self.push_page(page))
    }

    fn fetch(&mut self, args: Vec<String>) -> Result<Value, ShellError> {
        let mut args = args.into_iter();
        let mut url = None;
        let mut headed = false;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--headed" => headed = true,
                "--chrome-path" => {
                    self.chrome_path = Some(PathBuf::from(take_value(&mut args, &arg)?));
                    self.browser = None;
                }
                "--cdp-url" => {
                    self.cdp_url = Some(take_value(&mut args, &arg)?);
                    self.browser = None;
                }
                other if other.starts_with('-') => {
                    return Err(usage(format!("unknown option '{other}'")))
                }
                _ if url.is_none() => url = Some(arg),
                _ => return Err(usage(format!("unexpected argument '{arg}'"))),
            }
        }
        let url = url.ok_or_else(|| usage("a URL is required"))?;
        if self.browser.is_none() {
            self.browser = CdpBackend::new(self.chrome_path.clone(), self.cdp_url.clone(), None);
        }
        let browser = self.browser.clone().ok_or_else(|| {
            fail(
                "unsupported",
                "no Chrome/Chromium executable or CDP endpoint is configured",
                "Pass --chrome-path or --cdp-url, or set RUSTSCRAPLING_CHROME_PATH",
            )
        })?;
        let response = self
            .runtime
            .block_on(async move { browser.fetch(&url, headed).await })
            .map_err(|e| {
                let kind = match e.kind {
                    rustscrapling_fetch::FetchErrorKind::Blocked => "blocked",
                    rustscrapling_fetch::FetchErrorKind::Browser => "browser",
                    _ => "transport",
                };
                ShellError {
                    kind,
                    message: e.message,
                    hint: "Check the browser settings or try `get`",
                }
            })?;
        let page = Page {
            url: response.final_url,
            method: "GET".into(),
            status: Some(response.status),
            headers: response.headers,
            body: String::from_utf8_lossy(&response.body).into_owned(),
            source: "browser",
        };
        Ok(self.push_page(page))
    }

    fn curl2fetcher(&mut self, request: CurlRequest) -> Result<Value, ShellError> {
        let method = request.method.to_ascii_uppercase();
        if !["GET", "POST", "PUT", "DELETE"].contains(&method.as_str()) {
            return Err(fail(
                "unsupported",
                format!(
                    "Request method \"{}\" isn't supported by Scrapling yet",
                    request.method
                ),
                "Use get, post, put or delete",
            ));
        }
        let body_allowed = method == "POST" || method == "PUT";
        let mut headers = request.headers;
        let data = match request.data.filter(|_| body_allowed) {
            Some(Value::Object(form)) => {
                if !headers
                    .iter()
                    .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                {
                    headers.push((
                        "Content-Type".into(),
                        "application/x-www-form-urlencoded".into(),
                    ));
                }
                Some(
                    form.iter()
                        .map(|(k, v)| {
                            let pair = vec![(k.clone(), v.as_str().unwrap_or_default().to_owned())];
                            with_params("", &pair).trim_start_matches('?').to_owned()
                        })
                        .collect::<Vec<_>>()
                        .join("&"),
                )
            }
            Some(Value::String(text)) => Some(text),
            _ => None,
        };
        let flags = RequestFlags {
            headers,
            cookies: request.cookies,
            params: request.params,
            data,
            json: request.json_data.filter(|_| body_allowed),
            proxy: request.proxy,
            ..RequestFlags::default()
        };
        self.http(&method, &request.url, flags)
    }

    fn select(&self, kind: &str, args: Vec<String>) -> Result<Value, ShellError> {
        let mut first = false;
        let mut format = ContentFormat::Html;
        let mut partial = false;
        let mut case_sensitive = false;
        let mut all = false;
        let mut expression = None;
        for arg in args {
            match arg.as_str() {
                "--first" => first = true,
                "--text" => format = ContentFormat::Text,
                "--md" => format = ContentFormat::Markdown,
                "--partial" if kind == "find-text" => partial = true,
                "--case-sensitive" if kind == "find-text" || kind == "re" => case_sensitive = true,
                "--all" if kind == "find-text" || kind == "re" => all = true,
                other if other.starts_with("--") => {
                    return Err(usage(format!("unknown option '{other}'")))
                }
                _ if expression.is_none() => expression = Some(arg),
                _ => return Err(usage(format!("unexpected argument '{arg}'"))),
            }
        }
        let expression = expression.ok_or_else(|| usage(format!("{kind} needs an expression")))?;
        let dom = self.dom()?;
        let root = dom
            .document_element()
            .ok_or_else(|| fail("parse", "document has no root element", "Load an HTML page"))?;
        let parse_err = |e: String| fail("parse", e, "Check the expression syntax");
        let selections: Vec<Selection> = match kind {
            "css" => css(&dom, root, &expression).map_err(|e| parse_err(format!("{e:?}")))?,
            "xpath" => xpath(&dom, root, &expression, &[]).map_err(|e| parse_err(e.to_string()))?,
            "find-text" => find_by_text(
                &dom,
                root,
                &expression,
                FindByTextOpts {
                    first_match: !all,
                    partial,
                    case_sensitive,
                    clean_match: true,
                },
            )
            .into_iter()
            .map(Selection::Element)
            .collect(),
            _ => find_by_regex(
                &dom,
                root,
                &expression,
                FindByRegexOpts {
                    first_match: !all,
                    case_sensitive,
                    clean_match: true,
                },
            )
            .map_err(|e| parse_err(format!("invalid regex: {e}")))?
            .into_iter()
            .map(Selection::Element)
            .collect(),
        };
        let mut results = Vec::new();
        for selection in selections {
            results.push(match selection {
                Selection::Element(id) => render_elements(&dom, &[id], format).remove(0),
                Selection::Text(text) => text,
            });
            if first {
                break;
            }
        }
        Ok(json!(results))
    }

    fn first_css(&self, dom: &Dom, selector: &str) -> Result<NodeId, ShellError> {
        let root = dom
            .document_element()
            .ok_or_else(|| fail("parse", "document has no root element", "Load an HTML page"))?;
        css(dom, root, selector)
            .map_err(|e| fail("parse", format!("{e:?}"), "Check the CSS selector"))?
            .into_iter()
            .find_map(|selection| match selection {
                Selection::Element(id) => Some(id),
                Selection::Text(_) => None,
            })
            .ok_or_else(|| {
                fail(
                    "not_found",
                    "reference element not found",
                    "Check the selector with `css`",
                )
            })
    }

    fn convert(&self, format: ContentFormat, args: Vec<String>) -> Result<String, ShellError> {
        let mut full = false;
        let mut selector = None;
        for arg in args {
            match arg.as_str() {
                "--full" => full = true,
                other if other.starts_with("--") => {
                    return Err(usage(format!("unknown option '{other}'")))
                }
                _ if selector.is_none() => selector = Some(arg),
                _ => return Err(usage(format!("unexpected argument '{arg}'"))),
            }
        }
        let chunks = convert_content(&self.page()?.body, format, selector.as_deref(), !full)
            .map_err(|e| fail("parse", e, "Check the CSS selector"))?;
        Ok(chunks.join("\n"))
    }

    fn view(&self) -> Result<Value, ShellError> {
        let page = self.page()?;
        let directory = std::env::temp_dir();
        let path = (0..1000)
            .map(|n| {
                directory.join(format!(
                    "rustscrapling-view-{}-{n}.html",
                    std::process::id()
                ))
            })
            .find(|path| !path.exists())
            .ok_or_else(|| fail("io", "no free temporary file name", "Clean the temp dir"))?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        // Fetched pages may contain private data: owner-only file.
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options
            .open(&path)
            .map_err(|e| fail("io", e.to_string(), "Check the temporary directory"))?;
        file.write_all(page.body.as_bytes())
            .map_err(|e| fail("io", e.to_string(), "Check the temporary directory"))?;
        // Like upstream `webbrowser.open`, but only when a browser command is
        // configured explicitly; headless sessions just get the file path.
        let opened = std::env::var_os("BROWSER").is_some_and(|browser| {
            std::process::Command::new(browser)
                .arg(&path)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .is_ok()
        });
        Ok(json!({"saved_to": path.to_string_lossy(), "opened": opened}))
    }

    /// Execute one command line; `Ok(None)` means "exit".
    fn execute(&mut self, words: Vec<String>) -> Result<Option<String>, ShellError> {
        let mut words = words.into_iter();
        let Some(command) = words.next() else {
            return Ok(Some(String::new()));
        };
        let rest: Vec<String> = words.collect();
        let value = match command.as_str() {
            "exit" | "quit" => return Ok(None),
            "help" => return Ok(Some(banner())),
            "get" | "post" | "put" | "delete" => {
                let (url, flags) = parse_request_flags(rest)?;
                if command != "post"
                    && command != "put"
                    && (flags.data.is_some() || flags.json.is_some())
                {
                    return Err(usage(format!("{command} does not send a body")));
                }
                self.http(&command.to_ascii_uppercase(), &url, flags)?
            }
            "fetch" => self.fetch(rest)?,
            "load" => {
                let mut args = rest.into_iter();
                let file = args.next().ok_or_else(|| usage("load needs a file path"))?;
                let url = match (args.next().as_deref(), args.next()) {
                    (Some("--url"), Some(url)) => url,
                    (None, _) => format!("file://{file}"),
                    _ => return Err(usage("usage: load FILE [--url URL]")),
                };
                let body = std::fs::read_to_string(&file)
                    .map_err(|e| fail("io", format!("reading {file}: {e}"), "Check the path"))?;
                self.push_page(Page {
                    url,
                    method: "GET".into(),
                    status: None,
                    headers: Vec::new(),
                    body,
                    source: "file",
                })
            }
            "page" | "response" => self.page()?.summary(),
            "pages" => json!(self
                .pages
                .iter()
                .enumerate()
                .map(|(index, page)| {
                    let mut summary = page.summary();
                    summary["index"] = json!(index);
                    summary["current"] = json!(Some(index) == self.current);
                    summary
                })
                .collect::<Vec<_>>()),
            "use" => {
                let index = rest
                    .first()
                    .and_then(|n| n.parse::<usize>().ok())
                    .filter(|n| *n < self.pages.len())
                    .ok_or_else(|| usage("use N: N must index `pages`"))?;
                self.current = Some(index);
                self.page()?.summary()
            }
            "headers" => {
                let page = self.page()?;
                json!(page
                    .headers
                    .iter()
                    .map(|(k, v)| json!([k, v]))
                    .collect::<Vec<_>>())
            }
            "css" | "xpath" | "find-text" | "re" => self.select(&command, rest)?,
            "similar" => {
                let selector = rest
                    .first()
                    .ok_or_else(|| usage("similar needs a CSS selector"))?;
                let dom = self.dom()?;
                let base = self.first_css(&dom, selector)?;
                let found = find_similar(&dom, base, 0.2, &["href", "src"], false);
                json!(render_elements(&dom, &found, ContentFormat::Html))
            }
            "selector-gen" => {
                let selector = rest
                    .first()
                    .ok_or_else(|| usage("selector-gen needs a CSS selector"))?;
                let dom = self.dom()?;
                let element = self.first_css(&dom, selector)?;
                let generated = |r: Result<String, _>| {
                    r.map_err(|e: rustscrapling_select::SelectError| {
                        fail("parse", e.to_string(), "Choose another element")
                    })
                };
                json!({"css": generated(generate_css_selector(&dom, element))?,
                    "xpath": generated(generate_xpath_selector(&dom, element))?})
            }
            "text" => return self.convert(ContentFormat::Text, rest).map(Some),
            "md" | "markdown" => return self.convert(ContentFormat::Markdown, rest).map(Some),
            "html" => return self.convert(ContentFormat::Html, rest).map(Some),
            "uncurl" | "curl2fetcher" => {
                let line = rest.join(" ");
                let request = parse_curl(&line).map_err(|e| {
                    fail(
                        "invalid_arguments",
                        e,
                        "Paste a curl command copied from DevTools",
                    )
                })?;
                if command == "uncurl" {
                    request.to_json()
                } else {
                    self.curl2fetcher(request)?
                }
            }
            "view" => self.view()?,
            other => return Err(usage(format!("unknown command '{other}'"))),
        };
        Ok(Some(value.to_string()))
    }

    /// Run one (possibly multi-line) command, printing its result or error.
    /// Returns `(keep_running, succeeded)`.
    fn run_line(&mut self, line: &str) -> (bool, bool) {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return (true, true);
        }
        // `uncurl`/`curl2fetcher` take the raw remainder so the curl
        // command's own quoting survives exactly as pasted.
        let words = match trimmed.split_once(char::is_whitespace) {
            Some((head, tail)) if head == "uncurl" || head == "curl2fetcher" => {
                Ok(vec![head.to_owned(), tail.trim().to_owned()])
            }
            _ => shlex_split(trimmed),
        };
        let result = match words {
            Ok(words) => self.execute(words),
            Err(message) => Err(usage(message)),
        };
        match result {
            Ok(None) => (false, true),
            Ok(Some(output)) => {
                if !output.is_empty() {
                    println!("{output}");
                }
                (true, true)
            }
            Err(error) => {
                eprintln!(
                    "{}",
                    json!({"schema_version":1,"error":{"kind":error.kind,"message":error.message,"hint":error.hint}})
                );
                (true, false)
            }
        }
    }

    /// Feed lines (joining continuations); returns whether all succeeded.
    fn run_lines(&mut self, lines: &mut dyn Iterator<Item = String>, prompt: bool) -> bool {
        let mut ok = true;
        let mut pending = String::new();
        loop {
            if prompt {
                eprint!(
                    "{}",
                    if pending.is_empty() {
                        "rustscrapling> "
                    } else {
                        "... "
                    }
                );
                let _ = std::io::stderr().flush();
            }
            let Some(line) = lines.next() else { break };
            if pending.is_empty() {
                pending = line;
            } else {
                pending.push('\n');
                pending.push_str(&line);
            }
            if split_line(&pending).is_none() {
                continue;
            }
            let command = std::mem::take(&mut pending);
            let (keep_running, succeeded) = self.run_line(&command);
            ok &= succeeded;
            if !keep_running {
                return ok;
            }
        }
        if !pending.is_empty() {
            let (_, succeeded) = self.run_line(&pending);
            ok &= succeeded;
        }
        ok
    }
}

fn banner() -> String {
    let rows: &[(&str, &str)] = &[
        ("get URL [opts]", "HTTP GET (upstream Fetcher.get); safe redirects"),
        ("post|put URL [opts]", "HTTP POST/PUT with --data TEXT or --json JSON"),
        ("delete URL [opts]", "HTTP DELETE"),
        ("  opts", "-H 'K: V', --cookie 'a=1', --param k=v, --impersonate P, --proxy URL, --timeout S, --no-redirects"),
        ("fetch URL [--headed]", "Render with Chrome (upstream StealthyFetcher.fetch)"),
        ("load FILE [--url URL]", "Use a local HTML file as the page (upstream Selector)"),
        ("page / response", "Summary of the current page"),
        ("pages / use N", "Last 5 pages / switch the current page"),
        ("headers", "Response headers of the current page"),
        ("css SEL / xpath EXPR", "Select (::text, ::attr(x) supported); --first, --text, --md"),
        ("find-text TEXT / re PATTERN", "Text search; --all, --partial, --case-sensitive"),
        ("similar SEL", "Elements similar to the first match (find_similar)"),
        ("selector-gen SEL", "Stable CSS and XPath for the first match"),
        ("text|md|html [SEL] [--full]", "Upstream Convertor output (main content unless --full)"),
        ("uncurl 'curl ...'", "Convert a DevTools curl command to a request"),
        ("curl2fetcher 'curl ...'", "Convert a curl command and perform the request"),
        ("view", "Save the page to a temp file (opens $BROWSER if set)"),
        ("help / exit", "Show this help / leave (Ctrl+D also exits)"),
    ];
    let mut text = format!(
        "RustScrapling {} shell. Commands:\n",
        env!("CARGO_PKG_VERSION")
    );
    for (name, description) in rows {
        text.push_str(&format!("   - {name:<30} {description}\n"));
    }
    text
}

pub fn run(args: ShellArgs) -> Result<(), String> {
    let mut shell = Shell::new(args.chrome_path, args.cdp_url)?;
    if let Some(code) = args.code {
        let mut lines = code
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>()
            .into_iter();
        return if shell.run_lines(&mut lines, false) {
            Ok(())
        } else {
            Err(json!({"schema_version":1,"error":{"kind":"shell","message":"one or more shell commands failed","hint":"See the errors above"}}).to_string())
        };
    }
    let stdin = std::io::stdin();
    let interactive = stdin.is_terminal();
    if interactive {
        eprintln!("{}", banner());
    }
    let mut lines = stdin.lock().lines().map_while(Result::ok);
    let ok = shell.run_lines(&mut lines, interactive);
    if interactive {
        eprintln!("Bye Bye");
        Ok(())
    } else if ok {
        Ok(())
    } else {
        Err(json!({"schema_version":1,"error":{"kind":"shell","message":"one or more shell commands failed","hint":"See the errors above"}}).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_are_appended_before_fragment() {
        assert_eq!(
            with_params("https://e.test/a?x=1#top", &[("q".into(), "a b&c".into())]),
            "https://e.test/a?x=1&q=a+b%26c#top"
        );
    }

    #[test]
    fn continuation_lines_are_detected() {
        assert!(split_line("curl2fetcher curl 'https://e.test' \\").is_none());
        assert!(split_line("css 'h1").is_none());
        assert_eq!(split_line("css h1").unwrap().unwrap(), vec!["css", "h1"]);
    }
}
