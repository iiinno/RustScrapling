//! Exercise the actual command-line entry points against Scrapling oracle
//! fixtures, including reading a database written by the Python original.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rustscrapling"))
        .args(args)
        .output()
        .expect("CLI starts")
}

fn paths(output: &Output) -> Vec<String> {
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone())
        .unwrap()
        .lines()
        .map(ToOwned::to_owned)
        .collect()
}

fn json(name: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(fixture(name)).unwrap()).unwrap()
}

fn expected_paths(value: &Value) -> Vec<String> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

fn write_page(dir: &Path, name: &str, html: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, html).unwrap();
    path
}

#[test]
fn select_similar_relocate_and_adaptive_fallback_match_oracle() {
    let temporary = std::env::temp_dir().join(format!(
        "rustscrapling_stage2_cli_{}_{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    std::fs::create_dir_all(&temporary).unwrap();

    let stage1 = json("stage1.json");
    let general_html = stage1["docs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == "general")
        .unwrap()["html"]
        .as_str()
        .unwrap();
    let data = json("stage2_adaptive.json");
    let find = json("stage2_find.json");
    let page_a = write_page(&temporary, "before.html", general_html);
    let page_b = write_page(&temporary, "after.html", data["page_b"].as_str().unwrap());
    let db = temporary.join("oracle.db");
    std::fs::copy(fixture("oracle_storage.db"), &db).unwrap();
    let a = page_a.to_str().unwrap();
    let b = page_b.to_str().unwrap();
    let storage = db.to_str().unwrap();

    let direct = paths(&run(&[
        "select",
        a,
        "--css",
        "article[data-id='2']",
        "--compat",
        "--format",
        "text",
    ]));
    assert_eq!(direct, vec!["/html/body/main/section[1]/div/article[2]"]);

    let similar = paths(&run(&[
        "similar",
        a,
        "--css",
        "article[data-id='2']",
        "--compat",
        "--format",
        "text",
    ]));
    assert_eq!(similar, expected_paths(&find["similar_default"]));

    let args = [
        "--id",
        "prod-2",
        "--storage-file",
        storage,
        "--url",
        "https://example.com/shop",
        "--compat",
        "--format",
        "text",
    ];
    let mut relocate_args = vec!["relocate", b];
    relocate_args.extend(args);
    let relocated = paths(&run(&relocate_args));
    assert_eq!(relocated, expected_paths(&data["relocated_40"]));

    let mut fallback_args = vec!["select", b, "--css", "article.product", "--adaptive"];
    fallback_args.extend(args);
    assert_eq!(paths(&run(&fallback_args)), relocated);

    // Domain mismatches must not accidentally reuse saved fingerprints.
    let wrong_domain = run(&[
        "relocate",
        b,
        "--id",
        "prod-2",
        "--storage-file",
        storage,
        "--url",
        "https://other.example.net/shop",
        "--compat",
    ]);
    assert!(!wrong_domain.status.success());
    assert!(String::from_utf8_lossy(&wrong_domain.stderr).contains("no element saved"));

    // The default output for agents is JSON and uses the html5ever parser.
    let default_out = run(&["select", a, "--css", "article[data-id='2']"]);
    assert!(default_out.status.success());
    let default_json: Value = serde_json::from_slice(&default_out.stdout).unwrap();
    assert_eq!(default_json["schema_version"], 1);
    assert_eq!(default_json["command"], "select");
    assert_eq!(default_json["parser"], "html5");
    assert_eq!(default_json["results"][0]["kind"], "element");

    let limited_out = run(&[
        "similar",
        a,
        "--css",
        "article[data-id='2']",
        "--compat",
        "--max-chars",
        "125",
    ]);
    assert!(
        limited_out.status.success(),
        "{}",
        String::from_utf8_lossy(&limited_out.stderr)
    );
    assert!(String::from_utf8_lossy(&limited_out.stdout).chars().count() <= 125);
    let limited_json: Value = serde_json::from_slice(&limited_out.stdout).unwrap();
    assert_eq!(limited_json["truncated"], true);

    let output_file = temporary.join("selected.json");
    let saved = run(&[
        "select",
        a,
        "--css",
        "article[data-id='2']",
        "--compat",
        "--out",
        output_file.to_str().unwrap(),
    ]);
    assert!(saved.status.success());
    let receipt: Value = serde_json::from_slice(&saved.stdout).unwrap();
    assert_eq!(receipt["schema_version"], 1);
    assert_eq!(receipt["saved_to"], output_file.to_str().unwrap());
    let on_disk: Value = serde_json::from_slice(&std::fs::read(output_file).unwrap()).unwrap();
    assert_eq!(on_disk["parser"], "compat");
    assert_eq!(on_disk["results"][0]["path"], direct[0]);

    let ndjson = run(&[
        "similar",
        a,
        "--css",
        "article[data-id='2']",
        "--compat",
        "--format",
        "ndjson",
    ]);
    assert!(ndjson.status.success());
    let ndjson_lines: Vec<Value> = ndjson
        .stdout
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    assert_eq!(ndjson_lines.len(), similar.len());
    assert_eq!(ndjson_lines[0]["path"], similar[0]);

    let bad_limit = run(&[
        "select",
        a,
        "--css",
        "article[data-id='2']",
        "--max-chars",
        "4",
    ]);
    assert!(!bad_limit.status.success());
    let typed_error: Value = serde_json::from_slice(&bad_limit.stderr).unwrap();
    assert_eq!(typed_error["schema_version"], 1);
    assert!(typed_error["error"]["kind"].is_string());

    let fresh_db = temporary.join("fresh.db");
    let save = run(&[
        "select",
        a,
        "--css",
        "article[data-id='2']",
        "--compat",
        "--id",
        "prod-2",
        "--auto-save",
        "--storage-file",
        fresh_db.to_str().unwrap(),
        "--url",
        "https://example.com/shop",
    ]);
    assert!(
        save.status.success(),
        "{}",
        String::from_utf8_lossy(&save.stderr)
    );
    let fresh_relocated = run(&[
        "relocate",
        b,
        "--compat",
        "--id",
        "prod-2",
        "--storage-file",
        fresh_db.to_str().unwrap(),
        "--url",
        "https://example.com/shop",
        "--format",
        "text",
    ]);
    assert_eq!(paths(&fresh_relocated), relocated);

    drop((relocate_args, fallback_args));
    std::fs::remove_dir_all(&temporary).unwrap();
}
