//! Upstream-compatible content conversion for the pinned Scrapling v0.5
//! `Convertor._extract_content` contract (used by its MCP fetch tools and
//! `extract` CLI commands).
//!
//! With `main_content_only`, upstream does **not** extract an article: it
//! keeps `<body>`, drops `script`/`style`/`noscript`/`svg`, drops elements
//! hidden by `aria-hidden="true"`, `<template>`, `<slot hidden>` or inline
//! `display:none`/`visibility:hidden`/zero size or opacity, and removes
//! zero-width and control characters from text. `css_selector` then selects
//! inside that sanitised tree and every match becomes one output chunk.
//!
//! The document is parsed with the libxml2 compat backend (the same parser
//! and options as Scrapling's lxml `Selector`); Markdown uses the ported
//! `markdownify` converter (`crate::markdownify`).

use std::collections::HashSet;

use regex::Regex;
use rustscrapling_dom::libxml_backend::{parse_html, CompatOptions};
use rustscrapling_dom::{Dom, NodeId, NodeKind};
use rustscrapling_select::{css::css, Selection};

/// Output format of [`convert_content`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentFormat {
    Markdown,
    Html,
    Text,
}

impl ContentFormat {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "markdown" => Some(Self::Markdown),
            "html" => Some(Self::Html),
            "text" => Some(Self::Text),
            _ => None,
        }
    }
}

const NOISE_TAGS: [&str; 4] = ["script", "style", "noscript", "svg"];
const TEXT_IGNORED_TAGS: [&str; 5] = ["script", "style", "noscript", "svg", "iframe"];
const VOID_TAGS: [&str; 18] = [
    "area", "base", "basefont", "br", "col", "embed", "frame", "hr", "img", "input", "isindex",
    "keygen", "link", "meta", "param", "source", "track", "wbr",
];
/// libxml2 `htmlIsBooleanAttr`: always serialized without a value.
const BOOLEAN_ATTRS: [&str; 13] = [
    "checked", "compact", "declare", "defer", "disabled", "ismap", "multiple", "nohref",
    "noresize", "noshade", "nowrap", "readonly", "selected",
];

/// Convert `html` like upstream `Convertor._extract_content`, returning one
/// chunk per selected element (without upstream's trailing empty chunk).
pub fn convert_content(
    html: &str,
    format: ContentFormat,
    css_selector: Option<&str>,
    main_content_only: bool,
) -> Result<Vec<String>, String> {
    let trimmed = html.trim();
    let dom = parse_html(
        if trimmed.is_empty() {
            "<html/>"
        } else {
            trimmed
        },
        &CompatOptions::default(),
    )?;
    let Some(mut root) = dom.document_element() else {
        return Ok(Vec::new());
    };
    let mut dropped = HashSet::new();
    if main_content_only {
        if let Some(body) = first_descendant(&dom, root, "body") {
            root = body;
        }
        dropped = hidden_or_noise(&dom, root);
    }
    let renderer = Renderer {
        dom: &dom,
        root,
        dropped: &dropped,
        clean_text: main_content_only,
    };
    let mut targets = Vec::new();
    let text_base = css_selector.and_then(|selector| {
        let parts: Vec<&str> = selector.split(',').map(str::trim).collect();
        parts.iter().all(|part| part.ends_with("::text")).then(|| {
            parts
                .iter()
                .map(|part| part.trim_end_matches("::text"))
                .collect::<Vec<_>>()
                .join(", ")
        })
    });
    match (css_selector, text_base) {
        (None, _) => targets.push(Target::Node(root)),
        // `::text` yields the direct text nodes of each selected element, as
        // they stand after hidden and noise elements were dropped.
        (Some(_), Some(base)) if main_content_only => {
            for selection in css(&dom, root, &base).map_err(|error| format!("{error:?}"))? {
                if let Selection::Element(id) = selection {
                    if !renderer.is_dropped(id) {
                        targets
                            .extend(renderer.text_runs(id, |_| {}).into_iter().map(Target::Text));
                    }
                }
            }
        }
        (Some(selector), _) => {
            for selection in css(&dom, root, selector).map_err(|error| format!("{error:?}"))? {
                match selection {
                    Selection::Element(id) if !renderer.is_dropped(id) => {
                        targets.push(Target::Node(id))
                    }
                    Selection::Element(_) => {}
                    Selection::Text(text) => targets.push(Target::Text(text)),
                }
            }
        }
    }
    targets
        .into_iter()
        .map(|target| match target {
            Target::Text(text) => match format {
                // markdownify re-parses its input, so markup inside a text
                // result (e.g. "a <b>") becomes elements again.
                ContentFormat::Markdown if text.contains('<') => {
                    let fragment = parse_html(
                        &format!("<html><body>{text}</body></html>"),
                        &CompatOptions::default(),
                    )?;
                    let empty = HashSet::new();
                    let body = fragment
                        .document_element()
                        .and_then(|html| first_descendant(&fragment, html, "body"));
                    Ok(match body {
                        Some(body) => crate::markdownify::Converter {
                            dom: &fragment,
                            dropped: &empty,
                            clean: &str::to_owned,
                            fragment_root: std::cell::Cell::new(None),
                        }
                        .convert_children(body),
                        None => renderer.markdownify().convert_text(&text),
                    })
                }
                ContentFormat::Markdown => Ok(renderer.markdownify().convert_text(&text)),
                ContentFormat::Html | ContentFormat::Text => Ok(text),
            },
            Target::Node(id) => match format {
                ContentFormat::Html => Ok(renderer.html(id)),
                ContentFormat::Markdown => Ok(renderer.markdownify().convert_root(id)),
                ContentFormat::Text => Ok(renderer.text(id)),
            },
        })
        .collect()
}

/// Serialize elements of an already parsed `dom` (outer HTML, Markdown or
/// text) without upstream's main-content sanitizing; used by the interactive
/// shell to show selection results the way `Selector` would print them.
pub fn render_elements(dom: &Dom, ids: &[NodeId], format: ContentFormat) -> Vec<String> {
    let dropped = HashSet::new();
    ids.iter()
        .map(|&id| {
            let renderer = Renderer {
                dom,
                root: id,
                dropped: &dropped,
                clean_text: false,
            };
            match format {
                ContentFormat::Html => renderer.html(id),
                ContentFormat::Markdown => renderer.markdownify().convert_root(id),
                ContentFormat::Text => renderer.text(id),
            }
        })
        .collect()
}

enum Target {
    Node(NodeId),
    Text(String),
}

fn first_descendant(dom: &Dom, root: NodeId, tag: &str) -> Option<NodeId> {
    dom.descendants_or_self(root)
        .into_iter()
        .find(|&id| id != root && dom.node(id).is_element() && dom.node(id).tag == tag)
}

/// Subtree roots removed by upstream `_strip_noise_tags` (including `root`
/// itself) and `_sanitize_for_ai` (descendants only, like `.//`).
fn hidden_or_noise(dom: &Dom, root: NodeId) -> HashSet<NodeId> {
    let mut dropped = HashSet::new();
    for id in dom.descendants_or_self(root) {
        let node = dom.node(id);
        if !node.is_element() {
            continue;
        }
        // Beyond upstream (D-013): elements with the HTML `hidden` attribute
        // are not rendered either, so their text (e.g. hidden prompt
        // injection) is dropped too. Upstream only drops hidden <slot>s.
        let hidden_attribute = id != root && dom.attr(id, "hidden").is_some();
        if NOISE_TAGS.contains(&node.tag.as_str())
            || (id != root && is_hidden_element(dom, id))
            || hidden_attribute
        {
            dropped.insert(id);
        }
    }
    dropped
}

/// Port of upstream `_is_hidden_element` for `_HIDDEN_XPATH` candidates.
pub fn is_hidden_element(dom: &Dom, id: NodeId) -> bool {
    let node = dom.node(id);
    if node.tag == "template" || dom.attr(id, "aria-hidden") == Some("true") {
        return true;
    }
    if node.tag == "slot" && dom.attr(id, "hidden").is_some() {
        return true;
    }
    let Some(style) = dom.attr(id, "style") else {
        return false;
    };
    style_hides(style)
}

/// Whether an inline style declares `display:none`, `visibility:hidden` or a
/// zero opacity/font-size/(max-)width/(max-)height, compared declaration by
/// declaration after blanking CSS comments, strings and `url()` tokens.
pub fn style_hides(style: &str) -> bool {
    static ZERO: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static IMPORTANT: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let zero = ZERO.get_or_init(|| {
        Regex::new(r"^[+-]?(?:0+(?:\.0+)?|\.0+)(?:e[+-]?[0-9]+)?[a-z%]*$").expect("regex")
    });
    let important = IMPORTANT.get_or_init(|| Regex::new(r"![ \t\n]*important$").expect("regex"));
    let normalized = style.replace("\r\n", "\n").replace(['\r', '\x0c'], "\n");
    let blanked = blank_style_tokens(&normalized);
    for declaration in blanked.split(';') {
        let (property, value) = declaration.split_once(':').unwrap_or((declaration, ""));
        let property = property.trim_matches([' ', '\t', '\n']).to_lowercase();
        let value = value.trim_matches([' ', '\t', '\n']).to_lowercase();
        let value = important.replace(&value, "");
        let value = value.trim_end_matches([' ', '\t', '\n']);
        if (property == "display" && value == "none")
            || (property == "visibility" && value == "hidden")
        {
            return true;
        }
        if matches!(
            property.as_str(),
            "opacity" | "font-size" | "height" | "width" | "max-height" | "max-width"
        ) && zero.is_match(value)
        {
            return true;
        }
    }
    false
}

/// Port of upstream `_STYLE_TOKEN_PATTERN.sub(_blank_style_token, ...)`:
/// keep escapes, replace comments with a space and strings / unquoted
/// `url()` tokens with `""`.
fn blank_style_tokens(style: &str) -> String {
    let chars: Vec<char> = style.chars().collect();
    let mut out = String::with_capacity(style.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            out.push(c);
            out.push(chars[i + 1]);
            i += 2;
        } else if c == '"' || c == '\'' {
            i += 1;
            while i < chars.len() && chars[i] != c && chars[i] != '\n' {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 2;
                } else if chars[i] == '\\' {
                    break;
                } else {
                    i += 1;
                }
            }
            if i < chars.len() && chars[i] == c {
                i += 1;
            }
            out.push_str("\"\"");
        } else if is_url_token(&chars, i) {
            i += 4;
            while i < chars.len() && chars[i] != ')' {
                i += if chars[i] == '\\' { 2 } else { 1 };
            }
            i = (i + 1).min(chars.len());
            out.push_str("\"\"");
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i = (i + 2).min(chars.len());
            out.push(' ');
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

fn is_url_token(chars: &[char], i: usize) -> bool {
    let matches_url = chars.len() >= i + 4
        && chars[i..i + 3]
            .iter()
            .collect::<String>()
            .eq_ignore_ascii_case("url")
        && chars[i + 3] == '(';
    if !matches_url {
        return false;
    }
    if i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_' || chars[i - 1] == '-') {
        return false;
    }
    let next = chars[i + 4..]
        .iter()
        .find(|c| !matches!(c, ' ' | '\t' | '\n'));
    !matches!(next, Some('"') | Some('\''))
}

fn clean_text(text: &str) -> String {
    text.chars()
        .filter(|&c| {
            !matches!(
                c,
                '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{feff}' | '\u{2060}' | '\u{180e}'
            ) && !matches!(c as u32, 0x00..=0x08 | 0x0b | 0x0c | 0x0e..=0x1f)
        })
        .collect()
}

fn escape_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

struct Renderer<'a> {
    dom: &'a Dom,
    root: NodeId,
    dropped: &'a HashSet<NodeId>,
    clean_text: bool,
}

impl Renderer<'_> {
    fn markdownify(&self) -> crate::markdownify::Converter<'_> {
        crate::markdownify::Converter {
            dom: self.dom,
            dropped: self.dropped,
            clean: if self.clean_text {
                &clean_text
            } else {
                &str::to_owned
            },
            fragment_root: std::cell::Cell::new(None),
        }
    }

    fn is_dropped(&self, mut id: NodeId) -> bool {
        loop {
            if self.dropped.contains(&id) {
                return true;
            }
            if id == self.root {
                return false;
            }
            match self.dom.parent(id) {
                Some(parent) => id = parent,
                None => return false,
            }
        }
    }

    fn text_of(&self, data: &str) -> String {
        if self.clean_text {
            clean_text(data)
        } else {
            data.to_owned()
        }
    }

    /// lxml `tostring(method="html", with_tail=False)` of a sanitised node.
    fn html(&self, id: NodeId) -> String {
        let mut out = String::new();
        self.write(id, &mut out, false);
        out.trim().to_owned()
    }

    fn write(&self, id: NodeId, out: &mut String, raw_text: bool) {
        if self.dropped.contains(&id) {
            return;
        }
        let node = self.dom.node(id);
        match node.kind {
            NodeKind::Text => {
                let text = self.text_of(&node.data);
                if raw_text {
                    out.push_str(&text);
                } else {
                    out.push_str(&escape_text(&text));
                }
            }
            NodeKind::Element => {
                out.push('<');
                out.push_str(&node.tag);
                for (index, (name, value)) in node.attrs.iter().enumerate() {
                    out.push(' ');
                    out.push_str(name);
                    let valueless = index < 64 && node.valueless_attrs & (1 << index) != 0;
                    if valueless || BOOLEAN_ATTRS.contains(&name.to_ascii_lowercase().as_str()) {
                        continue;
                    }
                    // libxml2's HTML serializer quotes with ' when the value
                    // contains " (and no '), otherwise escapes " as &quot;.
                    if value.contains('"') && !value.contains('\'') {
                        out.push_str("='");
                        out.push_str(&value.replace('&', "&amp;"));
                        out.push('\'');
                    } else {
                        out.push_str("=\"");
                        out.push_str(&value.replace('&', "&amp;").replace('"', "&quot;"));
                        out.push('"');
                    }
                }
                out.push('>');
                if VOID_TAGS.contains(&node.tag.as_str()) {
                    return;
                }
                let raw = matches!(node.tag.as_str(), "script" | "style");
                for &child in self.dom.children(id) {
                    self.write(child, out, raw);
                }
                out.push_str("</");
                out.push_str(&node.tag);
                out.push('>');
            }
            NodeKind::Document => {
                for &child in self.dom.children(id) {
                    self.write(child, out, false);
                }
            }
            NodeKind::Comment | NodeKind::Doctype => {}
        }
    }

    /// `get_all_text(strip=True, ignore_tags=(script, style, noscript, svg,
    /// iframe))` followed by upstream's per-character whitespace collapsing.
    fn text(&self, id: NodeId) -> String {
        let mut parts = Vec::new();
        self.collect_text(id, &mut parts);
        let mut text = parts.join("\n");
        for separator in ['\n', '\r', '\t', ' '] {
            let mut collapsed = String::with_capacity(text.len());
            let mut previous = None;
            for c in text.chars() {
                if c == separator && previous == Some(separator) {
                    continue;
                }
                collapsed.push(c);
                previous = Some(c);
            }
            text = collapsed;
        }
        text
    }

    /// Text runs directly below `id`. Text nodes separated only by dropped
    /// elements form one run, as lxml merges a dropped element's tail into
    /// the preceding text.
    fn text_runs(&self, id: NodeId, mut visit: impl FnMut(NodeId)) -> Vec<String> {
        let mut runs = Vec::new();
        let mut pending: Option<String> = None;
        for &child in self.dom.children(id) {
            let node = self.dom.node(child);
            if node.kind == NodeKind::Text {
                pending
                    .get_or_insert_with(String::new)
                    .push_str(&self.text_of(&node.data));
            } else if self.dropped.contains(&child) || node.kind != NodeKind::Element {
                continue;
            } else {
                runs.extend(pending.take());
                visit(child);
            }
        }
        runs.extend(pending);
        runs
    }

    fn collect_text(&self, id: NodeId, parts: &mut Vec<String>) {
        let node = self.dom.node(id);
        if self.dropped.contains(&id)
            || (node.kind == NodeKind::Element && TEXT_IGNORED_TAGS.contains(&node.tag.as_str()))
        {
            return;
        }
        let flush = |pending: &mut Option<String>, parts: &mut Vec<String>| {
            if let Some(text) = pending.take() {
                let stripped = text.trim();
                if !stripped.is_empty() {
                    parts.push(stripped.to_owned());
                }
            }
        };
        let mut pending: Option<String> = None;
        for &child in self.dom.children(id) {
            let child_node = self.dom.node(child);
            match child_node.kind {
                NodeKind::Text => pending
                    .get_or_insert_with(String::new)
                    .push_str(&self.text_of(&child_node.data)),
                NodeKind::Element if !self.dropped.contains(&child) => {
                    flush(&mut pending, parts);
                    self.collect_text(child, parts);
                }
                _ => {}
            }
        }
        flush(&mut pending, parts);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_style_hiding_matches_upstream_rules() {
        for hidden in [
            "display:none",
            "DISPLAY : NONE !important",
            "color:red; visibility:hidden",
            "opacity:0",
            "font-size:0px",
            "max-height: .0em",
            "height:0",
            "width:+0.00%",
        ] {
            assert!(style_hides(hidden), "{hidden}");
        }
        for visible in [
            "opacity:0.95",
            "line-height:0.9",
            "display:block",
            "content:'display:none'",
            "background:url(display:none)",
            "/* display:none */ color:red",
            "font-family:\"a;display:none\"",
            "no/**/ne",
        ] {
            assert!(!style_hides(visible), "{visible}");
        }
    }

    #[test]
    fn main_content_keeps_body_minus_noise_and_hidden_content() {
        let html = "<html><head><title>T</title></head><body><h1>Title</h1>\
            <script>alert(1)</script><p style=\"display:none\">secret</p>\
            <p aria-hidden=\"true\">aria</p><template><p>tpl</p></template>\
            <p>Vis\u{200b}ible<svg><text>x</text></svg> tail</p>\
            <select id=\"c\"><option>Red</option><option>Blue</option></select></body></html>";
        let html_out = convert_content(html, ContentFormat::Html, None, true).unwrap();
        assert_eq!(html_out.len(), 1);
        let out = &html_out[0];
        assert!(
            out.starts_with("<body>") && out.ends_with("</body>"),
            "{out}"
        );
        for gone in [
            "alert", "secret", "aria", "tpl", "\u{200b}", "<svg", "<title>",
        ] {
            assert!(!out.contains(gone), "{gone} in {out}");
        }
        assert!(out.contains("<p>Visible tail</p>"), "{out}");
        let text = convert_content(html, ContentFormat::Text, Some("#c"), true).unwrap();
        assert_eq!(text, vec!["Red\nBlue".to_owned()]);
        let selected = convert_content(html, ContentFormat::Html, Some("p"), true).unwrap();
        assert_eq!(selected, vec!["<p>Visible tail</p>".to_owned()]);
        let full = convert_content(html, ContentFormat::Html, None, false).unwrap();
        assert!(full[0].starts_with("<html>") && full[0].contains("alert(1)"));
        let markdown = convert_content(html, ContentFormat::Markdown, Some("h1"), true).unwrap();
        assert_eq!(markdown, vec!["Title\n=====".to_owned()]);
    }
}
