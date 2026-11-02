//! Stage 2 performance targets on the Plan appendix-A page
//! (2000 products, ~416 KB): relocate <= 50 ms, find_by_text <= 5 ms,
//! find_similar <= 3 ms. Mirrors oracle/bench_appendix_a.py.
use std::time::Instant;

use rustscrapling_adaptive::{element_to_dict, find_similar, relocate};
use rustscrapling_dom::libxml_backend::{parse_html, CompatOptions};
use rustscrapling_select::css::css;
use rustscrapling_select::text_search::{find_by_text, FindByTextOpts};
use rustscrapling_select::Selection;

fn appendix_page() -> String {
    let mut items = String::new();
    for i in 0..2000 {
        items.push_str(&format!(
            "<div class=\"product p{}\" data-id=\"{}\"><h3 class=\"title\"><a href=\"/p/{}\">Product number {} name</a></h3><span class=\"price\">${}.99</span><p class=\"desc\">Description text for item {} lorem ipsum</p></div>",
            i % 7, i, i, i, i, i
        ));
    }
    format!("<html><body><div id='list'>{items}</div></body></html>")
}

fn best_of(n: usize, mut f: impl FnMut()) -> f64 {
    f(); // warm-up
    let mut best = f64::MAX;
    for _ in 0..n {
        let t = Instant::now();
        f();
        best = best.min(t.elapsed().as_secs_f64() * 1000.0);
    }
    best
}

fn main() {
    let page = appendix_page();
    let dom = parse_html(&page, &CompatOptions::default()).unwrap();
    let root = dom.document_element().unwrap();
    let products = css(&dom, root, ".product").unwrap();
    let first = match products[0] {
        Selection::Element(id) => id,
        _ => unreachable!(),
    };
    let target1500 = match products[1500] {
        Selection::Element(id) => id,
        _ => unreachable!(),
    };
    let saved = element_to_dict(&dom, target1500);

    let changed = page
        .replace("class=\"product", "class=\"item")
        .replace("Product number", "Item no.");
    let dom_b = parse_html(&changed, &CompatOptions::default()).unwrap();
    let root_b = dom_b.document_element().unwrap();

    let ms = best_of(3, || {
        let found = relocate(&dom_b, root_b, &saved, 0);
        assert_eq!(found.len(), 1);
    });
    println!("relocate after layout change: {ms:.2} ms (target <= 50)");

    let ms = best_of(5, || {
        let found = find_by_text(
            &dom,
            root,
            "Product number 1500 name",
            FindByTextOpts::default(),
        );
        assert_eq!(found.len(), 1);
    });
    println!("find_by_text first match: {ms:.2} ms (target <= 5)");

    let ms = best_of(5, || {
        let found = find_similar(&dom, first, 0.2, &["href", "src"], false);
        assert_eq!(found.len(), 1999);
    });
    println!("find_similar (1999 siblings): {ms:.2} ms (target <= 3)");
}
