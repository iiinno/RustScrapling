//! Markdown/HTML/text rendering (upstream Convertor port with markdownify,
//! and the Rust article extractor) never panics on hostile HTML.
#![no_main]
use libfuzzer_sys::fuzz_target;
use rustscrapling_extract::compat::{convert_content, ContentFormat};
use rustscrapling_extract::{extract, ExtractOptions};

fuzz_target!(|data: &[u8]| {
    let Ok(html) = std::str::from_utf8(data) else {
        return;
    };
    if html.len() > 64 * 1024 {
        return;
    }
    for format in [ContentFormat::Markdown, ContentFormat::Html, ContentFormat::Text] {
        for main in [true, false] {
            let _ = convert_content(html, format, None, main);
            let _ = convert_content(html, format, Some("p, li, a::text"), main);
        }
    }
    let _ = extract(
        html,
        ExtractOptions {
            page_url: Some("https://example.com/a"),
            query: Some("example"),
        },
    );
});
