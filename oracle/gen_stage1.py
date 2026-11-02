#!/usr/bin/env python3
"""Generate stage-1 fixtures from the pinned Scrapling oracle.

Output: fixtures/stage1.json
Each doc has css/xpath queries with the oracle's results, where an
element is identified by its lxml getpath (same-tag index rule) and
described by tag, ordered attributes, lxml text/tail and text_content.
Text results (from ::text, ::attr or text() queries) are stored as
plain strings. Errors are stored as {"error": true}.
"""
import json
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "scrapling-src"))
from scrapling import Selector  # noqa: E402

GENERAL_HTML = """
    <html>
    <head>
        <title>Complex Web Page</title>
        <style>
            .hidden { display: none; }
        </style>
    </head>
    <body>
        <header>
            <nav>
                <ul>
                    <li><a href="#home">Home</a></li>
                    <li><a href="#about">About</a></li>
                    <li><a href="#contact">Contact</a></li>
                </ul>
            </nav>
        </header>
        <main>
            <section id="products" schema='{"jsonable": "data"}'>
                <h2>Products</h2>
                <div class="product-list">
                    <article class="product" data-id="1">
                        <h3>Product 1</h3>
                        <p class="description">This is product 1</p>
                        <span class="price">$10.99</span>
                        <div class="hidden stock">In stock: 5</div>
                    </article>
                    <article class="product" data-id="2">
                        <h3>Product 2</h3>
                        <p class="description">This is product 2</p>
                        <span class="price">$20.99</span>
                        <div class="hidden stock">In stock: 3</div>
                    </article>
                    <article class="product" data-id="3">
                        <h3>Product 3</h3>
                        <p class="description">This is product 3</p>
                        <span class="price">$15.99</span>
                        <div class="hidden stock">Out of stock</div>
                    </article>
                </div>
            </section>
            <section id="reviews">
                <h2>Customer Reviews</h2>
                <div class="review-list">
                    <div class="review" data-rating="5">
                        <p class="review-text">Great product!</p>
                        <span class="reviewer">John Doe</span>
                    </div>
                    <div class="review" data-rating="4">
                        <p class="review-text">Good value for money.</p>
                        <span class="reviewer">Jane Smith</span>
                    </div>
                </div>
            </section>
        </main>
        <footer>
            <p>&copy; 2024 Our Company</p>
        </footer>
        <script id="page-data" type="application/json">
            {"lastUpdated": "2024-09-22T10:30:00Z", "totalProducts": 3}
        </script>
    </body>
    </html>
    """

COMPLEX_HTML = """
        <html>
            <body>
                <div class="container" data-test='{"key": "value"}'>
                    <p>First paragraph</p>
                    <!-- Comment -->
                    <p>Second paragraph</p>
                    <div class="nested">
                        <span id="special">Special content</span>
                        <span>Regular content</span>
                    </div>
                    <table>
                        <tr><td>Cell 1</td><td>Cell 2</td></tr>
                        <tr><td>Cell 3</td><td>Cell 4</td></tr>
                    </table>
                </div>
            </body>
        </html>
        """

FILTER_HTML = """
    <html><body>
        <ul>
            <li class="item" data-value="10">Apple</li>
            <li class="item" data-value="5">Banana</li>
            <li class="item" data-value="20">Cherry</li>
            <li class="item disabled" data-value="0">Durian</li>
        </ul>
    </body></html>
    """

CSS_QUERIES = {
    "general": [
        "main #products .product-list article.product",
        'main #products .product-list article.product:not(:contains("Out of stock"))',
        ".product",
        ".price",
        ".product .price::text",
        "#page-data::text",
        "#products::attr(schema)",
        "article.product[data-id=\"2\"] .price",
        "article[data-id]",
        "div.review-list > div.review",
        "ul li a",
        "li:first-child",
        "li:last-child",
        "li:nth-child(2)",
        "li:nth-child(odd)",
        "li:nth-child(even)",
        "article:nth-of-type(2)",
        "article:first-of-type",
        "article:last-of-type",
        "span:only-of-type",
        "p.description:empty",
        "div.stock:hidden",
        "*",
        "section, footer",
        "article.product + article.product",
        "article.product ~ article.product",
        "div.product-list article .price::text",
        "a[href^=\"#\"]",
        "a[href$=\"about\"]",
        "a[href*=\"contact\"]",
        "div[class~=\"stock\"]",
        "div[class|=\"hidden\"]",
        "span[class]",
        ":root",
        "body > main",
        "main > *",
        "article:not(.product)",
        "div:not(.review):not(.review-list)",
        "p::text",
        ".review::attr(data-rating)",
        "4 ayo",
    ],
    "complex": [
        "p::text",
        "p",
        "div.container > p",
        "div.container p",
        "span#special",
        "div.nested span",
        "table td",
        "td:first-child",
        "tr:nth-child(2) td",
        "div::attr(data-test)",
        "div.container::text",
    ],
    "filter": [
        "li.item",
        "li.item.disabled",
        "li[data-value=\"10\"]",
        "li[data-value>=\"10\"]",
        "li::text",
        "ul > li:nth-last-child(1)",
        "li + li",
    ],
}

XPATH_QUERIES = {
    "general": [
        ('//section[@id="reviews"]//div[contains(@class, "review") and @data-rating >= 4]', {}),
        ('//article[contains(@class, "product")][number(translate(substring-after(.//span[@class="price"], "$"), ",", "")) > 15]', {}),
        ("//article[@data-id='2']//span[@class='price']/text()", {}),
        ("//ul/li/a/text()", {}),
        ("//div[@class='product-list']/article[2]/h3/text()", {}),
        ("//article[position() = 1]", {}),
        ("//article[last()]", {}),
        ("//span[@class='price']", {}),
        ("//main//article[not(@data-id='1')]", {}),
        ("//section[@id='products']/@schema", {}),
        ("//footer/p/text()", {}),
        ("//a[starts-with(@href, '#')]", {}),
        ("//div[contains(concat(' ', normalize-space(@class), ' '), ' stock ')]", {}),
        ("//article[@data-id > 1 and @data-id < 3]", {}),
        ("//h3/../@data-id", {}),
        ("//article[1]/following-sibling::article", {}),
        ("//article[3]/preceding-sibling::article[1]", {}),
        ("//article[2]/ancestor::*", {}),
        ("//span[@class='reviewer']/parent::div", {}),
        ("//*[@id='page-data']", {}),
        ("count(//article)", {}),
        ("4 ayo", {}),
    ],
    "complex": [
        ("//td[text()=$cell_text]", {"cell_text": "Cell 1"}),
        ("//div[@class='container']/p", {}),
        ("//span[@id='special']/text()", {}),
        ("//table//td[2]", {}),
        ("//div[@class='nested']//span[1]", {}),
    ],
    "filter": [
        ("//li[@data-value >= 10]", {}),
        ("//li[contains(@class, 'disabled')]", {}),
    ],
}


def describe(sel):
    root = sel._root
    tree = root.getroottree()
    return {
        "kind": "element",
        "path": tree.getpath(root),
        "tag": root.tag,
        "text": root.text,
        "tail": root.tail,
        "text_content": root.text_content(),
        "attrib": [[k, v] for k, v in root.attrib.items()],
    }


def run_queries(page, kind, queries):
    out = []
    for q in queries:
        if kind == "xpath":
            expr, kwargs = q
        else:
            expr, kwargs = q, {}
        entry = {"expr": expr, "vars": kwargs}
        try:
            results = getattr(page, kind)(expr, **kwargs) if kwargs else getattr(page, kind)(expr)
            items = []
            for r in results:
                root = r._root
                if isinstance(root, str) or not hasattr(root, "tag") or not isinstance(getattr(root, "tag", None), str):
                    items.append({"kind": "text", "value": r.get()})
                else:
                    items.append(describe(r))
            entry["results"] = items
        except Exception as e:  # noqa: BLE001
            entry["results"] = []
            entry["error"] = f"{type(e).__name__}"
        out.append(entry)
    return out


def main():
    docs = []
    pages = {
        "general": GENERAL_HTML,
        "complex": COMPLEX_HTML,
        "filter": FILTER_HTML,
    }
    for name, html in pages.items():
        page = Selector(html, adaptive=False)
        docs.append({
            "name": name,
            "html": html,
            "css": run_queries(page, "css", CSS_QUERIES.get(name, [])),
            "xpath": run_queries(page, "xpath", XPATH_QUERIES.get(name, [])),
        })

    # Appendix-A synthetic page: counts only, fixture stays small.
    items = "".join(
        f'<div class="product p{i%7}" data-id="{i}"><h3 class="title"><a href="/p/{i}">Product number {i} name</a></h3>'
        f'<span class="price">${i}.99</span><p class="desc">Description text for item {i} lorem ipsum</p></div>'
        for i in range(2000))
    page_html = f"<html><body><div id='list'>{items}</div></body></html>"
    page = Selector(page_html, adaptive=False)
    counts = {}
    for expr in [".product", ".price", ".price::text", "#list > div", "div.product.p3", "a"]:
        counts[expr] = len(page.css(expr))
    xcounts = {}
    for expr in ["//span[@class='price']", "//div[@id='list']/div[1501]", "//a[text()='Product number 42 name']"]:
        xcounts[expr] = len(page.xpath(expr))
    docs.append({
        "name": "appendix",
        "html": "",
        "css_counts": counts,
        "xpath_counts": xcounts,
        "css": [],
        "xpath": [],
    })

    out_path = os.path.join(os.path.dirname(__file__), "..", "fixtures", "stage1.json")
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump({"docs": docs}, f, ensure_ascii=False, indent=1)
    total = sum(len(d["css"]) + len(d["xpath"]) for d in docs)
    print(f"wrote {out_path} docs={len(docs)} queries={total}")


if __name__ == "__main__":
    main()
