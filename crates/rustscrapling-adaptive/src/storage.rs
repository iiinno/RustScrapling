//! SQLite storage for adaptive fingerprints: the same table and the same
//! JSON as Scrapling's `SQLiteStorageSystem`, so files written by one
//! implementation are read by the other.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;
use rustscrapling_dom::{Dom, NodeId};

use crate::dict::{element_to_dict, ElementDict};

/// The storage key for a page URL: the registrable domain (what the
/// `tld` package calls `fld`), or "default" when there is none.
/// Scrapling lowercases the URL first (see StorageSystemMixin).
pub fn base_url(url: Option<&str>) -> String {
    let Some(url) = url.filter(|u| !u.is_empty()) else {
        return "default".to_string();
    };
    let lower = url.to_lowercase();
    let parsed = if lower.contains("://") {
        url::Url::parse(&lower)
    } else {
        url::Url::parse(&format!("http://{lower}"))
    };
    let Ok(parsed) = parsed else {
        return "default".to_string();
    };
    let Some(host) = parsed.host_str() else {
        return "default".to_string();
    };
    if host.parse::<std::net::IpAddr>().is_ok() {
        return "default".to_string();
    }
    match psl::domain(host.as_bytes()) {
        Some(domain) => String::from_utf8_lossy(domain.as_bytes()).into_owned(),
        None => "default".to_string(),
    }
}

pub struct SqliteStorage {
    conn: Mutex<Connection>,
    base: String,
}

impl SqliteStorage {
    pub fn open(storage_file: impl AsRef<Path>, url: Option<&str>) -> rusqlite::Result<Self> {
        let conn = Connection::open(storage_file)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS storage (
                id INTEGER PRIMARY KEY,
                url TEXT,
                identifier TEXT,
                element_data TEXT,
                UNIQUE (url, identifier)
            );",
        )?;
        Ok(SqliteStorage {
            conn: Mutex::new(conn),
            base: base_url(url),
        })
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn save_dict(&self, dict: &ElementDict, identifier: &str) -> rusqlite::Result<()> {
        let conn = self.conn.lock().expect("storage lock");
        // Bind the JSON as bytes: Scrapling stores orjson's bytes, so
        // its files hold a BLOB here; matching that keeps files written
        // by RustScrapling indistinguishable from the original's.
        conn.execute(
            "INSERT OR REPLACE INTO storage (url, identifier, element_data) VALUES (?1, ?2, ?3)",
            rusqlite::params![self.base, identifier, dict.to_json().as_bytes()],
        )?;
        Ok(())
    }

    pub fn save_element(
        &self,
        dom: &Dom,
        element: NodeId,
        identifier: &str,
    ) -> rusqlite::Result<()> {
        self.save_dict(&element_to_dict(dom, element), identifier)
    }

    pub fn retrieve(&self, identifier: &str) -> rusqlite::Result<Option<ElementDict>> {
        let conn = self.conn.lock().expect("storage lock");
        let mut stmt =
            conn.prepare("SELECT element_data FROM storage WHERE url = ?1 AND identifier = ?2")?;
        let mut rows = stmt.query(rusqlite::params![self.base, identifier])?;
        match rows.next()? {
            Some(row) => {
                // Scrapling stores orjson's bytes (a BLOB); accept both
                // BLOB and TEXT so files written either way read back.
                use rusqlite::types::Value;
                let text = match row.get::<_, Value>(0)? {
                    Value::Blob(data) => String::from_utf8(data).ok(),
                    Value::Text(s) => Some(s),
                    _ => None,
                };
                Ok(text.and_then(|s| ElementDict::from_json(&s)))
            }
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustscrapling_dom::libxml_backend::{parse_html, CompatOptions};
    use rustscrapling_select::css::css;
    use rustscrapling_select::Selection;

    /// Regression test: a database written by RustScrapling itself must
    /// read back (the TEXT/BLOB asymmetry once broke exactly this).
    #[test]
    fn rust_written_db_roundtrips() {
        let path = std::env::temp_dir().join(format!(
            "rustscrapling-storage-test-{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let html = "<html><body><div><article class=\"product p2\" data-id=\"2\">\
                    <h3><a href=\"/p/2\">Second</a></h3></article></div></body></html>";
        let dom = parse_html(html, &CompatOptions::default()).unwrap();
        let root = dom.document_element().unwrap();
        let el = match css(&dom, root, "article").unwrap()[0] {
            Selection::Element(id) => id,
            _ => unreachable!(),
        };
        let dict = element_to_dict(&dom, el);
        {
            let storage = SqliteStorage::open(&path, Some("https://example.com/shop")).unwrap();
            storage.save_dict(&dict, "prod").unwrap();
        }
        let storage = SqliteStorage::open(&path, Some("https://example.com/shop")).unwrap();
        let back = storage.retrieve("prod").unwrap().expect("stored dict");
        assert_eq!(back.to_json(), dict.to_json());
        assert!(storage.retrieve("missing").unwrap().is_none());
        let _ = std::fs::remove_file(&path);
    }
}
