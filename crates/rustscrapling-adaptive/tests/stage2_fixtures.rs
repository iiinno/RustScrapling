//! Stage 2 fixture verification against oracle-generated ground truth
//! (fixtures/stage2_*.json + fixtures/oracle_storage.db).

use std::collections::HashMap;
use std::path::PathBuf;

use rustscrapling_adaptive::seqmatch::SequenceMatcher;
use rustscrapling_adaptive::{
    base_url, calculate_similarity_score, element_to_dict, find_similar, relocate, SqliteStorage,
};
use rustscrapling_dom::libxml_backend::{parse_html, CompatOptions};
use rustscrapling_dom::{Dom, NodeId};
use rustscrapling_select::css::css;
use rustscrapling_select::text_search::{
    find_by_regex, find_by_text, FindByRegexOpts, FindByTextOpts,
};
use rustscrapling_select::Selection;
use serde_json::Value;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

fn load_json(name: &str) -> Value {
    let text = std::fs::read_to_string(fixture(name)).expect("fixture readable");
    serde_json::from_str(&text).expect("fixture parses")
}

fn path_map(dom: &Dom) -> HashMap<String, NodeId> {
    let mut map = HashMap::new();
    if let Some(root) = dom.document_element() {
        for id in dom.descendants_or_self(root) {
            if dom.node(id).is_element() {
                map.insert(dom.element_path(id), id);
            }
        }
    }
    map
}

fn paths_of(dom: &Dom, ids: &[NodeId]) -> Vec<String> {
    ids.iter().map(|&id| dom.element_path(id)).collect()
}

#[test]
fn seqmatch_ratios_match_difflib() {
    let data = load_json("stage2_seq.json");
    let mut checked = 0usize;
    for (a, b, expected) in data["strings"].as_array().unwrap().iter().map(|c| {
        (
            c[0].as_str().unwrap().chars().collect::<Vec<char>>(),
            c[1].as_str().unwrap().chars().collect::<Vec<char>>(),
            c[2].as_f64().unwrap(),
        )
    }) {
        assert_eq!(SequenceMatcher::new(&a, &b).ratio(), expected);
        checked += 1;
    }
    for (a, b, expected) in data["tuples"].as_array().unwrap().iter().map(|c| {
        (
            c[0].as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_str().unwrap().to_string())
                .collect::<Vec<String>>(),
            c[1].as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_str().unwrap().to_string())
                .collect::<Vec<String>>(),
            c[2].as_f64().unwrap(),
        )
    }) {
        assert_eq!(SequenceMatcher::new(&a, &b).ratio(), expected);
        checked += 1;
    }
    for (a, b, expected) in data["long"].as_array().unwrap().iter().map(|c| {
        (
            c[0].as_str().unwrap().chars().collect::<Vec<char>>(),
            c[1].as_str().unwrap().chars().collect::<Vec<char>>(),
            c[2].as_f64().unwrap(),
        )
    }) {
        assert_eq!(SequenceMatcher::new(&a, &b).ratio(), expected);
        checked += 1;
    }
    assert_eq!(checked, 30000);
}

#[test]
fn element_dicts_match_oracle() {
    let data = load_json("stage2_dicts.json");
    let stage1 = load_json("stage1.json");
    let html_by_name: HashMap<String, String> = stage1["docs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["name"].as_str().unwrap().to_string(),
                d["html"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    for doc in data["docs"].as_array().unwrap() {
        let name = doc["name"].as_str().unwrap();
        let dom = parse_html(&html_by_name[name], &CompatOptions::default()).expect("parse");
        let map = path_map(&dom);
        for entry in doc["entries"].as_array().unwrap() {
            let path = entry["path"].as_str().unwrap();
            let id = *map.get(path).unwrap_or_else(|| panic!("element {path}"));
            let dict = element_to_dict(&dom, id);
            let mine: Value = serde_json::from_str(&dict.to_json()).unwrap();
            assert_eq!(mine, entry["dict"], "dict mismatch at {name} {path}");
        }
    }
}

fn general_dom() -> (Dom, HashMap<String, NodeId>) {
    let stage1 = load_json("stage1.json");
    let html = stage1["docs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == "general")
        .unwrap()["html"]
        .as_str()
        .unwrap()
        .to_string();
    let dom = parse_html(&html, &CompatOptions::default()).expect("parse");
    let map = path_map(&dom);
    (dom, map)
}

#[test]
fn find_results_match_oracle() {
    let (dom, _map) = general_dom();
    let root = dom.document_element().unwrap();
    let data = load_json("stage2_find.json");

    let base_sel = css(&dom, root, "article[data-id='2']").unwrap();
    let base = match base_sel[0] {
        Selection::Element(id) => id,
        _ => panic!("base not an element"),
    };
    let expect = |key: &str| -> Vec<String> {
        data[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(
        paths_of(&dom, &find_similar(&dom, base, 0.2, &["href", "src"], false)),
        expect("similar_default")
    );
    assert_eq!(
        paths_of(&dom, &find_similar(&dom, base, 0.2, &["href", "src"], true)),
        expect("similar_match_text")
    );
    assert_eq!(
        paths_of(&dom, &find_similar(&dom, base, 0.9, &["href", "src"], false)),
        expect("similar_threshold_09")
    );

    let text_cases = data["by_text_cases"].as_array().unwrap();
    let text_expected = data["by_text"].as_array().unwrap();
    for (case, expected) in text_cases.iter().zip(text_expected) {
        let text = case[0].as_str().unwrap();
        let kw = &case[1];
        let opts = FindByTextOpts {
            first_match: kw.get("first_match").and_then(|v| v.as_bool()).unwrap_or(true),
            partial: kw.get("partial").and_then(|v| v.as_bool()).unwrap_or(false),
            case_sensitive: kw
                .get("case_sensitive")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            clean_match: kw.get("clean_match").and_then(|v| v.as_bool()).unwrap_or(true),
        };
        let got = paths_of(&dom, &find_by_text(&dom, root, text, opts));
        let want: Vec<String> = expected
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(got, want, "find_by_text {text} {kw}");
    }

    let regex_cases = data["by_regex_cases"].as_array().unwrap();
    let regex_expected = data["by_regex"].as_array().unwrap();
    for (case, expected) in regex_cases.iter().zip(regex_expected) {
        let pattern = case[0].as_str().unwrap();
        let kw = &case[1];
        let opts = FindByRegexOpts {
            first_match: kw.get("first_match").and_then(|v| v.as_bool()).unwrap_or(true),
            case_sensitive: kw
                .get("case_sensitive")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            clean_match: kw.get("clean_match").and_then(|v| v.as_bool()).unwrap_or(true),
        };
        let got = paths_of(&dom, &find_by_regex(&dom, root, pattern, opts).unwrap());
        let want: Vec<String> = expected
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(got, want, "find_by_regex {pattern} {kw}");
    }
}

#[test]
fn oracle_database_relocates() {
    let data = load_json("stage2_adaptive.json");
    // Work on a copy: opening the DB read-write may create WAL sidecars.
    let copy = std::env::temp_dir().join(format!("rs_oracle_db_{}.db", std::process::id()));
    std::fs::copy(fixture("oracle_storage.db"), &copy).unwrap();
    let storage = SqliteStorage::open(&copy, Some("https://example.com/shop")).unwrap();
    assert_eq!(storage.base(), data["db_url_key"].as_str().unwrap());

    let dict = storage
        .retrieve(data["identifier"].as_str().unwrap())
        .unwrap()
        .expect("element stored");
    assert_eq!(dict.to_json(), data["raw_json"].as_str().unwrap());

    let page_b = data["page_b"].as_str().unwrap();
    let dom = parse_html(page_b, &CompatOptions::default()).expect("parse page B");
    let root = dom.document_element().unwrap();
    let map = path_map(&dom);
    let want = |key: &str| -> Vec<String> {
        data[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(paths_of(&dom, &relocate(&dom, root, &dict, 40)), want("relocated_40"));
    assert_eq!(paths_of(&dom, &relocate(&dom, root, &dict, 0)), want("relocated_0"));
    for (path, score) in data["scores"].as_object().unwrap() {
        let id = map[path];
        assert_eq!(
            calculate_similarity_score(&dom, &dict, id),
            score.as_f64().unwrap(),
            "score mismatch at {path}"
        );
    }
    let _ = std::fs::remove_file(&copy);
}

#[test]
fn base_url_cases() {
    assert_eq!(base_url(Some("https://example.com/shop")), "example.com");
    assert_eq!(base_url(Some("https://sub.example.co.uk/x")), "example.co.uk");
    assert_eq!(base_url(Some("http://www.bbc.co.uk/news")), "bbc.co.uk");
    assert_eq!(base_url(Some("example.com")), "example.com");
    assert_eq!(base_url(Some("http://localhost:8000/a")), "default");
    assert_eq!(base_url(Some("http://127.0.0.1/a")), "default");
    assert_eq!(base_url(None), "default");
    assert_eq!(base_url(Some("")), "default");
}
