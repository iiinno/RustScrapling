//! Selector generation, a faithful port of Scrapling's
//! `SelectorsGeneration._general_selection` (core/mixins.py).
//!
//! Rules, mirroring the original: walk from the element up to (but not
//! including) `html`. An element with an `id` contributes `#id` (CSS),
//! `*[@id='...']` (full XPath) or `[@id='...']` (short XPath) and, for
//! short paths, terminates the walk. Other elements contribute their
//! tag, plus `:nth-of-type(n)` / `[n]` when they have same-tag siblings.

use crate::Result;
use rustscrapling_dom::{Dom, NodeId};

fn parent_element(dom: &Dom, el: NodeId) -> Option<NodeId> {
    let mut cur = dom.parent(el);
    while let Some(p) = cur {
        if dom.node(p).is_element() {
            return Some(p);
        }
        cur = dom.parent(p);
    }
    None
}

fn general_selection(dom: &Dom, el: NodeId, css: bool, full_path: bool) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut target = el;
    loop {
        let parent = parent_element(dom, target);
        if parent.is_none() {
            break;
        }
        if let Some(id) = dom.attr(target, "id") {
            if css {
                parts.push(format!("#{}", id));
                if !full_path {
                    let mut rev = parts.clone();
                    rev.reverse();
                    return rev.join(" > ");
                }
            } else if full_path {
                parts.push(format!("*[@id='{}']", id));
            } else {
                parts.push(format!("[@id='{}']", id));
                let mut rev = parts.clone();
                rev.reverse();
                return format!("//*{}", rev.join("/"));
            }
        } else {
            let tag = dom.node(target).tag.clone();
            let mut part = tag.clone();
            // Position among same-tag element siblings.
            let sibs: Vec<NodeId> = dom
                .element_siblings(target)
                .into_iter()
                .filter(|&s| dom.node(s).tag == tag)
                .collect();
            if sibs.len() > 1 {
                let position = sibs.iter().position(|&s| s == target).unwrap() + 1;
                if css {
                    part += &format!(":nth-of-type({})", position);
                } else {
                    part += &format!("[{}]", position);
                }
            }
            parts.push(part);
        }
        target = parent.unwrap();
        if dom.node(target).tag == "html" {
            break;
        }
    }
    let mut rev = parts.clone();
    rev.reverse();
    if css {
        rev.join(" > ")
    } else {
        format!("//{}", rev.join("/"))
    }
}

pub fn generate_css_selector(dom: &Dom, el: NodeId) -> Result<String> {
    Ok(general_selection(dom, el, true, false))
}

pub fn generate_full_css_selector(dom: &Dom, el: NodeId) -> Result<String> {
    Ok(general_selection(dom, el, true, true))
}

pub fn generate_xpath_selector(dom: &Dom, el: NodeId) -> Result<String> {
    Ok(general_selection(dom, el, false, false))
}

pub fn generate_full_xpath_selector(dom: &Dom, el: NodeId) -> Result<String> {
    Ok(general_selection(dom, el, false, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{css, xpath, Selection};
    use rustscrapling_dom::html5_backend::{parse_html, Html5Options};

    fn find_first(dom: &Dom, selector: &str) -> NodeId {
        match css::css(dom, dom.root, selector)
            .unwrap()
            .into_iter()
            .next()
        {
            Some(Selection::Element(id)) => id,
            other => panic!("no element for {}: {:?}", selector, other),
        }
    }

    #[test]
    fn generation_cases_from_scrapling_tests() {
        // (html body content, target css, expected css, expected xpath)
        let cases: Vec<(&str, &str, &str, &str)> = vec![
            (
                "<section><p>A</p><p>B</p></section>",
                "p:first-of-type",
                "body > section > p:nth-of-type(1)",
                "//body/section/p[1]",
            ),
            (
                "<section><p>A</p><p>B</p></section>",
                "p:nth-of-type(2)",
                "body > section > p:nth-of-type(2)",
                "//body/section/p[2]",
            ),
            (
                "<section><p>A</p></section><section><p>B</p></section>",
                "section:first-of-type > p",
                "body > section:nth-of-type(1) > p",
                "//body/section[1]/p",
            ),
            (
                "<section><p>A</p></section><section><p>B</p></section>",
                "section:nth-of-type(2) > p",
                "body > section:nth-of-type(2) > p",
                "//body/section[2]/p",
            ),
            (
                "<section><span>before</span><p>A</p><em>between</em><p>B</p></section>",
                "p:first-of-type",
                "body > section > p:nth-of-type(1)",
                "//body/section/p[1]",
            ),
            (
                "<section><span>before</span><p>A</p><em>between</em><p>B</p></section>",
                "p:nth-of-type(2)",
                "body > section > p:nth-of-type(2)",
                "//body/section/p[2]",
            ),
            (
                "<section><span>before</span><p>A</p><em>after</em></section>",
                "p",
                "body > section > p",
                "//body/section/p",
            ),
            (
                "<section><p>A</p><p>B</p></section><section><p>C</p></section>",
                "section:first-of-type > p:first-of-type",
                "body > section:nth-of-type(1) > p:nth-of-type(1)",
                "//body/section[1]/p[1]",
            ),
        ];
        for (body, target_css, expected_css, expected_xpath) in cases {
            let html = format!("<html><body>{}</body></html>", body);
            let dom = parse_html(&html, &Html5Options::default()).unwrap();
            let target = find_first(&dom, target_css);
            for full in [false, true] {
                let got_css = if full {
                    generate_full_css_selector(&dom, target).unwrap()
                } else {
                    generate_css_selector(&dom, target).unwrap()
                };
                assert_eq!(
                    got_css, expected_css,
                    "css full={} case {}",
                    full, target_css
                );
                let got_xpath = if full {
                    generate_full_xpath_selector(&dom, target).unwrap()
                } else {
                    generate_xpath_selector(&dom, target).unwrap()
                };
                assert_eq!(
                    got_xpath, expected_xpath,
                    "xpath full={} case {}",
                    full, target_css
                );
                // Round trip: the generated selector selects exactly the target.
                let back = css::css(&dom, dom.root, &got_css).unwrap();
                assert_eq!(
                    back,
                    vec![Selection::Element(target)],
                    "css round trip {}",
                    got_css
                );
                let back = xpath::xpath(&dom, dom.root, &got_xpath, &[]).unwrap();
                assert_eq!(
                    back,
                    vec![Selection::Element(target)],
                    "xpath round trip {}",
                    got_xpath
                );
            }
        }
    }

    #[test]
    fn generation_id_shortcuts() {
        let html = "<html><body><section id=\"target\"><p>A</p><p>B</p></section><section>C</section></body></html>";
        let dom = parse_html(html, &Html5Options::default()).unwrap();
        for target_css in ["#target", "#target > p:first-of-type"] {
            let target = find_first(&dom, target_css);
            let is_p = dom.node(target).tag == "p";
            for full in [false, true] {
                let got_css = if full {
                    generate_full_css_selector(&dom, target).unwrap()
                } else {
                    generate_css_selector(&dom, target).unwrap()
                };
                let mut expected_css = if full { "body > #target" } else { "#target" }.to_string();
                if is_p {
                    expected_css += " > p:nth-of-type(1)";
                }
                assert_eq!(got_css, expected_css, "css {} full={}", target_css, full);
                let got_xpath = if full {
                    generate_full_xpath_selector(&dom, target).unwrap()
                } else {
                    generate_xpath_selector(&dom, target).unwrap()
                };
                let mut expected_xpath = if full {
                    "//body/*[@id='target']"
                } else {
                    "//*[@id='target']"
                }
                .to_string();
                if is_p {
                    expected_xpath += "/p[1]";
                }
                assert_eq!(
                    got_xpath, expected_xpath,
                    "xpath {} full={}",
                    target_css, full
                );
            }
        }
    }
}
