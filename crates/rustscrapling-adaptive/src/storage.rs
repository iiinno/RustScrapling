//! SQLite storage for adaptive fingerprints: the same table and the same
//! JSON as Scrapling's `SQLiteStorageSystem`, so files written by one
//! implementation are read by the other.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{types::ValueRef, Connection};
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
                // orjson's dumps returns bytes, so Scrapling stores
                // element_data as a BLOB. Rust stores the same bytes, and
                // reading accepts TEXT too so older Rust-written databases
                // still load.
                let value = row.get_ref(0)?;
                let bytes = match value {
                    ValueRef::Text(bytes) | ValueRef::Blob(bytes) => bytes,
                    _ => {
                        return Err(rusqlite::Error::InvalidColumnType(
                            0,
                            "element_data".to_string(),
                            value.data_type(),
                        ))
                    }
                };
                Ok(std::str::from_utf8(bytes)
                    .ok()
                    .and_then(ElementDict::from_json))
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

        // Rows written by older Rust versions are TEXT; the same reader
        // must accept them as well as the BLOB rows written above.
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute(
                "INSERT OR REPLACE INTO storage (url, identifier, element_data) \
                 VALUES (?1, ?2, ?3)",
                rusqlite::params!["example.com", "prod_text", dict.to_json()],
            )
            .unwrap();
        }
        let storage = SqliteStorage::open(&path, Some("https://example.com/shop")).unwrap();
        let text_back = storage
            .retrieve("prod_text")
            .unwrap()
            .expect("stored text dict");
        assert_eq!(text_back.to_json(), dict.to_json());

        let _ = std::fs::remove_file(&path);
    }
}
