//! Seed discovery for the six crawler templates in Plan 7.3.
//! Feed/index pages are inputs, not scraped results. The actual page crawl
//! still runs through the same bounded scheduler, robots policy and checkpoint.

use super::{host_allowed, robots_allowed, valid_url, CrawlSpec, Fetcher};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use url::Url;

const MAX_FEED_BYTES: usize = 8 * 1024 * 1024;
const MAX_INDEX_PAGES: usize = 64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrawlTemplate {
    #[default]
    Crawl,
    Sitemap,
    XmlFeed,
    CsvFeed,
    Shopify,
    Content,
}

impl CrawlTemplate {
    pub fn follows_links(self) -> bool {
        matches!(self, Self::Crawl | Self::Content)
    }
}

fn checked_link(base: &Url, value: &str, spec: &CrawlSpec) -> Option<String> {
    let mut url = base.join(value.trim()).ok()?;
    if valid_url(url.as_str()).is_err() || !host_allowed(spec, &url) {
        return None;
    }
    url.set_fragment(None);
    Some(url.to_string())
}

async fn load(
    url: &Url,
    spec: &CrawlSpec,
    fetcher: &Arc<dyn Fetcher>,
    robots: &mut HashMap<String, Option<super::RobotsPolicy>>,
) -> Result<(Url, String), String> {
    if spec.obey_robots && !robots_allowed(url, fetcher, robots).await {
        return Err(format!("feed source blocked by robots.txt: {url}"));
    }
    let page = fetcher.fetch(url.as_str()).await?;
    if !(200..300).contains(&page.status) {
        return Err(format!(
            "feed request failed with HTTP {}: {url}",
            page.status
        ));
    }
    if page.body.len() > MAX_FEED_BYTES {
        return Err("feed exceeds 8 MiB limit".into());
    }
    let base = valid_url(&page.url)?;
    Ok((base, page.body))
}

pub(super) async fn resolve_seeds(
    spec: &CrawlSpec,
    fetcher: &Arc<dyn Fetcher>,
) -> Result<Vec<String>, String> {
    if matches!(spec.template, CrawlTemplate::Crawl | CrawlTemplate::Content) {
        return Ok(spec.start_urls.clone());
    }
    let mut seeds = Vec::new();
    let mut seen = HashSet::new();
    let mut robots = HashMap::new();
    let limit = spec.max_pages.min(10_000);
    match spec.template {
        CrawlTemplate::Sitemap => {
            let mut indexes: VecDeque<(String, usize)> =
                spec.start_urls.iter().cloned().map(|s| (s, 0)).collect();
            let mut explored = HashSet::new();
            while let Some((source, depth)) = indexes.pop_front() {
                if explored.len() >= MAX_INDEX_PAGES || seeds.len() >= limit {
                    break;
                }
                if !explored.insert(source.clone()) {
                    continue;
                }
                let url = valid_url(&source)?;
                let (base, body) = load(&url, spec, fetcher, &mut robots).await?;
                let doc = roxmltree::Document::parse(&body)
                    .map_err(|err| format!("invalid sitemap XML: {err}"))?;
                let is_index = doc.root_element().tag_name().name() == "sitemapindex";
                if !is_index && doc.root_element().tag_name().name() != "urlset" {
                    return Err("sitemap requires urlset or sitemapindex root".into());
                }
                let child_tag = if is_index { "sitemap" } else { "url" };
                for group in doc
                    .descendants()
                    .filter(|n| n.is_element() && n.tag_name().name() == child_tag)
                {
                    let Some(loc) = group
                        .children()
                        .find(|n| n.is_element() && n.tag_name().name() == "loc")
                        .and_then(|n| n.text())
                    else {
                        continue;
                    };
                    let Some(link) = checked_link(&base, loc, spec) else {
                        continue;
                    };
                    if is_index {
                        if depth < 4 && !explored.contains(&link) {
                            indexes.push_back((link, depth + 1));
                        }
                    } else if seen.insert(link.clone()) {
                        seeds.push(link);
                        if seeds.len() >= limit {
                            break;
                        }
                    }
                }
            }
        }
        CrawlTemplate::XmlFeed => {
            for source in &spec.start_urls {
                if seeds.len() >= limit {
                    break;
                }
                let url = valid_url(source)?;
                let (base, body) = load(&url, spec, fetcher, &mut robots).await?;
                let doc = roxmltree::Document::parse(&body)
                    .map_err(|err| format!("invalid XML feed: {err}"))?;
                if !matches!(doc.root_element().tag_name().name(), "rss" | "feed" | "RDF") {
                    return Err("XML feed must be RSS or Atom".into());
                }
                for entry in doc
                    .descendants()
                    .filter(|n| n.is_element() && matches!(n.tag_name().name(), "item" | "entry"))
                {
                    let link = entry
                        .children()
                        .filter(|n| n.is_element() && n.tag_name().name() == "link")
                        .find_map(|n| n.attribute("href").or_else(|| n.text()));
                    if let Some(link) = link.and_then(|s| checked_link(&base, s, spec)) {
                        if seen.insert(link.clone()) {
                            seeds.push(link);
                            if seeds.len() >= limit {
                                break;
                            }
                        }
                    }
                }
            }
        }
        CrawlTemplate::CsvFeed => {
            for source in &spec.start_urls {
                if seeds.len() >= limit {
                    break;
                }
                let url = valid_url(source)?;
                let (base, body) = load(&url, spec, fetcher, &mut robots).await?;
                let mut reader = csv::ReaderBuilder::new()
                    .flexible(true)
                    .from_reader(body.trim_start_matches('\u{feff}').as_bytes());
                let headers = reader
                    .headers()
                    .map_err(|err| format!("CSV header error: {err}"))?;
                let Some(col) = headers.iter().position(|h| {
                    matches!(
                        h.trim().to_ascii_lowercase().as_str(),
                        "url" | "link" | "href"
                    )
                }) else {
                    return Err("CSV feed needs a url, link or href column".into());
                };
                for row in reader.records() {
                    let row = row.map_err(|err| format!("CSV row error: {err}"))?;
                    if let Some(link) = row.get(col).and_then(|s| checked_link(&base, s, spec)) {
                        if seen.insert(link.clone()) {
                            seeds.push(link);
                            if seeds.len() >= limit {
                                break;
                            }
                        }
                    }
                }
            }
        }
        CrawlTemplate::Shopify => {
            for source in &spec.start_urls {
                let site = valid_url(source)?;
                for page_index in 1..=20 {
                    if seeds.len() >= limit {
                        break;
                    }
                    let mut catalog = site
                        .join("/products.json")
                        .map_err(|e| format!("invalid Shopify URL: {e}"))?;
                    catalog
                        .query_pairs_mut()
                        .append_pair("limit", "250")
                        .append_pair("page", &page_index.to_string());
                    let (base, body) = load(&catalog, spec, fetcher, &mut robots).await?;
                    let value: serde_json::Value = serde_json::from_str(&body)
                        .map_err(|e| format!("invalid Shopify products JSON: {e}"))?;
                    let products = value
                        .get("products")
                        .and_then(|p| p.as_array())
                        .ok_or("Shopify response has no products array")?;
                    for product in products {
                        let Some(handle) = product.get("handle").and_then(|v| v.as_str()) else {
                            continue;
                        };
                        if handle.is_empty()
                            || !handle
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                        {
                            continue;
                        }
                        if let Some(link) =
                            checked_link(&base, &format!("/products/{handle}"), spec)
                        {
                            if seen.insert(link.clone()) {
                                seeds.push(link);
                                if seeds.len() >= limit {
                                    break;
                                }
                            }
                        }
                    }
                    if products.len() < 250 {
                        break;
                    }
                }
            }
        }
        CrawlTemplate::Crawl | CrawlTemplate::Content => unreachable!(),
    }
    Ok(seeds)
}

#[cfg(test)]
mod tests {
    use super::super::{FetchFuture, FetchPage};
    use super::*;

    struct Fixture;
    impl Fetcher for Fixture {
        fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
            Box::pin(async move {
                let body = if url.ends_with("/sitemap.xml") {
                    "<sitemapindex><sitemap><loc>/child.xml</loc></sitemap></sitemapindex>"
                } else if url.ends_with("/child.xml") {
                    "<urlset xmlns='http://www.sitemaps.org/schemas/sitemap/0.9'><url><loc>/a</loc></url><url><loc>/b</loc></url></urlset>"
                } else if url.ends_with("/rss") {
                    "<rss><channel><item><link>https://example.org/a</link></item></channel></rss>"
                } else if url.ends_with("/atom") {
                    "<feed xmlns='http://www.w3.org/2005/Atom'><entry><link href='/a'/></entry></feed>"
                } else if url.ends_with("/csv") {
                    "name,url\none,/a\ntwo,/b"
                } else if url.contains("products.json") {
                    r#"{"products":[{"handle":"widget"},{"handle":"sale-2026"}]}"#
                } else {
                    "<html>page</html>"
                };
                Ok(FetchPage {
                    headers: Default::default(),
                    status: 200,
                    url: url.into(),
                    content_type: "text/plain".into(),
                    body: body.into(),
                })
            })
        }
    }

    #[tokio::test]
    async fn all_feed_templates_seed_bounded_pages() {
        let fetcher: Arc<dyn Fetcher> = Arc::new(Fixture);
        let samples = [
            (CrawlTemplate::Sitemap, "/sitemap.xml", vec!["/a", "/b"]),
            (CrawlTemplate::XmlFeed, "/rss", vec!["/a"]),
            (CrawlTemplate::XmlFeed, "/atom", vec!["/a"]),
            (CrawlTemplate::CsvFeed, "/csv", vec!["/a", "/b"]),
            (
                CrawlTemplate::Shopify,
                "/",
                vec!["/products/widget", "/products/sale-2026"],
            ),
        ];
        for (template, path, expected) in samples {
            let spec = CrawlSpec {
                start_urls: vec![format!("https://example.org{path}")],
                template,
                max_pages: 3,
                ..CrawlSpec::default()
            };
            let seeds = resolve_seeds(&spec, &fetcher).await.unwrap();
            assert_eq!(
                seeds,
                expected
                    .into_iter()
                    .map(|p| format!("https://example.org{p}"))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn templates_default_to_crawl_and_ignore_foreign_domains() {
        let spec =
            CrawlSpec::from_toml("start_urls=['https://example.org']\ntemplate='content'").unwrap();
        assert_eq!(spec.template, CrawlTemplate::Content);
        assert!(spec.template.follows_links());
        assert!(!CrawlTemplate::Sitemap.follows_links());
        let base = Url::parse("https://example.org/sitemap.xml").unwrap();
        assert!(checked_link(&base, "https://other.example/p", &spec).is_none());
    }
}
