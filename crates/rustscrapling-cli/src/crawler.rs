//! HTTP-backed crawler adapter: JSON, NDJSON, CSV or XML, streamed page by page.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Args, ValueEnum};
use rustscrapling_http::{HttpClient, HttpOptions};
use rustscrapling_spider::{
    crawl, CrawlItem, CrawlSpec, CrawlTemplate, FetchFuture, FetchPage, Fetcher,
};
use serde_json::json;

#[derive(Copy, Clone, Debug, Default, ValueEnum)]
pub enum CrawlFormat {
    Json,
    #[default]
    Ndjson,
    Csv,
    Xml,
}

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
    /// Streaming output encoding. NDJSON is the default.
    #[arg(long, value_enum, default_value_t = CrawlFormat::Ndjson)]
    pub format: CrawlFormat,
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
                headers: Default::default(),
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
    let markdown_output = args.markdown || spec.template == CrawlTemplate::Content;
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
    let mut first = true;
    match args.format {
        CrawlFormat::Json => writer.write_all(b"[\n"),
        CrawlFormat::Xml => writer.write_all(
            b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<crawl schema_version=\"1\">\n",
        ),
        CrawlFormat::Csv => writer.write_all(b"url,depth,status,error,links,content\r\n"),
        CrawlFormat::Ndjson => Ok(()),
    }
    .map_err(|e| format!("writing crawl header: {e}"))?;
    let summary = runtime.block_on(crawl(&spec, Arc::new(HttpBackend { client }), |result| {
        let mut body = result.body.clone();
        if markdown_output && result.error.is_none() && result.status.is_some_and(|s| s < 400) {
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
        write_record(
            &mut writer,
            args.format,
            &mut first,
            result,
            body.as_deref(),
        )
        .map_err(|e| format!("writing crawl output: {e}"))
    }))?;
    match args.format {
        CrawlFormat::Json => writer.write_all(b"\n]\n"),
        CrawlFormat::Xml => writer.write_all(b"</crawl>\n"),
        CrawlFormat::Ndjson | CrawlFormat::Csv => Ok(()),
    }
    .map_err(|e| format!("closing crawl output: {e}"))?;
    writer
        .flush()
        .map_err(|e| format!("flushing crawl output: {e}"))?;
    eprintln!("{}", json!({"schema_version":1,"summary":summary}));
    Ok(())
}

fn write_record(
    writer: &mut dyn Write,
    format: CrawlFormat,
    first: &mut bool,
    item: &CrawlItem,
    content: Option<&str>,
) -> io::Result<()> {
    let value = json!({
        "schema_version": 1,
        "url": item.url,
        "depth": item.depth,
        "status": item.status,
        "links": item.links,
        "error": item.error,
        "content": content,
    });
    match format {
        CrawlFormat::Ndjson => writeln!(writer, "{value}"),
        CrawlFormat::Json => {
            if !*first {
                writer.write_all(b",\n")?;
            }
            *first = false;
            write!(writer, "{value}")
        }
        CrawlFormat::Csv => writeln!(
            writer,
            "{},{},{},{},{},{}",
            csv_cell(&item.url),
            item.depth,
            item.status.map(|s| s.to_string()).unwrap_or_default(),
            csv_cell(item.error.as_deref().unwrap_or_default()),
            csv_cell(&item.links.join(" ")),
            csv_cell(content.unwrap_or_default())
        ),
        CrawlFormat::Xml => {
            writeln!(writer, "<item><url>{}</url><depth>{}</depth><status>{}</status><error>{}</error><content>{}</content>",
                xml_text(&item.url),
                item.depth,
                item.status.map(|s| s.to_string()).unwrap_or_default(),
                xml_text(item.error.as_deref().unwrap_or_default()),
                xml_text(content.unwrap_or_default()))?;
            for link in &item.links {
                writeln!(writer, "<link>{}</link>", xml_text(link))?;
            }
            writer.write_all(b"</item>\n")
        }
    }
}

/// Always quote cells so embedded commas, CRLF and quotes cannot change
/// column boundaries. Doubled quotes are RFC 4180-compatible.
fn csv_cell(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn xml_text(value: &str) -> String {
    let mut escaped = String::new();
    for c in value.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            '\t' | '\n' | '\r' => escaped.push(c),
            c if c >= '\u{20}' && c != '\u{fffe}' && c != '\u{ffff}' => escaped.push(c),
            _ => {}
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_crawl_encodings_remain_parseable_with_unsafe_content() {
        let item = CrawlItem {
            url: "https://example.org/?a=1&b=2".into(),
            depth: 0,
            status: Some(200),
            links: vec!["https://example.org/t?x=<a>".into()],
            error: None,
            body: None,
        };
        for format in [
            CrawlFormat::Ndjson,
            CrawlFormat::Json,
            CrawlFormat::Csv,
            CrawlFormat::Xml,
        ] {
            let mut bytes = Vec::new();
            let mut first = true;
            write_record(
                &mut bytes,
                format,
                &mut first,
                &item,
                Some("A,\"B\"\n<C>&hello"),
            )
            .unwrap();
            let output = String::from_utf8(bytes).unwrap();
            match format {
                CrawlFormat::Ndjson | CrawlFormat::Json => {
                    assert_eq!(
                        serde_json::from_str::<serde_json::Value>(&output).unwrap()["status"],
                        200
                    );
                }
                CrawlFormat::Csv => assert!(output.contains("\"A,\"\"B\"\"\n<C>&hello\"")),
                CrawlFormat::Xml => {
                    assert!(output.contains("&lt;C&gt;&amp;hello"));
                    assert!(output.contains("<item>"));
                }
            }
        }
    }
}
