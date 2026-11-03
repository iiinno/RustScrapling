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
allowed-domain and link-regex filters, request-aware deduplication, `max_pages`,
`max_depth`, robots policy, pacing, and checkpoint persistence. Rejected
follow-ups are silently skipped, as with extracted HTML links. Items are emitted
as returned by the parser and are not stored in checkpoints.

The parser can yield `SpiderRequest::new("/submit").method("POST")
.header("Content-Type", "application/json").body(br#"{"ok":true}"#.to_vec())
.priority(10)`. Priority is descending, stable FIFO for equal priority;
deduplication includes method, URL, headers, and body, but not priority. HTTP
`AutoFetcher` forwards request details to the real HTTP client. HTTP GET with
custom headers is also supported. Rich requests stay on the HTTP tier since
the browser tier currently only supports URL navigation. A legacy `Fetcher`
that implements only `fetch(url)` still supports plain GET; rich requests
return `SpiderFetchError::Unsupported` through `fetch_request` rather than
silently being sent as GET. The crawler emits the typed error as page error text.

The checkpoint stores plain GET requests as the original `[url, depth]` pairs,
with rich entries as `{"request": {...}, "depth": n}` in the same queue. The
default spec hash is unchanged. With a `crawldir`, any request body or header
is rejected before scheduling unless `persist_sensitive_requests = true` is
explicitly set in the TOML spec. Opting in stores the **original bodies and
headers in plaintext** in a private checkpoint file (mode 0600 on Unix).
Request payloads may contain credentials, so do not opt in unless the crawl
directory is trusted; there is no at-rest encryption. Development replay caches only plain GET requests;
rich requests always reach the upstream fetcher so different request bodies
cannot share cached responses. Use a fresh crawl directory or
`development_mode` to reparse old pages.

**Remaining S-03 / related gaps:** cookies can be supplied manually using a
`Cookie` request header but there is no first-class cookie jar or per-request
session; no typed metadata, callback selector, or errback routing exists.
S-09/S-10 crash-atomic item delivery is not implemented: checkpoint resume
alone does not give exactly-once output after abrupt process failure. The
legacy `Fetcher::fetch(url)` and CLI backend continue to support GET only;
custom rich requests require a `Fetcher::fetch_request` implementation (the
built-in `AutoFetcher` supports it). Requests have an 8 MiB body cap, 64
headers and an 8 KiB per-header-value limit; these bounds are enforced when
the crawler accepts a parser request. Queued **and in-flight** request bodies
and headers share a combined 32 MiB limit; overflow fails explicitly, including
after checkpoint restore. Plain FIFO crawls retain O(1) dequeue; while
nonzero-priority requests are pending, the scheduler scans the queue per
dispatch (worst-case quadratic cost for very large prioritized frontiers).
Priority-bucket optimization remains for S-06. The opt-in flag only affects checkpoint
persistence, not regular HTTP request execution without a `crawldir`.
