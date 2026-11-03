//! Both HTML parsers and the DOM accessors must accept any input without
//! panicking, and keep the arena consistent (every child points back).
#![no_main]
use libfuzzer_sys::fuzz_target;
use rustscrapling_dom::{html5_backend, libxml_backend};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    for dom in [
        html5_backend::parse_html(text, &html5_backend::Html5Options::default()),
        libxml_backend::parse_html(text, &libxml_backend::CompatOptions::default()),
    ]
    .into_iter()
    .flatten()
    {
        for (index, node) in dom.nodes.iter().enumerate() {
            for &child in &node.children {
                assert_eq!(dom.node(child).parent, Some(index as u32));
            }
        }
        if let Some(root) = dom.document_element() {
            let _ = dom.text_content(root);
            for id in dom.descendants_or_self(root).into_iter().take(256) {
                let _ = dom.element_path(id);
                let _ = dom.tail(id);
            }
        }
    }
});
