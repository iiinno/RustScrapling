use rustscrapling_dom::html5_backend::{parse_html, Html5Options};
use rustscrapling_extract::{extract, extract_dom, ExtractOptions};

#[test]
fn article_and_metadata_fixture() {
    let source = include_str!("fixtures/news.html");
    let options = ExtractOptions {
        page_url: Some("https://news.example/entry/123"),
        query: None,
    };
    let output = extract(source, options).unwrap();
    assert_eq!(
        output.metadata.title.as_deref(),
        Some("Satellite instruments discover an ocean current")
    );
    assert_eq!(output.metadata.author.as_deref(), Some("A. Researcher"));
    assert_eq!(
        output.metadata.published_at.as_deref(),
        Some("2026-10-30T12:30:00Z")
    );
    assert_eq!(
        output.metadata.modified_at.as_deref(),
        Some("2026-10-31T02:00:00Z")
    );
    assert_eq!(
        output.metadata.canonical_url.as_deref(),
        Some("https://news.example/reports/ocean-current")
    );
    assert_eq!(
        output
            .metadata
            .open_graph
            .get("og:image")
            .map(String::as_str),
        Some("https://news.example/cover.png")
    );
    assert_eq!(output.metadata.json_ld.len(), 1);
    for snippet in [
        "research team collected satellite",
        "current changes between seasons",
        "instrument calibration",
    ] {
        assert!(output.text.contains(snippet), "missing {snippet}");
    }
    for unwanted in [
        "Site title",
        "Unrelated article suggestions",
        "Share this",
        "Subscribe",
        "Copyright",
    ] {
        assert!(!output.text.contains(unwanted), "unexpected {unwanted}");
    }
    assert!(output.markdown.contains("# Satellite instruments"));
    assert!(output
        .markdown
        .contains("[full paper](https://news.example/paper)"));
    assert_eq!(
        output.headings,
        vec!["Satellite instruments discover an ocean current"]
    );
    assert_eq!(output.paragraphs.len(), 3);
    let dom = parse_html(source, &Html5Options::default()).unwrap();
    assert_eq!(output, extract_dom(&dom, options).unwrap());
    assert_eq!(
        serde_json::to_string(&output).unwrap(),
        serde_json::to_string(&extract(source, options).unwrap()).unwrap()
    );
}

#[test]
fn malicious_markup_is_inert_and_hidden_instructions_are_removed() {
    let output = extract(
        include_str!("fixtures/injection.html"),
        ExtractOptions {
            page_url: Some("https://safe.example/article"),
            query: None,
        },
    )
    .unwrap();
    for unwanted in [
        "Hidden prompt",
        "Hidden instruction",
        "Invisible instruction",
        "Screen reader-only",
        "Inert prompt",
        "Expanded payload",
        "Ignore previous instructions",
        "Comment instructing",
        "Ignore policy",
        "alert('script",
        "Footer",
        "sidebar instructions",
    ] {
        assert!(!output.text.contains(unwanted), "text leaked: {unwanted}");
        assert!(
            !output.markdown.contains(unwanted),
            "markdown leaked: {unwanted}"
        );
    }
    for unsafe_html in [
        "onclick=",
        "onmouseover=",
        "javascript:",
        "data:text/html",
        "<script",
        "<iframe",
        "<noscript",
    ] {
        assert!(
            !output.html.contains(unsafe_html),
            "unsafe markup leaked: {unsafe_html}"
        );
    }
    assert!(output.html.contains("href=\"https://safe.example/safe\""));
    assert!(
        output.text.contains("malicious link"),
        "visible anchor text must survive"
    );
    assert!(output.text.contains("ordinary source material"));
}

#[test]
fn jsonld_graph_and_technical_markdown_fixture() {
    let page = extract(
        include_str!("fixtures/technical.html"),
        ExtractOptions {
            page_url: Some("https://docs.example/docs/"),
            query: Some("search terms relevance"),
        },
    )
    .unwrap();
    assert_eq!(page.metadata.author.as_deref(), Some("Lin, Rin"));
    assert_eq!(page.metadata.title.as_deref(), Some("Query engine notes"));
    assert_eq!(page.metadata.published_at.as_deref(), Some("2026-11-01"));
    assert_eq!(page.metadata.modified_at.as_deref(), Some("2026-11-02"));
    assert_eq!(
        page.metadata.canonical_url.as_deref(),
        Some("https://docs.example/docs/query")
    );
    assert!(
        page.markdown.contains("```"),
        "code fences should survive: {}",
        page.markdown
    );
    assert!(
        page.markdown.contains("| Rust"),
        "table should survive: {}",
        page.markdown
    );
    assert_eq!(page.paragraphs[0].index, 0);
    assert!(page.paragraphs[0].score > page.paragraphs[1].score);
}
