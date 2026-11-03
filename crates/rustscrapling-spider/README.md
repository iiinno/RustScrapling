# Custom Spider parsing

`crawl_with_parser` adds asynchronous, application-defined extraction to the
existing bounded crawler. Implement `SpiderParser` to yield both structured
JSON items and follow-up URLs from each successful response:

```rust
use rustscrapling_spider::{
    crawl_with_parser, CrawlItem, CrawlSpec, Fetcher, ParseFuture,
    SpiderEvent, SpiderOutput, SpiderParser, SpiderRequest,
};
use std::sync::Arc;

struct ArticleParser;

impl SpiderParser for ArticleParser {
    fn parse<'a>(&'a self, page: &'a CrawlItem) -> ParseFuture<'a> {
        Box::pin(async move {
            let mut outputs = vec![SpiderOutput::Item(serde_json::json!({
                "url": page.url,
                "body": page.body,
            }))];
            if page.depth == 0 {
                outputs.push(SpiderOutput::Request(SpiderRequest::new("/next")));
            }
            Ok(outputs)
        })
    }
}

async fn collect(fetcher: Arc<dyn Fetcher>) -> Result<(), String> {
    let spec = CrawlSpec {
        start_urls: vec!["https://example.org/articles".into()],
        max_pages: 20,
        max_depth: 2,
        ..CrawlSpec::default()
    };
    crawl_with_parser(&spec, fetcher, &ArticleParser, |event| {
        match event {
            SpiderEvent::Page(page) => eprintln!("{}: {:?}", page.url, page.status),
            SpiderEvent::Item(value) => println!("{value}"),
        }
        Ok(())
    }).await?;
    Ok(())
}
```

The parser runs on 2xx pages; failures are still emitted as `Page` events.
While a custom parser is active, follow-up discovery is driven by its yielded
requests. Ordinary `crawl` retains automatic HTML link discovery unchanged.
Relative requests are resolved against the final response URL (after redirects).

Requests enter the existing scheduler, with URL/credential validation,
allowed-domain and link-regex filters, fragment deduplication, `max_pages`,
`max_depth`, robots policy, pacing, and checkpoint persistence. Rejected
follow-ups are silently skipped, as with extracted HTML links. Items are emitted
as returned by the parser and are not stored in checkpoints.

Current requests are URL-only HTTP(S) GETs, matching the existing `Fetcher`
interface. HTTP method/body/headers, request metadata and per-request callback
selection are not available yet. Checkpoints retain their existing format and
spec hash; changing the parser code does not invalidate an existing checkpoint.
Use a fresh crawl directory or `development_mode` to reparse old pages.
