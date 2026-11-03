//! Rust port of python-markdownify 1.2.3 (`MarkdownConverter` with default
//! options), which pinned Scrapling v0.5 uses for every Markdown output.
//!
//! Original: https://github.com/matthewwithanm/python-markdownify
//! Copyright 2012-2018 Matthew Tretter, MIT License
//! (licenses/markdownify-MIT.txt). Ported to operate on RustScrapling's
//! arena DOM instead of a BeautifulSoup tree: text nodes separated only by
//! dropped elements are merged, exactly as re-parsing lxml's serialized
//! output would merge them.
//!
//! Defaults reproduced: autolinks, bullets `*+-`, underlined h1/h2,
//! `escape_asterisks`/`escape_underscores` without `escape_misc`,
//! two-space line breaks, `strip_document`/`strip_pre` = strip, `**`/`*`
//! emphasis, empty sub/sup markers, no table header inference, no wrap.

use std::collections::HashSet;

use rustscrapling_dom::{Dom, NodeId, NodeKind};

/// One child in the virtual (post-drop) tree.
#[derive(Clone, Debug)]
enum Item {
    Text(String),
    Element(NodeId),
}

pub(crate) struct Converter<'a> {
    pub dom: &'a Dom,
    pub dropped: &'a HashSet<NodeId>,
    /// Text cleaning applied to every text node (ZWC/control removal).
    pub clean: &'a dyn Fn(&str) -> String,
    /// The converted fragment's root: upstream re-parses the fragment, so
    /// nothing above it exists for parent/sibling lookups.
    pub fragment_root: std::cell::Cell<Option<NodeId>>,
}

const BLOCK_INSIDE: [&str; 17] = [
    "p",
    "blockquote",
    "article",
    "div",
    "section",
    "ol",
    "ul",
    "li",
    "dl",
    "dt",
    "dd",
    "table",
    "thead",
    "tbody",
    "tfoot",
    "tr",
    "td",
];

fn heading_level(tag: &str) -> Option<usize> {
    let digits = tag.strip_prefix('h')?;
    let end = digits
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit())
        .map_or(digits.len(), |(i, _)| i);
    if end == 0 {
        return None;
    }
    digits[..end].parse().ok()
}

fn remove_inside(tag: Option<&str>) -> bool {
    tag.is_some_and(|tag| {
        heading_level(tag).is_some() || BLOCK_INSIDE.contains(&tag) || tag == "th"
    })
}

fn remove_outside(tag: Option<&str>) -> bool {
    remove_inside(tag) || tag == Some("pre")
}

/// Python `str.strip()` without arguments.
fn py_strip(text: &str) -> &str {
    text.trim_matches(py_space)
}

fn py_space(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\x1c'..='\x1f')
}

fn strip_chars<'t>(text: &'t str, chars: &[char]) -> &'t str {
    text.trim_matches(|c| chars.contains(&c))
}

const WS: [char; 4] = [' ', '\t', '\r', '\n'];

fn chomp(text: &str) -> (&'static str, &'static str, String) {
    let prefix = if text.starts_with(' ') { " " } else { "" };
    let suffix = if text.ends_with(' ') { " " } else { "" };
    (prefix, suffix, py_strip(text).to_owned())
}

/// `^(.*)` with MULTILINE: transform every line, keeping empty lines empty.
fn indent_lines(text: &str, indent: &str) -> String {
    text.split('\n')
        .map(|line| {
            if line.is_empty() {
                String::new()
            } else {
                format!("{indent}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn collapse(text: &str, set: &[char], replacement: &str, must_contain_newline: bool) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if set.contains(&chars[i]) {
            let start = i;
            while i < chars.len() && set.contains(&chars[i]) {
                i += 1;
            }
            let run = &chars[start..i];
            if !must_contain_newline || run.iter().any(|c| matches!(c, '\r' | '\n')) {
                out.push_str(replacement);
            } else {
                out.extend(run);
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Split into (leading newlines, content, trailing newlines).
fn extract_newlines(text: &str) -> (usize, &str, usize) {
    let lead = text.len() - text.trim_start_matches('\n').len();
    let rest = &text[lead..];
    let content = rest.trim_end_matches('\n');
    (lead, content, rest.len() - content.len())
}

impl Converter<'_> {
    /// Parent inside the fragment (None at the fragment root).
    fn parent(&self, id: NodeId) -> Option<NodeId> {
        if self.fragment_root.get() == Some(id) {
            return None;
        }
        self.dom
            .parent(id)
            .filter(|parent| self.dom.node(*parent).is_element())
    }

    fn tag(&self, id: NodeId) -> &str {
        &self.dom.node(id).tag
    }

    fn attr(&self, id: NodeId, name: &str) -> Option<&str> {
        self.dom.attr(id, name)
    }

    /// Children after dropping, with adjacent text merged.
    fn children(&self, id: NodeId) -> Vec<Item> {
        let mut items = Vec::new();
        let mut pending: Option<String> = None;
        for &child in self.dom.children(id) {
            let node = self.dom.node(child);
            match node.kind {
                NodeKind::Text => pending
                    .get_or_insert_with(String::new)
                    .push_str(&(self.clean)(&node.data)),
                NodeKind::Element if !self.dropped.contains(&child) => {
                    items.extend(pending.take().map(Item::Text));
                    items.push(Item::Element(child));
                }
                _ => {}
            }
        }
        items.extend(pending.map(Item::Text));
        items
    }

    fn element_children(&self, id: NodeId) -> Vec<NodeId> {
        self.children(id)
            .into_iter()
            .filter_map(|item| match item {
                Item::Element(id) => Some(id),
                Item::Text(_) => None,
            })
            .collect()
    }

    fn descendants_named(&self, id: NodeId, names: &[&str]) -> usize {
        let mut count = 0;
        for child in self.element_children(id) {
            if names.contains(&self.tag(child)) {
                count += 1;
            }
            count += self.descendants_named(child, names);
        }
        count
    }

    fn descendant_tags(&self, id: NodeId, names: &[&str], out: &mut Vec<NodeId>) {
        for child in self.element_children(id) {
            if names.contains(&self.tag(child)) {
                out.push(child);
            }
            self.descendant_tags(child, names, out);
        }
    }

    /// Convert a fragment rooted at `root` (an element) as markdownify does
    /// for a document containing only that element.
    pub fn convert_root(&self, root: NodeId) -> String {
        self.fragment_root.set(Some(root));
        let items = vec![Item::Element(root)];
        let text = self.process_children(&items, None, &HashSet::new(), false);
        text.trim_matches('\n').to_owned()
    }

    /// Convert the children of `parent` as the top level of a document
    /// (used for text fragments that markdownify re-parses as HTML).
    pub fn convert_children(&self, parent: NodeId) -> String {
        self.fragment_root.set(Some(parent));
        let items = self.children(parent);
        let text = self.process_children(&items, None, &HashSet::new(), false);
        text.trim_matches('\n').to_owned()
    }

    /// Convert a lone text fragment.
    pub fn convert_text(&self, text: &str) -> String {
        let items = vec![Item::Text(text.to_owned())];
        let text = self.process_children(&items, None, &HashSet::new(), false);
        text.trim_matches('\n').to_owned()
    }

    fn item_tag<'b>(&'b self, item: Option<&Item>) -> Option<&'b str> {
        match item {
            Some(Item::Element(id)) => Some(self.tag(*id)),
            _ => None,
        }
    }

    fn process_children(
        &self,
        items: &[Item],
        parent: Option<NodeId>,
        parent_tags: &HashSet<String>,
        in_pre: bool,
    ) -> String {
        let parent_tag = parent.map(|id| self.tag(id));
        let inside = remove_inside(parent_tag);
        let mut strings = Vec::new();
        for (index, item) in items.iter().enumerate() {
            let previous = index.checked_sub(1).and_then(|i| items.get(i));
            let next = items.get(index + 1);
            let string = match item {
                Item::Element(id) => self.process_tag(*id, parent_tags),
                Item::Text(text) => {
                    if py_strip(text).is_empty() {
                        if inside && (previous.is_none() || next.is_none()) {
                            continue;
                        }
                        if remove_outside(self.item_tag(previous))
                            || remove_outside(self.item_tag(next))
                        {
                            continue;
                        }
                    }
                    self.process_text(text, previous, next, parent_tag, parent_tags)
                }
            };
            if !string.is_empty() {
                strings.push(string);
            }
        }
        if in_pre {
            return strings.concat();
        }
        let mut updated: Vec<String> = vec![String::new()];
        for string in strings {
            let (mut lead, content, trail) = extract_newlines(&string);
            if updated.last().is_some_and(|last| !last.is_empty()) && lead > 0 {
                let previous_trail = updated.pop().unwrap_or_default();
                lead = 2.min(previous_trail.len().max(lead));
            }
            updated.push("\n".repeat(lead));
            updated.push(content.to_owned());
            updated.push("\n".repeat(trail));
        }
        updated.concat()
    }

    fn process_text(
        &self,
        text: &str,
        previous: Option<&Item>,
        next: Option<&Item>,
        parent_tag: Option<&str>,
        parent_tags: &HashSet<String>,
    ) -> String {
        let mut text = text.to_owned();
        if !parent_tags.contains("pre") {
            text = collapse(&text, &['\t', ' ', '\r', '\n'], "\n", true);
            text = collapse(&text, &['\t', ' '], " ", false);
        }
        if !parent_tags.contains("_noformat") {
            text = text.replace('*', "\\*").replace('_', "\\_");
        }
        let inside = remove_inside(parent_tag);
        if remove_outside(self.item_tag(previous)) || (inside && previous.is_none()) {
            text = text.trim_start_matches(WS).to_owned();
        }
        if remove_outside(self.item_tag(next)) || (inside && next.is_none()) {
            text = text.trim_end_matches(py_space).to_owned();
        }
        text
    }

    fn has_pre_ancestor(&self, id: NodeId) -> bool {
        let mut current = self.parent(id);
        while let Some(parent) = current {
            if self.dom.node(parent).kind == NodeKind::Element && self.tag(parent) == "pre" {
                return true;
            }
            current = self.parent(parent);
        }
        false
    }

    fn process_tag(&self, id: NodeId, parent_tags: &HashSet<String>) -> String {
        let tag = self.tag(id).to_owned();
        let mut child_tags = parent_tags.clone();
        child_tags.insert(tag.clone());
        if heading_level(&tag).is_some() || tag == "td" || tag == "th" {
            child_tags.insert("_inline".into());
        }
        if matches!(tag.as_str(), "pre" | "code" | "kbd" | "samp") {
            child_tags.insert("_noformat".into());
        }
        let in_pre = tag == "pre" || self.has_pre_ancestor(id);
        let items = self.children(id);
        let text = self.process_children(&items, Some(id), &child_tags, in_pre);
        self.convert(id, &tag, text, parent_tags)
    }

    fn inline(&self, markup: &str, text: String, parent_tags: &HashSet<String>) -> String {
        if parent_tags.contains("_noformat") {
            return text;
        }
        let (prefix, suffix, text) = chomp(&text);
        if text.is_empty() {
            return String::new();
        }
        format!("{prefix}{markup}{text}{markup}{suffix}")
    }

    fn convert(
        &self,
        id: NodeId,
        tag: &str,
        text: String,
        parent_tags: &HashSet<String>,
    ) -> String {
        let inline_parent = parent_tags.contains("_inline");
        match tag {
            "a" => {
                if parent_tags.contains("_noformat") {
                    return text;
                }
                let (prefix, suffix, text) = chomp(&text);
                if text.is_empty() {
                    return String::new();
                }
                let href = self.attr(id, "href");
                let title = self.attr(id, "title").filter(|title| !title.is_empty());
                if let (Some(href), None) = (href, title) {
                    if text.replace("\\_", "_") == href {
                        return format!("<{href}>");
                    }
                }
                let title_part = title
                    .map(|title| format!(" \"{}\"", title.replace('"', "\\\"")))
                    .unwrap_or_default();
                match href.filter(|href| !href.is_empty()) {
                    Some(href) => format!("{prefix}[{text}]({href}{title_part}){suffix}"),
                    None => text,
                }
            }
            "b" | "strong" => self.inline("**", text, parent_tags),
            "em" | "i" => self.inline("*", text, parent_tags),
            "del" | "s" => self.inline("~~", text, parent_tags),
            "sub" | "sup" => self.inline("", text, parent_tags),
            "blockquote" => {
                let text = strip_chars(&text, &WS);
                if inline_parent {
                    return format!(" {text} ");
                }
                if text.is_empty() {
                    return "\n".into();
                }
                let quoted = text
                    .split('\n')
                    .map(|line| {
                        if line.is_empty() {
                            ">".to_owned()
                        } else {
                            format!("> {line}")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("\n{quoted}\n\n")
            }
            "br" => {
                if inline_parent {
                    if text.is_empty() {
                        " ".into()
                    } else {
                        format!("{text} ")
                    }
                } else {
                    format!("  \n{text}")
                }
            }
            "code" | "kbd" | "samp" => {
                if parent_tags.contains("_noformat") {
                    return text;
                }
                let (prefix, suffix, text) = chomp(&text);
                if text.is_empty() {
                    return String::new();
                }
                let mut longest = 0;
                let mut run = 0;
                for c in text.chars() {
                    if c == '`' {
                        run += 1;
                        longest = longest.max(run);
                    } else {
                        run = 0;
                    }
                }
                let delimiter = "`".repeat(longest + 1);
                let text = if longest > 0 {
                    format!(" {text} ")
                } else {
                    text
                };
                format!("{prefix}{delimiter}{text}{delimiter}{suffix}")
            }
            "div" | "article" | "section" | "dl" => {
                if inline_parent {
                    return format!(" {} ", py_strip(&text));
                }
                let text = py_strip(&text);
                if text.is_empty() {
                    String::new()
                } else {
                    format!("\n\n{text}\n\n")
                }
            }
            "dd" => {
                let text = py_strip(&text);
                if inline_parent {
                    return format!(" {text} ");
                }
                if text.is_empty() {
                    return "\n".into();
                }
                let indented = indent_lines(text, "    ");
                let mut chars = indented.chars();
                chars.next();
                format!(":{}\n", chars.as_str())
            }
            "dt" => {
                let text = collapse(py_strip(&text), &['\t', ' ', '\r', '\n'], " ", false);
                if inline_parent {
                    return format!(" {text} ");
                }
                if text.is_empty() {
                    return "\n".into();
                }
                format!("\n\n{text}\n")
            }
            "hr" => "\n\n---\n\n".into(),
            "img" => {
                let alt = self.attr(id, "alt").unwrap_or("");
                let src = self.attr(id, "src").unwrap_or("");
                let title = self.attr(id, "title").unwrap_or("");
                if inline_parent {
                    return alt.to_owned();
                }
                let title_part = if title.is_empty() {
                    String::new()
                } else {
                    format!(" \"{}\"", title.replace('"', "\\\""))
                };
                format!("![{alt}]({src}{title_part})")
            }
            "video" => {
                if inline_parent {
                    return text;
                }
                let mut src = self.attr(id, "src").unwrap_or("").to_owned();
                if src.is_empty() {
                    let mut sources = Vec::new();
                    self.descendant_tags(id, &["source"], &mut sources);
                    if let Some(source) =
                        sources.into_iter().find(|s| self.attr(*s, "src").is_some())
                    {
                        src = self.attr(source, "src").unwrap_or("").to_owned();
                    }
                }
                let poster = self.attr(id, "poster").unwrap_or("");
                match (src.is_empty(), poster.is_empty()) {
                    (false, false) => format!("[![{text}]({poster})]({src})"),
                    (false, true) => format!("[{text}]({src})"),
                    (true, false) => format!("![{text}]({poster})"),
                    (true, true) => text,
                }
            }
            "ul" | "ol" => {
                let siblings = self
                    .dom
                    .parent(id)
                    .map(|parent| self.children(parent))
                    .unwrap_or_default();
                let position = siblings
                    .iter()
                    .position(|item| matches!(item, Item::Element(e) if *e == id));
                let next = position.and_then(|p| {
                    siblings[p + 1..].iter().find(|item| match item {
                        Item::Element(_) => true,
                        Item::Text(text) => !py_strip(text).is_empty(),
                    })
                });
                let before_paragraph = match next {
                    Some(Item::Element(next)) => !matches!(self.tag(*next), "ul" | "ol"),
                    Some(Item::Text(_)) => true,
                    None => false,
                };
                if parent_tags.contains("li") {
                    return format!("\n{}", text.trim_end_matches(py_space));
                }
                format!("\n\n{text}{}", if before_paragraph { "\n" } else { "" })
            }
            "li" => {
                let text = py_strip(&text);
                if text.is_empty() {
                    return "\n".into();
                }
                let parent = self.parent(id);
                let bullet = match parent {
                    Some(parent) if self.tag(parent) == "ol" => {
                        let start = self
                            .attr(parent, "start")
                            .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_numeric()))
                            .and_then(|s| s.parse::<i64>().ok())
                            .unwrap_or(1);
                        let previous = self
                            .element_children(parent)
                            .into_iter()
                            .take_while(|child| *child != id)
                            .filter(|child| self.tag(*child) == "li")
                            .count() as i64;
                        format!("{}.", start + previous)
                    }
                    _ => {
                        let mut depth: i64 = -1;
                        let mut current = Some(id);
                        while let Some(node) = current {
                            if self.dom.node(node).is_element() && self.tag(node) == "ul" {
                                depth += 1;
                            }
                            current = self.parent(node);
                        }
                        let bullets = ['*', '+', '-'];
                        bullets[depth.rem_euclid(3) as usize].to_string()
                    }
                };
                let bullet = format!("{bullet} ");
                let width = bullet.chars().count();
                let indented = indent_lines(text, &" ".repeat(width));
                let rest: String = indented.chars().skip(width).collect();
                format!("{bullet}{rest}\n")
            }
            "p" => {
                if inline_parent {
                    return format!(" {} ", strip_chars(&text, &WS));
                }
                let text = strip_chars(&text, &WS);
                if text.is_empty() {
                    String::new()
                } else {
                    format!("\n\n{text}\n\n")
                }
            }
            "pre" => {
                if text.is_empty() {
                    return String::new();
                }
                let trimmed_start = {
                    // re_pre_lstrip: ^[ \n]*\n
                    let leading: String = text
                        .chars()
                        .take_while(|c| matches!(c, ' ' | '\n'))
                        .collect();
                    match leading.rfind('\n') {
                        Some(last) => &text[last + 1..],
                        None => &text[..],
                    }
                };
                let trimmed = trimmed_start.trim_end_matches([' ', '\n']);
                format!("\n\n```\n{trimmed}\n```\n\n")
            }
            "q" => format!("\"{text}\""),
            "script" | "style" => String::new(),
            "table" => format!("\n\n{}\n\n", py_strip(&text)),
            "caption" => format!("{}\n\n", py_strip(&text)),
            "figcaption" => format!("\n\n{}\n\n", py_strip(&text)),
            "td" | "th" => {
                let colspan = self.colspan(id);
                format!(
                    " {}{}",
                    py_strip(&text).replace('\n', " "),
                    " |".repeat(colspan)
                )
            }
            "tr" => self.convert_tr(id, text),
            _ => {
                if let Some(level) = heading_level(tag) {
                    return self.heading(level, text, inline_parent);
                }
                text
            }
        }
    }

    fn colspan(&self, id: NodeId) -> usize {
        self.attr(id, "colspan")
            .filter(|value| !value.is_empty() && value.chars().all(|c| c.is_ascii_digit()))
            .and_then(|value| value.parse::<usize>().ok())
            .map_or(1, |value| value.clamp(1, 1000))
    }

    fn heading(&self, level: usize, text: String, inline_parent: bool) -> String {
        if inline_parent {
            return text;
        }
        let level = level.clamp(1, 6);
        let text = py_strip(&text);
        if level <= 2 {
            let text = text.trim_end_matches(py_space);
            if text.is_empty() {
                return String::new();
            }
            let pad = if level == 1 { "=" } else { "-" };
            return format!("\n\n{text}\n{}\n\n", pad.repeat(text.chars().count()));
        }
        let text = collapse(text, &['\t', ' ', '\r', '\n'], " ", false);
        format!("\n\n{} {text}\n\n", "#".repeat(level))
    }

    fn convert_tr(&self, id: NodeId, text: String) -> String {
        let mut cells = Vec::new();
        self.descendant_tags(id, &["td", "th"], &mut cells);
        let parent = self.parent(id);
        let parent_tag = parent.map(|p| self.tag(p).to_owned()).unwrap_or_default();
        let is_first_row = match parent {
            Some(parent) => self.element_children(parent).first() == Some(&id),
            None => true,
        };
        let is_headrow = cells.iter().all(|cell| self.tag(*cell) == "th")
            || (parent_tag == "thead"
                && parent.is_some_and(|p| self.descendants_named(p, &["tr"]) == 1));
        let grandparent = parent.and_then(|p| self.parent(p));
        let is_head_row_missing = (is_first_row && parent_tag != "tbody")
            || (is_first_row
                && parent_tag == "tbody"
                && grandparent.is_none_or(|g| self.descendants_named(g, &["thead"]) < 1));
        let full_colspan: usize = cells.iter().map(|cell| self.colspan(*cell)).sum();
        let mut overline = String::new();
        let mut underline = String::new();
        if is_headrow && is_first_row {
            underline = format!("| {} |\n", vec!["---"; full_colspan].join(" | "));
        } else if is_head_row_missing
            || (is_first_row
                && (parent_tag == "table"
                    || (parent_tag == "tbody"
                        && parent.is_some_and(|p| self.previous_element_sibling(p).is_none()))))
        {
            overline.push_str(&format!("| {} |\n", vec![""; full_colspan].join(" | ")));
            overline.push_str(&format!("| {} |\n", vec!["---"; full_colspan].join(" | ")));
        }
        format!("{overline}|{text}\n{underline}")
    }

    fn previous_element_sibling(&self, id: NodeId) -> Option<NodeId> {
        let parent = self.parent(id)?;
        let siblings = self.element_children(parent);
        let position = siblings.iter().position(|s| *s == id)?;
        position.checked_sub(1).map(|p| siblings[p])
    }
}
