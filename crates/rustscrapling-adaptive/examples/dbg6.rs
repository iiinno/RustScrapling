use std::time::Instant;

use rayon::prelude::*;
use rustscrapling_adaptive::{calculate_similarity_score, element_to_dict, relocate};
use rustscrapling_dom::libxml_backend::{parse_html, CompatOptions};
use rustscrapling_select::css::css;
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

fn main() {
    let page = appendix_page();
    let dom = parse_html(&page, &CompatOptions::default()).unwrap();
    let root = dom.document_element().unwrap();
    let products = css(&dom, root, ".product").unwrap();
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
    let candidates: Vec<_> = dom_b
        .descendants_or_self(root_b)
        .into_iter()
        .skip(1)
        .filter(|&id| dom_b.node(id).is_element())
        .collect();

    let t = Instant::now();
    for &c in &candidates {
        std::hint::black_box(element_to_dict(&dom_b, c));
    }
    println!("dict all seq: {:?}", t.elapsed());

    let t = Instant::now();
    let mut acc = 0.0;
    for &c in &candidates {
        acc += calculate_similarity_score(&dom_b, &saved, c);
    }
    println!("score all seq: {:?} (acc {acc})", t.elapsed());

    let t = Instant::now();
    let scores: Vec<f64> = candidates
        .par_iter()
        .map(|&c| calculate_similarity_score(&dom_b, &saved, c))
        .collect();
    println!("score all par: {:?} (n={})", t.elapsed(), scores.len());

    let t = Instant::now();
    let found = relocate(&dom_b, root_b, &saved, 0);
    println!("relocate(): {:?} found={}", t.elapsed(), found.len());
    println!("rayon threads: {}", rayon::current_num_threads());
}
