# RustScrapling crawler templates

The TOML specification supports six template names: crawl, sitemap, xml_feed,
csv_feed, shopify and content. The default is crawl.

## SitemapSpider

    template = "sitemap"
    start_urls = ["https://example.org/sitemap.xml"]
    allowed_domains = ["example.org"]
    max_pages = 150
    concurrency = 8
    obey_robots = true
    delay_ms = 200
    auto_throttle = true
    crawldir = "./crawl-state"

URL sets and nested sitemap indexes are supported. The index resolver is
bounded to 64 index documents and four levels. Only HTTP(S) links in
allowed domains are queued; credentials and external links are rejected.

`auto_throttle` is opt-in and defaults to `false` (the previous fixed-delay
behavior remains unchanged). When enabled, RustScrapling applies separate
per-origin request-start gates with a minimum of `delay_ms` or 100 milliseconds
when `delay_ms = 0`. Responses such as HTTP 429 and 503 and connection errors
increase the origin's delay; successful responses gradually move it towards
the configured minimum or half the observed request latency. The normal
adaptive delay is bounded at 30 seconds unless a larger floor is configured.
`Retry-After` is accepted as delta-seconds or an RFC 2822 HTTP-date, capped at
24 hours and measured from the response time. Even already-waiting requests
recheck the gate before starting. Requests already in flight are not cancelled.

With `obey_robots = true`, matching `Crawl-delay` and `Request-rate` directives
are interpreted as additional lower bounds for the origin's spacing. Specific
RustScrapling user-agent groups take priority over `*` groups; matched groups
are combined. Unreachable robots.txt fails closed. Neither the robots policy
nor AutoThrottle enables a distributed, cross-process rate-limit guarantee.

## Development replay cache

To experiment with CSS/XPath/Markdown extraction without fetching the same
pages repeatedly, set these in a crawl TOML:

    development_mode = true
    development_cache_dir = "./.rustscrapling-dev-cache"

The cache is optional and off by default. Successful (2xx), non-challenge
responses of at most 8 MiB are stored under hashed request URLs, with private
Unix directory/file permissions (0700/0600). Old/corrupt entries are rejected
rather than silently refetched. If no `development_cache_dir` is provided,
the cache lives under `crawldir/dev-cache` or `.rustscrapling-dev-cache`.
Development runs deliberately start a new crawl on every invocation and do
not consume/write the production checkpoint; replaying cached pages should
still invoke extraction again. Robots.txt is **always fetched live** when
`obey_robots = true`, so fully offline replay requires turning off that check
in a trusted, previously recorded development environment.

## XMLFeedSpider / CSVFeedSpider

    template = "xml_feed" # or "csv_feed"
    start_urls = ["https://example.org/feed.xml"]
    max_pages = 50

RSS and Atom item/entry links are accepted. CSV requires a url, link or
href column. Input feeds are size-limited to 8 MiB. The feed templates
retrieve listed pages only and do not traverse unrelated HTML links.

## ShopifySpider

    template = "shopify"
    start_urls = ["https://shop.example.org"]
    max_pages = 200

Uses the public /products.json response, following up to 20 pages of
250 products each. Only validated product handles become /products/HANDLE
URLs. Some stores disable or restrict this endpoint; HTTP errors are not
treated as successful crawls.

## ContentSpider / CrawlSpider

    template = "content" # or "crawl"
    start_urls = ["https://example.org/docs"]
    max_depth = 2
    max_pages = 150
    concurrency = 8

Both traverse same-domain HTTP(S) hyperlinks. Content automatically
converts pages to Markdown for CLI output, while crawl outputs HTML
unless --markdown is explicitly provided.

## CLI output

    rustscrapling crawl spec.toml
    rustscrapling crawl spec.toml --format json --out crawl.json
    rustscrapling crawl spec.toml --format csv --out crawl.csv
    rustscrapling crawl spec.toml --format xml --out crawl.xml

NDJSON is the default (one record per completed page). JSON streams an
array, while CSV and XML stream escaped records. Summary information goes
to stderr rather than corrupting stdout data. If interrupted during a
record, output may be partial; the crawldir checkpoint saves frontier
state after each batch. The output and checkpoint are not transactional,
so an exactly-once guarantee across crashes is not yet available.

These templates use the same bounded scheduler and typed Fetcher API,
but are not full behavioral equivalents of all Python Scrapling Spider
hooks or middleware. They are checked against deterministic fixtures;
external website success rates remain to be measured.
