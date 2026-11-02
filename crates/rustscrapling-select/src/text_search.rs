//! Text-based search: ports of Scrapling's `find_by_text` and
//! `find_by_regex`, plus the `clean_spaces` / `TextHandler.clean` text
//! normalization they (and the adaptive scorer) rely on.

use regex::RegexBuilder;
use rustscrapling_dom::{Dom, NodeId};

/// Python `str.strip()` character set: Unicode whitespace plus the C0
/// separators \x1c-\x1f that Python also treats as whitespace.
fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

pub fn py_strip(s: &str) -> &str {
    s.trim_matches(is_py_space)
}

/// Port of Scrapling's `clean_spaces`: tabs become spaces, newlines and
/// carriage returns are deleted, runs of spaces collapse to one.
pub fn clean_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_space = false;
    for c in s.chars() {
        match c {
            '\n' | '\r' => {}
            '\t' | ' ' => {
                if !last_was_space {
                    out.push(' ');
                }
                last_was_space = true;
            }
            _ => {
                out.push(c);
                last_was_space = false;
            }
        }
    }
    out
}

/// Port of `TextHandler.clean()`: `clean_spaces` plus stripping.
pub fn clean_text(s: &str) -> String {
    py_strip(&clean_spaces(s)).to_string()
}

/// Elements below `scope` whose own text has non-whitespace content:
/// XPath `.//*[normalize-space(text())]`, the candidate set both
/// find_by_text and find_by_regex use. Implemented as a direct walk
/// (semantics: the element's first text-node child contains a character
/// other than space/tab/CR/LF, which is exactly what normalize-space
/// tests); the stage fixtures assert it agrees with the XPath form.
fn targets_with_text(dom: &Dom, scope: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    for id in dom.descendants_or_self(scope) {
        if id == scope || !dom.node(id).is_element() {
            continue;
        }
        let has_text = dom
            .children(id)
            .iter()
            .find(|&&c| dom.node(c).kind == rustscrapling_dom::NodeKind::Text)
            .map(|&c| {
                dom.node(c)
                    .data
                    .chars()
                    .any(|ch| !matches!(ch, ' ' | '\t' | '\n' | '\r'))
            })
            .unwrap_or(false);
        if has_text {
            out.push(id);
        }
    }
    out
}

#[derive(Debug, Clone, Copy)]
pub struct FindByTextOpts {
    pub first_match: bool,
    pub partial: bool,
    pub case_sensitive: bool,
    pub clean_match: bool,
}

impl Default for FindByTextOpts {
    fn default() -> Self {
        FindByTextOpts {
            first_match: true,
            partial: false,
            case_sensitive: false,
            clean_match: true,
        }
    }
}

/// Port of `Selector.find_by_text`. Returns all matches, or just the
/// first when `first_match` is set (Scrapling returns a single Selector
/// in that case; the Vec shape keeps one API for both).
pub fn find_by_text(dom: &Dom, scope: NodeId, text: &str, opts: FindByTextOpts) -> Vec<NodeId> {
    let query = if opts.case_sensitive {
        text.to_string()
    } else {
        text.to_lowercase()
    };
    let mut results = Vec::new();
    for node in targets_with_text(dom, scope) {
        let mut node_text = dom.text(node).unwrap_or("").to_string();
        if opts.clean_match {
            node_text = clean_text(&node_text);
        }
        if !opts.case_sensitive {
            node_text = node_text.to_lowercase();
        }
        let hit = if opts.partial {
            node_text.contains(&query)
        } else {
            node_text == query
        };
        if hit {
            results.push(node);
            if opts.first_match {
                break;
            }
        }
    }
    results
}

#[derive(Debug, Clone, Copy)]
pub struct FindByRegexOpts {
    pub first_match: bool,
    pub case_sensitive: bool,
    pub clean_match: bool,
}

impl Default for FindByRegexOpts {
    fn default() -> Self {
        FindByRegexOpts {
            first_match: true,
            case_sensitive: false,
            clean_match: true,
        }
    }
}

/// Port of `Selector.find_by_regex`. The pattern language differs from
/// Python's `re` (no look-around, `(?<name>..)` groups); see
/// docs/COMPAT.md.
pub fn find_by_regex(
    dom: &Dom,
    scope: NodeId,
    pattern: &str,
    opts: FindByRegexOpts,
) -> Result<Vec<NodeId>, regex::Error> {
    let re = RegexBuilder::new(pattern)
        .case_insensitive(!opts.case_sensitive)
        .build()?;
    let mut results = Vec::new();
    for node in targets_with_text(dom, scope) {
        let raw = dom.text(node).unwrap_or("");
        let input = if opts.clean_match {
            clean_text(raw)
        } else {
            raw.to_string()
        };
        if re.is_match(&input) {
            results.push(node);
            if opts.first_match {
                break;
            }
        }
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_semantics() {
        assert_eq!(clean_spaces("a\t\tb\nc\r d  e"), "a bc d e");
        assert_eq!(clean_text("  a \n b  "), "a b");
        assert_eq!(clean_spaces("no change"), "no change");
        // clean_spaces does not strip
        assert_eq!(clean_spaces("  x  "), " x ");
    }
}
