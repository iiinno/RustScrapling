//! Writes fixtures/rust_storage.db with the Rust implementation so the
//! oracle-side verifier (oracle/verify_stage2.py) can prove Scrapling
//! reads and relocates from it identically.
use rustscrapling_adaptive::SqliteStorage;
use rustscrapling_dom::libxml_backend::{parse_html, CompatOptions};
use rustscrapling_select::css::css;
use rustscrapling_select::Selection;

fn main() {
    let stage1: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string("fixtures/stage1.json").unwrap()).unwrap();
    let html = stage1["docs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == "general")
        .unwrap()["html"]
        .as_str()
        .unwrap()
        .to_string();
    let dom = parse_html(&html, &CompatOptions::default()).unwrap();
    let root = dom.document_element().unwrap();
    let target = match css(&dom, root, "article.product").unwrap()[1] {
        Selection::Element(id) => id,
        _ => panic!("not an element"),
    };
    let db = std::path::Path::new("fixtures/rust_storage.db");
    let _ = std::fs::remove_file(db);
    let storage = SqliteStorage::open(db, Some("https://example.com/shop")).unwrap();
    storage.save_element(&dom, target, "prod-2").unwrap();
    println!("wrote {}", db.display());
}
