//! Appendix-A benchmark for RustScrapling stage 1 targets.
//! Same synthetic page as oracle/bench_appendix_a.py (2000 products).
//! Prints best-of-5 timings like the oracle script.

use rustscrapling_dom::html5_backend::{parse_html as parse_html5, Html5Options};
use rustscrapling_dom::libxml_backend::{parse_html as parse_libxml, CompatOptions};
use rustscrapling_select::css;
use std::time::Instant;

fn appendix_page() -> String {
    let mut items = String::new();
    for i in 0..2000 {
        items.push_str(&format!(
            "<div class=\"product p{}\" data-id=\"{}\"><h3 class=\"title\"><a href=\"/p/{}\">Product number {} name</a></h3><span class=\"price\">${}.99</span><p class=\"desc\">Description text for item {} lorem ipsum</p></div>",
            i % 7, i, i, i, i, i
        ));
    }
    format!("<html><body><div id='list'>{}</div></body></html>", items)
}

fn best_of_5(label: &str, mut f: impl FnMut()) {
    // Warm up once, then take the best of 5 like the oracle script.
    f();
    let mut best = f64::MAX;
    for _ in 0..5 {
        let start = Instant::now();
        f();
        best = best.min(start.elapsed().as_secs_f64() * 1000.0);
    }
    println!("{:<38} {:9.2} ms", label, best);
}

fn main() {
    let page = appendix_page();
    println!("page_bytes={}", page.len());

    best_of_5("html5ever parse", || {
        let _ = parse_html5(&page, &Html5Options::default()).unwrap();
    });
    best_of_5("libxml2 parse (compat)", || {
        let _ = parse_libxml(&page, &CompatOptions::default()).unwrap();
    });

    let dom5 = parse_html5(&page, &Html5Options::default()).unwrap();
    best_of_5("css('.price::text') on html5 dom", || {
        let r = css::css(&dom5, dom5.root, ".price::text").unwrap();
        assert_eq!(r.len(), 2000);
    });
    let domx = parse_libxml(&page, &CompatOptions::default()).unwrap();
    best_of_5("css('.price::text') on libxml dom", || {
        let r = css::css(&domx, domx.root, ".price::text").unwrap();
        assert_eq!(r.len(), 2000);
    });
    best_of_5("xpath //span[@class='price'] (html5)", || {
        let r = rustscrapling_select::xpath::xpath(&dom5, dom5.root, "//span[@class='price']", &[])
            .unwrap();
        assert_eq!(r.len(), 2000);
    });
    println!("targets: parse <= 15 ms, css <= 5 ms (Plan stage 1)");
}
