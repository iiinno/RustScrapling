//! Compare default mode (html5ever) against the compat fixtures and
//! print every difference, for docs/COMPAT.md classification.

use rustscrapling_dom::html5_backend::{parse_html, Html5Options};
use rustscrapling_dom::Dom;
use rustscrapling_select::{css, xpath, Selection};
use serde_json::Value;

fn describe(dom: &Dom, sel: &Selection) -> Value {
    match sel {
        Selection::Element(id) => {
            let node = dom.node(*id);
            serde_json::json!({
                "kind": "element",
                "path": dom.element_path(*id),
                "tag": node.tag,
                "text": dom.text(*id),
                "tail": dom.tail(*id),
                "text_content": dom.text_content(*id),
                "attrib": node.attrs.iter().map(|(k, v)| vec![k.clone(), v.clone()]).collect::<Vec<_>>(),
            })
        }
        Selection::Text(s) => serde_json::json!({"kind": "text", "value": s}),
    }
}

fn main() {
    let path = format!("{}/../../fixtures/stage1.json", env!("CARGO_MANIFEST_DIR"));
    let raw = std::fs::read_to_string(&path).unwrap();
    let fixtures: Value = serde_json::from_str(&raw).unwrap();
    let mut diffs = 0;
    let mut total = 0;
    for doc in fixtures["docs"].as_array().unwrap() {
        let name = doc["name"].as_str().unwrap();
        if name == "appendix" {
            continue;
        }
        let html = doc["html"].as_str().unwrap();
        let dom = parse_html(html, &Html5Options::default()).unwrap();
        for kind in ["css", "xpath"] {
            for q in doc[kind].as_array().unwrap() {
                total += 1;
                let expr = q["expr"].as_str().unwrap();
                let expected_error = q.get("error").is_some();
                let got = if kind == "css" {
                    css::css(&dom, dom.root, expr)
                } else {
                    let vars: Vec<(String, String)> = q["vars"]
                        .as_object()
                        .map(|m| {
                            m.iter()
                                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                                .collect()
                        })
                        .unwrap_or_default();
                    xpath::xpath(&dom, dom.root, expr, &vars)
                };
                let differs = match (&got, expected_error) {
                    (Err(_), true) => false,
                    (Err(_), false) => true,
                    (Ok(_), true) => true,
                    (Ok(results), false) => {
                        let described: Vec<Value> =
                            results.iter().map(|r| describe(&dom, r)).collect();
                        Value::Array(described) != q["results"]
                    }
                };
                if differs {
                    diffs += 1;
                    let got_desc = match &got {
                        Ok(results) => {
                            let d: Vec<Value> = results.iter().map(|r| describe(&dom, r)).collect();
                            serde_json::to_string(&d).unwrap()
                        }
                        Err(e) => format!("ERROR {}", e),
                    };
                    println!(
                        "DIFF [{} {}] {}\n  compat: {}\n  default: {}",
                        name,
                        kind,
                        expr,
                        serde_json::to_string(&q["results"]).unwrap(),
                        got_desc
                    );
                }
            }
        }
    }
    println!(
        "{} of {} queries differ between default and compat",
        diffs, total
    );
}
