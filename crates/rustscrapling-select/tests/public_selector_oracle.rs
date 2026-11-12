use regex::{Regex, RegexBuilder};
use rustscrapling_dom::libxml_backend::{parse_html, CompatOptions};
use rustscrapling_dom::Dom;
use rustscrapling_select::public_selector::{index, slice, RegexFirst};
use rustscrapling_select::{
    FindFilter, PublicSelectorError, RegexOptions, RegexPattern, SelectError, Selector, TextOptions,
};
use serde_json::{json, Value};

type Result<T> = std::result::Result<T, String>;

fn category(error: PublicSelectorError) -> String {
    match error {
        PublicSelectorError::Select(SelectError::Syntax(_)) => "SelectorSyntaxError",
        PublicSelectorError::Select(SelectError::Type(_)) | PublicSelectorError::Type(_) => "TypeError",
        PublicSelectorError::Key(_) => "KeyError",
        PublicSelectorError::Index => "IndexError",
        PublicSelectorError::Value(_) => "ValueError",
        PublicSelectorError::Regex(_) => "RegexError",
    }.to_owned()
}

fn string(value: &Value) -> Result<&str> {
    value.as_str().ok_or_else(|| "TypeError".to_owned())
}

fn describe(dom: &Dom, item: &Selector<'_>) -> Value {
    match item.element_id() {
        Some(id) => {
            let node = dom.node(id);
            json!({
                "kind": "element", "path": dom.element_path(id), "tag": node.tag,
                "text": dom.text(id), "tail": dom.tail(id), "text_content": dom.text_content(id),
                "attrib": node.attrs.iter().map(|(key, value)| vec![key, value]).collect::<Vec<_>>()
            })
        }
        None => json!({"kind": "text", "value": item.text()}),
    }
}

fn describe_optional(dom: &Dom, item: Option<Selector<'_>>) -> Value {
    item.as_ref().map_or(Value::Null, |item| describe(dom, item))
}

fn pairs(value: &Value) -> Result<Vec<(String, String)>> {
    value.as_object().ok_or_else(|| "TypeError".to_owned())?
        .iter().map(|(key, value)| Ok((key.clone(), string(value)?.to_owned()))).collect()
}

fn compile_regex(pattern: &str, flags: &str) -> Result<Regex> {
    if flags.chars().any(|flag| !matches!(flag, 'i' | 'm' | 's')) {
        return Err("fixture flag is unsupported".into());
    }
    RegexBuilder::new(pattern).case_insensitive(flags.contains('i'))
        .multi_line(flags.contains('m')).dot_matches_new_line(flags.contains('s'))
        .build().map_err(|_| "RegexError".to_owned())
}

enum Predicate {
    TextContains(String),
    AttributeEquals(String, String),
}

impl Predicate {
    fn matches(&self, item: &Selector<'_>) -> bool {
        match self {
            Self::TextContains(text) => item.text().contains(text),
            Self::AttributeEquals(key, value) => item.getitem(key).is_ok_and(|actual| actual == value),
        }
    }
}

enum Filter {
    Tag(String),
    Tags(Vec<String>),
    Attributes(Vec<(String, String)>),
    Pattern(Regex),
    Predicate(Predicate),
}

fn filters(case: &Value) -> Result<Vec<Filter>> {
    let Some(arguments) = case.get("args") else { return Ok(Vec::new()); };
    arguments.as_array().ok_or_else(|| "TypeError".to_owned())?.iter().map(|argument| {
        let value = &argument["value"];
        Ok(match string(&argument["kind"])? {
            "tag" => Filter::Tag(string(value)?.to_owned()),
            "tags" => Filter::Tags(value.as_array().ok_or_else(|| "TypeError".to_owned())?
                .iter().map(|value| Ok(string(value)?.to_owned())).collect::<Result<_>>()?),
            "attributes" => Filter::Attributes(pairs(value)?),
            "regex" => Filter::Pattern(compile_regex(string(value)?, argument["flags"].as_str().unwrap_or(""))?),
            "predicate" => Filter::Predicate(match string(&value["kind"])? {
                "text_contains" => Predicate::TextContains(string(&value["value"])?.to_owned()),
                "attribute_equals" => Predicate::AttributeEquals(string(&value["key"])?.to_owned(), string(&value["value"])?.to_owned()),
                _ => return Err("unknown fixture predicate".into()),
            }),
            _ => return Err("TypeError".into()),
        })
    }).collect()
}

fn find(dom: &Dom, item: &Selector<'_>, case: &Value) -> Result<Value> {
    if item.is_text() {
        return if case["op"] == "find" {
            Ok(describe_optional(dom, item.find(&[], &[]).map_err(category)?))
        } else {
            Ok(Value::Array(item.find_all(&[], &[]).map_err(category)?
                .iter().map(|item| describe(dom, item)).collect()))
        };
    }
    let prepared = filters(case)?;
    let tag_lists: Vec<Vec<&str>> = prepared.iter().map(|filter| match filter {
        Filter::Tags(tags) => tags.iter().map(String::as_str).collect(),
        _ => Vec::new(),
    }).collect();
    let attribute_lists: Vec<Vec<(&str, &str)>> = prepared.iter().map(|filter| match filter {
        Filter::Attributes(attributes) => attributes.iter().map(|(key, value)| (key.as_str(), value.as_str())).collect(),
        _ => Vec::new(),
    }).collect();
    let predicates: Vec<_> = prepared.iter().map(|filter| match filter {
        Filter::Predicate(predicate) => Some(move |item: &Selector<'_>| predicate.matches(item)),
        _ => None,
    }).collect();
    let borrowed: Vec<_> = prepared.iter().enumerate().map(|(i, filter)| match filter {
        Filter::Tag(tag) => FindFilter::Tag(tag),
        Filter::Tags(_) => FindFilter::Tags(&tag_lists[i]),
        Filter::Attributes(_) => FindFilter::Attributes(&attribute_lists[i]),
        Filter::Pattern(pattern) => FindFilter::Pattern(pattern),
        Filter::Predicate(_) => FindFilter::Predicate(predicates[i].as_ref().unwrap()),
    }).collect();
    let keywords = case.get("kwargs").map(pairs).transpose()?.unwrap_or_default();
    let keywords: Vec<_> = keywords.iter().map(|(key, value)| (key.as_str(), value.as_str())).collect();
    if case["op"] == "find" {
        Ok(describe_optional(dom, item.find(&borrowed, &keywords).map_err(category)?))
    } else {
        Ok(Value::Array(item.find_all(&borrowed, &keywords).map_err(category)?
            .iter().map(|item| describe(dom, item)).collect()))
    }
}

fn execute(dom: &Dom, case: &Value) -> Result<Value> {
    let page = Selector::new(dom, dom.root).map_err(category)?;
    let target = &case["target"];
    let selected = if let Some(expression) = target["css"].as_str() {
        page.css(expression).map_err(category)?
    } else if let Some(expression) = target["xpath"].as_str() {
        page.xpath(expression, &[]).map_err(category)?
    } else { vec![page] };
    let operation = string(&case["op"])?;
    if operation == "index" {
        return Ok(describe(dom, index(&selected, case["position"].as_i64().unwrap() as isize).map_err(category)?));
    }
    if operation == "slice" {
        let values = slice(&selected, case["start"].as_i64().map(|n| n as isize),
            case["stop"].as_i64().map(|n| n as isize), case["step"].as_i64().unwrap_or(1) as isize).map_err(category)?;
        return Ok(Value::Array(values.iter().map(|item| describe(dom, item)).collect()));
    }
    let item = index(&selected, target["index"].as_i64().unwrap_or(0) as isize).map_err(category)?;
    let options = &case["options"];
    match operation {
        "find_all" | "find" => find(dom, item, case),
        "parent" => Ok(describe_optional(dom, item.parent())),
        "next" => Ok(describe_optional(dom, item.next())),
        "previous" => Ok(describe_optional(dom, item.previous())),
        "siblings" => Ok(Value::Array(item.siblings().iter().map(|item| describe(dom, item)).collect())),
        "getitem" => Ok(json!(item.getitem(string(&case["key"])?).map_err(category)?)),
        "contains" => Ok(json!(item.contains(string(&case["key"])?))),
        "get_all_text" => {
            let defaults = TextOptions::default();
            let ignore_tags: Vec<_> = if let Some(tags) = options.get("ignore_tags") {
                tags.as_array().unwrap().iter().map(string).collect::<Result<_>>()?
            } else { defaults.ignore_tags.to_vec() };
            Ok(json!(item.get_all_text(TextOptions {
                separator: options["separator"].as_str().unwrap_or(defaults.separator),
                strip: options["strip"].as_bool().unwrap_or(defaults.strip),
                ignore_tags: &ignore_tags,
                valid_values: options["valid_values"].as_bool().unwrap_or(defaults.valid_values),
            })))
        }
        "re" | "re_first" => {
            let pattern_text = string(&case["pattern"])?;
            let compiled = if case["compiled"].as_bool().unwrap_or(false) {
                Some(compile_regex(pattern_text, case["flags"].as_str().unwrap_or(""))?)
            } else { None };
            let pattern = compiled.as_ref().map_or(RegexPattern::Text(pattern_text), RegexPattern::Compiled);
            let defaults = RegexOptions::default();
            let options = RegexOptions {
                replace_entities: options["replace_entities"].as_bool().unwrap_or(defaults.replace_entities),
                clean_match: options["clean_match"].as_bool().unwrap_or(defaults.clean_match),
                case_sensitive: options["case_sensitive"].as_bool().unwrap_or(defaults.case_sensitive),
            };
            if operation == "re" {
                Ok(json!(item.re(pattern, options).map_err(category)?))
            } else {
                Ok(match item.re_first_or(pattern, case.get("default").cloned().unwrap_or(Value::Null), options).map_err(category)? {
                    RegexFirst::Match(value) => json!(value),
                    RegexFirst::Default(value) => value,
                })
            }
        }
        _ => Err("unknown fixture operation".into()),
    }
}

#[test]
fn public_selector_matches_fixed_oracle() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/public-selector.json")).unwrap();
    let oracle_path = std::env::var("SELECTOR_PUBLIC_ORACLE")
        .expect("run the pinned oracle driver first and set SELECTOR_PUBLIC_ORACLE");
    let oracle: Value = serde_json::from_str(&std::fs::read_to_string(oracle_path).unwrap()).unwrap();
    assert_eq!(oracle["upstream_revision"], fixture["upstream_revision"]);
    assert_eq!(oracle["cases"].as_array().unwrap().len(), fixture["cases"].as_array().unwrap().len());
    let mut results = Vec::new();
    let mut failures = Vec::new();
    for (case, expected) in fixture["cases"].as_array().unwrap().iter().zip(oracle["cases"].as_array().unwrap()) {
        assert_eq!(case["id"], expected["id"]);
        assert_eq!(case["inventory_ids"], expected["inventory_ids"]);
        let document = &fixture["documents"][case["document"].as_str().unwrap()];
        let dom = parse_html(document["html"].as_str().unwrap(), &CompatOptions {
            keep_comments: document["keep_comments"].as_bool().unwrap_or(false),
        }).expect("synthetic fixture parses");
        let outcome = match execute(&dom, case) {
            Ok(value) => json!({"value": value}),
            Err(error) => json!({"error": error}),
        };
        let passed = outcome == expected["outcome"];
        if !passed { failures.push(case["id"].as_str().unwrap().to_owned()); }
        results.push(json!({
            "id": case["id"], "inventory_ids": case["inventory_ids"],
            "outcome": outcome, "expected": expected["outcome"],
            "status": if passed { "PASS" } else { "FAIL" },
        }));
    }
    let output = json!({"schema_version": 1, "upstream_revision": fixture["upstream_revision"],
        "scope": "selector-public-use-coverage", "cases": results,
        "known_unsupported": fixture["known_unsupported"]});
    let path = std::env::var("SELECTOR_PUBLIC_RESULTS").expect("SELECTOR_PUBLIC_RESULTS is required");
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path).unwrap();
    file.write_all(serde_json::to_string_pretty(&output).unwrap().as_bytes()).unwrap();
    assert!(failures.is_empty(), "fixed oracle differences: {}", failures.join(", "));
}
