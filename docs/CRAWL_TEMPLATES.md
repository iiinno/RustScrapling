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
    crawldir = "./crawl-state"

URL sets and nested sitemap indexes are supported. The index resolver is
bounded to 64 index documents and four levels. Only HTTP(S) links in
allowed domains are queued; credentials and external links are rejected.

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
