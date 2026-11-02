//! `element_to_dict`: the adaptive fingerprint of an element, a faithful
//! port of Scrapling's `_StorageTools.element_to_dict`, including its JSON
//! shape (orjson key order) so storage files are interchangeable with the
//! original implementation.

use rustscrapling_dom::{Dom, NodeId, NodeKind};
use serde_json::{Map, Value};

/// A tag in a siblings list. lxml exposes a comment node's `tag` as a
/// factory function object, which is neither a string nor serializable;
/// Scrapling therefore cannot store dicts whose siblings include comments
/// (orjson raises). In memory we keep them as `Other`, which compares
/// equal to another `Other` (the lxml comment tag singleton compares
/// identical), and serializes as null.
///
/// Tags are interned: sibling lists can hold thousands of entries, almost
/// all the same few tag names, and fingerprints are built per candidate
/// element during relocate.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SibTag {
    Tag(std::sync::Arc<str>),
    Other,
}

pub fn intern_tag(s: &str) -> std::sync::Arc<str> {
    use std::collections::HashMap;
    use std::sync::{OnceLock, RwLock};
    static CACHE: OnceLock<RwLock<HashMap<String, std::sync::Arc<str>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| RwLock::new(HashMap::new()));
    if let Some(v) = cache.read().expect("tag cache").get(s) {
        return v.clone();
    }
    let arc: std::sync::Arc<str> = std::sync::Arc::from(s);
    cache
        .write()
        .expect("tag cache")
        .insert(s.to_string(), arc.clone());
    arc
}

#[derive(Debug, Clone, PartialEq)]
pub struct ElementDict {
    pub tag: String,
    /// Cleaned attributes: values stripped, empty values dropped,
    /// document order preserved.
    pub attributes: Vec<(String, String)>,
    /// The element's own text, stripped; `None` when it has no text.
    /// May be `Some("")` when the text was whitespace only.
    pub text: Option<String>,
    /// Tag names from the root element down to this element.
    pub path: Vec<String>,
    pub parent_name: Option<String>,
    /// Raw parent attributes (not cleaned), document order.
    pub parent_attribs: Vec<(String, String)>,
    pub parent_text: Option<String>,
    /// Sibling tags (elements and comments), excluding this element.
    pub siblings: Option<Vec<SibTag>>,
    /// Child element tags (comments excluded).
    pub children: Option<Vec<String>>,
}

/// Python `str.strip()` semantics (Unicode whitespace, plus the C0
/// separators Python also strips).
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

fn text_field(dom: &Dom, id: NodeId) -> Option<String> {
    match dom.text(id) {
        Some(t) if !t.is_empty() => Some(py_strip(t).to_string()),
        _ => None,
    }
}

fn element_parent(dom: &Dom, id: NodeId) -> Option<NodeId> {
    dom.parent(id)
        .filter(|&p| dom.node(p).kind == NodeKind::Element)
}

pub fn element_to_dict(dom: &Dom, id: NodeId) -> ElementDict {
    let node = dom.node(id);
    let mut attributes = Vec::new();
    for (k, v) in &node.attrs {
        let stripped = py_strip(v);
        if !stripped.is_empty() {
            attributes.push((k.clone(), stripped.to_string()));
        }
    }
    // Path: this element's tag preceded by element ancestors, root first.
    let mut path = vec![node.tag.clone()];
    let mut cur = element_parent(dom, id);
    while let Some(p) = cur {
        path.push(dom.node(p).tag.clone());
        cur = element_parent(dom, p);
    }
    path.reverse();

    let mut dict = ElementDict {
        tag: node.tag.clone(),
        attributes,
        text: text_field(dom, id),
        path,
        parent_name: None,
        parent_attribs: Vec::new(),
        parent_text: None,
        siblings: None,
        children: None,
    };

    if let Some(parent) = element_parent(dom, id) {
        let pnode = dom.node(parent);
        dict.parent_name = Some(pnode.tag.clone());
        dict.parent_attribs = pnode.attrs.clone();
        dict.parent_text = text_field(dom, parent);
        // Sibling tags repeat heavily; intern through a per-call cache
        // so the global cache lock is taken at most once per distinct
        // tag instead of once per entry.
        let mut local_cache: std::collections::HashMap<&str, std::sync::Arc<str>> =
            std::collections::HashMap::new();
        let siblings: Vec<SibTag> = dom
            .children(parent)
            .iter()
            .copied()
            .filter(|&c| c != id)
            .filter_map(|c| match dom.node(c).kind {
                NodeKind::Element => {
                    let tag = dom.node(c).tag.as_str();
                    let arc = local_cache
                        .entry(tag)
                        .or_insert_with(|| intern_tag(tag))
                        .clone();
                    Some(SibTag::Tag(arc))
                }
                NodeKind::Comment => Some(SibTag::Other),
                _ => None,
            })
            .collect();
        if !siblings.is_empty() {
            dict.siblings = Some(siblings);
        }
    }

    let children: Vec<String> = dom
        .element_children(id)
        .iter()
        .map(|&c| dom.node(c).tag.clone())
        .collect();
    if !children.is_empty() {
        dict.children = Some(children);
    }
    dict
}

fn attrs_object(attrs: &[(String, String)]) -> Value {
    let mut m = Map::new();
    for (k, v) in attrs {
        m.insert(k.clone(), Value::String(v.clone()));
    }
    Value::Object(m)
}

impl ElementDict {
    /// Serialize exactly like orjson's `dumps` of the Python dict:
    /// insertion-ordered keys, compact separators, tuples as arrays.
    pub fn to_json(&self) -> String {
        let mut m = Map::new();
        m.insert("tag".into(), Value::String(self.tag.clone()));
        m.insert("attributes".into(), attrs_object(&self.attributes));
        m.insert(
            "text".into(),
            self.text
                .as_ref()
                .map(|t| Value::String(t.clone()))
                .unwrap_or(Value::Null),
        );
        m.insert(
            "path".into(),
            Value::Array(self.path.iter().map(|t| Value::String(t.clone())).collect()),
        );
        if let Some(parent_name) = &self.parent_name {
            m.insert("parent_name".into(), Value::String(parent_name.clone()));
            m.insert("parent_attribs".into(), attrs_object(&self.parent_attribs));
            m.insert(
                "parent_text".into(),
                self.parent_text
                    .as_ref()
                    .map(|t| Value::String(t.clone()))
                    .unwrap_or(Value::Null),
            );
        }
        if let Some(siblings) = &self.siblings {
            m.insert(
                "siblings".into(),
                Value::Array(
                    siblings
                        .iter()
                        .map(|s| match s {
                            SibTag::Tag(t) => Value::String(t.to_string()),
                            SibTag::Other => Value::Null,
                        })
                        .collect(),
                ),
            );
        }
        if let Some(children) = &self.children {
            m.insert(
                "children".into(),
                Value::Array(children.iter().map(|t| Value::String(t.clone())).collect()),
            );
        }
        serde_json::to_string(&Value::Object(m)).expect("serialize element dict")
    }

    pub fn from_json(s: &str) -> Option<ElementDict> {
        let v: Value = serde_json::from_str(s).ok()?;
        let obj = v.as_object()?;
        let get_str = |k: &str| obj.get(k).and_then(|x| x.as_str()).map(|x| x.to_string());
        let get_attrs = |k: &str| -> Vec<(String, String)> {
            obj.get(k)
                .and_then(|x| x.as_object())
                .map(|m| {
                    m.iter()
                        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                        .collect()
                })
                .unwrap_or_default()
        };
        let get_str_list = |k: &str| -> Vec<String> {
            obj.get(k)
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default()
        };
        let siblings = obj.get("siblings").and_then(|x| x.as_array()).map(|a| {
            a.iter()
                .map(|x| match x.as_str() {
                    Some(s) => SibTag::Tag(intern_tag(s)),
                    None => SibTag::Other,
                })
                .collect()
        });
        let children = obj.get("children").and_then(|x| {
            if x.is_null() {
                None
            } else {
                Some(get_str_list("children"))
            }
        });
        Some(ElementDict {
            tag: get_str("tag")?,
            attributes: get_attrs("attributes"),
            text: get_str("text"),
            path: get_str_list("path"),
            parent_name: get_str("parent_name"),
            parent_attribs: get_attrs("parent_attribs"),
            parent_text: get_str("parent_text"),
            siblings,
            children,
        })
    }
}
