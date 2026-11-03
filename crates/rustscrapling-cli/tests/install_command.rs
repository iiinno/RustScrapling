//! `rustscrapling install` (Plan C-05) against a loopback mirror: checksum
//! enforcement, cleanup on failure, user-pinned versions, the installed
//! record and its use as the default browser. No network access.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

fn zip_with_chrome(script: &str) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut buffer);
    let options = zip::write::SimpleFileOptions::default().unix_permissions(0o755);
    zip.add_directory("chrome-linux64/", options).unwrap();
    zip.start_file("chrome-linux64/chrome", options).unwrap();
    zip.write_all(script.as_bytes()).unwrap();
    zip.start_file("chrome-linux64/resources.pak", options)
        .unwrap();
    zip.write_all(b"data").unwrap();
    zip.finish().unwrap();
    buffer.into_inner()
}

/// Serves `routes` (path → body); records requested paths.
fn mirror(routes: Vec<(String, Vec<u8>)>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buffer = [0u8; 4096];
            let n = stream.read(&mut buffer).unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..n]).to_string();
            let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
            log.lock().unwrap().push(path.clone());
            let (status, body) = match routes.iter().find(|(p, _)| *p == path) {
                Some((_, body)) => ("200 OK", body.clone()),
                None => ("404 Not Found", Vec::new()),
            };
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            );
            let _ = stream.write_all(&body);
        }
    });
    (base, seen)
}

fn install(mirror: &str, dir: &Path, args: &[&str]) -> (i32, Value) {
    let output = Command::new(env!("CARGO_BIN_EXE_rustscrapling"))
        .arg("install")
        .arg("--dir")
        .arg(dir)
        .args(["--platform", "linux64"])
        .args(args)
        .env("RUSTSCRAPLING_CFT_MIRROR", mirror)
        .output()
        .unwrap();
    let code = output.status.code().unwrap_or(-1);
    let stream = if code == 0 {
        &output.stdout
    } else {
        &output.stderr
    };
    let text = String::from_utf8_lossy(stream);
    let last = text.lines().last().unwrap_or_default().to_owned();
    (
        code,
        serde_json::from_str(&last).unwrap_or_else(|_| panic!("{text}")),
    )
}

fn leftovers(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn pinned_checksum_mismatch_installs_nothing() {
    let dir = tempdir("mismatch");
    // A tampered archive served at the pinned version's path.
    let (base, seen) = mirror(vec![(
        "/156.0.8078.80/linux64/chrome-linux64.zip".into(),
        zip_with_chrome("#!/bin/sh\necho evil\n"),
    )]);
    let (code, error) = install(&base, &dir, &[]);
    assert_eq!(code, 1);
    assert_eq!(error["error"]["kind"], "checksum", "{error}");
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains("b711c720f774be355197f32edf37e0651418ba036d92446ee2da19ac017540a7"));
    assert_eq!(leftovers(&dir), Vec::<String>::new());
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["/156.0.8078.80/linux64/chrome-linux64.zip"]
    );
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn user_pinned_version_installs_records_and_is_used() {
    let dir = tempdir("user");
    let archive = zip_with_chrome("#!/bin/sh\necho 'Google Chrome for Testing 1.2.3.4'\n");
    let digest: String = Sha256::digest(&archive)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let url = "https://storage.googleapis.com/chrome-for-testing-public/1.2.3.4/linux64/chrome-linux64.zip";
    let listing = format!(
        r#"{{"versions":[{{"version":"1.2.3.4","downloads":{{"chrome":[{{"platform":"linux64","url":"{url}"}}]}}}}]}}"#
    );
    let (base, _) = mirror(vec![
        (
            "/known-good-versions-with-downloads.json".into(),
            listing.into_bytes(),
        ),
        ("/1.2.3.4/linux64/chrome-linux64.zip".into(), archive),
    ]);

    // Unpinned version without a digest is refused before downloading.
    let (code, error) = install(&base, &dir, &["--version", "1.2.3.4"]);
    assert_eq!(code, 1);
    assert_eq!(error["error"]["kind"], "unverified");
    assert_eq!(leftovers(&dir), Vec::<String>::new());

    // Wrong user digest: checksum error, nothing left behind.
    let wrong = "0".repeat(64);
    let (code, error) = install(&base, &dir, &["--version", "1.2.3.4", "--sha256", &wrong]);
    assert_eq!(code, 1);
    assert_eq!(error["error"]["kind"], "checksum");
    assert!(leftovers(&dir)
        .iter()
        .all(|name| !name.starts_with(".rustscrapling-install")));
    assert!(!dir.join("current.json").exists());

    let (code, done) = install(&base, &dir, &["--version", "1.2.3.4", "--sha256", &digest]);
    assert_eq!(code, 0, "{done}");
    assert_eq!(done["status"], "installed");
    assert_eq!(done["checksum_source"], "user");
    assert_eq!(done["files"], 2);
    assert_eq!(
        done["reported_version"],
        "Google Chrome for Testing 1.2.3.4"
    );
    let record: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("current.json")).unwrap()).unwrap();
    assert_eq!(record["sha256"], digest.as_str());
    assert_eq!(record["executable"], done["executable"]);

    let (code, again) = install(&base, &dir, &["--version", "1.2.3.4", "--sha256", &digest]);
    assert_eq!(code, 0);
    assert_eq!(again["status"], "already_installed");

    // The shell's `fetch` prefers the installed executable over system
    // browsers: the
    // fake "chrome" exits immediately, so launching it must fail as a
    // browser error rather than "no browser configured".
    let output = Command::new(env!("CARGO_BIN_EXE_rustscrapling"))
        .args(["shell", "-c", "fetch http://127.0.0.1:9/"])
        .env("RUSTSCRAPLING_BROWSERS_PATH", &dir)
        .env_remove("RUSTSCRAPLING_CHROME_PATH")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let error: Value = serde_json::from_str(stderr.lines().next().unwrap()).unwrap();
    assert_eq!(error["error"]["kind"], "browser", "{stderr}");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn dry_run_reports_plan_without_side_effects() {
    let dir = tempdir("dry");
    let (code, plan) = install("http://127.0.0.1:9", &dir, &["--dry-run"]);
    assert_eq!(code, 0);
    assert_eq!(plan["dry_run"], true);
    assert_eq!(plan["checksum_source"], "pinned");
    assert_eq!(plan["version"], "156.0.8078.80");
    assert!(plan["side_effects"]["permissions"]
        .as_str()
        .unwrap()
        .contains("no root"));
    assert_eq!(leftovers(&dir), Vec::<String>::new());
    std::fs::remove_dir_all(dir).ok();
}

fn tempdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rs-install-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
