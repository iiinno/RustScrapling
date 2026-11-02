//! Criterion measurements of Plan.md Appendix A, 2000 products.
//! DOM parsing and fixture construction occur outside the timed loop.

use std::hint::black_box;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion};
use rustscrapling_adaptive::{element_to_dict, find_similar, relocate};
use rustscrapling_dom::libxml_backend::{parse_html, CompatOptions};
use rustscrapling_select::css::css;
use rustscrapling_select::text_search::{find_by_text, FindByTextOpts};
use rustscrapling_select::Selection;

fn appendix_html() -> String {
    let mut products = String::new();
    for i in 0..2000 {
        products.push_str(&format!(
            "<div class=\"product p{}\" data-id=\"{}\"><h3 class=\"title\"><a href=\"/p/{}\">Product number {} name</a></h3><span class=\"price\">\x24{}.99</span><p class=\"desc\">Description text for item {} lorem ipsum</p></div>",
            i % 7, i, i, i, i, i,
        ));
    }
    format!("<html><body><div id='list'>{products}</div></body></html>")
}

fn bench_appendix(c: &mut Criterion) {
    let html = appendix_html();
    let dom = parse_html(&html, &CompatOptions::default()).unwrap();
    let root = dom.document_element().unwrap();
    let products = css(&dom, root, ".product").unwrap();
    let nodes: Vec<_> = products
        .into_iter()
        .map(|item| match item {
            Selection::Element(node) => node,
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(nodes.len(), 2000);
    let original = element_to_dict(&dom, nodes[1500]);

    let changed = html
        .replace("class=\"product", "class=\"item")
        .replace("Product number", "Item no.");
    let changed_dom = parse_html(&changed, &CompatOptions::default()).unwrap();
    let changed_root = changed_dom.document_element().unwrap();

    let mut group = c.benchmark_group("plan_appendix_a");
    group.sample_size(30);
    group.measurement_time(Duration::from_secs(8));
    group.bench_function("relocate_2000", |b| {
        b.iter(|| {
            black_box(relocate(
                black_box(&changed_dom),
                black_box(changed_root),
                black_box(&original),
                0,
            ))
        })
    });
    group.bench_function("find_by_text_1500", |b| {
        b.iter(|| {
            black_box(find_by_text(
                black_box(&dom),
                black_box(root),
                black_box("Product number 1500 name"),
                FindByTextOpts::default(),
            ))
        })
    });
    group.bench_function("find_similar_1999", |b| {
        b.iter(|| {
            black_box(find_similar(
                black_box(&dom),
                black_box(nodes[0]),
                0.2,
                &["href", "src"],
                false,
            ))
        })
    });
    group.finish();
}

criterion_group!(benches, bench_appendix);
criterion_main!(benches);
