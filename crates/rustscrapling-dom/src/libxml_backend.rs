//! Compat-mode parser: libxml2 (the same engine and version as the
//! lxml copy bundled with the oracle) building the arena DOM.
//!
//! Parser options mirror Scrapling's `parser.py`:
//! `recover=True, remove_blank_text=True, remove_comments=(not keep_comments),
//! encoding="utf-8", huge_tree=True`.
//! NUL bytes are removed from the input first, as lxml refuses them and
//! Scrapling input is expected to be cleaned the same way.
//!
//! The tree walk uses the raw libxml2 structures through the `libxml`
//! crate's public bindings. The safe wrapper allocates a vector and
//! strings for every node, which roughly doubled the parse time on the
//! Appendix-A page; the raw walk keeps the whole parse inside the
//! stage-1 budget.

use crate::{Dom, NodeId};
use libxml::bindings::{
    xmlAttr, xmlElementType, xmlElementType_XML_CDATA_SECTION_NODE,
    xmlElementType_XML_COMMENT_NODE, xmlElementType_XML_DOCUMENT_TYPE_NODE,
    xmlElementType_XML_DTD_NODE, xmlElementType_XML_ELEMENT_NODE, xmlElementType_XML_TEXT_NODE,
    xmlNode,
};
use libxml::parser::{Parser, ParserOptions};
use std::ffi::CStr;

pub struct CompatOptions {
    pub keep_comments: bool,
}

impl Default for CompatOptions {
    fn default() -> Self {
        CompatOptions {
            keep_comments: false,
        }
    }
}

pub fn parse_html(input: &str, opts: &CompatOptions) -> Result<Dom, String> {
    // Byte-level NUL removal: NUL is a single byte in UTF-8, and the
    // common case (no NUL present) avoids copying the input entirely.
    let cleaned: std::borrow::Cow<'_, [u8]> = if input.as_bytes().contains(&0) {
        std::borrow::Cow::Owned(input.bytes().filter(|b| *b != 0).collect())
    } else {
        std::borrow::Cow::Borrowed(input.as_bytes())
    };
    let parser = Parser::default_html();
    let options = ParserOptions {
        recover: true,
        no_error: true,
        no_warning: true,
        no_blanks: true,
        huge: true,
        encoding: Some("utf-8"),
        ..Default::default()
    };
    let doc = parser
        .parse_string_with_options(cleaned, options)
        .map_err(|e| format!("libxml2 parse failed: {:?}", e))?;
    let mut dom = Dom::new();
    unsafe {
        let doc_ptr = doc.doc_ptr() as *mut xmlNode;
        if !doc_ptr.is_null() {
            let mut child = (*doc_ptr).children;
            while !child.is_null() {
                convert(child, dom.root, &mut dom, opts.keep_comments);
                child = (*child).next;
            }
        }
    }
    Ok(dom)
}

unsafe fn convert(node_ptr: *mut xmlNode, parent: NodeId, dom: &mut Dom, keep_comments: bool) {
    let node_type: xmlElementType = (*node_ptr).type_;
    if node_type == xmlElementType_XML_ELEMENT_NODE {
        let tag = cstr_to_string((*node_ptr).name as *const i8);
        let id = dom.add_element(parent, &tag, ordered_attrs(node_ptr));
        let mut child = (*node_ptr).children;
        while !child.is_null() {
            convert(child, id, dom, keep_comments);
            child = (*child).next;
        }
    } else if node_type == xmlElementType_XML_TEXT_NODE
        || node_type == xmlElementType_XML_CDATA_SECTION_NODE
    {
        let content = cstr_to_string((*node_ptr).content as *const i8);
        if !content.is_empty() {
            dom.add_text(parent, &content);
        }
    } else if node_type == xmlElementType_XML_COMMENT_NODE {
        if keep_comments {
            let content = cstr_to_string((*node_ptr).content as *const i8);
            dom.add_comment(parent, &content);
        }
    } else if node_type == xmlElementType_XML_DOCUMENT_TYPE_NODE
        || node_type == xmlElementType_XML_DTD_NODE
    {
        let name = cstr_to_string((*node_ptr).name as *const i8);
        dom.add_doctype(parent, &name);
    }
    // Other node kinds (processing instructions, entity refs) do not
    // survive into the arena; Scrapling cannot select them either.
}

/// Attributes in document order, read from the libxml2 `xmlAttr` chain.
/// The safe wrapper only exposes attributes as a hash map, which would
/// lose the order that adaptive fingerprints depend on (Plan section 9.2).
unsafe fn ordered_attrs(node_ptr: *mut xmlNode) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut attr: *mut xmlAttr = (*node_ptr).properties;
    while !attr.is_null() {
        let name = cstr_to_string((*attr).name as *const i8);
        let mut value = String::new();
        let mut child = (*attr).children;
        while !child.is_null() {
            if !(*child).content.is_null() {
                value.push_str(&cstr_to_string((*child).content as *const i8));
            }
            child = (*child).next;
        }
        out.push((name, value));
        attr = (*attr).next;
    }
    out
}

unsafe fn cstr_to_string(ptr: *const i8) -> String {
    if ptr.is_null() {
        return String::new();
    }
    CStr::from_ptr(ptr).to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NodeKind;

    #[test]
    fn parses_like_lxml() {
        let html = "<html><body><div id='a'>hi<span>there</span> tail</div></body></html>";
        let dom = parse_html(html, &CompatOptions::default()).unwrap();
        let root = dom.document_element().unwrap();
        assert_eq!(dom.node(root).tag, "html");
        let divs: Vec<NodeId> = dom
            .descendants_or_self(dom.root)
            .into_iter()
            .filter(|&n| dom.node(n).tag == "div")
            .collect();
        assert_eq!(divs.len(), 1);
        assert_eq!(dom.attr(divs[0], "id"), Some("a"));
        assert_eq!(dom.text(divs[0]), Some("hi"));
        let spans: Vec<NodeId> = dom
            .descendants_or_self(dom.root)
            .into_iter()
            .filter(|&n| dom.node(n).tag == "span")
            .collect();
        assert_eq!(dom.tail(spans[0]), Some(" tail"));
    }

    #[test]
    fn no_implied_tbody_in_compat_mode() {
        // Verified against the oracle (Scrapling/lxml) on 2026-11-02:
        // css('tbody') finds 0 elements for this input, css('tr') finds 1.
        // libxml2's HTML parser does not insert tbody; html5ever does.
        // This is a classified default-vs-compat difference (docs/COMPAT.md).
        let html = "<html><body><table><tr><td>1</td></tr></table></body></html>";
        let dom = parse_html(html, &CompatOptions::default()).unwrap();
        let tags: Vec<String> = dom
            .descendants_or_self(dom.root)
            .into_iter()
            .filter(|&n| dom.node(n).is_element())
            .map(|n| dom.node(n).tag.clone())
            .collect();
        assert!(!tags.iter().any(|t| t == "tbody"), "tags: {:?}", tags);
        assert!(tags.iter().any(|t| t == "tr"), "tags: {:?}", tags);
    }

    #[test]
    fn whitespace_text_nodes_are_kept_like_lxml() {
        // Scrapling passes remove_blank_text=True to lxml, but libxml2's
        // HTML parser still keeps whitespace text nodes. Verified against
        // the oracle via fixtures/stage1.json (div.container::text returns
        // whitespace strings). The arena must keep them too.
        let html = "<html><body><div class=\"container\">\n  <p>First</p>\n  <p>Second</p>\n</div></body></html>";
        let dom = parse_html(html, &CompatOptions::default()).unwrap();
        let texts: Vec<String> = dom
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Text)
            .map(|n| n.data.clone())
            .collect();
        assert_eq!(texts, vec!["\n  ", "First", "\n  ", "Second", "\n"]);
    }

    #[test]
    fn attribute_order_is_document_order() {
        let html = "<html><body><div z='1' a='2' m='3'>x</div></body></html>";
        let dom = parse_html(html, &CompatOptions::default()).unwrap();
        let div = dom
            .descendants_or_self(dom.root)
            .into_iter()
            .find(|&n| dom.node(n).tag == "div")
            .unwrap();
        let names: Vec<&str> = dom
            .node(div)
            .attrs
            .iter()
            .map(|(k, _)| k.as_str())
            .collect();
        assert_eq!(names, vec!["z", "a", "m"]);
    }
}
