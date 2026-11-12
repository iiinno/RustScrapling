//! Public Selector operations over the existing CSS, XPath, and arena DOM.
//! Semantics follow Scrapling 95cf9019, parser.py and core/custom_types.py.

use crate::{css, text_search::py_strip, xpath, SelectError, SelectionNode};
use regex::{Regex, RegexBuilder};
use rustscrapling_dom::{libxml_backend::html_entity_codepoint, Dom, NodeId, NodeKind};
use std::borrow::Cow;
use std::sync::OnceLock;

#[derive(Debug)]
pub enum PublicSelectorError {
    Select(SelectError),
    Type(String),
    Key(String),
    Index,
    Value(String),
    Regex(regex::Error),
}

impl std::fmt::Display for PublicSelectorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Select(error) => write!(f, "{error}"),
            Self::Type(message) => write!(f, "type error: {message}"),
            Self::Key(key) => write!(f, "missing attribute: {key}"),
            Self::Index => write!(f, "selector index out of range"),
            Self::Value(message) => write!(f, "value error: {message}"),
            Self::Regex(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for PublicSelectorError {}

impl From<SelectError> for PublicSelectorError {
    fn from(value: SelectError) -> Self {
        Self::Select(value)
    }
}

impl From<regex::Error> for PublicSelectorError {
    fn from(value: regex::Error) -> Self {
        Self::Regex(value)
    }
}

type Result<T> = std::result::Result<T, PublicSelectorError>;

#[derive(Clone, Debug)]
pub struct Selector<'dom> {
    dom: &'dom Dom,
    node: SelectionNode,
}

pub enum FindFilter<'a> {
    Tag(&'a str),
    Tags(&'a [&'a str]),
    Attributes(&'a [(&'a str, &'a str)]),
    Pattern(&'a Regex),
    Predicate(&'a dyn Fn(&Selector<'_>) -> bool),
}

#[derive(Clone, Copy, Debug)]
pub struct TextOptions<'a> {
    pub separator: &'a str,
    pub strip: bool,
    pub ignore_tags: &'a [&'a str],
    pub valid_values: bool,
}

impl Default for TextOptions<'_> {
    fn default() -> Self {
        Self {
            separator: "\n",
            strip: false,
            ignore_tags: &["script", "style"],
            valid_values: true,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RegexOptions {
    pub replace_entities: bool,
    pub clean_match: bool,
    pub case_sensitive: bool,
}

impl Default for RegexOptions {
    fn default() -> Self {
        Self {
            replace_entities: true,
            clean_match: false,
            case_sensitive: true,
        }
    }
}

#[derive(Clone, Copy)]
pub enum RegexPattern<'a> {
    Text(&'a str),
    Compiled(&'a Regex),
}

#[derive(Debug, PartialEq, Eq)]
pub enum RegexFirst<T> {
    Match(String),
    Default(T),
}

impl<'dom> Selector<'dom> {
    pub fn new(dom: &'dom Dom, id: NodeId) -> Result<Self> {
        let node = dom.nodes.get(id as usize).ok_or_else(|| {
            PublicSelectorError::Value("node is not in the supplied DOM".into())
        })?;
        let id = if node.kind == NodeKind::Document {
            dom.document_element().ok_or_else(|| {
                PublicSelectorError::Value("document has no element".into())
            })?
        } else {
            id
        };
        Ok(Self { dom, node: SelectionNode::Node(id) })
    }

    fn wrap(&self, node: SelectionNode) -> Self {
        Self { dom: self.dom, node }
    }

    pub fn element_id(&self) -> Option<NodeId> {
        match self.node {
            SelectionNode::Node(id) if self.dom.node(id).is_element() => Some(id),
            _ => None,
        }
    }

    pub fn is_text(&self) -> bool {
        match &self.node {
            SelectionNode::Node(id) => self.dom.node(*id).kind == NodeKind::Text,
            SelectionNode::Attribute(_, _) => true,
        }
    }

    pub fn text(&self) -> &'dom str {
        match &self.node {
            SelectionNode::Node(id) if self.is_text() => &self.dom.node(*id).data,
            SelectionNode::Node(id) => self.dom.text(*id).unwrap_or_default(),
            SelectionNode::Attribute(id, name) => self.dom.attr(*id, name).unwrap_or_default(),
        }
    }

    pub fn getitem(&self, key: &str) -> Result<&'dom str> {
        if self.is_text() {
            return Err(PublicSelectorError::Type("text nodes do not have attributes".into()));
        }
        self.element_id()
            .and_then(|id| self.dom.attr(id, key))
            .ok_or_else(|| PublicSelectorError::Key(key.to_owned()))
    }

    pub fn contains(&self, key: &str) -> bool {
        self.element_id().is_some_and(|id| self.dom.attr(id, key).is_some())
    }

    pub fn css(&self, expression: &str) -> Result<Vec<Self>> {
        if self.is_text() {
            return Ok(Vec::new());
        }
        let SelectionNode::Node(scope) = self.node else { unreachable!() };
        Ok(css::select_nodes(self.dom, scope, expression)?
            .into_iter().map(|node| self.wrap(node)).collect())
    }

    pub fn xpath(&self, expression: &str, variables: &[(String, String)]) -> Result<Vec<Self>> {
        if self.is_text() {
            return Ok(Vec::new());
        }
        let SelectionNode::Node(scope) = self.node else { unreachable!() };
        Ok(xpath::select_nodes(self.dom, scope, expression, variables)?
            .into_iter().map(|node| self.wrap(node)).collect())
    }

    pub fn parent(&self) -> Option<Self> {
        let id = match &self.node {
            SelectionNode::Attribute(owner, _) => Some(*owner),
            SelectionNode::Node(id) if self.is_text() => {
                let container = self.dom.parent(*id)?;
                let children = self.dom.children(container);
                let position = children.iter().position(|child| child == id)?;
                // lxml smart strings attach a tail to the preceding element or comment.
                position.checked_sub(1).map(|i| children[i]).or(Some(container))
            }
            SelectionNode::Node(id) => self.dom.parent(*id),
        }?;
        (self.dom.node(id).kind != NodeKind::Document)
            .then(|| self.wrap(SelectionNode::Node(id)))
    }

    pub fn children(&self) -> Vec<Self> {
        self.element_id().map(|id| {
            self.dom.element_children(id).into_iter()
                .map(|child| self.wrap(SelectionNode::Node(child))).collect()
        }).unwrap_or_default()
    }

    pub fn siblings(&self) -> Vec<Self> {
        self.parent().map(|parent| {
            parent.children().into_iter().filter(|child| child.node != self.node).collect()
        }).unwrap_or_default()
    }

    pub fn next(&self) -> Option<Self> {
        self.dom.next_element_sibling(self.element_id()?)
            .map(|id| self.wrap(SelectionNode::Node(id)))
    }

    pub fn previous(&self) -> Option<Self> {
        self.dom.prev_element_sibling(self.element_id()?)
            .map(|id| self.wrap(SelectionNode::Node(id)))
    }

    pub fn get_all_text(&self, options: TextOptions<'_>) -> String {
        if self.is_text() {
            return self.text().to_owned();
        }
        let SelectionNode::Node(root) = self.node else { unreachable!() };
        let mut stack = vec![root];
        let mut strings = Vec::new();
        while let Some(id) = stack.pop() {
            let node = self.dom.node(id);
            match node.kind {
                NodeKind::Element
                    if options.ignore_tags.contains(&"*")
                        || options.ignore_tags.contains(&node.tag.as_str()) => {}
                NodeKind::Text => {
                    let text = if options.strip { py_strip(&node.data) } else { &node.data };
                    if !node.data.is_empty() && (!options.valid_values || !py_strip(text).is_empty()) {
                        strings.push(text);
                    }
                }
                NodeKind::Element | NodeKind::Document => {
                    stack.extend(self.dom.children(id).iter().rev().copied());
                }
                _ => {}
            }
        }
        strings.join(options.separator)
    }

    pub fn find_all(&self, filters: &[FindFilter<'_>], keywords: &[(&str, &str)]) -> Result<Vec<Self>> {
        if self.is_text() {
            return Ok(Vec::new());
        }
        if filters.is_empty() && keywords.is_empty() {
            return Err(PublicSelectorError::Type("at least one search filter is required".into()));
        }
        let mut tags = Vec::new();
        let mut attributes: Vec<(&str, &str)> = Vec::new();
        for filter in filters {
            match filter {
                FindFilter::Tag(tag) => tags.push(*tag),
                FindFilter::Tags(items) => tags.extend(items.iter().copied()),
                FindFilter::Attributes(items) => {
                    for &(key, value) in *items {
                        set_attribute(&mut attributes, key, value);
                    }
                }
                _ => {}
            }
        }
        for &(key, value) in keywords {
            let key = match key { "class_" => "class", "for_" => "for", _ => key };
            set_attribute(&mut attributes, key, value);
        }
        tags.sort_unstable();
        tags.dedup();
        if tags.is_empty() {
            tags.push("*");
        }
        let mut expressions = Vec::new();
        for tag in tags {
            let mut expression = tag.to_owned();
            for &(key, value) in &attributes {
                let classes: Vec<_> = value.split(py_space).filter(|s| !s.is_empty()).collect();
                if key == "class" && !classes.is_empty() {
                    for class in classes {
                        expression.push_str(&format!("[class~=\"{}\"]", escape_css_string(class)));
                    }
                } else {
                    expression.push_str(&format!("[{key}=\"{}\"]", escape_css_string(value)));
                }
            }
            if expression != "*" {
                expressions.push(expression);
            }
        }
        let mut results = if expressions.is_empty() {
            self.xpath(".//*", &[])?
        } else {
            self.css(&expressions.join(", "))?
        };
        for filter in filters {
            if let FindFilter::Pattern(pattern) = filter {
                results.retain(|item| pattern.is_match(item.text()));
            }
        }
        for filter in filters {
            if let FindFilter::Predicate(predicate) = filter {
                results.retain(|item| predicate(item));
            }
        }
        Ok(results)
    }

    pub fn find(&self, filters: &[FindFilter<'_>], keywords: &[(&str, &str)]) -> Result<Option<Self>> {
        Ok(self.find_all(filters, keywords)?.into_iter().next())
    }

    /// Text patterns also accept Python's `\Z` end-of-text anchor.
    /// Other syntax uses the regex crate; unsupported constructs return a regex error.
    /// Compiled expressions retain their flags even when `case_sensitive` is supplied.
    pub fn re(&self, pattern: RegexPattern<'_>, options: RegexOptions) -> Result<Vec<String>> {
        let regex = match pattern {
            RegexPattern::Text(pattern) => Cow::Owned(RegexBuilder::new(&python_end_anchor(pattern))
                .case_insensitive(!options.case_sensitive).build()?),
            RegexPattern::Compiled(pattern) => Cow::Borrowed(pattern),
        };
        let text = if options.clean_match {
            Cow::Owned(clean_regex_text(self.text()))
        } else {
            Cow::Borrowed(self.text())
        };
        let mut result = Vec::new();
        let first_group = usize::from(regex.captures_len() > 1);
        for captures in regex.captures_iter(&text) {
            for group in first_group..regex.captures_len() {
                let value = captures.get(group).map_or("", |value| value.as_str());
                result.push(if options.replace_entities {
                    replace_entities(value)
                } else {
                    value.to_owned()
                });
            }
        }
        Ok(result)
    }

    pub fn re_first(&self, pattern: RegexPattern<'_>, options: RegexOptions) -> Result<Option<String>> {
        Ok(self.re(pattern, options)?.into_iter().next())
    }

    pub fn re_first_or<T>(&self, pattern: RegexPattern<'_>, default: T, options: RegexOptions) -> Result<RegexFirst<T>> {
        Ok(match self.re_first(pattern, options)? {
            Some(value) => RegexFirst::Match(value),
            None => RegexFirst::Default(default),
        })
    }
}

/// Python list indexing, including negative indices and an explicit out-of-range error.
pub fn index<T>(items: &[T], position: isize) -> Result<&T> {
    let length = items.len() as i128;
    let position = position as i128;
    let position = if position < 0 { length + position } else { position };
    if position < 0 || position >= length {
        return Err(PublicSelectorError::Index);
    }
    Ok(&items[position as usize])
}

/// Python slice bounds. An omitted negative stop differs from an explicit `-1`.
pub fn slice<T: Clone>(items: &[T], start: Option<isize>, stop: Option<isize>, step: isize) -> Result<Vec<T>> {
    if step == 0 {
        return Err(PublicSelectorError::Value("slice step cannot be zero".into()));
    }
    let length = items.len() as i128;
    let step = step as i128;
    let reverse = step < 0;
    let bound = |value: isize| {
        let value = value as i128;
        let value = if value < 0 { value + length } else { value };
        if reverse { value.clamp(-1, length - 1) } else { value.clamp(0, length) }
    };
    let mut position = start.map(bound).unwrap_or(if reverse { length - 1 } else { 0 });
    let stop = stop.map(bound).unwrap_or(if reverse { -1 } else { length });
    let mut result = Vec::new();
    while if reverse { position > stop } else { position < stop } {
        result.push(items[position as usize].clone());
        position += step;
    }
    Ok(result)
}

fn set_attribute<'a>(attributes: &mut Vec<(&'a str, &'a str)>, key: &'a str, value: &'a str) {
    if let Some(item) = attributes.iter_mut().find(|item| item.0 == key) {
        item.1 = value;
    } else {
        attributes.push((key, value));
    }
}

fn escape_css_string(value: &str) -> String {
    value.replace('"', "\\\"").replace('\n', "\\A ").replace('\r', "\\D ").replace('\u{c}', "\\C ")
}

fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

fn python_end_anchor(pattern: &str) -> Cow<'_, str> {
    let mut chars = pattern.char_indices();
    let mut output = String::new();
    let mut copied = 0;
    while let Some((_, character)) = chars.next() {
        if character == '\\' {
            if let Some((index, 'Z')) = chars.next() {
                output.push_str(&pattern[copied..index]);
                output.push('z');
                copied = index + 1;
            }
        }
    }
    if copied == 0 {
        Cow::Borrowed(pattern)
    } else {
        output.push_str(&pattern[copied..]);
        Cow::Owned(output)
    }
}

fn clean_regex_text(value: &str) -> String {
    let mut output = String::new();
    let mut last_space = false;
    for c in value.chars() {
        let c = if matches!(c, '\t' | '\r' | '\n') { ' ' } else { c };
        if c != ' ' || !last_space {
            output.push(c);
        }
        last_space = c == ' ';
    }
    py_strip(&output).to_owned()
}

fn replace_entities(value: &str) -> String {
    static ENTITIES: OnceLock<Regex> = OnceLock::new();
    let expression = ENTITIES.get_or_init(|| {
        Regex::new(r"(?i)&([a-z0-9]+|#[0-9]+|#x[a-f0-9]+)(;?)").expect("constant entity regex")
    });
    expression.replace_all(value, |captures: &regex::Captures<'_>| {
        let entity = &captures[1];
        let number = if entity.starts_with("#x") || entity.starts_with("#X") {
            u32::from_str_radix(&entity[2..], 16).ok()
        } else if let Some(decimal) = entity.strip_prefix('#') {
            decimal.parse().ok()
        } else {
            html_entity_codepoint(entity).or_else(|| html_entity_codepoint(&entity.to_ascii_lowercase()))
        };
        let converted = number.and_then(entity_character);
        match converted {
            Some(character) => character.to_string(),
            None if &captures[2] == ";" => String::new(),
            None => captures[0].to_owned(),
        }
    }).into_owned()
}

fn entity_character(number: u32) -> Option<char> {
    if number == 0 || (0xd800..=0xdfff).contains(&number) {
        return Some('\u{fffd}');
    }
    const WINDOWS_1252: [u32; 32] = [
        0x20ac, 0, 0x201a, 0x192, 0x201e, 0x2026, 0x2020, 0x2021,
        0x2c6, 0x2030, 0x160, 0x2039, 0x152, 0, 0x17d, 0,
        0, 0x2018, 0x2019, 0x201c, 0x201d, 0x2022, 0x2013, 0x2014,
        0x2dc, 0x2122, 0x161, 0x203a, 0x153, 0, 0x17e, 0x178,
    ];
    let number = if (0x80..=0x9f).contains(&number) {
        let mapped = WINDOWS_1252[(number - 0x80) as usize];
        if mapped == 0 { return None; }
        mapped
    } else {
        number
    };
    char::from_u32(number)
}
