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

#[test]
fn credential_bearing_urls_are_not_emitted_in_article_or_canonical_metadata() {
    let output = extract(
        "<html><head><link rel='canonical' href='https://user:secret@news.example/article'></head>\
         <body><main><article><h1>Research on authenticated URL safety</h1>\
         <p>The visible article text is retained in safe output formats even if hyperlinks are unsafe.</p>\
         <a href='https://alice:token@example.org/private'>read more</a>\
         <img src='https://bob:pass@example.org/pixel.png' alt='image'>\
         </article></main></body></html>",
        ExtractOptions {
            page_url: Some("https://news.example/source"),
            query: None,
        },
    )
    .unwrap();
    assert_eq!(
        output.metadata.canonical_url.as_deref(),
        Some("https://news.example/source")
    );
    assert!(output.text.contains("visible article text is retained"));
    assert!(output.text.contains("read more"));
    for content in [&output.html, &output.markdown] {
        assert!(!content.contains("secret@"));
        assert!(!content.contains("alice:token"));
        assert!(!content.contains("bob:pass"));
    }
    // A credential-bearing base URL cannot be used to turn ordinary links into
    // credential-bearing absolute URLs during sanitization.
    let with_bad_base = extract(
        "<html><body><article><h1>Long enough article heading</h1><p>Some ordinary visible paragraph content for the article extraction.</p><a href='/next'>Next</a></article></body></html>",
        ExtractOptions {
            page_url: Some("https://name:password@news.example/source"),
            query: None,
        },
    )
    .unwrap();
    assert!(!with_bad_base.html.contains("password@"));
    assert_eq!(with_bad_base.metadata.canonical_url, None);
}

#[test]
fn schema_graph_resolves_author_references_and_full_type_urls() {
    let output = extract(
        r##"<html><head><script type="application/ld+json">
        {
          "@context":"https://schema.org",
          "@graph":[
            {
              "@id":"https://example.org/stories/1#article",
              "@type":["https://schema.org/NewsArticle","CreativeWork"],
              "headline":"Graph article headline",
              "datePublished":"2026-11-03",
              "author":[
                {"@id":"https://example.org/stories/1#ada"},
                {"name":"Bob Author"}
              ]
            },
            {
              "@id":"https://example.org/stories/1#ada",
              "@type":"Person",
              "name":"Ada Researcher"
            }
          ]
        }
        </script></head>
        <body><main><p>The report contains enough readable text to be extracted.</p></main></body></html>"##,
        ExtractOptions::default(),
    )
    .unwrap();
    assert_eq!(
        output.metadata.title.as_deref(),
        Some("Graph article headline")
    );
    assert_eq!(
        output.metadata.author.as_deref(),
        Some("Ada Researcher, Bob Author")
    );
    assert_eq!(output.metadata.published_at.as_deref(), Some("2026-11-03"));
    assert!(!output.metadata.author.unwrap().contains("https://"));
}

#[test]
fn jsonld_author_cycle_or_unresolved_reference_does_not_leak_ids() {
    let output = extract(
        r##"<html><head><script type="application/ld+json">
        {"@graph":[
          {"@type":"Article", "headline":"Cycle article", "author":[
            {"@id":"https://example.org/#cycle-a"},
            {"@id":"https://example.org/#unresolved"}
          ]},
          {"@id":"https://example.org/#cycle-a", "@id2":"noise", "name2":"not a name",
           "@type":"Person", "other":"plain", "name":""}
        ]}
        </script></head><body><main><p>This article provides an ordinary visible paragraph.</p></main></body></html>"##,
        ExtractOptions::default(),
    )
    .unwrap();
    assert_eq!(output.metadata.title.as_deref(), Some("Cycle article"));
    assert_eq!(output.metadata.author, None);
}
