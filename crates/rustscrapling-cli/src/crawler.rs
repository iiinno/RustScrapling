//! HTTP-backed crawler adapter: a streamed NDJSON command with no hidden batch buffering.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;

use clap::Args;
use rustscrapling_http::{HttpClient, HttpOptions};
use rustscrapling_spider::{crawl, CrawlSpec, FetchFuture, FetchPage, Fetcher};
use serde_json::json;

#[derive(Debug, Args)]
pub struct CrawlArgs {
    /// TOML specification of seeds, bounds, robots policy and crawldir.
    pub spec: PathBuf,
    /// Write streamed NDJSON to this file instead of stdout.
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Emit sanitized article Markdown rather than source HTML in the body field.
    #[arg(long)]
    pub markdown: bool,
}

struct HttpBackend {
    client: HttpClient,
}

impl Fetcher for HttpBackend {
    fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
        Box::pin(async move {
            let response = self
                .client
                .get(url)
                .await
                .map_err(|e| format!("{}: {}", e.kind.as_str(), e.message))?;
            let content_type = response
                .headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                .map(|(_, v)| v.clone())
                .unwrap_or_default();
            Ok(FetchPage {
                status: response.status,
                url: response.final_url,
                content_type,
                body: String::from_utf8_lossy(&response.body).into_owned(),
            })
        })
    }
}

pub fn run(args: CrawlArgs) -> Result<(), String> {
    let spec = CrawlSpec::from_file(&args.spec)?;
    let mut writer: Box<dyn Write> = if let Some(path) = args.out {
        Box::new(BufWriter::new(
            File::create(&path).map_err(|e| format!("writing {}: {e}", path.display()))?,
        ))
    } else {
        Box::new(BufWriter::new(io::stdout()))
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("initializing crawler runtime: {e}"))?;
    let client = HttpClient::new(HttpOptions::default())
        .map_err(|e| format!("initializing HTTP client: {e}"))?;
    let summary = runtime.block_on(crawl(&spec, Arc::new(HttpBackend { client }), |result| {
        let mut body = result.body.clone();
        if args.markdown && result.error.is_none() && result.status.is_some_and(|s| s < 400) {
            if let Some(html) = body.as_deref() {
                body = rustscrapling_extract::extract(
                    html,
                    rustscrapling_extract::ExtractOptions {
                        page_url: Some(&result.url),
                        query: None,
                    },
                )
                .ok()
                .map(|page| page.markdown);
            }
        }
        writeln!(
            writer,
            "{}",
            json!({
                "schema_version": 1,
                "url": result.url,
                "depth": result.depth,
                "status": result.status,
                "links": result.links,
                "error": result.error,
                "content": body,
            })
        )
        .map_err(|e| format!("writing crawl output: {e}"))
    }))?;
    writer
        .flush()
        .map_err(|e| format!("flushing crawl output: {e}"))?;
    eprintln!("{}", json!({"schema_version":1,"summary":summary}));
    Ok(())
}
