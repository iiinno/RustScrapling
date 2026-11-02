use crate::protocol::{DaemonError, FetchRequest, FetchResult, FetchTier};
use rusqlite::{params, Connection, OpenFlags};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use url::Url;

pub(crate) const MAX_CACHE_BODY: usize = 1024 * 1024;
pub(crate) const MAX_CACHE_ENTRIES: usize = 32;
const MAX_KEY: usize = 4096;
const MAX_JSON: usize = 6 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct Persistence(Arc<Mutex<Connection>>);

pub(crate) struct Restored {
    pub cache: Vec<(String, FetchResult, Duration)>,
    pub learning: Vec<(String, FetchTier, Duration)>,
}

impl Persistence {
    pub fn open(state_file: &Path, ttl: Duration) -> Result<(Self, Restored), DaemonError> {
        // The state directory has already been checked/restricted by write_state.
        // A distinct sibling survives removal of the transient token state file.
        let mut name = state_file
            .file_name()
            .ok_or_else(storage_error)?
            .to_os_string();
        name.push(".sqlite3");
        let path = state_file.with_file_name(name);
        let existing = match std::fs::symlink_metadata(&path) {
            Ok(meta) => {
                if !meta.is_file() || meta.file_type().is_symlink() {
                    return Err(storage_error());
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if meta.permissions().mode() & 0o077 != 0 {
                        return Err(storage_error());
                    }
                }
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(_) => return Err(storage_error()),
        };
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        let conn = Connection::open_with_flags(path.clone(), flags).map_err(|_| storage_error())?;
        #[cfg(unix)]
        if !existing {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .map_err(|_| storage_error())?;
        }
        #[cfg(not(unix))]
        let _ = existing;

        conn.execute_batch(
            "PRAGMA trusted_schema=OFF;
             PRAGMA mmap_size=0;
             CREATE TABLE IF NOT EXISTS daemon_cache (
                 request_key TEXT PRIMARY KEY NOT NULL,
                 response BLOB NOT NULL,
                 created_ms INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS daemon_learning (
                 host TEXT PRIMARY KEY NOT NULL,
                 tier TEXT NOT NULL,
                 expires_ms INTEGER NOT NULL
             );",
        )
        .map_err(|_| storage_error())?;
        let now = now_ms();
        let ttl_ms = i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX);
        let mut restored = Restored {
            cache: Vec::new(),
            learning: Vec::new(),
        };
        {
            let mut stmt = conn
                .prepare(
                    "SELECT request_key, response, created_ms FROM daemon_cache
                     WHERE created_ms > ?1 AND created_ms <= ?2
                       AND length(request_key) <= ?3 AND length(response) <= ?4
                     ORDER BY created_ms DESC LIMIT ?5",
                )
                .map_err(|_| storage_error())?;
            let rows = stmt
                .query_map(
                    params![
                        now.saturating_sub(ttl_ms),
                        now,
                        MAX_KEY as i64,
                        MAX_JSON as i64,
                        MAX_CACHE_ENTRIES as i64
                    ],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Vec<u8>>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
                .map_err(|_| storage_error())?;
            for (key, bytes, created) in rows.flatten() {
                if let (Ok(request), Ok(response)) = (
                    serde_json::from_str::<FetchRequest>(&key),
                    serde_json::from_slice::<FetchResult>(&bytes),
                ) {
                    if safe_request(&request) && safe_response(&response) {
                        restored.cache.push((
                            key,
                            response,
                            Duration::from_millis((now - created) as u64),
                        ));
                    }
                }
            }
        }
        {
            let mut stmt = conn
                .prepare(
                    "SELECT host, tier, expires_ms FROM daemon_learning
                     WHERE expires_ms > ?1 AND expires_ms <= ?2 AND length(host) <= 253",
                )
                .map_err(|_| storage_error())?;
            let rows = stmt
                .query_map(params![now, now.saturating_add(ttl_ms)], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })
                .map_err(|_| storage_error())?;
            for (host, tier, expires) in rows.flatten() {
                if let Some(tier) = tier_from_str(&tier) {
                    // Do not trust arbitrary host strings from a modified database.
                    if !host.is_empty()
                        && host
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b".-:".contains(&b))
                    {
                        restored.learning.push((
                            host,
                            tier,
                            Duration::from_millis((expires - now) as u64),
                        ));
                    }
                }
            }
        }
        Ok((Self(Arc::new(Mutex::new(conn))), restored))
    }

    pub async fn save_learning(&self, host: String, tier: FetchTier, ttl: Duration) {
        let store = self.clone();
        let expires = now_ms().saturating_add(i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX));
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(conn) = store.0.lock() {
                let _ = conn.execute(
                    "INSERT OR REPLACE INTO daemon_learning(host,tier,expires_ms) VALUES (?1,?2,?3)",
                    params![host, tier_as_str(tier), expires],
                );
                let _ = conn.execute(
                    "DELETE FROM daemon_learning WHERE expires_ms <= ?1",
                    params![now_ms()],
                );
            }
        })
        .await;
    }

    pub async fn save_cache(&self, key: String, response: FetchResult, ttl: Duration) {
        if key.len() > MAX_KEY || !safe_response(&response) {
            return;
        }
        let Ok(serialized) = serde_json::to_vec(&response) else {
            return;
        };
        if serialized.len() > MAX_JSON {
            return;
        }
        let store = self.clone();
        let created = now_ms();
        let min_age = created.saturating_sub(i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX));
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(conn) = store.0.lock() {
                if conn
                    .execute(
                        "INSERT OR REPLACE INTO daemon_cache(request_key,response,created_ms)
                         VALUES (?1,?2,?3)",
                        params![key, serialized, created],
                    )
                    .is_ok()
                {
                    let _ = conn.execute(
                        "DELETE FROM daemon_cache WHERE created_ms <= ?1
                         OR request_key NOT IN (
                            SELECT request_key FROM daemon_cache
                            ORDER BY created_ms DESC LIMIT ?2
                         )",
                        params![min_age, MAX_CACHE_ENTRIES as i64],
                    );
                }
            }
        })
        .await;
    }

    pub async fn evict_cache(&self, key: String) {
        let store = self.clone();
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(conn) = store.0.lock() {
                let _ = conn.execute("DELETE FROM daemon_cache WHERE request_key = ?1", [key]);
            }
        })
        .await;
    }
}

pub(crate) fn safe_request(request: &FetchRequest) -> bool {
    if request.method != "GET" || request.body.is_some() {
        return false;
    }
    let Ok(url) = Url::parse(&request.url) else {
        return false;
    };
    matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && request.headers.keys().all(|name| {
            ["accept", "accept-language", "user-agent"]
                .iter()
                .any(|allowed| name.eq_ignore_ascii_case(allowed))
        })
}

pub(crate) fn safe_response(response: &FetchResult) -> bool {
    (200..300).contains(&response.status)
        && response.body.len() <= MAX_CACHE_BODY
        && (has_header(&response.headers, "etag") || has_header(&response.headers, "last-modified"))
        && !response.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("set-cookie")
                || name.eq_ignore_ascii_case("vary")
                || (name.eq_ignore_ascii_case("cache-control")
                    && (value.to_ascii_lowercase().contains("no-store")
                        || value.to_ascii_lowercase().contains("private")))
        })
        && Url::parse(&response.final_url).is_ok_and(|url| {
            matches!(url.scheme(), "http" | "https")
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
        })
}

fn has_header(headers: &[(String, String)], name: &str) -> bool {
    headers
        .iter()
        .any(|(key, _)| key.eq_ignore_ascii_case(name))
}

fn tier_as_str(tier: FetchTier) -> &'static str {
    match tier {
        FetchTier::Auto => "auto",
        FetchTier::Http => "http",
        FetchTier::Browser => "browser",
        FetchTier::Headed => "headed",
    }
}

fn tier_from_str(tier: &str) -> Option<FetchTier> {
    match tier {
        "http" => Some(FetchTier::Http),
        "browser" => Some(FetchTier::Browser),
        "headed" => Some(FetchTier::Headed),
        _ => None,
    }
}

fn now_ms() -> i64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}

fn storage_error() -> DaemonError {
    // Deliberately omit paths and database contents from IPC/error messages.
    DaemonError::new(
        "storage",
        "daemon persistence database could not be accessed",
    )
}
