//! Similarity scoring and the adaptive searches: `relocate` and
//! `find_similar`, faithful ports of Scrapling's `parser.py` logic.
//!
//! The scorer computes exactly the quantities of
//! `Selector.__calculate_similarity_score`, but reads them from the Dom
//! through borrowed views instead of materializing an ElementDict per
//! candidate (the dict form is only needed for storage). Every ratio and
//! rounding step is unchanged, so scores are bit-identical.

use std::collections::HashMap;
use std::sync::Mutex;

use rayon::prelude::*;
use rustscrapling_dom::{Dom, NodeId, NodeKind};
use rustscrapling_select::text_search::clean_spaces;

use crate::dict::{py_strip, ElementDict, SibTag};
use crate::seqmatch::{py_round2, seq_ratio, str_ratio, SequenceMatcher};

type AttrView<'a> = smallvec::SmallVec<[(&'a str, &'a str); 8]>;
type ParentStats = (f64, f64, Option<f64>, u32);

fn dict_diff_views(dict1: &AttrView, dict2: &AttrView) -> f64 {
    let keys1: smallvec::SmallVec<[&str; 8]> = dict1.iter().map(|(k, _)| *k).collect();
    let keys2: smallvec::SmallVec<[&str; 8]> = dict2.iter().map(|(k, _)| *k).collect();
    let values1: smallvec::SmallVec<[&str; 8]> = dict1.iter().map(|(_, v)| *v).collect();
    let values2: smallvec::SmallVec<[&str; 8]> = dict2.iter().map(|(_, v)| *v).collect();
    let mut score = SequenceMatcher::new(&keys1, &keys2).ratio() * 0.5;
    score += SequenceMatcher::new(&values1, &values2).ratio() * 0.5;
    score
}

/// A sibling tag as a borrowed view: scoring compares thousands of
/// these, so they must not carry allocation or refcount costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, std::hash::Hash)]
enum SibRef<'a> {
    Tag(&'a str),
    Other,
}

/// Memoization for the repeated sub-computations in a relocate sweep.
/// Every cached value is a pure function of its key and of the fixed
/// original dict, and components are cached separately so the caller
/// adds them in the original order - scores stay bit-identical to the
/// uncached computation; this only avoids recomputing the same ratio
/// for the thousands of candidates that share a parent or a sibling
/// list on real pages.
#[derive(Default)]
pub struct ScoreCaches<'a> {
    /// Parent stage components by candidate-parent node:
    /// (tag ratio, attributes dict diff, parent text ratio, checks).
    parents: Mutex<HashMap<Option<NodeId>, ParentStats>>,
    /// Siblings ratio by the candidate's sibling list.
    siblings: Mutex<HashMap<Vec<SibRef<'a>>, f64>>,
}

fn sib_refs(siblings: &[SibTag]) -> Vec<SibRef<'_>> {
    siblings
        .iter()
        .map(|s| match s {
            SibTag::Tag(t) => SibRef::Tag(t),
            SibTag::Other => SibRef::Other,
        })
        .collect()
}

fn view_lookup<'a>(attrs: &AttrView<'a>, key: &str) -> Option<&'a str> {
    attrs.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// The candidate's cleaned attributes as borrowed views: values
/// stripped, empty values dropped, document order (the same cleaning
/// `_StorageTools.__clean_attributes` applies in element_to_dict).
fn cleaned_attr_view<'a>(dom: &'a Dom, id: NodeId) -> AttrView<'a> {
    dom.node(id)
        .attrs
        .iter()
        .filter_map(|(k, v)| {
            let stripped = py_strip(v);
            if stripped.is_empty() {
                None
            } else {
                Some((k.as_str(), stripped))
            }
        })
        .collect()
}

fn raw_attr_view<'a>(dom: &'a Dom, id: NodeId) -> AttrView<'a> {
    dom.node(id)
        .attrs
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect()
}

/// element_to_dict's text field as a borrowed slice: the element's own
/// text stripped, or "" when there is none.
fn text_view(dom: &Dom, id: NodeId) -> &str {
    match dom.text(id) {
        Some(t) if !t.is_empty() => py_strip(t),
        _ => "",
    }
}

fn element_parent(dom: &Dom, id: NodeId) -> Option<NodeId> {
    dom.parent(id)
        .filter(|&p| dom.node(p).kind == NodeKind::Element)
}

fn truthy(t: &Option<String>) -> Option<&str> {
    t.as_deref().filter(|s| !s.is_empty())
}

/// Port of `Selector.__calculate_similarity_score`. Returns the score
/// rounded to 2 decimals with Python's rounding, exactly like the
/// original.
pub fn calculate_similarity_score(dom: &Dom, original: &ElementDict, candidate: NodeId) -> f64 {
    let refs: Option<Vec<SibRef>> = original.siblings.as_ref().map(|s| sib_refs(s));
    let uniform = uniform_tag(refs.as_deref());
    let mut scratch = Vec::new();
    score_inner(
        dom,
        original,
        refs.as_deref(),
        uniform,
        candidate,
        &mut scratch,
        None,
    )
}

fn uniform_tag<'a>(siblings: Option<&[SibRef<'a>]>) -> Option<SibRef<'a>> {
    let siblings = siblings?;
    let first = *siblings.first()?;
    siblings.iter().all(|s| *s == first).then_some(first)
}

/// A common adaptive case is a long list of identical sibling tags. When
/// the original list is uniform, a candidate with no matching sibling tags
/// has ratio 0, while an equally long, uniform list has ratio 1. Both are
/// exact SequenceMatcher results and need no temporary sibling allocation.
fn uniform_sibling_ratio(
    dom: &Dom,
    candidate: NodeId,
    original_len: usize,
    original_tag: SibRef<'_>,
) -> Option<f64> {
    let Some(parent) = element_parent(dom, candidate) else {
        return Some(0.0);
    };
    let mut count = 0;
    let mut seen_matching = false;
    let mut all_matching = true;
    for &child in dom.children(parent) {
        if child == candidate {
            continue;
        }
        let tag = match dom.node(child).kind {
            NodeKind::Element => SibRef::Tag(dom.node(child).tag.as_str()),
            NodeKind::Comment => SibRef::Other,
            _ => continue,
        };
        count += 1;
        if tag == original_tag {
            seen_matching = true;
        } else {
            all_matching = false;
        }
    }
    if !seen_matching {
        Some(0.0)
    } else if all_matching && count == original_len {
        Some(1.0)
    } else {
        None
    }
}

fn score_inner<'a>(
    dom: &'a Dom,
    original: &ElementDict,
    original_siblings: Option<&[SibRef<'a>]>,
    uniform_sibling_tag: Option<SibRef<'a>>,
    candidate: NodeId,
    scratch: &mut Vec<SibRef<'a>>,
    caches: Option<&ScoreCaches<'a>>,
) -> f64 {
    let node = dom.node(candidate);
    let mut score: f64 = 0.0;
    let mut checks: u32 = 0;

    score += if original.tag == node.tag { 1.0 } else { 0.0 };
    checks += 1;

    if let Some(text) = truthy(&original.text) {
        score += str_ratio(text, text_view(dom, candidate));
        checks += 1;
    }

    let candidate_attrs = cleaned_attr_view(dom, candidate);
    let original_attrs: AttrView = original
        .attributes
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    score += dict_diff_views(&original_attrs, &candidate_attrs);
    checks += 1;

    for attrib in ["class", "id", "href", "src"] {
        if let Some(value) = view_lookup(&original_attrs, attrib).filter(|v| !v.is_empty()) {
            score += str_ratio(value, view_lookup(&candidate_attrs, attrib).unwrap_or(""));
            checks += 1;
        }
    }

    // Candidate path as borrowed tag views, root first.
    let mut candidate_path: smallvec::SmallVec<[&str; 12]> = smallvec::smallvec![node.tag.as_str()];
    let mut cur = element_parent(dom, candidate);
    while let Some(p) = cur {
        candidate_path.push(dom.node(p).tag.as_str());
        cur = element_parent(dom, p);
    }
    candidate_path.reverse();
    let original_path: smallvec::SmallVec<[&str; 12]> =
        original.path.iter().map(|s| s.as_str()).collect();
    score += seq_ratio(&original_path, &candidate_path);
    checks += 1;

    if let Some(parent_name) = truthy(&original.parent_name) {
        let parent = element_parent(dom, candidate);
        let components = caches.and_then(|c| c.parents.lock().unwrap().get(&parent).copied());
        let (tag_ratio, parent_diff, parent_text_ratio, parent_checks) = match components {
            Some(cached) => cached,
            None => {
                let computed = match parent {
                    Some(parent) => {
                        let tag_ratio = str_ratio(parent_name, &dom.node(parent).tag);
                        let original_parent_attrs: AttrView = original
                            .parent_attribs
                            .iter()
                            .map(|(k, v)| (k.as_str(), v.as_str()))
                            .collect();
                        let candidate_parent_attrs = raw_attr_view(dom, parent);
                        let parent_diff =
                            dict_diff_views(&original_parent_attrs, &candidate_parent_attrs);
                        let mut checks = 2u32;
                        let parent_text_ratio = truthy(&original.parent_text).map(|pt| {
                            checks += 1;
                            str_ratio(pt, text_view(dom, parent))
                        });
                        (tag_ratio, parent_diff, parent_text_ratio, checks)
                    }
                    None => (0.0, 0.0, None, 0),
                };
                if let Some(c) = caches {
                    c.parents.lock().unwrap().insert(parent, computed);
                }
                computed
            }
        };
        // Added in the original order (tag, attributes, text).
        if parent_checks > 0 {
            score += tag_ratio;
            score += parent_diff;
            if let Some(r) = parent_text_ratio {
                score += r;
            }
            checks += parent_checks;
        }
    }

    if let Some(siblings) = original_siblings {
        if !siblings.is_empty() {
            let ratio = match uniform_sibling_tag
                .and_then(|tag| uniform_sibling_ratio(dom, candidate, siblings.len(), tag))
            {
                Some(ratio) => ratio,
                None => {
                    candidate_siblings_into(dom, candidate, scratch);
                    if siblings == scratch.as_slice() {
                        1.0
                    } else if let Some(c) = caches {
                        match c.siblings.lock().unwrap().get(scratch.as_slice()) {
                            Some(&r) => r,
                            None => {
                                let r = seq_ratio(siblings, scratch);
                                if scratch.len() <= 64 {
                                    c.siblings.lock().unwrap().insert(scratch.clone(), r);
                                }
                                r
                            }
                        }
                    } else {
                        seq_ratio(siblings, scratch)
                    }
                }
            };
            score += ratio;
            checks += 1;
        }
    }

    py_round2((score / checks as f64) * 100.0)
}

/// The candidate's sibling tags (elements and comments), excluding the
/// candidate itself - the `siblings` field of element_to_dict, built
/// only when the score needs it, into a caller-owned scratch buffer.
fn candidate_siblings_into<'a>(dom: &'a Dom, id: NodeId, out: &mut Vec<SibRef<'a>>) {
    out.clear();
    let Some(parent) = element_parent(dom, id) else {
        return;
    };
    out.extend(
        dom.children(parent)
            .iter()
            .copied()
            .filter(|&c| c != id)
            .filter_map(|c| match dom.node(c).kind {
                NodeKind::Element => Some(SibRef::Tag(dom.node(c).tag.as_str())),
                NodeKind::Comment => Some(SibRef::Other),
                _ => None,
            }),
    );
}

/// Port of `Selector.relocate`: score every element below `scope`
/// (the scope element itself is not a candidate, matching XPath `.//*`),
/// in parallel, and return the elements tied at the highest score when
/// that score reaches `percentage`.
pub fn relocate(dom: &Dom, scope: NodeId, original: &ElementDict, percentage: i64) -> Vec<NodeId> {
    let candidates: Vec<NodeId> = dom
        .descendants_or_self(scope)
        .into_iter()
        .skip(1)
        .filter(|&id| dom.node(id).is_element())
        .collect();
    if candidates.is_empty() {
        return Vec::new();
    }
    let original_siblings: Option<Vec<SibRef>> = original.siblings.as_ref().map(|s| sib_refs(s));
    let uniform = uniform_tag(original_siblings.as_deref());
    let caches = ScoreCaches::default();
    let scores: Vec<f64> = candidates
        .par_iter()
        .map_init(Vec::new, |scratch, &c| {
            score_inner(
                dom,
                original,
                original_siblings.as_deref(),
                uniform,
                c,
                scratch,
                Some(&caches),
            )
        })
        .collect();
    let highest = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if highest >= percentage as f64 {
        candidates
            .into_iter()
            .zip(scores)
            .filter(|(_, s)| *s == highest)
            .map(|(c, _)| c)
            .collect()
    } else {
        Vec::new()
    }
}

fn get_attributes<'a>(dom: &'a Dom, id: NodeId, ignore: &[&str]) -> AttrView<'a> {
    dom.node(id)
        .attrs
        .iter()
        .filter(|(k, _)| !ignore.contains(&k.as_str()))
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect()
}

/// Port of `Selector.__are_alike`.
#[expect(
    clippy::too_many_arguments,
    reason = "preserve upstream similarity scoring inputs without an allocation-heavy context object"
)]
fn are_alike<'a>(
    dom: &'a Dom,
    original: NodeId,
    original_attributes: &AttrView<'a>,
    candidate: NodeId,
    ignore_attributes: &[&str],
    similarity_threshold: f64,
    match_text: bool,
    ratios: &mut HashMap<(&'a str, &'a str), f64>,
) -> bool {
    let candidate_attributes = get_attributes(dom, candidate, ignore_attributes);
    let mut score: f64 = 0.0;
    let mut checks: usize = 0;

    if !original_attributes.is_empty() {
        for &(k, v) in original_attributes {
            let candidate_value = candidate_attributes
                .iter()
                .find(|(ck, _)| *ck == k)
                .map(|(_, cv)| *cv)
                .unwrap_or("");
            score += *ratios
                .entry((v, candidate_value))
                .or_insert_with(|| str_ratio(v, candidate_value));
        }
        checks += original_attributes.len().max(candidate_attributes.len());
    } else if candidate_attributes.is_empty() {
        score += 1.0;
        checks += 1;
    }

    if match_text {
        let original_text = clean_spaces(dom.text(original).unwrap_or(""));
        let candidate_text = clean_spaces(dom.text(candidate).unwrap_or(""));
        score += str_ratio(&original_text, &candidate_text);
        checks += 1;
    }

    if checks > 0 {
        py_round2(score / checks as f64) >= similarity_threshold
    } else {
        false
    }
}

/// Port of `Selector.find_similar`.
pub fn find_similar(
    dom: &Dom,
    node: NodeId,
    similarity_threshold: f64,
    ignore_attributes: &[&str],
    match_text: bool,
) -> Vec<NodeId> {
    let depth = dom
        .ancestors(node)
        .iter()
        .filter(|&&a| dom.node(a).is_element())
        .count();
    let target_attrs = get_attributes(dom, node, ignore_attributes);

    // Scrapling builds the XPath `//grandparent/parent/tag
    // [count(ancestor::*) = depth]` and evaluates it from the document
    // root. The direct walk below selects exactly the same elements:
    // tag chain at the top of the ancestor axis and total depth equal.
    let mut path_parts = vec![dom.node(node).tag.clone()];
    if let Some(parent) = dom.parent(node).filter(|&p| dom.node(p).is_element()) {
        path_parts.insert(0, dom.node(parent).tag.clone());
        if let Some(grandparent) = dom.parent(parent).filter(|&p| dom.node(p).is_element()) {
            path_parts.insert(0, dom.node(grandparent).tag.clone());
        }
    }
    let Some(root) = dom.document_element() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // Many elements in a product list share attribute values. Reuse exact
    // SequenceMatcher results when identical value pairs recur.
    let mut ratios = HashMap::new();
    for candidate in dom.descendants_or_self(root) {
        if candidate == node || !dom.node(candidate).is_element() {
            continue;
        }
        if dom.node(candidate).tag != path_parts[path_parts.len() - 1] {
            continue;
        }
        // Depth check and tag chain via parent walks (no allocation):
        // ancestors must number `depth`, and the nearest ones must be
        // the parent/grandparent tags of path_parts, in order.
        let mut chain_ok = true;
        let mut ancestor_count = 0usize;
        let mut cur = element_parent(dom, candidate);
        let mut chain_iter = path_parts[..path_parts.len() - 1].iter().rev();
        while let Some(p) = cur {
            if let Some(part) = chain_iter.next() {
                if &dom.node(p).tag != part {
                    chain_ok = false;
                    break;
                }
            }
            ancestor_count += 1;
            cur = element_parent(dom, p);
        }
        if !chain_ok || ancestor_count != depth {
            continue;
        }
        if are_alike(
            dom,
            node,
            &target_attrs,
            candidate,
            ignore_attributes,
            similarity_threshold,
            match_text,
            &mut ratios,
        ) {
            out.push(candidate);
        }
    }
    out
}
