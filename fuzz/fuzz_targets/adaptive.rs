//! Adaptive fingerprints, relocation and similarity on fuzzed documents:
//! save an element from one document and relocate it in a mutated copy.
#![no_main]
use libfuzzer_sys::fuzz_target;
use rustscrapling_adaptive::{element_to_dict, find_similar, relocate};
use rustscrapling_dom::libxml_backend::{parse_html, CompatOptions};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if text.len() > 64 * 1024 {
        return;
    }
    let Ok(dom) = parse_html(text, &CompatOptions::default()) else {
        return;
    };
    let Some(root) = dom.document_element() else {
        return;
    };
    let elements: Vec<u32> = dom
        .descendants_or_self(root)
        .into_iter()
        .filter(|&id| dom.node(id).is_element())
        .collect();
    let Some(&target) = elements.get(elements.len() / 2) else {
        return;
    };
    let saved = element_to_dict(&dom, target);
    let _ = find_similar(&dom, target, 0.2, &["href", "src"], true);
    let mutated = text.replace("class=", "data-c=").replace('a', "b");
    if let Ok(other) = parse_html(&mutated, &CompatOptions::default()) {
        if let Some(other_root) = other.document_element() {
            for percentage in [0, 40] {
                let _ = relocate(&other, other_root, &saved, percentage);
            }
        }
    }
});
