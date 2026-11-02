//! Safe rendering into HTML/text. Attribute and URL allow-lists prevent the
//! original HTML from entering a second parser with active markup intact.

use rustscrapling_dom::{Dom, NodeId, NodeKind};
use url::Url;

const BLOCK_TAGS: &[&str] = &[
    "address",
    "article",
    "blockquote",
    "br",
    "dd",
    "div",
    "dl",
    "dt",
    "figcaption",
    "figure",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "hr",
    "li",
    "main",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "td",
    "th",
    "tr",
    "ul",
];

pub(crate) fn excluded(dom: &Dom, id: NodeId) -> bool {
    let node = dom.node(id);
    if node.kind != NodeKind::Element {
        return false;
    }
    if matches!(
        node.tag.as_str(),
        "head"
            | "script"
            | "style"
            | "noscript"
            | "template"
            | "svg"
            | "canvas"
            | "iframe"
            | "object"
            | "embed"
            | "form"
            | "input"
            | "textarea"
            | "select"
            | "option"
            | "button"
            | "dialog"
            | "nav"
            | "footer"
            | "aside"
            | "header"
            | "audio"
            | "video"
            | "source"
            | "track"
            | "meta"
            | "link"
    ) {
        return true;
    }
    if dom.attr(id, "hidden").is_some() || dom.attr(id, "inert").is_some() {
        return true;
    }
    // Closed <details> bodies are not visible to the reader. Dropping them
    // also avoids carrying instructions hidden behind collapsed UI.
    if node.tag == "details" && dom.attr(id, "open").is_none() {
        return true;
    }
    if dom
        .attr(id, "aria-hidden")
        .is_some_and(|v| v.eq_ignore_ascii_case("true"))
    {
        return true;
    }
    if dom.attr(id, "role").is_some_and(|role| {
        matches!(
            role.to_ascii_lowercase().as_str(),
            "navigation"
                | "banner"
                | "contentinfo"
                | "complementary"
                | "search"
                | "dialog"
                | "alertdialog"
        )
    }) {
        return true;
    }
    if let Some(style) = dom.attr(id, "style") {
        let declarations: Vec<String> = style
            .to_ascii_lowercase()
            .split(';')
            .map(|d| d.chars().filter(|c| !c.is_whitespace()).collect::<String>())
            .map(|d| d.trim_end_matches("!important").to_owned())
            .collect();
        if declarations.iter().any(|s| {
            matches!(
                s.as_str(),
                "display:none"
                    | "visibility:hidden"
                    | "content-visibility:hidden"
                    | "opacity:0"
                    | "font-size:0"
            )
        }) {
            return true;
        }
        if declarations
            .iter()
            .any(|s| s == "position:absolute" || s == "position:fixed")
            && declarations.iter().any(|s| {
                s.starts_with("left:-9999")
                    || s.starts_with("top:-9999")
                    || s.starts_with("clip:rect(0")
            })
        {
            return true;
        }
    }
    [dom.attr(id, "id"), dom.attr(id, "class")]
        .into_iter()
        .flatten()
        .flat_map(|value| value.split(|c: char| !c.is_ascii_alphanumeric()))
        .any(|token| {
            matches!(
                token.to_ascii_lowercase().as_str(),
                "advertisement"
                    | "advertising"
                    | "ads"
                    | "adslot"
                    | "sidebar"
                    | "cookie"
                    | "newsletter"
                    | "subscribe"
                    | "subscription"
                    | "paywall"
                    | "popover"
                    | "modal"
                    | "social"
                    | "sharing"
                    | "sharebar"
                    | "comments"
                    | "comment"
                    | "recommendations"
                    | "related"
                    | "promoted"
                    | "sponsored"
                    | "breadcrumb"
                    | "breadcrumbs"
                    | "pagination"
                    | "toolbar"
                    | "navigation"
                    | "hidden"
                    | "visually"
                    | "sr"
            )
        })
}

pub(crate) fn has_excluded_ancestor(dom: &Dom, mut id: NodeId, root: NodeId) -> bool {
    loop {
        if excluded(dom, id) {
            return true;
        }
        if id == root {
            return false;
        }
        match dom.parent(id) {
            Some(parent) => id = parent,
            None => return false,
        }
    }
}

pub(crate) fn visible_text(dom: &Dom, id: NodeId) -> String {
    let mut result = String::new();
    fn walk(dom: &Dom, id: NodeId, out: &mut String) {
        if excluded(dom, id) {
            return;
        }
        let node = dom.node(id);
        if node.kind == NodeKind::Text {
            out.push_str(&node.data);
        }
        if node.tag == "br" {
            out.push(' ');
        }
        for &child in dom.children(id) {
            walk(dom, child, out);
        }
        if BLOCK_TAGS.contains(&node.tag.as_str()) {
            out.push(' ');
        }
    }
    walk(dom, id, &mut result);
    normalize(&result)
}

pub(crate) fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn plain_text(dom: &Dom, id: NodeId) -> String {
    let mut result = String::new();
    fn walk(dom: &Dom, id: NodeId, out: &mut String) {
        if excluded(dom, id) {
            return;
        }
        let node = dom.node(id);
        if node.kind == NodeKind::Text {
            out.push_str(&node.data);
        }
        if node.tag == "br" {
            out.push('\n');
        }
        for &child in dom.children(id) {
            walk(dom, child, out);
        }
        if BLOCK_TAGS.contains(&node.tag.as_str()) {
            out.push('\n');
        }
    }
    walk(dom, id, &mut result);
    let mut lines = Vec::new();
    for line in result.lines() {
        let line = normalize(line);
        if !line.is_empty() {
            lines.push(line);
        }
    }
    lines.join("\n")
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Accept only http(s) URLs, fragments and ordinary relative references.
/// Reject active schemes and malformed control-byte URLs even when the
/// original page supplied them as attributes or entity-escaped values.
pub(crate) fn safe_url(raw: &str, page_url: Option<&str>) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.chars().any(|c| c.is_control()) {
        return None;
    }
    if let Ok(parsed) = Url::parse(raw) {
        return matches!(parsed.scheme(), "http" | "https").then(|| parsed.to_string());
    }
    // A colon preceding a slash/query/fragment denotes an unsupported scheme.
    let first_part = raw.split(['/', '?', '#']).next().unwrap_or_default();
    if first_part.contains(':') || raw.starts_with('\\') {
        return None;
    }
    if let Some(base) = page_url
        .and_then(|url| Url::parse(url).ok())
        .filter(|b| matches!(b.scheme(), "http" | "https"))
    {
        return base
            .join(raw)
            .ok()
            .filter(|u| matches!(u.scheme(), "http" | "https"))
            .map(|u| u.to_string());
    }
    Some(raw.to_owned())
}

pub(crate) fn sanitized_html(dom: &Dom, id: NodeId, page_url: Option<&str>) -> String {
    let mut output = String::new();
    fn write(dom: &Dom, id: NodeId, page_url: Option<&str>, out: &mut String) {
        if excluded(dom, id) {
            return;
        }
        let node = dom.node(id);
        match node.kind {
            NodeKind::Text => {
                out.push_str(&escape_html(&node.data));
                return;
            }
            NodeKind::Element => {}
            _ => return,
        }
        // Limit renderable tags to HTML prose/formatting so a malformed DOM
        // cannot smuggle SVG/XML, foreign content, or custom active elements.
        let tag = node.tag.as_str();
        let allowed = matches!(
            tag,
            "a" | "abbr"
                | "article"
                | "b"
                | "blockquote"
                | "br"
                | "caption"
                | "code"
                | "dd"
                | "del"
                | "details"
                | "div"
                | "dl"
                | "dt"
                | "em"
                | "figcaption"
                | "figure"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "hr"
                | "i"
                | "img"
                | "ins"
                | "kbd"
                | "li"
                | "main"
                | "mark"
                | "ol"
                | "p"
                | "pre"
                | "q"
                | "s"
                | "samp"
                | "section"
                | "small"
                | "span"
                | "strong"
                | "sub"
                | "summary"
                | "sup"
                | "table"
                | "tbody"
                | "td"
                | "tfoot"
                | "th"
                | "thead"
                | "time"
                | "tr"
                | "u"
                | "ul"
                | "var"
        );
        if tag == "img"
            && ["width", "height"].iter().any(|k| {
                dom.attr(id, k)
                    .is_some_and(|v| v.trim() == "0" || v.trim() == "1")
            })
        {
            return;
        }
        if allowed {
            out.push('<');
            out.push_str(tag);
            for (name, value) in &node.attrs {
                let safe_value = match (tag, name.as_str()) {
                    ("a", "href") | ("img", "src") => safe_url(value, page_url),
                    ("img", "alt") | (_, "title") | (_, "lang") => Some(value.clone()),
                    _ => None,
                };
                if let Some(value) = safe_value {
                    out.push(' ');
                    out.push_str(name);
                    out.push_str("=\"");
                    out.push_str(&escape_html(&value));
                    out.push('"');
                }
            }
            out.push('>');
        }
        if !matches!(tag, "br" | "hr" | "img") {
            for &child in dom.children(id) {
                write(dom, child, page_url, out);
            }
            if allowed {
                out.push_str("</");
                out.push_str(tag);
                out.push('>');
            }
        }
    }
    write(dom, id, page_url, &mut output);
    output
}
