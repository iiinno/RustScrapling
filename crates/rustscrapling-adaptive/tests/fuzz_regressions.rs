//! Inputs found by `fuzz/` (Plan G-09). `relocate` once deadlocked on the
//! sibling-ratio cache whenever a cache miss happened: the lookup guard was
//! still held by the `match` while the miss path re-locked the same mutex.
use std::sync::mpsc;
use std::time::Duration;

use rustscrapling_adaptive::{element_to_dict, find_similar, relocate};
use rustscrapling_dom::libxml_backend::{parse_html, CompatOptions};

#[test]
fn relocate_on_fuzzed_shadow_page_completes() {
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        let text = include_str!("fuzz_relocate_deadlock.html");
        let dom = parse_html(text, &CompatOptions::default()).unwrap();
        let root = dom.document_element().unwrap();
        let elements: Vec<u32> = dom
            .descendants_or_self(root)
            .into_iter()
            .filter(|&id| dom.node(id).is_element())
            .collect();
        let target = elements[elements.len() / 2];
        let saved = element_to_dict(&dom, target);
        let _ = find_similar(&dom, target, 0.2, &["href", "src"], true);
        let mutated = text.replace("class=", "data-c=").replace('a', "b");
        let other = parse_html(&mutated, &CompatOptions::default()).unwrap();
        let other_root = other.document_element().unwrap();
        for percentage in [0, 40] {
            let _ = relocate(&other, other_root, &saved, percentage);
        }
        done.send(()).unwrap();
    });
    finished
        .recv_timeout(Duration::from_secs(20))
        .expect("relocate must not deadlock");
}
