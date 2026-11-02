"""Stage 2 oracle fixtures: SequenceMatcher ratios (3 kinds x 10000),
element_to_dict outputs, find_similar / find_by_text / find_by_regex
results, and an adaptive save->relocate flow with a real SQLite file
written by the original implementation."""
import json
import os
import random
import sqlite3
import sys
from difflib import SequenceMatcher

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "scrapling-src"))
from scrapling import Selector  # noqa: E402
from scrapling.core.utils import _StorageTools  # noqa: E402
from lxml import html as lh  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
FIX = os.path.join(HERE, "..", "fixtures")
os.makedirs(FIX, exist_ok=True)

# ---------------------------------------------------------------- seqmatch
rng = random.Random(20261102)
ALPHABET = "abcd efghあいうえお🙂\n\t"
strings = []
for _ in range(10000):
    a = "".join(rng.choice(ALPHABET) for _ in range(rng.randrange(0, 50)))
    b = "".join(rng.choice(ALPHABET) for _ in range(rng.randrange(0, 50)))
    strings.append([a, b, SequenceMatcher(None, a, b).ratio()])

TAGS = ["html", "body", "div", "span", "a", "p", "article", "section",
        "table", "tr", "td", "ul", "li", "h3", "header", "footer",
        "main", "nav", "form", "input", "button", "img", "em", "strong"]
tuples = []
for _ in range(10000):
    a = [rng.choice(TAGS) for _ in range(rng.randrange(0, 14))]
    b = [rng.choice(TAGS) for _ in range(rng.randrange(0, 14))]
    tuples.append([a, b, SequenceMatcher(None, a, b).ratio()])

longs = []
LONG_ALPHA = "ab"
for _ in range(10000):
    la = rng.randrange(150, 420)
    lb = rng.randrange(150, 420)
    a = "".join(rng.choice(LONG_ALPHA + ("c" if rng.random() < 0.05 else "")) for _ in range(la))
    b = "".join(rng.choice(LONG_ALPHA + ("c" if rng.random() < 0.05 else "")) for _ in range(lb))
    longs.append([a, b, SequenceMatcher(None, a, b).ratio()])

with open(os.path.join(FIX, "stage2_seq.json"), "w") as f:
    json.dump({"strings": strings, "tuples": tuples, "long": longs}, f)
print("seq fixtures done")

# ---------------------------------------------------------------- documents
sys.path.insert(0, HERE)
from gen_stage1 import GENERAL_HTML, COMPLEX_HTML, FILTER_HTML  # noqa: E402

DOCS = {"general": GENERAL_HTML, "complex": COMPLEX_HTML, "filter": FILTER_HTML}


def conv(v):
    if isinstance(v, tuple):
        return [conv(x) for x in v]
    if isinstance(v, dict):
        return {k: conv(x) for k, x in v.items()}
    if isinstance(v, str) or v is None:
        return v
    return "<other>"


def path_of(el):
    return el.getroottree().getpath(el)


dict_docs = []
for name in ("general", "filter"):
    # The ground truth must come from Scrapling's own parse (Selector),
    # whose blank-text removal differs from lxml's default parser.
    root = Selector(DOCS[name])._root
    entries = []
    for el in root.iter():
        if not isinstance(el.tag, str):
            continue
        entries.append({"path": path_of(el), "dict": conv(_StorageTools.element_to_dict(el))})
    dict_docs.append({"name": name, "entries": entries})
with open(os.path.join(FIX, "stage2_dicts.json"), "w") as f:
    json.dump({"docs": dict_docs}, f)
print("dict fixtures done")

# ---------------------------------------------------------------- find_*
general = Selector(DOCS["general"])
root_path = "/html"


def getpaths(selectors):
    return [path_of(s._root) for s in selectors]


base = general.css("article[data-id='2']")[0]
find_fixture = {
    "similar_default": getpaths(base.find_similar()),
    "similar_match_text": getpaths(base.find_similar(match_text=True)),
    "similar_threshold_09": getpaths(base.find_similar(similarity_threshold=0.9)),
}
text_cases = [
    ("In stock: 5", {}),
    ("In stock: 5", {"first_match": False}),
    ("in STOCK: 5", {"first_match": False}),
    ("Product", {"first_match": False, "partial": True}),
    ("Product 2", {"first_match": False}),
    ("Out of stock", {"first_match": False}),
    ("Missing text", {"first_match": False}),
    ("IN STOCK: 5", {"first_match": False, "case_sensitive": True}),
]
text_results = []
for text, kw in text_cases:
    r = general.find_by_text(text, **kw)
    if hasattr(r, "_root"):
        text_results.append([path_of(r._root)])
    else:
        text_results.append(getpaths(r))
find_fixture["by_text"] = text_results
find_fixture["by_text_cases"] = [[t, kw] for t, kw in text_cases]
regex_cases = [
    (r"\$[0-9]+\.[0-9]{2}", {}),
    (r"\$[0-9]+\.[0-9]{2}", {"first_match": False}),
    (r"product \d", {"first_match": False}),
    (r"^In stock: \d$", {"first_match": False}),
    (r"In\s+stock", {"first_match": False, "clean_match": False}),
]
regex_results = []
for pat, kw in regex_cases:
    r = general.find_by_regex(pat, **kw)
    if hasattr(r, "_root"):
        regex_results.append([path_of(r._root)])
    else:
        regex_results.append(getpaths(r))
find_fixture["by_regex"] = regex_results
find_fixture["by_regex_cases"] = [[p, kw] for p, kw in regex_cases]
with open(os.path.join(FIX, "stage2_find.json"), "w") as f:
    json.dump(find_fixture, f)
print("find fixtures done")

# ---------------------------------------------------------------- adaptive
PAGE_A = DOCS["general"]
PAGE_B = """<!DOCTYPE html>
<html lang="en">
<head>
  <title>Shop &amp; Go</title>
  <meta charset="utf-8">
</head>
<body>
  <header class="site-header">
    <nav id="main-nav">
      <ul>
        <li><a href="/home" class="nav-link">Home</a></li>
        <li><a href="/products" class="nav-link active">Products</a></li>
      </ul>
    </nav>
  </header>
  <main id="products">
    <section class="catalog">
      <div class="product-list">
        <article class="item featured" data-id="1" data-price="9.99"><h3>Product 1</h3><span class="price">$9.99</span><p class="stock in">In stock</p></article>
        <article class="item" data-id="2" data-price="21.50"><h3>Product Two</h3><span class="price">$21.50</span><p class="stock out">Out of stock</p></article>
        <article class="item" data-id="3" data-price="5.00"><h3>Product 3</h3><span class="price">$5.00</span><p class="stock in">In stock</p></article>
      </div>
    </section>
  </main>
  <footer><p>Copyright 2026</p></footer>
</body>
</html>"""

db_path = os.path.join(FIX, "oracle_storage.db")
if os.path.exists(db_path):
    os.remove(db_path)
args = {"storage_file": db_path, "url": "https://example.com/shop"}
sa = Selector(PAGE_A, url="https://example.com/shop", adaptive=True, storage_args=args)
target = sa.css("article.product")[1]
sa.save(target, "prod-2")

con = sqlite3.connect(db_path)
row = con.execute("SELECT url, identifier, element_data FROM storage").fetchone()
con.close()
raw_json = row[2] if isinstance(row[2], str) else row[2].decode()

sb = Selector(PAGE_B, url="https://example.com/shop", adaptive=True, storage_args=args)
data = sb.retrieve("prod-2")
relocated_40 = [path_of(el) for el in sb.relocate(data, percentage=40)]
relocated_0 = [path_of(el) for el in sb.relocate(data, percentage=0)]
score_fn = sb._Selector__calculate_similarity_score
scores = {}
for el in sb.css("article") + sb.css("header a") + sb.css("footer p"):
    scores[path_of(el._root)] = score_fn(data, el._root)

with open(os.path.join(FIX, "stage2_adaptive.json"), "w") as f:
    json.dump({
        "db_url_key": row[0],
        "identifier": row[1],
        "raw_json": raw_json,
        "page_b": PAGE_B,
        "relocated_40": relocated_40,
        "relocated_0": relocated_0,
        "scores": scores,
    }, f)
print("adaptive fixtures done: relocated_40 =", relocated_40)
