//! Default-mode parser: html5ever (the HTML5 spec parser, the same tree
//! a browser builds) writing directly into the arena DOM through a
//! TreeSink implementation. Building RcDom first and converting it
//! cost an extra ~4 ms on the Appendix-A page; the direct sink keeps
//! the full parse inside the stage-1 budget.
//!
//! Comments are dropped unless `keep_comments` is set, and NUL bytes
//! are removed before parsing (html5ever would replace them with
//! U+FFFD). Whitespace text is kept, as in libxml2 and in browsers.

use crate::{Dom, NodeId, NodeKind};
use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::{parse_document, ParseOpts};
use markup5ever::interface::tree_builder::{ElementFlags, NodeOrText, TreeSink};
use markup5ever::interface::QuirksMode;
use markup5ever::{Attribute, ExpandedName, QualName};
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;

#[derive(Default)]
pub struct Html5Options {
    pub keep_comments: bool,
}

pub fn parse_html(input: &str, opts: &Html5Options) -> Result<Dom, String> {
    // NUL removal at the str level (NUL is one byte in UTF-8). The
    // input is already valid UTF-8, so feed it in one tendril instead
    // of the incremental from_utf8 path, which revalidates in chunks.
    let cleaned: Cow<'_, str> = if input.contains('\0') {
        Cow::Owned(input.chars().filter(|c| *c != '\0').collect())
    } else {
        Cow::Borrowed(input)
    };
    let sink = ArenaSink::new(opts.keep_comments);
    let dom = parse_document(sink, ParseOpts::default()).one(StrTendril::from_slice(&cleaned));
    Ok(dom)
}

struct ArenaSink {
    dom: RefCell<Dom>,
    /// QualName per node id, kept only so `elem_name` can answer during
    /// the parse. Indexed by NodeId.
    names: RefCell<Vec<Option<Box<QualName>>>>,
    template_contents: RefCell<HashMap<NodeId, NodeId>>,
    keep_comments: bool,
}

impl ArenaSink {
    fn new(keep_comments: bool) -> Self {
        let dom = Dom::new();
        let names = vec![None]; // document node
        ArenaSink {
            dom: RefCell::new(dom),
            names: RefCell::new(names),
            template_contents: RefCell::new(HashMap::new()),
            keep_comments,
        }
    }

    fn register_name(&self, id: NodeId, name: Option<QualName>) {
        let mut names = self.names.borrow_mut();
        while names.len() <= id as usize {
            names.push(None);
        }
        // Boxed so the QualName never moves again: elem_name hands out
        // references into it while the parser is borrowing the sink.
        names[id as usize] = name.map(Box::new);
    }

    fn attr_pairs(attrs: Vec<Attribute>) -> Vec<(String, String)> {
        attrs
            .into_iter()
            .map(|a| {
                let key = if let Some(prefix) = &a.name.prefix {
                    format!("{}:{}", prefix, a.name.local)
                } else {
                    a.name.local.to_string()
                };
                (key, a.value.to_string())
            })
            .collect()
    }

    fn append_node_or_text(&self, parent: NodeId, child: NodeOrText<NodeId>) {
        match child {
            NodeOrText::AppendNode(id) => {
                if !self.keep_comments && self.dom.borrow().node(id).kind == NodeKind::Comment {
                    return;
                }
                self.dom.borrow_mut().attach(parent, id);
            }
            NodeOrText::AppendText(text) => {
                if !text.is_empty() {
                    self.dom.borrow_mut().add_text(parent, &text);
                }
            }
        }
    }
}

impl TreeSink for ArenaSink {
    type Handle = NodeId;
    type Output = Dom;
    type ElemName<'a> = ExpandedName<'a>;

    fn finish(self) -> Dom {
        self.dom.into_inner()
    }

    fn parse_error(&self, _msg: Cow<'static, str>) {}

    fn get_document(&self) -> NodeId {
        self.dom.borrow().root
    }

    fn elem_name<'a>(&'a self, target: &'a NodeId) -> ExpandedName<'a> {
        // The names table outlives this call through the sink borrow;
        // ExpandedName only borrows the stored QualName parts.
        let names = self.names.borrow();
        let qname: &QualName = names
            .get(*target as usize)
            .and_then(|n| n.as_deref())
            .expect("elem_name called on a non-element");
        // The QualName lives in a Box inside the sink, so its address
        // is stable; the trait contract ties the returned name to the
        // &'a borrow of the sink. Extending the reference past the
        // Ref guard is safe because the guard protects the Vec, not
        // the boxed value, and nothing mutates names while the parser
        // holds an ExpandedName.
        let ptr: *const QualName = qname;
        unsafe { (*ptr).expanded() }
    }

    fn create_element(
        &self,
        name: QualName,
        attrs: Vec<Attribute>,
        _flags: ElementFlags,
    ) -> NodeId {
        let pairs = Self::attr_pairs(attrs);
        let local = name.local.to_string();
        let id = self.dom.borrow_mut().create_detached_element(&local, pairs);
        self.register_name(id, Some(name));
        if local == "template" {
            let contents = self.dom.borrow_mut().create_detached(NodeKind::Document);
            self.register_name(contents, None);
            self.dom.borrow_mut().attach(id, contents);
            self.template_contents.borrow_mut().insert(id, contents);
        }
        id
    }

    fn create_comment(&self, text: StrTendril) -> NodeId {
        let id = self.dom.borrow_mut().create_detached(NodeKind::Comment);
        self.dom.borrow_mut().nodes[id as usize].data = text.to_string();
        self.register_name(id, None);
        id
    }

    fn create_pi(&self, _target: StrTendril, data: StrTendril) -> NodeId {
        // The arena has no processing-instruction kind; compat mode
        // drops PIs too. A detached comment node stands in so the
        // handle stays valid.
        let id = self.dom.borrow_mut().create_detached(NodeKind::Comment);
        self.dom.borrow_mut().nodes[id as usize].data = data.to_string();
        self.register_name(id, None);
        id
    }

    fn append(&self, parent: &NodeId, child: NodeOrText<NodeId>) {
        self.append_node_or_text(*parent, child);
    }

    fn append_based_on_parent_node(
        &self,
        element: &NodeId,
        prev_element: &NodeId,
        child: NodeOrText<NodeId>,
    ) {
        let has_parent = self.dom.borrow().node(*element).parent.is_some();
        if has_parent {
            self.append_before_sibling(element, child);
        } else {
            self.append(prev_element, child);
        }
    }

    fn append_doctype_to_document(
        &self,
        name: StrTendril,
        _public_id: StrTendril,
        _system_id: StrTendril,
    ) {
        let root = self.dom.borrow().root;
        self.dom.borrow_mut().add_doctype(root, &name);
    }

    fn get_template_contents(&self, target: &NodeId) -> NodeId {
        *self
            .template_contents
            .borrow()
            .get(target)
            .expect("get_template_contents on a non-template")
    }

    fn same_node(&self, x: &NodeId, y: &NodeId) -> bool {
        x == y
    }

    fn set_quirks_mode(&self, _mode: QuirksMode) {}

    fn append_before_sibling(&self, sibling: &NodeId, new_node: NodeOrText<NodeId>) {
        match new_node {
            NodeOrText::AppendNode(id) => {
                if !self.keep_comments && self.dom.borrow().node(id).kind == NodeKind::Comment {
                    return;
                }
                self.dom.borrow_mut().insert_before(*sibling, id);
            }
            NodeOrText::AppendText(text) => {
                if !text.is_empty() {
                    self.dom.borrow_mut().insert_text_before(*sibling, &text);
                }
            }
        }
    }

    fn add_attrs_if_missing(&self, target: &NodeId, attrs: Vec<Attribute>) {
        let pairs = Self::attr_pairs(attrs);
        let mut dom = self.dom.borrow_mut();
        let node = &mut dom.nodes[*target as usize];
        for (k, v) in pairs {
            if !node.attrs.iter().any(|(ek, _)| *ek == k) {
                node.attrs.push((k, v));
            }
        }
    }

    fn remove_from_parent(&self, target: &NodeId) {
        self.dom.borrow_mut().detach(*target);
    }

    fn reparent_children(&self, node: &NodeId, new_parent: &NodeId) {
        let mut dom = self.dom.borrow_mut();
        let children = dom.nodes[*node as usize].children.clone();
        for child in children {
            dom.attach(*new_parent, child);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_document() {
        let html = "<html><body><div id='a'>hi<span>there</span> tail</div></body></html>";
        let dom = parse_html(html, &Html5Options::default()).unwrap();
        let root = dom.document_element().unwrap();
        assert_eq!(dom.node(root).tag, "html");
        assert_eq!(dom.element_count(), 5);
        assert!(dom.text_content(dom.root).contains("there"));
    }

    #[test]
    fn html5_inserts_tbody_too() {
        let html = "<html><body><table><tr><td>1</td></tr></table></body></html>";
        let dom = parse_html(html, &Html5Options::default()).unwrap();
        let tags: Vec<String> = dom
            .descendants_or_self(dom.root)
            .into_iter()
            .filter(|&n| dom.node(n).is_element())
            .map(|n| dom.node(n).tag.clone())
            .collect();
        assert!(tags.iter().any(|t| t == "tbody"), "tags: {:?}", tags);
    }

    #[test]
    fn foster_parenting_and_text_merging() {
        // Text inside a table but outside cells is foster-parented
        // before the table by the HTML5 algorithm.
        let html = "<html><body><table>hello<tr><td>1</td></tr></table></body></html>";
        let dom = parse_html(html, &Html5Options::default()).unwrap();
        let body_text = dom.text_content(dom.root);
        assert!(body_text.contains("hello"), "text: {}", body_text);
        let body = dom
            .descendants_or_self(dom.root)
            .into_iter()
            .find(|&n| dom.node(n).tag == "body")
            .unwrap();
        // 'hello' must be a direct text child of body (before the table).
        let first_text = dom.children(body).first().copied().unwrap();
        assert_eq!(dom.node(first_text).kind, NodeKind::Text);
        assert_eq!(dom.node(first_text).data, "hello");
    }
}
