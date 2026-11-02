"""Reverse-direction check: Scrapling (oracle) reads the SQLite file
written by RustScrapling and relocates the same element on the changed
page. Run after `cargo run -p rustscrapling-adaptive --example
write_rust_storage`."""
import json
import os
import sqlite3
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "scrapling-src"))
from scrapling import Selector  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
FIX = os.path.join(HERE, "..", "fixtures")

with open(os.path.join(FIX, "stage2_adaptive.json")) as f:
    expected = json.load(f)

db_path = os.path.join(FIX, "rust_storage.db")
assert os.path.exists(db_path), "rust_storage.db missing; run the Rust example first"
con = sqlite3.connect(db_path)
row = con.execute("SELECT url, identifier, element_data FROM storage").fetchone()
con.close()
raw = row[2] if isinstance(row[2], str) else row[2].decode()
assert row[0] == expected["db_url_key"], row[0]
assert json.loads(raw) == json.loads(expected["raw_json"]), "stored JSON differs"
assert raw == expected["raw_json"], "stored JSON is not byte-identical"

args = {"storage_file": db_path, "url": "https://example.com/shop"}
sb = Selector(expected["page_b"], url="https://example.com/shop", adaptive=True, storage_args=args)
data = sb.retrieve("prod-2")
got = [el.getroottree().getpath(el) for el in sb.relocate(data, percentage=40)]
assert got == expected["relocated_40"], (got, expected["relocated_40"])
print("verify_stage2: oracle reads RustScrapling DB, JSON byte-identical, relocate matches")
