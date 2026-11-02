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

fn best(label: &str, mut f: impl FnMut()) {
    f();
    let mut b = f64::MAX;
    for _ in 0..5 {
        let s = Instant::now();
        f();
        b = b.min(s.elapsed().as_secs_f64() * 1000.0);
    }
    println!("{:<34} {:9.2} ms", label, b);
}

fn main() {
    use html5ever::tendril::TendrilSink;
    use html5ever::{parse_document, ParseOpts};
    use markup5ever_rcdom::RcDom;
    let page = appendix_page();
    best("html5ever->RcDom only", || {
        let d = parse_document(RcDom::default(), ParseOpts::default())
            .from_utf8()
            .read_from(&mut page.as_bytes())
            .unwrap();
        std::hint::black_box(&d.document);
    });
    best("full html5 backend (rcdom+convert)", || {
        let d = rustscrapling_dom::html5_backend::parse_html(
            &page,
            &rustscrapling_dom::html5_backend::Html5Options::default(),
        )
        .unwrap();
        std::hint::black_box(d.element_count());
    });
}
