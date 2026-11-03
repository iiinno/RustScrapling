//! Inputs found by `fuzz/` (Plan G-09) that once crashed an engine. Each
//! must now return a result or a typed error without panicking.
use rustscrapling_dom::libxml_backend::{parse_html, CompatOptions};
use rustscrapling_select::{css::css, xpath::xpath};

#[test]
fn trailing_backslash_in_css_identifier_is_not_a_panic() {
    let dom = parse_html("<p class=a>x</p>", &CompatOptions::default()).unwrap();
    for selector in [".bb\\", "p.a\\", "#\\", "a[b=c\\"] {
        let _ = css(&dom, dom.root, selector);
        let _ = xpath(&dom, dom.root, selector, &[]);
    }
}
