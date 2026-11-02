//! Extract readable article content, metadata and relevant paragraphs from HTML.
//!
//! The DOM and HTML sanitization are shared by the Markdown and plain-text
//! outputs. Inputs from the network remain *untrusted source material*: HTML
//! filtering can remove invisible instructions and active content, but cannot
//! determine whether visible article prose is an instruction to an LLM.

mod metadata;
mod rank;
mod sanitize;

pub use metadata::Metadata;
pub use rank::{rank_paragraphs, RankedParagraph};

use rustscrapling_dom::html5_backend::{parse_html, Html5Options};
use rustscrapling_dom::{Dom, NodeId, NodeKind};
use serde::Serialize;

#[derive(Clone, Copy, Debug, Default)]
pub struct ExtractOptions<'a> {
    /// Original page URL, used to resolve canonical links and body hyperlinks.
    pub page_url: Option<&'a str>,
    /// Optional search query. If set, paragraphs are ordered by BM25 score.
    pub query: Option<&'a str>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ExtractedPage {
    pub metadata: Metadata,
    /// Article HTML after active elements, hidden content and unsafe URLs are removed.
    pub html: String,
    pub markdown: String,
    pub text: String,
    pub headings: Vec<String>,
    /// In document order without a query, decreasing relevance with one.
    pub paragraphs: Vec<RankedParagraph>,
}

/// Extract from raw HTML using the default HTML5 parser.
pub fn extract(html: &str, options: ExtractOptions<'_>) -> Result<ExtractedPage, String> {
    let dom = parse_html(html, &Html5Options::default())?;
    extract_dom(&dom, options)
}

/// Extract from a previously parsed DOM, without changing the arena.
pub fn extract_dom(dom: &Dom, options: ExtractOptions<'_>) -> Result<ExtractedPage, String> {
    let metadata = metadata::extract_metadata(dom, options.page_url);
    let root = best_content_node(dom).unwrap_or(dom.root);
    let html = sanitize::sanitized_html(dom, root, options.page_url);
    let markdown = htmd::convert(&html).map_err(|err| format!("Markdown conversion: {err}"))?;
    let text = sanitize::plain_text(dom, root);
    let headings = dom
        .descendants_or_self(root)
        .into_iter()
        .filter(|&id| {
            matches!(
                dom.node(id).tag.as_str(),
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
            ) && !sanitize::has_excluded_ancestor(dom, id, root)
        })
        .map(|id| sanitize::normalize(&sanitize::visible_text(dom, id)))
        .filter(|s| !s.is_empty())
        .collect();
    let paragraphs = rank::rank_paragraphs(
        &collect_paragraphs(dom, root),
        options.query.unwrap_or_default(),
    );
    Ok(ExtractedPage {
        metadata,
        html,
        markdown,
        text,
        headings,
        paragraphs,
    })
}

/// Convert the main article content to Markdown, with the same exclusions as
/// [`extract`]. Unlike converting a whole page, this omits page chrome.
pub fn markdown(html: &str) -> Result<String, String> {
    Ok(extract(html, ExtractOptions::default())?.markdown)
}

fn best_content_node(dom: &Dom) -> Option<NodeId> {
    // Compute text, hyperlink and paragraph statistics once. Rewalking every
    // candidate would grow quadratically on large category/listing pages.
    #[derive(Clone, Copy, Default)]
    struct Stats {
        chars: usize,
        link_chars: usize,
        paragraphs: usize,
    }
    let order = dom.descendants_or_self(dom.root);
    let mut stats = vec![Stats::default(); dom.nodes.len()];
    for &id in order.iter().rev() {
        if sanitize::excluded(dom, id) {
            continue;
        }
        let node = dom.node(id);
        let mut current = Stats::default();
        if node.kind == NodeKind::Text {
            current.chars = node.data.chars().filter(|c| !c.is_whitespace()).count();
        }
        for &child in dom.children(id) {
            current.chars += stats[child as usize].chars;
            current.link_chars += stats[child as usize].link_chars;
            current.paragraphs += stats[child as usize].paragraphs;
        }
        if node.tag == "a" {
            current.link_chars = current.chars;
        }
        if node.tag == "p" && current.chars >= 35 {
            current.paragraphs += 1;
        }
        stats[id as usize] = current;
    }
    let mut best: Option<(NodeId, f64)> = None;
    for id in order {
        let node = dom.node(id);
        if !node.is_element() || sanitize::has_excluded_ancestor(dom, id, dom.root) {
            continue;
        }
        if !matches!(
            node.tag.as_str(),
            "article" | "main" | "section" | "div" | "body"
        ) {
            continue;
        }
        let metrics = stats[id as usize];
        let len = metrics.chars;
        if len < 35 {
            continue;
        }
        let density = metrics.link_chars as f64 / len.max(1) as f64;
        let identifiers = format!(
            "{} {}",
            dom.attr(id, "id").unwrap_or_default(),
            dom.attr(id, "class").unwrap_or_default()
        );
        let named_content = identifiers
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|token| {
                matches!(
                    token.to_ascii_lowercase().as_str(),
                    "article" | "content" | "entry" | "story" | "post" | "main" | "body"
                )
            });
        let structural_bonus = match node.tag.as_str() {
            "article" => 440.0,
            "main" => 350.0,
            "body" => -450.0,
            "section" => 0.0,
            _ => 0.0,
        };
        let role_bonus = if dom.attr(id, "role") == Some("main") {
            300.0
        } else {
            0.0
        };
        let score = len as f64 * (1.0 - density.min(1.0) * 1.6)
            + metrics.paragraphs as f64 * 85.0
            + structural_bonus
            + role_bonus
            + if named_content { 260.0 } else { 0.0 };
        if best.is_none_or(|(_, prev)| score > prev) {
            best = Some((id, score));
        }
    }
    best.map(|(id, _)| id).or_else(|| {
        dom.descendants_or_self(dom.root)
            .into_iter()
            .find(|&id| dom.node(id).tag == "body")
    })
}

fn collect_paragraphs(dom: &Dom, root: NodeId) -> Vec<String> {
    let paragraphs: Vec<_> = dom
        .descendants_or_self(root)
        .into_iter()
        .filter(|&id| {
            matches!(dom.node(id).tag.as_str(), "p" | "li" | "blockquote")
                && !sanitize::has_excluded_ancestor(dom, id, root)
                // Don't duplicate nested list items and quoted paragraphs.
                && !dom.ancestors(id).iter().take_while(|&&a| a != root)
                    .any(|&a| matches!(dom.node(a).tag.as_str(), "p" | "li" | "blockquote"))
        })
        .map(|id| sanitize::normalize(&sanitize::visible_text(dom, id)))
        .filter(|value| !value.is_empty())
        .collect();
    if !paragraphs.is_empty() {
        return paragraphs;
    }
    let text = sanitize::plain_text(dom, root);
    text.split('\n')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
