//! CSS and XPath engines on fuzzed selectors and HTML: errors are fine,
//! panics and hangs are not. Input = selector, NUL, HTML.
#![no_main]
use libfuzzer_sys::fuzz_target;
use rustscrapling_dom::libxml_backend::{parse_html, CompatOptions};
use rustscrapling_select::{css::css, text_search, xpath::xpath};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let (selector, html) = text.split_once('\0').unwrap_or((text, "<p class=a>x<b>y</b></p><div id=d>z</div>"));
    if selector.len() > 512 {
        return;
    }
    let Ok(dom) = parse_html(html, &CompatOptions::default()) else {
        return;
    };
    let root = dom.root;
    let _ = css(&dom, root, selector);
    let _ = xpath(&dom, root, selector, &[]);
    let _ = text_search::find_by_text(
        &dom,
        root,
        selector,
        text_search::FindByTextOpts {
            first_match: false,
            partial: true,
            case_sensitive: false,
            clean_match: true,
        },
    );
});
