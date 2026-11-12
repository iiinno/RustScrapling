//! Selection engines for RustScrapling: CSS (cssselect-compatible),
//! XPath 1.0, and selector generation, all over the arena DOM.

use rustscrapling_dom::{Dom, NodeId};

pub mod css;
pub mod generate;
pub mod public_selector;
pub mod text_search;
pub mod xpath;

pub use public_selector::{
    FindFilter, PublicSelectorError, RegexOptions, RegexPattern, Selector, TextOptions,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SelectionNode {
    Node(NodeId),
    Attribute(NodeId, String),
}

impl SelectionNode {
    fn selection(&self, dom: &Dom) -> Selection {
        match self {
            Self::Node(id) => match dom.node(*id).kind {
                rustscrapling_dom::NodeKind::Text | rustscrapling_dom::NodeKind::Comment => {
                    Selection::Text(dom.node(*id).data.clone())
                }
                _ => Selection::Element(*id),
            },
            Self::Attribute(owner, name) => {
                Selection::Text(dom.attr(*owner, name).unwrap_or_default().to_owned())
            }
        }
    }
}

/// One selected item. CSS `::text` and `::attr(name)` and XPath
/// `text()` / `@attr` results come back as plain strings, the way
/// Scrapling wraps them as text pseudo-elements.
#[derive(Debug, Clone, PartialEq)]
pub enum Selection {
    Element(NodeId),
    Text(String),
}

impl Selection {
    /// The string a caller gets from `.get()`: text content for
    /// elements, the string itself for text results.
    pub fn get(&self, dom: &Dom) -> String {
        match self {
            Selection::Element(id) => dom.text_content(*id),
            Selection::Text(s) => s.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SelectError {
    /// The selector could not be parsed. Scrapling surfaces these as
    /// SelectorSyntaxError, so callers can rely on the category.
    Syntax(String),
    /// The expression evaluated to something Scrapling's API cannot
    /// return (for example a scalar from XPath). Scrapling raises
    /// TypeError there.
    Type(String),
}

impl std::fmt::Display for SelectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SelectError::Syntax(m) => write!(f, "selector syntax error: {}", m),
            SelectError::Type(m) => write!(f, "selector type error: {}", m),
        }
    }
}

impl std::error::Error for SelectError {}

pub type Result<T> = std::result::Result<T, SelectError>;

pub(crate) fn syntax<T>(msg: impl Into<String>) -> Result<T> {
    Err(SelectError::Syntax(msg.into()))
}
