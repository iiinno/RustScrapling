//! Arena DOM for RustScrapling (Plan section 5 and 8).
//!
//! All nodes live in one `Vec<Node>` and are referenced by `NodeId`.
//! Text is stored as dedicated text nodes, mirroring how libxml2 sees a
//! tree, so the lxml notions of an element's `text` (the text node before
//! its first child element) and `tail` (the text node right after an
//! element, as a child of its parent) can be reproduced exactly.

pub type NodeId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Document,
    Element,
    Text,
    Comment,
    Doctype,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub kind: NodeKind,
    /// Element tag name, lowercased. Empty for non-element nodes.
    pub tag: String,
    /// Attributes in source order (order matters for adaptive fingerprints).
    pub attrs: Vec<(String, String)>,
    /// Character data for text, comment and doctype nodes.
    pub data: String,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
}

impl Node {
    fn new(kind: NodeKind) -> Self {
        Node {
            kind,
            tag: String::new(),
            attrs: Vec::new(),
            data: String::new(),
            parent: None,
            children: Vec::new(),
        }
    }

    pub fn is_element(&self) -> bool {
        self.kind == NodeKind::Element
    }
}

#[derive(Debug, Clone)]
pub struct Dom {
    pub nodes: Vec<Node>,
    /// The document node.
    pub root: NodeId,
}

impl Default for Dom {
    fn default() -> Self {
        Self::new()
    }
}

impl Dom {
    pub fn new() -> Self {
        let mut dom = Dom {
            nodes: Vec::new(),
            root: 0,
        };
        dom.root = dom.push(Node::new(NodeKind::Document));
        dom
    }

    fn push(&mut self, node: Node) -> NodeId {
        let id = self.nodes.len() as NodeId;
        self.nodes.push(node);
        id
    }

    /// Append a child node under `parent` and return its id.
    pub fn append_child(&mut self, parent: NodeId, mut node: Node) -> NodeId {
        node.parent = Some(parent);
        let id = self.push(node);
        self.nodes[parent as usize].children.push(id);
        id
    }

    pub fn add_element(
        &mut self,
        parent: NodeId,
        tag: &str,
        attrs: Vec<(String, String)>,
    ) -> NodeId {
        let mut node = Node::new(NodeKind::Element);
        node.tag = tag.to_ascii_lowercase();
        node.attrs = attrs;
        self.append_child(parent, node)
    }

    /// Create a node that is not attached anywhere yet.
    pub fn create_detached(&mut self, kind: NodeKind) -> NodeId {
        self.push(Node::new(kind))
    }

    pub fn create_detached_element(&mut self, tag: &str, attrs: Vec<(String, String)>) -> NodeId {
        let mut node = Node::new(NodeKind::Element);
        node.tag = tag.to_ascii_lowercase();
        node.attrs = attrs;
        self.push(node)
    }

    /// Attach a detached node as the last child of `parent`.
    pub fn attach(&mut self, parent: NodeId, child: NodeId) {
        self.detach(child);
        self.nodes[child as usize].parent = Some(parent);
        self.nodes[parent as usize].children.push(child);
    }

    /// Attach a detached node immediately before `sibling`.
    pub fn insert_before(&mut self, sibling: NodeId, child: NodeId) {
        let parent = self.nodes[sibling as usize]
            .parent
            .expect("insert_before: sibling has no parent");
        self.detach(child);
        let siblings = &mut self.nodes[parent as usize].children;
        let pos = siblings
            .iter()
            .position(|&s| s == sibling)
            .expect("insert_before: sibling not in parent");
        siblings.insert(pos, child);
        self.nodes[child as usize].parent = Some(parent);
    }

    /// Remove a node from its parent (the node stays in the arena).
    pub fn detach(&mut self, child: NodeId) {
        if let Some(parent) = self.nodes[child as usize].parent.take() {
            self.nodes[parent as usize].children.retain(|&c| c != child);
        }
    }

    /// Insert character data immediately before `sibling`, merging into
    /// a preceding text sibling when one exists.
    pub fn insert_text_before(&mut self, sibling: NodeId, data: &str) {
        let parent = self.nodes[sibling as usize]
            .parent
            .expect("insert_text_before: sibling has no parent");
        let siblings = self.nodes[parent as usize].children.clone();
        let pos = siblings
            .iter()
            .position(|&s| s == sibling)
            .expect("insert_text_before: sibling not in parent");
        if pos > 0 {
            let prev = siblings[pos - 1];
            if self.nodes[prev as usize].kind == NodeKind::Text {
                self.nodes[prev as usize].data.push_str(data);
                return;
            }
        }
        let mut node = Node::new(NodeKind::Text);
        node.data = data.to_string();
        let id = self.push(node);
        self.nodes[id as usize].parent = Some(parent);
        self.nodes[parent as usize].children.insert(pos, id);
    }

    pub fn add_text(&mut self, parent: NodeId, data: &str) -> NodeId {
        // Consecutive character data is merged into one text node, the way
        // libxml2 and html5ever both coalesce adjacent text.
        if let Some(&last) = self.nodes[parent as usize].children.last() {
            if self.nodes[last as usize].kind == NodeKind::Text {
                self.nodes[last as usize].data.push_str(data);
                return last;
            }
        }
        let mut node = Node::new(NodeKind::Text);
        node.data = data.to_string();
        self.append_child(parent, node)
    }

    pub fn add_comment(&mut self, parent: NodeId, data: &str) -> NodeId {
        let mut node = Node::new(NodeKind::Comment);
        node.data = data.to_string();
        self.append_child(parent, node)
    }

    pub fn add_doctype(&mut self, parent: NodeId, data: &str) -> NodeId {
        let mut node = Node::new(NodeKind::Doctype);
        node.data = data.to_string();
        self.append_child(parent, node)
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).parent
    }

    pub fn children(&self, id: NodeId) -> &[NodeId] {
        &self.node(id).children
    }

    pub fn document_element(&self) -> Option<NodeId> {
        self.children(self.root)
            .iter()
            .copied()
            .find(|&c| self.node(c).is_element())
    }

    pub fn attr(&self, id: NodeId, name: &str) -> Option<&str> {
        self.node(id)
            .attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn element_children(&self, id: NodeId) -> Vec<NodeId> {
        self.children(id)
            .iter()
            .copied()
            .filter(|&c| self.node(c).is_element())
            .collect()
    }

    /// Element siblings including `id` itself (elements only), in order.
    pub fn element_siblings(&self, id: NodeId) -> Vec<NodeId> {
        match self.parent(id) {
            Some(p) => self.element_children(p),
            None => vec![id],
        }
    }

    pub fn next_element_sibling(&self, id: NodeId) -> Option<NodeId> {
        let sibs = self.element_siblings(id);
        let pos = sibs.iter().position(|&s| s == id)?;
        sibs.get(pos + 1).copied()
    }

    pub fn prev_element_sibling(&self, id: NodeId) -> Option<NodeId> {
        let sibs = self.element_siblings(id);
        let pos = sibs.iter().position(|&s| s == id)?;
        pos.checked_sub(1).map(|p| sibs[p])
    }

    /// lxml `.text`: content of the first child when it is a text node.
    pub fn text(&self, id: NodeId) -> Option<&str> {
        let first = self.children(id).first().copied()?;
        let node = self.node(first);
        if node.kind == NodeKind::Text {
            Some(&node.data)
        } else {
            None
        }
    }

    /// lxml `.tail`: content of the next sibling when it is a text node.
    pub fn tail(&self, id: NodeId) -> Option<&str> {
        let parent = self.parent(id)?;
        let sibs = self.children(parent);
        let pos = sibs.iter().position(|&s| s == id)?;
        let next = sibs.get(pos + 1).copied()?;
        let node = self.node(next);
        if node.kind == NodeKind::Text {
            Some(&node.data)
        } else {
            None
        }
    }

    /// All descendant character data concatenated in document order.
    /// Comment content is excluded, matching lxml `text_content()` closely
    /// enough for extraction use; script and style text is included.
    pub fn text_content(&self, id: NodeId) -> String {
        let mut out = String::new();
        self.collect_text(id, &mut out);
        out
    }

    fn collect_text(&self, id: NodeId, out: &mut String) {
        for &child in self.children(id) {
            let node = self.node(child);
            match node.kind {
                NodeKind::Text => out.push_str(&node.data),
                NodeKind::Element => self.collect_text(child, out),
                _ => {}
            }
        }
    }

    /// Every node in document order (pre-order), starting at `id`.
    pub fn descendants_or_self(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        self.walk(id, &mut out);
        out
    }

    fn walk(&self, id: NodeId, out: &mut Vec<NodeId>) {
        out.push(id);
        for &child in self.children(id) {
            self.walk(child, out);
        }
    }

    pub fn ancestors(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut cur = self.parent(id);
        while let Some(p) = cur {
            out.push(p);
            cur = self.parent(p);
        }
        out
    }

    pub fn element_count(&self) -> usize {
        self.nodes.iter().filter(|n| n.is_element()).count()
    }

    /// 1-based index of `id` among same-tag element siblings (0 when not an
    /// element). Used by generated XPath and by fixture identities.
    pub fn same_tag_index(&self, id: NodeId) -> usize {
        let node = self.node(id);
        if !node.is_element() {
            return 0;
        }
        let mut idx = 0;
        for sib in self.element_siblings(id) {
            if self.node(sib).tag == node.tag {
                idx += 1;
            }
            if sib == id {
                return idx;
            }
        }
        idx
    }

    /// Absolute element path like `/html/body/div[2]/span`, where `[n]` is
    /// the 1-based same-tag index and is omitted when it is 1 and the tag
    /// is unique among siblings. This matches the shape of generated full
    /// XPath selectors closely enough to identify elements in fixtures.
    pub fn element_path(&self, id: NodeId) -> String {
        let mut parts: Vec<String> = Vec::new();
        let mut cur = Some(id);
        while let Some(n) = cur {
            let node = self.node(n);
            if node.is_element() {
                let same: Vec<NodeId> = self
                    .element_siblings(n)
                    .into_iter()
                    .filter(|&s| self.node(s).tag == node.tag)
                    .collect();
                if same.len() > 1 {
                    parts.push(format!("{}[{}]", node.tag, self.same_tag_index(n)));
                } else {
                    parts.push(node.tag.clone());
                }
            }
            cur = self.parent(n);
        }
        parts.reverse();
        format!("/{}", parts.join("/"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (Dom, NodeId, NodeId) {
        let mut dom = Dom::new();
        let html = dom.add_element(dom.root, "html", vec![]);
        let body = dom.add_element(html, "body", vec![]);
        let div = dom.add_element(body, "div", vec![("id".to_string(), "x".to_string())]);
        dom.add_text(div, "before");
        let span = dom.add_element(div, "span", vec![]);
        dom.add_text(span, "inner");
        dom.add_text(div, "after");
        (dom, div, span)
    }

    #[test]
    fn text_and_tail_follow_lxml() {
        let (dom, div, span) = sample();
        assert_eq!(dom.text(div), Some("before"));
        assert_eq!(dom.text(span), Some("inner"));
        assert_eq!(dom.tail(span), Some("after"));
        assert_eq!(dom.tail(div), None);
    }

    #[test]
    fn text_content_concatenates() {
        let (dom, div, _) = sample();
        assert_eq!(dom.text_content(div), "beforeinnerafter");
    }

    #[test]
    fn element_path_uses_same_tag_index() {
        let mut dom = Dom::new();
        let html = dom.add_element(dom.root, "html", vec![]);
        let body = dom.add_element(html, "body", vec![]);
        let _d1 = dom.add_element(body, "div", vec![]);
        let d2 = dom.add_element(body, "div", vec![]);
        assert_eq!(dom.element_path(d2), "/html/body/div[2]");
    }
}

pub mod html5_backend;
pub mod libxml_backend;
