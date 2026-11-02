use std::collections::BTreeMap;

use rustscrapling_dom::Dom;
use serde::Serialize;
use serde_json::Value;

use crate::sanitize::{has_excluded_ancestor, normalize, safe_url, visible_text};

/// Article metadata in a deterministic order. JSON-LD remains original data;
/// its presence does not grant the source the authority to issue instructions.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Metadata {
    pub title: Option<String>,
    pub description: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<String>,
    pub modified_at: Option<String>,
    pub canonical_url: Option<String>,
    pub open_graph: BTreeMap<String, String>,
    pub json_ld: Vec<Value>,
}

pub(crate) fn extract_metadata(dom: &Dom, page_url: Option<&str>) -> Metadata {
    let mut metadata = Metadata::default();
    let mut named = BTreeMap::<String, String>::new();
    let mut title_tag = None;
    let mut first_heading = None;
    let mut canonical = None;
    let mut time_date = None;

    for id in dom.descendants_or_self(dom.root) {
        let node = dom.node(id);
        match node.tag.as_str() {
            "title" => {
                if title_tag.is_none() {
                    title_tag = non_empty(visible_text(dom, id));
                }
            }
            "h1" => {
                if first_heading.is_none() && !has_excluded_ancestor(dom, id, dom.root) {
                    first_heading = non_empty(visible_text(dom, id));
                }
            }
            "meta" => {
                let key = dom
                    .attr(id, "property")
                    .or_else(|| dom.attr(id, "name"))
                    .or_else(|| dom.attr(id, "itemprop"))
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase();
                let value = dom
                    .attr(id, "content")
                    .and_then(|v| non_empty(normalize(v)));
                if let Some(value) = value {
                    if key.starts_with("og:") {
                        metadata
                            .open_graph
                            .entry(key.clone())
                            .or_insert(value.clone());
                    }
                    named.entry(key).or_insert(value);
                }
            }
            "link" => {
                if canonical.is_none()
                    && dom.attr(id, "rel").is_some_and(|rel| {
                        rel.split_whitespace()
                            .any(|value| value.eq_ignore_ascii_case("canonical"))
                    })
                {
                    canonical = dom
                        .attr(id, "href")
                        .and_then(|href| safe_url(href, page_url));
                }
            }
            "time" => {
                if time_date.is_none() && !has_excluded_ancestor(dom, id, dom.root) {
                    time_date = dom
                        .attr(id, "datetime")
                        .and_then(|v| non_empty(normalize(v)));
                }
            }
            "script" => {
                let kind = dom
                    .attr(id, "type")
                    .unwrap_or_default()
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .trim();
                if kind.eq_ignore_ascii_case("application/ld+json") {
                    let contents = dom.text_content(id);
                    // Bound hostile or accidentally huge JSON-LD blobs.
                    if contents.len() <= 1_048_576 {
                        if let Ok(value) = serde_json::from_str::<Value>(&contents) {
                            match value {
                                Value::Array(arr) => metadata.json_ld.extend(arr),
                                Value::Object(_) => metadata.json_ld.push(value),
                                _ => {}
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let structured = metadata
        .json_ld
        .iter()
        .flat_map(article_objects)
        .collect::<Vec<_>>();
    metadata.title = get(&named, &["og:title", "twitter:title"])
        .or_else(|| structured.iter().find_map(|o| json_text(o, &["headline"])))
        .or(first_heading)
        .or(title_tag)
        .or_else(|| structured.iter().find_map(|o| json_text(o, &["name"])));
    metadata.description = get(
        &named,
        &["og:description", "description", "twitter:description"],
    )
    .or_else(|| {
        structured
            .iter()
            .find_map(|o| json_text(o, &["description"]))
    });
    metadata.author = get(
        &named,
        &[
            "author",
            "article:author",
            "parsely-author",
            "dc.creator",
            "dcterms.creator",
        ],
    )
    .or_else(|| {
        structured
            .iter()
            .find_map(|o| o.get("author").and_then(author_name))
    });
    metadata.published_at = get(
        &named,
        &[
            "article:published_time",
            "datepublished",
            "date",
            "pubdate",
            "publishdate",
            "dc.date",
            "dcterms.issued",
        ],
    )
    .or_else(|| {
        structured
            .iter()
            .find_map(|o| json_text(o, &["datePublished", "dateCreated"]))
    })
    .or(time_date);
    metadata.modified_at = get(
        &named,
        &["article:modified_time", "datemodified", "last-modified"],
    )
    .or_else(|| {
        structured
            .iter()
            .find_map(|o| json_text(o, &["dateModified"]))
    });
    metadata.canonical_url = canonical
        .or_else(|| get(&named, &["og:url"]).and_then(|s| safe_url(&s, page_url)))
        .or_else(|| {
            structured.iter().find_map(|o| {
                o.get("mainEntityOfPage")
                    .and_then(|v| v.as_str().or_else(|| v.get("@id").and_then(Value::as_str)))
                    .and_then(|s| safe_url(s, page_url))
            })
        })
        .or_else(|| page_url.and_then(|value| safe_url(value, None)));
    metadata
}

fn non_empty(text: String) -> Option<String> {
    (!text.is_empty()).then_some(text)
}

fn get(values: &BTreeMap<String, String>, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| values.get(*name).cloned())
}

fn json_text(value: &Value, names: &[&str]) -> Option<String> {
    names.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .and_then(|s| non_empty(normalize(s)))
    })
}

fn author_name(value: &Value) -> Option<String> {
    match value {
        Value::String(name) => non_empty(normalize(name)),
        Value::Object(_) => json_text(value, &["name", "@id"]),
        Value::Array(authors) => {
            let names = authors.iter().filter_map(author_name).collect::<Vec<_>>();
            (!names.is_empty()).then_some(names.join(", "))
        }
        _ => None,
    }
}

fn article_objects(value: &Value) -> Vec<&Value> {
    let mut candidates = Vec::new();
    fn walk<'a>(value: &'a Value, out: &mut Vec<&'a Value>) {
        match value {
            Value::Object(map) => {
                let is_article = map.get("@type").is_some_and(|kind| {
                    let allowed = |value: &str| {
                        matches!(
                            value,
                            "Article"
                                | "NewsArticle"
                                | "BlogPosting"
                                | "TechArticle"
                                | "Report"
                                | "ScholarlyArticle"
                                | "WebPage"
                        )
                    };
                    kind.as_str().is_some_and(allowed)
                        || kind.as_array().is_some_and(|items| {
                            items.iter().filter_map(Value::as_str).any(allowed)
                        })
                });
                if is_article || map.contains_key("headline") {
                    out.push(value);
                }
                if let Some(graph) = map.get("@graph") {
                    walk(graph, out);
                }
            }
            Value::Array(items) => {
                for item in items {
                    walk(item, out);
                }
            }
            _ => {}
        }
    }
    walk(value, &mut candidates);
    candidates
}
