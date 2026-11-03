//! Opt-in development replay cache. Each entry belongs to one requested URL;
//! it is not a shared HTTP cache, and does not cache failure/challenge pages.
//! The URL is hashed in the filename to avoid exposing query credentials.

use super::{FetchFuture, FetchPage, Fetcher};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 12 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheEntry {
    version: u8,
    request_hash: String,
    page: FetchPage,
}

pub(super) struct CachingFetcher {
    upstream: Arc<dyn Fetcher>,
    root: PathBuf,
}

impl CachingFetcher {
    pub fn new(upstream: Arc<dyn Fetcher>, root: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&root)
            .map_err(|e| format!("creating development cache directory: {e}"))?;
        let metadata = std::fs::symlink_metadata(&root)
            .map_err(|e| format!("checking development cache directory: {e}"))?;
        if !metadata.file_type().is_dir() {
            return Err("development cache must be a real directory, not a symlink".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                    .map_err(|e| format!("protecting development cache: {e}"))?;
            }
        }
        Ok(Self { upstream, root })
    }

    fn entry_path(&self, hash: &str) -> PathBuf {
        self.root.join(format!("{hash}.json"))
    }

    async fn load(&self, hash: &str) -> Result<Option<FetchPage>, String> {
        let path = self.entry_path(hash);
        let meta = match tokio::fs::symlink_metadata(&path).await {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("checking development cache: {error}")),
        };
        if !meta.file_type().is_file() || meta.len() > MAX_FILE_BYTES {
            return Err("development cache entry is unsafe or oversized".into());
        }
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| format!("reading development cache: {e}"))?;
        let entry: CacheEntry = serde_json::from_slice(&bytes)
            .map_err(|_| "invalid development cache entry; remove it to refetch".to_owned())?;
        if entry.version != 1
            || entry.request_hash != hash
            || entry.page.body.len() > MAX_BODY_BYTES
            || !(200..300).contains(&entry.page.status)
        {
            return Err("invalid development cache entry; remove it to refetch".into());
        }
        Ok(Some(entry.page))
    }

    async fn store(&self, hash: String, page: FetchPage) -> Result<(), String> {
        let root = self.root.clone();
        let bytes = serde_json::to_vec(&CacheEntry {
            version: 1,
            request_hash: hash.clone(),
            page,
        })
        .map_err(|e| format!("serializing development cache: {e}"))?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Ok(());
        }
        tokio::task::spawn_blocking(move || write_entry(&root, &hash, &bytes))
            .await
            .map_err(|e| format!("development cache task failed: {e}"))?
    }
}

fn write_entry(root: &Path, hash: &str, bytes: &[u8]) -> Result<(), String> {
    let final_path = root.join(format!("{hash}.json"));
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pending = root.join(format!("{hash}.{}.{nonce}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&pending)
        .map_err(|e| format!("creating development cache entry: {e}"))?;
    let written = file
        .write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| format!("writing development cache entry: {e}"));
    drop(file);
    if written.is_ok() {
        // Linking is atomic and never overwrites an existing final path,
        // unlike a Unix rename. Readers see only a fully fsynced entry.
        let linked = std::fs::hard_link(&pending, &final_path);
        let _ = std::fs::remove_file(&pending);
        if let Err(error) = linked {
            if std::fs::symlink_metadata(&final_path).is_ok_and(|meta| meta.file_type().is_file()) {
                // Two processes cached the same URL concurrently.
                return Ok(());
            }
            return Err(format!("saving development cache entry: {error}"));
        }
    } else {
        let _ = std::fs::remove_file(&pending);
    }
    written
}

fn is_cacheable(page: &FetchPage) -> bool {
    if !(200..300).contains(&page.status) || page.body.len() > MAX_BODY_BYTES {
        return false;
    }
    let prefix = page
        .body
        .chars()
        .take(8192)
        .collect::<String>()
        .to_ascii_lowercase();
    ![
        "cf-chl-",
        "challenge-platform",
        "cf-turnstile",
        "captcha challenge",
    ]
    .iter()
    .any(|needle| prefix.contains(needle))
}

impl Fetcher for CachingFetcher {
    fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
        Box::pin(async move {
            // robots.txt must be checked against the current site policy;
            // saved pages may be replayed but permission is never replayed.
            let robots = url::Url::parse(url)
                .ok()
                .is_some_and(|parsed| parsed.path() == "/robots.txt");
            if robots {
                return self.upstream.fetch(url).await;
            }
            let hash = blake3::hash(url.as_bytes()).to_hex().to_string();
            if let Some(page) = self.load(&hash).await? {
                return Ok(page);
            }
            let page = self.upstream.fetch(url).await?;
            if is_cacheable(&page) {
                self.store(hash, page.clone()).await?;
            }
            Ok(page)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Source(AtomicUsize);

    impl Fetcher for Source {
        fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
            Box::pin(async move {
                let call = self.0.fetch_add(1, Ordering::SeqCst);
                if call > 0 && !url.ends_with("/robots.txt") {
                    return Err("upstream intentionally offline".into());
                }
                Ok(FetchPage {
                    headers: Default::default(),
                    status: 200,
                    url: url.to_owned(),
                    content_type: "text/html".into(),
                    body: "<h1>saved source</h1>".into(),
                })
            })
        }
    }

    #[tokio::test]
    async fn caches_successful_pages_and_fails_closed_on_corrupt_entry() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("private");
        let source = Arc::new(Source(AtomicUsize::new(0)));
        let fetcher = CachingFetcher::new(source.clone(), root.clone()).unwrap();
        let url = "https://example.org/article?token=privatevalue";
        assert_eq!(fetcher.fetch(url).await.unwrap().status, 200);
        assert_eq!(
            fetcher.fetch(url).await.unwrap().body,
            "<h1>saved source</h1>"
        );
        assert_eq!(source.0.load(Ordering::SeqCst), 1);

        let entries = std::fs::read_dir(&root)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(entries.len(), 1);
        let path = entries[0].path();
        assert!(!path.to_string_lossy().contains("privatevalue"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        std::fs::write(&path, "corrupt").unwrap();
        let error = fetcher.fetch(url).await.unwrap_err();
        assert!(error.contains("invalid development cache"));
        assert_eq!(source.0.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cache_does_not_replay_robots_or_rate_limit_pages() {
        struct ShiftingSource(AtomicUsize);
        impl Fetcher for ShiftingSource {
            fn fetch<'a>(&'a self, url: &'a str) -> FetchFuture<'a> {
                Box::pin(async move {
                    let call = self.0.fetch_add(1, Ordering::SeqCst);
                    Ok(FetchPage {
                        headers: Default::default(),
                        status: if url.ends_with("/blocked") { 429 } else { 200 },
                        url: url.to_owned(),
                        content_type: "text/plain".into(),
                        body: format!("call {call}"),
                    })
                })
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let source = Arc::new(ShiftingSource(AtomicUsize::new(0)));
        let cache = CachingFetcher::new(source.clone(), temp.path().join("cache")).unwrap();
        for url in [
            "https://example.org/robots.txt",
            "https://example.org/blocked",
        ] {
            let first = cache.fetch(url).await.unwrap();
            let second = cache.fetch(url).await.unwrap();
            assert_ne!(first.body, second.body, "{url} must not be replayed");
        }
        assert_eq!(source.0.load(Ordering::SeqCst), 4);
    }
}
