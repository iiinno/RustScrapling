//! Bounded, restartable crawler with a fetcher supplied by the acquisition layer.
//! Its `crawl` callback emits each result immediately instead of retaining page bodies.

use futures_util::stream::{FuturesUnordered, StreamExt};
use rustscrapling_dom::html5_backend::{parse_html, Html5Options};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
use url::Url;

pub type FetchFuture<'a> = Pin<Box<dyn Future<Output = Result<FetchPage, String>> + Send + 'a>>;

/// Adapter for the HTTP / automatic acquisition crate. Tests use a deterministic mock.
pub trait Fetcher: Send + Sync {
    fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a>;
}

/// Adapter that connects the crawler to RustScrapling's HTTP / browser tier
/// policy. It can be shared across several crawler instances.
#[derive(Clone)]
pub struct AutoFetcher(pub rustscrapling_fetch::Fetcher);

impl Fetcher for AutoFetcher {
    fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
        Box::pin(async move {
            let response = self
                .0
                .fetch(
                    rustscrapling_http::HttpRequest::get(url),
                    rustscrapling_fetch::FetchTier::Auto,
                )
                .await
                .map_err(|error| error.to_string())?;
            let content_type = response
                .headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("content-type"))
                .map(|(_, val)| val.to_ascii_lowercase())
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FetchPage {
    pub status: u16,
    pub url: String,
    pub content_type: String,
    pub body: String,
}

fn default_max_pages() -> usize {
    150
}
fn default_concurrency() -> usize {
    8
}
fn default_max_depth() -> usize {
    2
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CrawlSpec {
    pub start_urls: Vec<String>,
    pub allowed_domains: Vec<String>,
    #[serde(default = "default_max_pages")]
    pub max_pages: usize,
    #[serde(default = "default_max_depth")]
    pub max_depth: usize,
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    pub obey_robots: bool,
    pub delay_ms: u64,
    pub crawldir: Option<PathBuf>,
}

impl Default for CrawlSpec {
    fn default() -> Self {
        Self {
            start_urls: Vec::new(),
            allowed_domains: Vec::new(),
            max_pages: default_max_pages(),
            max_depth: default_max_depth(),
            concurrency: default_concurrency(),
            obey_robots: false,
            delay_ms: 0,
            crawldir: None,
        }
    }
}

impl CrawlSpec {
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let spec: Self = toml::from_str(text).map_err(|e| format!("invalid crawler TOML: {e}"))?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, String> {
        let source =
            std::fs::read_to_string(path).map_err(|e| format!("reading crawl spec: {e}"))?;
        Self::from_toml(&source)
    }

    fn validate(&self) -> Result<(), String> {
        if self.start_urls.is_empty() {
            return Err("start_urls must not be empty".into());
        }
        if !(1..=64).contains(&self.concurrency) {
            return Err("concurrency must be 1..=64".into());
        }
        if self.max_pages == 0 || self.max_pages > 1_000_000 {
            return Err("max_pages must be 1..=1000000".into());
        }
        for seed in &self.start_urls {
            valid_url(seed)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CrawlItem {
    pub url: String,
    pub depth: usize,
    pub status: Option<u16>,
    pub links: Vec<String>,
    pub error: Option<String>,
    pub body: Option<String>,
}

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
struct Progress {
    visited_hashes: Vec<String>,
    queue: Vec<(String, usize)>,
}

/// Summary excludes response bodies, so it stays bounded for long crawls.
#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct CrawlSummary {
    pub visited: usize,
    pub successful: usize,
    pub failed: usize,
    pub remaining: usize,
}

fn valid_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|_| format!("invalid URL {raw}"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("crawler only accepts HTTP and HTTPS URLs".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("crawler URLs cannot contain embedded credentials".into());
    }
    Ok(url)
}

fn key(url: &str) -> String {
    blake3::hash(url.as_bytes()).to_hex().to_string()
}

#[derive(Clone, Debug)]
struct RobotsRule {
    pattern: String,
    allow: bool,
}

/// Implements the common RFC 9309 path-prefix and `*`/`$` matching syntax.
/// Specific RustScrapling groups win over the generic `*` group.
fn parse_robots(source: &str) -> Vec<RobotsRule> {
    let mut general = Vec::new();
    let mut specific = Vec::new();
    let mut agents = Vec::<String>::new();
    let mut group_has_rules = false;
    for raw in source.lines() {
        let line = raw.split('#').next().unwrap_or_default().trim();
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let key = name.trim().to_ascii_lowercase();
        let value = value.trim();
        if key == "user-agent" {
            if group_has_rules {
                agents.clear();
                group_has_rules = false;
            }
            agents.push(value.to_ascii_lowercase());
        } else if matches!(key.as_str(), "allow" | "disallow") {
            group_has_rules = true;
            if value.is_empty() {
                continue;
            }
            let rule = RobotsRule {
                pattern: value.to_owned(),
                allow: key == "allow",
            };
            if agents.iter().any(|a| a == "rustscrapling") {
                specific.push(rule);
            } else if agents.iter().any(|a| a == "*") {
                general.push(rule);
            }
        }
    }
    if specific.is_empty() {
        general
    } else {
        specific
    }
}

fn robots_rule_matches(pattern: &str, path: &str) -> bool {
    let anchored = pattern.ends_with('$');
    let content = pattern.strip_suffix('$').unwrap_or(pattern);
    let expression = format!(
        "^{}{}",
        regex::escape(content).replace("\\*", ".*"),
        if anchored { "$" } else { "" }
    );
    regex::Regex::new(&expression).is_ok_and(|re| re.is_match(path))
}

fn robots_permitted(rules: &[RobotsRule], path: &str) -> bool {
    let matched = rules
        .iter()
        .filter(|r| robots_rule_matches(&r.pattern, path))
        .max_by_key(|r| (r.pattern.len(), r.allow));
    matched.is_none_or(|rule| rule.allow)
}

async fn robots_allowed(
    url: &Url,
    fetcher: &Arc<dyn Fetcher>,
    cache: &mut HashMap<String, Option<Vec<RobotsRule>>>,
) -> bool {
    let host_key = format!(
        "{}://{}:{}",
        url.scheme(),
        url.host_str().unwrap_or_default(),
        url.port_or_known_default().unwrap_or(0)
    );
    if !cache.contains_key(&host_key) {
        let mut robots = url.clone();
        robots.set_path("/robots.txt");
        robots.set_query(None);
        robots.set_fragment(None);
        let policy = match fetcher.fetch(robots.as_str()).await {
            Ok(page) if page.status == 404 => Some(Vec::new()),
            Ok(page) if (200..300).contains(&page.status) => Some(parse_robots(&page.body)),
            // If robot policy cannot be established while obedience is
            // explicitly requested, fail closed instead of ignoring it.
            _ => None,
        };
        cache.insert(host_key.clone(), policy);
    }
    cache
        .get(&host_key)
        .and_then(Option::as_ref)
        .is_some_and(|rules| robots_permitted(rules, url.path()))
}

fn host_allowed(spec: &CrawlSpec, url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    if spec.allowed_domains.is_empty() {
        return spec
            .start_urls
            .iter()
            .filter_map(|s| Url::parse(s).ok())
            .any(|seed| seed.host_str().is_some_and(|h| h == host));
    }
    spec.allowed_domains.iter().any(|domain| {
        let domain = domain.trim_start_matches('.');
        !domain.is_empty()
            && (host.eq_ignore_ascii_case(domain)
                || host
                    .to_ascii_lowercase()
                    .ends_with(&format!(".{}", domain.to_ascii_lowercase())))
    })
}

/// Extract HTTP(S) links using the same HTML5 DOM as the selector crate.
pub fn page_links(base: &str, html: &str) -> Vec<String> {
    let Ok(base) = valid_url(base) else {
        return Vec::new();
    };
    let Ok(dom) = parse_html(html, &Html5Options::default()) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut links = Vec::new();
    for node in dom.descendants_or_self(dom.root) {
        if !matches!(dom.node(node).tag.as_str(), "a" | "area") {
            continue;
        }
        let Some(href) = dom.attr(node, "href") else {
            continue;
        };
        let Ok(mut url) = base.join(href.trim()) else {
            continue;
        };
        if valid_url(url.as_str()).is_err() {
            continue;
        }
        url.set_fragment(None);
        let value = url.to_string();
        if seen.insert(value.clone()) {
            links.push(value);
        }
    }
    links
}

fn state_path(spec: &CrawlSpec) -> Option<PathBuf> {
    spec.crawldir
        .as_ref()
        .map(|dir| dir.join("crawl-state.json"))
}

fn restore(spec: &CrawlSpec) -> Result<Option<Progress>, String> {
    let Some(path) = state_path(spec) else {
        return Ok(None);
    };
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("reading state: {e}"))?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| format!("invalid state: {e}"))
}

fn persist(spec: &CrawlSpec, progress: &Progress) -> Result<(), String> {
    let Some(path) = state_path(spec) else {
        return Ok(());
    };
    let parent = path.parent().ok_or("state has no parent directory")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("creating crawldir: {e}"))?;
    let pending = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec(progress).map_err(|e| e.to_string())?;
    std::fs::write(&pending, bytes).map_err(|e| format!("writing state: {e}"))?;
    // Windows does not rename over an existing target; removal before rename
    // is safe only for the *known* state file in an explicit crawldir.
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("removing old state: {e}"))?;
    }
    std::fs::rename(pending, path).map_err(|e| format!("saving state: {e}"))
}

/// Crawl using bounded batches of concurrent fetches. The callback is invoked
/// for each completed response and should stream NDJSON or similar output.
pub async fn crawl<F>(
    spec: &CrawlSpec,
    fetcher: Arc<dyn Fetcher>,
    mut emit: F,
) -> Result<CrawlSummary, String>
where
    F: FnMut(&CrawlItem) -> Result<(), String>,
{
    spec.validate()?;
    let state = restore(spec)?.unwrap_or_else(|| Progress {
        queue: spec
            .start_urls
            .iter()
            .cloned()
            .map(|url| (url, 0))
            .collect(),
        ..Progress::default()
    });
    let mut visited: HashSet<String> = state.visited_hashes.into_iter().collect();
    let mut queued: HashSet<String> = state.queue.iter().map(|(url, _)| key(url)).collect();
    let mut queue: VecDeque<(String, usize)> = state.queue.into();
    let mut summary = CrawlSummary::default();
    let mut site_clock = HashMap::<String, Instant>::new();
    let mut robot_cache = HashMap::<String, Option<Vec<RobotsRule>>>::new();
    while !queue.is_empty() && visited.len() < spec.max_pages {
        let mut batch = FuturesUnordered::new();
        while batch.len() < spec.concurrency && !queue.is_empty() && visited.len() < spec.max_pages
        {
            let (raw, depth) = queue.pop_front().unwrap();
            queued.remove(&key(&raw));
            let url = match valid_url(&raw) {
                Ok(u) => u,
                Err(_) => continue,
            };
            if depth > spec.max_depth || !host_allowed(spec, &url) {
                continue;
            }
            let digest = key(url.as_str());
            if !visited.insert(digest) {
                continue;
            }
            if spec.obey_robots && !robots_allowed(&url, &fetcher, &mut robot_cache).await {
                summary.failed += 1;
                emit(&CrawlItem {
                    url: raw,
                    depth,
                    status: None,
                    links: Vec::new(),
                    error: Some("blocked by robots.txt policy".into()),
                    body: None,
                })?;
                continue;
            }
            if spec.delay_ms > 0 {
                let host = url.host_str().unwrap_or_default().to_string();
                let now = Instant::now();
                let desired = site_clock
                    .get(&host)
                    .map(|last| *last + Duration::from_millis(spec.delay_ms))
                    .unwrap_or(now);
                if desired > now {
                    tokio::time::sleep(desired - now).await;
                }
                site_clock.insert(host, Instant::now());
            }
            let backend = Arc::clone(&fetcher);
            batch.push(async move {
                let result = backend.fetch(url.as_str()).await;
                (url.to_string(), depth, result)
            });
        }
        while let Some((url, depth, result)) = batch.next().await {
            let item = match result {
                Ok(response) => {
                    let links = if response.content_type.contains("html") && depth < spec.max_depth
                    {
                        page_links(&response.url, &response.body)
                    } else {
                        Vec::new()
                    };
                    if (200..300).contains(&response.status) {
                        summary.successful += 1;
                    } else {
                        summary.failed += 1;
                    }
                    CrawlItem {
                        url,
                        depth,
                        status: Some(response.status),
                        links,
                        error: None,
                        body: Some(response.body),
                    }
                }
                Err(err) => {
                    summary.failed += 1;
                    CrawlItem {
                        url,
                        depth,
                        status: None,
                        links: Vec::new(),
                        error: Some(err),
                        body: None,
                    }
                }
            };
            for link in &item.links {
                if let Ok(parsed) = valid_url(link) {
                    if host_allowed(spec, &parsed)
                        && !visited.contains(&key(link))
                        && queued.insert(key(link))
                    {
                        queue.push_back((link.clone(), depth + 1));
                    }
                }
            }
            emit(&item)?;
        }
        let mut fingerprints: Vec<String> = visited.iter().cloned().collect();
        fingerprints.sort();
        persist(
            spec,
            &Progress {
                visited_hashes: fingerprints,
                queue: queue.iter().cloned().collect(),
            },
        )?;
    }
    summary.visited = visited.len();
    summary.remaining = queue.len();
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Mock;
    impl Fetcher for Mock {
        fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
            Box::pin(async move {
                Ok(FetchPage {
                    status: 200,
                    url: url.into(),
                    content_type: "text/html".into(),
                    body: if url.ends_with("/a") {
                        "<a href='/b'>b</a><a href='/b#top'>repeat</a>".into()
                    } else {
                        "<p>done</p>".into()
                    },
                })
            })
        }
    }

    #[tokio::test]
    async fn dedupe_and_resume() {
        let temp = tempfile::tempdir().unwrap();
        let spec = CrawlSpec {
            start_urls: vec!["https://example.com/a".into()],
            crawldir: Some(temp.path().to_path_buf()),
            ..CrawlSpec::default()
        };
        let mut pages = Vec::new();
        let first = crawl(&spec, Arc::new(Mock), |item| {
            pages.push(item.url.clone());
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(first.successful, 2);
        assert_eq!(pages.len(), 2);
        let second = crawl(&spec, Arc::new(Mock), |_| panic!("should not refetch"))
            .await
            .unwrap();
        assert_eq!(second.successful, 0);
        assert_eq!(second.visited, 2);
    }

    #[test]
    fn rejects_unsafe_urls_and_dedupes_fragments() {
        assert!(CrawlSpec::from_toml("start_urls=['file:///etc/passwd']").is_err());
        assert!(CrawlSpec::from_toml("start_urls=['https://name:secret@example.com/']").is_err());
        let links = page_links(
            "https://example.com/a",
            "<a href='/b'>b</a><a href='/b#x'>b2</a><a href='javascript:alert(1)'>no</a>",
        );
        assert_eq!(links, vec!["https://example.com/b"]);
    }

    #[test]
    fn robots_specificity_and_wildcards() {
        let policy = parse_robots(
            "User-agent: *\nDisallow: /private\nAllow: /private/public\nDisallow: /*.secret$\n",
        );
        assert!(!robots_permitted(&policy, "/private/key"));
        assert!(robots_permitted(&policy, "/private/public/a"));
        assert!(!robots_permitted(&policy, "/x.secret"));
        assert!(robots_permitted(&policy, "/public"));
    }
}
