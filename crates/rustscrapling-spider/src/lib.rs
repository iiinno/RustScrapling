//! Bounded, restartable crawler with a fetcher supplied by the acquisition layer.
//! Its `crawl` callback emits each result immediately instead of retaining page bodies.

use futures_util::stream::{FuturesUnordered, StreamExt};
use rustscrapling_dom::html5_backend::{parse_html, Html5Options};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
use url::Url;

mod templates;
pub use templates::CrawlTemplate;

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
    pub template: CrawlTemplate,
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
            template: CrawlTemplate::default(),
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
    #[serde(default)]
    spec_hash: String,
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
    // RFC 9309 matches the request target, including any query component.
    // Checking only url.path() can accidentally bypass a query-specific rule.
    let mut path = url.path().to_owned();
    if let Some(query) = url.query() {
        path.push('?');
        path.push_str(query);
    }
    cache
        .get(&host_key)
        .and_then(Option::as_ref)
        .is_some_and(|rules| robots_permitted(rules, &path))
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
    page_links_limited(base, html, usize::MAX)
}

/// Limit links before allocating result strings, not just when adding them to
/// the crawl queue. A large sitemap-like HTML page can otherwise exhaust
/// memory even when the crawl specification requests only a few pages.
fn page_links_limited(base: &str, html: &str, limit: usize) -> Vec<String> {
    page_links_filtered(base, html, limit, |_| true)
}

/// Count only eligible new links towards the frontier budget. Applying the
/// limit before allowed-domain and visited checks can silently discard useful
/// links when a page starts with navigation to other domains or old pages.
fn page_links_filtered<F>(base: &str, html: &str, limit: usize, mut eligible: F) -> Vec<String>
where
    F: FnMut(&Url) -> bool,
{
    if limit == 0 {
        return Vec::new();
    }
    let Ok(base) = valid_url(base) else {
        return Vec::new();
    };
    let Ok(dom) = parse_html(html, &Html5Options::default()) else {
        return Vec::new();
    };
    // HTML's first <base href> inside <head> sets relative-link resolution.
    // Keep the same safety checks as ordinary fetched and discovered URLs.
    let base = dom
        .descendants_or_self(dom.root)
        .into_iter()
        .find(|&id| {
            dom.node(id).tag == "base"
                && dom.attr(id, "href").is_some()
                && dom.ancestors(id).iter().any(|&a| dom.node(a).tag == "head")
        })
        .and_then(|id| base.join(dom.attr(id, "href")?).ok())
        .filter(|url| valid_url(url.as_str()).is_ok())
        .unwrap_or(base);
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
        if !eligible(&url) {
            continue;
        }
        let value = url.to_string();
        if seen.insert(value.clone()) {
            links.push(value);
            if links.len() == limit {
                break;
            }
        }
    }
    links
}

fn state_path(spec: &CrawlSpec) -> Option<PathBuf> {
    spec.crawldir
        .as_ref()
        .map(|dir| dir.join("crawl-state.json"))
}

fn spec_hash(spec: &CrawlSpec) -> Result<String, String> {
    let bytes = serde_json::to_vec(spec).map_err(|e| format!("encoding crawl spec: {e}"))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn restore(spec: &CrawlSpec) -> Result<Option<Progress>, String> {
    let Some(path) = state_path(spec) else {
        return Ok(None);
    };
    // Recover the last intact checkpoint if Windows had to move the
    // previous file aside before an interrupted replacement.
    let backup = path.with_extension("json.bak");
    let mut last_error = None;
    for candidate in [&path, &backup] {
        if !candidate.exists() {
            continue;
        }
        match std::fs::read(candidate)
            .map_err(|e| e.to_string())
            .and_then(|bytes| serde_json::from_slice::<Progress>(&bytes).map_err(|e| e.to_string()))
        {
            Ok(progress) => {
                if !progress.spec_hash.is_empty() && progress.spec_hash != spec_hash(spec)? {
                    return Err("checkpoint belongs to a different crawl specification".into());
                }
                return Ok(Some(progress));
            }
            Err(reason) => last_error = Some(reason),
        }
    }
    match last_error {
        Some(reason) => Err(format!("invalid crawl state and backup: {reason}")),
        None => Ok(None),
    }
}

fn persist(spec: &CrawlSpec, progress: &Progress) -> Result<(), String> {
    let Some(path) = state_path(spec) else {
        return Ok(());
    };
    let parent = path.parent().ok_or("state has no parent directory")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("creating crawldir: {e}"))?;
    let pending = path.with_extension("json.tmp");
    let mut data = progress.clone();
    data.spec_hash = spec_hash(spec)?;
    let bytes = serde_json::to_vec(&data).map_err(|e| e.to_string())?;
    let mut file = std::fs::File::create(&pending).map_err(|e| format!("creating state: {e}"))?;
    file.write_all(&bytes)
        .map_err(|e| format!("writing state: {e}"))?;
    file.sync_all().map_err(|e| format!("syncing state: {e}"))?;
    drop(file);
    #[cfg(windows)]
    {
        let backup = path.with_extension("json.bak");
        if path.exists() {
            if backup.exists() {
                std::fs::remove_file(&backup).map_err(|e| format!("removing old backup: {e}"))?;
            }
            std::fs::rename(&path, &backup).map_err(|e| format!("backing up state: {e}"))?;
        }
        if let Err(err) = std::fs::rename(&pending, &path) {
            if !path.exists() && backup.exists() {
                let _ = std::fs::rename(&backup, &path);
            }
            return Err(format!("saving state: {err}"));
        }
        if backup.exists() {
            std::fs::remove_file(backup).map_err(|e| format!("cleaning state backup: {e}"))?;
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(pending, path).map_err(|e| format!("saving state: {e}"))
    }
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
    let state = if let Some(saved) = restore(spec)? {
        saved
    } else {
        let seeds = templates::resolve_seeds(spec, &fetcher).await?;
        let mut seen_seeds = HashSet::new();
        Progress {
            queue: seeds
                .into_iter()
                .filter_map(|raw| {
                    let mut url = valid_url(&raw).ok()?;
                    if !host_allowed(spec, &url) {
                        return None;
                    }
                    url.set_fragment(None);
                    let normalized = url.to_string();
                    seen_seeds
                        .insert(key(&normalized))
                        .then_some((normalized, 0))
                })
                .take(spec.max_pages)
                .collect(),
            ..Progress::default()
        }
    };
    let mut visited: HashSet<String> = state.visited_hashes.into_iter().collect();
    let mut queued: HashSet<String> = state.queue.iter().map(|(url, _)| key(url)).collect();
    let mut queue: VecDeque<(String, usize)> = state.queue.into();
    let mut summary = CrawlSummary::default();
    // Shared per-host gates must run in the futures themselves. A timestamp
    // computed while filling a batch can be stale before the first poll.
    let mut site_clock = HashMap::<String, Arc<tokio::sync::Mutex<Option<Instant>>>>::new();
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
            let host_clock = if spec.delay_ms > 0 {
                let host = url.host_str().unwrap_or_default().to_owned();
                Some(Arc::clone(
                    site_clock
                        .entry(host)
                        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(None))),
                ))
            } else {
                None
            };
            let delay = Duration::from_millis(spec.delay_ms);
            let backend = Arc::clone(&fetcher);
            batch.push(async move {
                if let Some(host_clock) = host_clock {
                    let mut last_start = host_clock.lock().await;
                    if let Some(last) = *last_start {
                        if let Some(next) = last.checked_add(delay) {
                            tokio::time::sleep_until(tokio::time::Instant::from_std(next)).await;
                        }
                    }
                    // Release the gate before awaiting the network response:
                    // different requests can overlap once their start times
                    // have been spaced apart.
                    *last_start = Some(Instant::now());
                }
                let result = backend.fetch(url.as_str()).await;
                (url.to_string(), depth, result)
            });
        }
        while let Some((url, depth, result)) = batch.next().await {
            let item = match result {
                Ok(response) => {
                    let links = if spec.template.follows_links()
                        && response.content_type.contains("html")
                        && depth < spec.max_depth
                    {
                        page_links_filtered(
                            &response.url,
                            &response.body,
                            spec.max_pages
                                .saturating_sub(visited.len())
                                .saturating_sub(queue.len()),
                            |candidate| {
                                host_allowed(spec, candidate)
                                    && !visited.contains(&key(candidate.as_str()))
                                    && !queued.contains(&key(candidate.as_str()))
                            },
                        )
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
                spec_hash: spec_hash(spec)?,
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
    use std::sync::Mutex;

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

    struct TimedFetcher(Mutex<Vec<Instant>>);

    impl Fetcher for TimedFetcher {
        fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
            Box::pin(async move {
                self.0.lock().unwrap().push(Instant::now());
                Ok(FetchPage {
                    status: 200,
                    url: url.into(),
                    content_type: "text/plain".into(),
                    body: String::new(),
                })
            })
        }
    }

    #[tokio::test]
    async fn per_host_delay_applies_to_fetch_starts_not_batch_construction() {
        let fetcher = Arc::new(TimedFetcher(Mutex::new(Vec::new())));
        let spec = CrawlSpec {
            start_urls: vec![
                "https://example.com/one".into(),
                "https://example.com/two".into(),
                "https://example.com/three".into(),
            ],
            concurrency: 3,
            max_depth: 0,
            delay_ms: 60,
            ..CrawlSpec::default()
        };
        let summary = crawl(&spec, fetcher.clone(), |_| Ok(())).await.unwrap();
        assert_eq!(summary.successful, 3);
        let starts = fetcher.0.lock().unwrap();
        assert_eq!(starts.len(), 3);
        for times in starts.windows(2) {
            assert!(
                times[1].duration_since(times[0]) >= Duration::from_millis(40),
                "requests to one host must not launch together"
            );
        }
    }

    struct SlowRobotFetcher(Mutex<Vec<(String, Instant)>>);

    impl Fetcher for SlowRobotFetcher {
        fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
            Box::pin(async move {
                if url.ends_with("/robots.txt") {
                    if url.contains("other.example") {
                        tokio::time::sleep(Duration::from_millis(180)).await;
                    }
                    return Ok(FetchPage {
                        status: 200,
                        url: url.into(),
                        content_type: "text/plain".into(),
                        body: "User-agent: *\nAllow: /\n".into(),
                    });
                }
                self.0.lock().unwrap().push((url.into(), Instant::now()));
                Ok(FetchPage {
                    status: 200,
                    url: url.into(),
                    content_type: "text/plain".into(),
                    body: String::new(),
                })
            })
        }
    }

    #[tokio::test]
    async fn per_host_delay_survives_slow_batch_preparation() {
        let fetcher = Arc::new(SlowRobotFetcher(Mutex::new(Vec::new())));
        let spec = CrawlSpec {
            start_urls: vec![
                "https://example.com/first".into(),
                "https://other.example/page".into(),
                "https://example.com/second".into(),
            ],
            concurrency: 3,
            max_depth: 0,
            obey_robots: true,
            delay_ms: 90,
            ..CrawlSpec::default()
        };
        assert_eq!(
            crawl(&spec, fetcher.clone(), |_| Ok(()))
                .await
                .unwrap()
                .successful,
            3
        );
        let starts = fetcher.0.lock().unwrap();
        let same_host: Vec<_> = starts
            .iter()
            .filter(|(url, _)| url.contains("example.com"))
            .map(|(_, time)| *time)
            .collect();
        assert_eq!(same_host.len(), 2);
        assert!(same_host[1].duration_since(same_host[0]) >= Duration::from_millis(65));
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
    fn respects_html_base_and_bounds_link_discovery() {
        let html = "<html><head><base href='https://example.com/docs/'></head><body>\
            <a href='one#top'>first</a><a href='one#next'>duplicate</a>\
            <a href='two'>second</a><a href='three'>third</a></body></html>";
        assert_eq!(
            page_links_limited("https://example.com/index", html, 2),
            vec![
                "https://example.com/docs/one".to_owned(),
                "https://example.com/docs/two".to_owned(),
            ]
        );
        assert!(page_links_limited("https://example.com/index", html, 0).is_empty());
        assert_eq!(
            page_links_limited(
                "https://example.com/index",
                "<html><head><base href='javascript:alert(1)'></head><body><a href='/safe'>safe</a></body></html>",
                1,
            ),
            vec!["https://example.com/safe".to_owned()]
        );
    }

    struct LinkFlood;
    impl Fetcher for LinkFlood {
        fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
            Box::pin(async move {
                let body = if url.ends_with("/start") {
                    (0..2000)
                        .map(|id| format!("<a href='/page/{id}'>Page</a>"))
                        .collect::<String>()
                } else {
                    "<p>Done</p>".to_owned()
                };
                Ok(FetchPage {
                    status: 200,
                    url: url.to_owned(),
                    content_type: "text/html".into(),
                    body,
                })
            })
        }
    }

    #[tokio::test]
    async fn large_link_pages_do_not_grow_queue_beyond_max_pages() {
        let spec = CrawlSpec {
            start_urls: vec!["https://example.com/start".into()],
            max_pages: 3,
            max_depth: 1,
            concurrency: 1,
            ..CrawlSpec::default()
        };
        let mut visited = Vec::new();
        let result = crawl(&spec, Arc::new(LinkFlood), |item| {
            visited.push(item.url.clone());
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(result.visited, 3);
        assert_eq!(result.remaining, 0);
        assert_eq!(visited.len(), 3);
        assert!(visited[1].ends_with("/page/0"));
        assert!(visited[2].ends_with("/page/1"));
    }

    struct FrontLoadedNavigation;

    impl Fetcher for FrontLoadedNavigation {
        fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
            Box::pin(async move {
                let body = if url.ends_with("/start") {
                    let mut links = (0..400)
                        .map(|id| format!("<a href='https://outside.example/{id}'>external</a>"))
                        .collect::<String>();
                    links.push_str(&"<a href='/start'>already visited</a>".repeat(400));
                    links.push_str("<a href='/target'>wanted link</a>");
                    links
                } else {
                    "<p>target found</p>".to_owned()
                };
                Ok(FetchPage {
                    status: 200,
                    url: url.to_owned(),
                    content_type: "text/html".into(),
                    body,
                })
            })
        }
    }

    #[tokio::test]
    async fn link_budget_skips_external_and_visited_links_before_counting() {
        let spec = CrawlSpec {
            start_urls: vec!["https://example.com/start".into()],
            max_pages: 2,
            max_depth: 1,
            concurrency: 1,
            ..CrawlSpec::default()
        };
        let mut emitted = Vec::new();
        let result = crawl(&spec, Arc::new(FrontLoadedNavigation), |item| {
            emitted.push(item.clone());
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(result.successful, 2);
        assert_eq!(result.visited, 2);
        assert_eq!(result.remaining, 0);
        assert_eq!(emitted.len(), 2);
        assert_eq!(emitted[0].links, vec!["https://example.com/target"]);
        assert_eq!(emitted[1].url, "https://example.com/target");
    }

    #[tokio::test]
    async fn seed_limit_counts_unique_eligible_urls_not_duplicate_prefixes() {
        let mut seeds = (0..200)
            .map(|id| format!("https://example.com/first#section{id}"))
            .collect::<Vec<_>>();
        seeds.push("https://example.com/second".into());
        let spec = CrawlSpec {
            start_urls: seeds,
            max_pages: 2,
            max_depth: 0,
            concurrency: 1,
            ..CrawlSpec::default()
        };
        let mut urls = Vec::new();
        let result = crawl(&spec, Arc::new(Mock), |item| {
            urls.push(item.url.clone());
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(result.successful, 2);
        assert_eq!(
            urls,
            ["https://example.com/first", "https://example.com/second"]
        );
    }

    #[test]
    fn robots_specificity_and_wildcards() {
        let policy = parse_robots(
            "User-agent: *\nDisallow: /private\nAllow: /private/public\nDisallow: /*.secret$\nDisallow: /search?private=true\n",
        );
        assert!(!robots_permitted(&policy, "/private/key"));
        assert!(robots_permitted(&policy, "/private/public/a"));
        assert!(!robots_permitted(&policy, "/x.secret"));
        assert!(robots_permitted(&policy, "/public"));
        assert!(!robots_permitted(&policy, "/search?private=true"));
        assert!(robots_permitted(&policy, "/search?private=false"));
    }

    struct RobotFetcher;
    impl Fetcher for RobotFetcher {
        fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
            Box::pin(async move {
                Ok(FetchPage {
                    status: 200,
                    url: url.into(),
                    content_type: "text/plain".into(),
                    body: "User-agent: *\nDisallow: /search?private=true\n".into(),
                })
            })
        }
    }

    #[tokio::test]
    async fn robots_rules_cover_query_parameters() {
        let backend: Arc<dyn Fetcher> = Arc::new(RobotFetcher);
        let mut cache = HashMap::new();
        let blocked = Url::parse("https://example.com/search?private=true").unwrap();
        let allowed = Url::parse("https://example.com/search?private=false").unwrap();
        assert!(!robots_allowed(&blocked, &backend, &mut cache).await);
        assert!(robots_allowed(&allowed, &backend, &mut cache).await);
    }

    #[test]
    fn resume_rejects_mismatched_spec_and_recovers_backup() {
        let dir = tempfile::tempdir().unwrap();
        let spec = CrawlSpec {
            start_urls: vec!["https://example.org/one".into()],
            crawldir: Some(dir.path().to_path_buf()),
            ..CrawlSpec::default()
        };
        persist(&spec, &Progress::default()).unwrap();
        let other = CrawlSpec {
            start_urls: vec!["https://example.org/two".into()],
            ..spec.clone()
        };
        assert!(restore(&other)
            .unwrap_err()
            .contains("different crawl specification"));
        let file = state_path(&spec).unwrap();
        std::fs::copy(&file, file.with_extension("json.bak")).unwrap();
        std::fs::write(&file, b"{incomplete").unwrap();
        assert!(restore(&spec).unwrap().is_some());
    }
}
