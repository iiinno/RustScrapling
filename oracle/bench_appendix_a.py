import sys, time, tempfile, os
sys.path.insert(0, sys.argv[1])
from scrapling import Selector
from lxml import html as lh

items = "".join(
    f'<div class="product p{i%7}" data-id="{i}"><h3 class="title"><a href="/p/{i}">Product number {i} name</a></h3>'
    f'<span class="price">${i}.99</span><p class="desc">Description text for item {i} lorem ipsum</p></div>'
    for i in range(2000))
page = f"<html><body><div id='list'>{items}</div></body></html>"

def t(label, f, n=5):
    best = min((lambda s: (f(), time.perf_counter() - s)[1])(time.perf_counter()) for _ in range(n))
    print(f"{label:<38} {best*1000:9.2f} ms")

t("raw lxml parse", lambda: lh.fromstring(page))
t("Scrapling Selector() parse", lambda: Selector(page))
s = Selector(page)
root = lh.fromstring(page)
t("raw lxml cssselect .price text", lambda: [e.text for e in root.cssselect(".price")])
t("Scrapling css('.price::text').getall", lambda: s.css(".price::text").getall())
first = s.css(".product")[0]
t("find_similar (2000 siblings)", lambda: first.find_similar())
t("find_by_text", lambda: s.find_by_text("Product number 1500 name", first_match=True))

db = os.path.join(tempfile.mkdtemp(dir=os.environ.get("TMPDIR", "/tmp")), "a.db")
args = {"storage_file": db, "url": "https://example.com"}
sa = Selector(page, url="https://example.com", adaptive=True, storage_args=args)
sa.save(sa.css(".product")[1500], "target")
changed = page.replace('class="product', 'class="item').replace("Product number", "Item no.")
sb = Selector(changed, url="https://example.com", adaptive=True, storage_args=args)
data = sb.retrieve("target")
t("relocate after layout change (~10k nodes)", lambda: sb.relocate(data, percentage=0), n=1)
print(f"page_bytes={len(page.encode())}")
