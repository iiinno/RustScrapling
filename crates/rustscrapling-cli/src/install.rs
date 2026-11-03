//! `rustscrapling install` (Plan C-05): install Chrome for Testing with
//! version and SHA-256 verification.
//!
//! Upstream `scrapling install` runs `playwright install chromium` and
//! `playwright install-deps chromium` (the latter uses the system package
//! manager and usually root). This command only writes into a per-user
//! directory: it downloads the Chrome for Testing archive over verified TLS,
//! checks its SHA-256 against a pinned manifest (or a user-supplied digest),
//! extracts it through a staging directory, smoke-runs `--version` and records
//! the result in `current.json`. It never installs system packages; missing
//! shared libraries are reported with a hint instead. Every failure removes
//! the partial download and staging directory.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Args;
use rustscrapling_browser::installed::{browsers_dir, read_record, InstallRecord};
use rustscrapling_http::{HttpClient, HttpOptions, HttpRequest};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const MANIFEST: &str = include_str!("chrome_for_testing.json");
const KNOWN_GOOD: &str =
    "https://googlechromelabs.github.io/chrome-for-testing/known-good-versions-with-downloads.json";
const LAST_KNOWN_GOOD: &str =
    "https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json";
const MAX_ARCHIVE_BYTES: usize = 512 * 1024 * 1024;
const STORAGE: &str = "https://storage.googleapis.com/chrome-for-testing-public";
const LISTING: &str = "https://googlechromelabs.github.io/chrome-for-testing";

/// `RUSTSCRAPLING_CFT_MIRROR` replaces both official hosts (for corporate
/// mirrors). Checksums are still enforced, so a mirror cannot alter builds.
fn mirrored(url: &str) -> String {
    match std::env::var("RUSTSCRAPLING_CFT_MIRROR") {
        Ok(mirror) if !mirror.is_empty() => {
            let mirror = mirror.trim_end_matches('/');
            url.replacen(STORAGE, mirror, 1)
                .replacen(LISTING, mirror, 1)
        }
        _ => url.to_owned(),
    }
}

#[derive(Args)]
pub struct InstallArgs {
    /// Chrome for Testing version: exact (e.g. 156.0.8078.80), "pinned"
    /// (the default, verified by this release) or "stable" (latest).
    #[arg(long, default_value = "pinned")]
    version: String,
    /// Expected SHA-256 of the archive (required for versions that are not
    /// pinned, unless --allow-unpinned).
    #[arg(long)]
    sha256: Option<String>,
    /// Accept an unpinned archive verified only by HTTPS (recorded as such).
    #[arg(long)]
    allow_unpinned: bool,
    /// Installation root (default: RUSTSCRAPLING_BROWSERS_PATH or the
    /// per-user data directory).
    #[arg(long)]
    dir: Option<PathBuf>,
    /// Reinstall even when this version is already installed.
    #[arg(short, long)]
    force: bool,
    /// Print what would be done (URL, checksum, paths, side effects) and exit.
    #[arg(long)]
    dry_run: bool,
    /// Override the platform (linux64, mac-arm64, mac-x64, win64, win32).
    #[arg(long)]
    platform: Option<String>,
}

struct Failure {
    kind: &'static str,
    message: String,
    hint: &'static str,
}

fn fail(kind: &'static str, message: impl Into<String>, hint: &'static str) -> Failure {
    Failure {
        kind,
        message: message.into(),
        hint,
    }
}

impl From<Failure> for String {
    fn from(failure: Failure) -> String {
        json!({"schema_version":1,"error":{"kind":failure.kind,"message":failure.message,"hint":failure.hint}})
            .to_string()
    }
}

fn host_platform() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("linux64"),
        ("macos", "aarch64") => Some("mac-arm64"),
        ("macos", "x86_64") => Some("mac-x64"),
        ("windows", "x86_64") => Some("win64"),
        ("windows", "x86") => Some("win32"),
        _ => None,
    }
}

/// Executable inside the extracted archive, relative to the install directory.
fn executable_relative(platform: &str) -> PathBuf {
    match platform {
        "win64" | "win32" => PathBuf::from(format!("chrome-{platform}")).join("chrome.exe"),
        "mac-arm64" | "mac-x64" => PathBuf::from(format!("chrome-{platform}"))
            .join("Google Chrome for Testing.app")
            .join("Contents")
            .join("MacOS")
            .join("Google Chrome for Testing"),
        _ => PathBuf::from(format!("chrome-{platform}")).join("chrome"),
    }
}

struct Resolved {
    version: String,
    url: String,
    sha256: Option<String>,
    checksum_source: &'static str,
    size: Option<u64>,
}

fn pinned(platform: &str, version: Option<&str>) -> Option<Resolved> {
    let manifest: Value = serde_json::from_str(MANIFEST).ok()?;
    let entry = manifest["versions"].as_array()?.iter().find(|entry| {
        version.is_none_or(|v| entry["version"] == v) && entry["downloads"][platform].is_object()
    })?;
    let download = &entry["downloads"][platform];
    Some(Resolved {
        version: entry["version"].as_str()?.to_owned(),
        url: download["url"].as_str()?.to_owned(),
        sha256: Some(download["sha256"].as_str()?.to_owned()),
        checksum_source: "pinned",
        size: download["size"].as_u64(),
    })
}

fn client(timeout: Duration, max_body: usize) -> Result<HttpClient, Failure> {
    HttpClient::new(HttpOptions {
        impersonate: None,
        stealthy_headers: false,
        timeout,
        read_timeout: timeout,
        max_body_bytes: max_body,
        retry_on_status: true,
        ..HttpOptions::default()
    })
    .map_err(|e| {
        fail(
            "transport",
            e.message,
            "Check network access and proxy settings",
        )
    })
}

async fn fetch(url: &str, timeout: Duration, max_body: usize) -> Result<Vec<u8>, Failure> {
    let response = client(timeout, max_body)?
        .request(HttpRequest::get(url))
        .await
        .map_err(|e| {
            fail(
                e.kind.as_str(),
                format!("{url}: {}", e.message),
                "Check network access and proxy settings",
            )
        })?;
    if response.status != 200 {
        return Err(fail(
            "http_status",
            format!("{url}: HTTP {}", response.status),
            "The version or platform may not exist; check --version and --platform",
        ));
    }
    Ok(response.body)
}

async fn resolve(args: &InstallArgs, platform: &str) -> Result<Resolved, Failure> {
    let user_digest = args.sha256.as_ref().map(|d| d.to_ascii_lowercase());
    if let Some(digest) = &user_digest {
        if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(fail(
                "invalid_arguments",
                "--sha256 must be 64 hexadecimal characters",
                "Pass the archive's SHA-256 digest",
            ));
        }
    }
    let requested = match args.version.as_str() {
        "pinned" => None,
        other => Some(other),
    };
    if requested != Some("stable") {
        if let Some(mut resolved) = pinned(platform, requested) {
            if let Some(digest) = user_digest {
                if Some(&digest) != resolved.sha256.as_ref() {
                    return Err(fail(
                        "checksum",
                        format!(
                            "--sha256 differs from the pinned digest for {}",
                            resolved.version
                        ),
                        "Remove --sha256 or check the digest you were given",
                    ));
                }
            }
            resolved.checksum_source = "pinned";
            return Ok(resolved);
        }
        if requested.is_none() {
            return Err(fail(
                "unsupported",
                format!("no pinned Chrome for Testing build for platform {platform}"),
                "Pass --version with --sha256, or install Chrome yourself and set RUSTSCRAPLING_CHROME_PATH",
            ));
        }
    }
    // Resolve the download URL from the Chrome for Testing JSON endpoints.
    let (endpoint, version) = match requested {
        Some("stable") => (LAST_KNOWN_GOOD, None),
        other => (KNOWN_GOOD, other),
    };
    let listing: Value = serde_json::from_slice(
        &fetch(&mirrored(endpoint), Duration::from_secs(60), 64 << 20).await?,
    )
    .map_err(|e| {
        fail(
            "parse",
            format!("Chrome for Testing listing: {e}"),
            "Retry later",
        )
    })?;
    let entry = match version {
        None => listing["channels"]["Stable"].clone(),
        Some(version) => listing["versions"]
            .as_array()
            .and_then(|all| all.iter().find(|entry| entry["version"] == version))
            .cloned()
            .ok_or_else(|| {
                fail(
                    "not_found",
                    format!("Chrome for Testing has no version {version}"),
                    "See the known-good versions list",
                )
            })?,
    };
    let version = entry["version"].as_str().unwrap_or_default().to_owned();
    let url = entry["downloads"]["chrome"]
        .as_array()
        .and_then(|downloads| downloads.iter().find(|d| d["platform"] == platform))
        .and_then(|d| d["url"].as_str())
        .ok_or_else(|| {
            fail(
                "not_found",
                format!("Chrome for Testing {version} has no {platform} build"),
                "Choose another version or platform",
            )
        })?
        .to_owned();
    if let Some(pinned) = pinned(platform, Some(&version)) {
        return Ok(pinned);
    }
    let (sha256, checksum_source) = match user_digest {
        Some(digest) => (Some(digest), "user"),
        None if args.allow_unpinned => (None, "unpinned"),
        None => {
            return Err(fail(
                "unverified",
                format!(
                "Chrome for Testing {version} ({platform}) has no pinned SHA-256 in this release"
            ),
                "Pass --sha256 DIGEST, or --allow-unpinned to rely on HTTPS only",
            ))
        }
    };
    Ok(Resolved {
        version,
        url,
        sha256,
        checksum_source,
        size: None,
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn extract(archive: &[u8], destination: &Path) -> Result<u64, Failure> {
    let io = |e: std::io::Error| fail("io", e.to_string(), "Check disk space and permissions");
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive)).map_err(|e| {
        fail(
            "archive",
            e.to_string(),
            "The download is not a valid zip archive",
        )
    })?;
    let mut files = 0;
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|e| fail("archive", e.to_string(), "The archive is damaged"))?;
        // `enclosed_name` rejects absolute paths and `..` traversal.
        let relative = entry.enclosed_name().ok_or_else(|| {
            fail(
                "archive",
                format!(
                    "unsafe path in archive: {}",
                    entry.name().unwrap_or_default()
                ),
                "Refusing to extract outside the install directory",
            )
        })?;
        let target = destination.join(relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&target).map_err(io)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        #[cfg(unix)]
        if entry.is_symlink() {
            // Symlink targets are short paths; never trust the declared size.
            let mut link = String::new();
            (&mut entry)
                .take(4096)
                .read_to_string(&mut link)
                .map_err(io)?;
            let resolved = target.parent().unwrap_or(destination).join(&link);
            if Path::new(&link).is_absolute()
                || !normalize(&resolved).starts_with(normalize(destination))
            {
                return Err(fail(
                    "archive",
                    format!(
                        "unsafe symlink in archive: {} -> {link}",
                        entry.name().unwrap_or_default()
                    ),
                    "Refusing to extract outside the install directory",
                ));
            }
            std::os::unix::fs::symlink(&link, &target).map_err(io)?;
            continue;
        }
        let mut output = std::fs::File::create(&target).map_err(io)?;
        std::io::copy(&mut entry, &mut output).map_err(io)?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode & 0o755))
                .map_err(io)?;
        }
        files += 1;
    }
    Ok(files)
}

/// Lexical normalization (no filesystem access) for symlink containment.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn missing_libraries(executable: &Path) -> Vec<String> {
    if !cfg!(target_os = "linux") {
        return Vec::new();
    }
    std::process::Command::new("ldd")
        .arg(executable)
        .output()
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|line| line.contains("not found"))
                .filter_map(|line| line.split_whitespace().next().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn side_effects(directory: &Path, root: &Path) -> Value {
    json!({
        "writes": [directory.to_string_lossy(), root.join("current.json").to_string_lossy()],
        "temporary": [root.join(".rustscrapling-install-*").to_string_lossy()],
        "network": ["googlechromelabs.github.io (version listing, only for unpinned versions)",
                    "storage.googleapis.com (archive download)"],
        "permissions": "current user only; no root, no system packages, no PATH or registry changes",
        "cleanup_on_failure": "partial download and staging directory are removed",
        "uninstall": format!("delete {}", directory.to_string_lossy()),
    })
}

pub fn run(args: InstallArgs) -> Result<(), String> {
    let platform = match &args.platform {
        Some(platform) => platform.clone(),
        None => host_platform()
            .ok_or_else(|| {
                fail(
                    "unsupported",
                    format!(
                        "Chrome for Testing has no build for {}/{}",
                        std::env::consts::OS,
                        std::env::consts::ARCH
                    ),
                    "Install Chromium from your OS and set RUSTSCRAPLING_CHROME_PATH",
                )
            })?
            .to_owned(),
    };
    if !["linux64", "mac-arm64", "mac-x64", "win64", "win32"].contains(&platform.as_str()) {
        return Err(fail(
            "invalid_arguments",
            format!("unknown platform {platform}"),
            "Use linux64, mac-arm64, mac-x64, win64 or win32",
        )
        .into());
    }
    let root = args.dir.clone().or_else(browsers_dir).ok_or_else(|| {
        fail(
            "invalid_arguments",
            "cannot determine the per-user data directory",
            "Pass --dir or set RUSTSCRAPLING_BROWSERS_PATH",
        )
    })?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| fail("runtime", e.to_string(), "Retry"))?;
    let resolved = runtime.block_on(resolve(&args, &platform))?;
    let directory = root.join(format!("chrome-{}-{platform}", resolved.version));
    let executable = directory.join(executable_relative(&platform));
    let plan = json!({
        "schema_version": 1,
        "product": "chrome-for-testing",
        "version": resolved.version,
        "platform": platform,
        "url": resolved.url,
        "sha256": resolved.sha256,
        "checksum_source": resolved.checksum_source,
        "size": resolved.size,
        "directory": directory.to_string_lossy(),
        "executable": executable.to_string_lossy(),
        "side_effects": side_effects(&directory, &root),
    });
    if args.dry_run {
        let mut plan = plan;
        plan["dry_run"] = json!(true);
        println!("{plan}");
        return Ok(());
    }
    if !args.force
        && executable.is_file()
        && read_record(&root).is_some_and(|record| record.executable == executable)
    {
        println!(
            "{}",
            json!({"schema_version":1,"status":"already_installed","version":resolved.version,
                "executable":executable.to_string_lossy(),
                "hint":"Pass --force to reinstall"})
        );
        return Ok(());
    }
    let io = |e: std::io::Error| {
        fail(
            "io",
            e.to_string(),
            "Check disk space and permissions of the install directory",
        )
    };
    std::fs::create_dir_all(&root).map_err(io)?;
    let staging = root.join(format!(".rustscrapling-install-{}", std::process::id()));
    let result = (|| -> Result<(String, u64, Vec<String>, String), Failure> {
        eprintln!("Downloading {}", mirrored(&resolved.url));
        let archive = runtime.block_on(fetch(
            &mirrored(&resolved.url),
            Duration::from_secs(900),
            MAX_ARCHIVE_BYTES,
        ))?;
        let digest = hex(&Sha256::digest(&archive));
        if let Some(expected) = &resolved.sha256 {
            if &digest != expected {
                return Err(fail(
                    "checksum",
                    format!("SHA-256 mismatch: expected {expected}, got {digest}"),
                    "The download was altered or the pin is wrong; nothing was installed",
                ));
            }
        }
        if let Some(size) = resolved.size {
            if archive.len() as u64 != size {
                return Err(fail(
                    "checksum",
                    format!("size mismatch: expected {size}, got {}", archive.len()),
                    "The download was altered; nothing was installed",
                ));
            }
        }
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging).map_err(io)?;
        let files = extract(&archive, &staging)?;
        drop(archive);
        let staged_executable = staging.join(executable_relative(&platform));
        if !staged_executable.is_file() {
            return Err(fail(
                "archive",
                format!(
                    "archive does not contain {}",
                    executable_relative(&platform).display()
                ),
                "Choose another version or platform",
            ));
        }
        if directory.exists() {
            std::fs::remove_dir_all(&directory).map_err(io)?;
        }
        std::fs::rename(&staging, &directory).map_err(io)?;
        let missing = missing_libraries(&executable);
        let reported = if platform == host_platform().unwrap_or_default() && missing.is_empty() {
            std::process::Command::new(&executable)
                .arg("--version")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
                .unwrap_or_default()
        } else {
            String::new()
        };
        Ok((digest, files, missing, reported))
    })();
    let (digest, files, missing, reported) = match result {
        Ok(values) => values,
        Err(failure) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(failure.into());
        }
    };
    let record = InstallRecord {
        product: "chrome-for-testing".into(),
        version: resolved.version.clone(),
        platform: platform.clone(),
        url: resolved.url.clone(),
        sha256: digest.clone(),
        checksum_source: resolved.checksum_source.into(),
        directory: directory.clone(),
        executable: executable.clone(),
    };
    let record_file = root.join("current.json");
    let temporary = root.join(".current.json.tmp");
    std::fs::write(
        &temporary,
        serde_json::to_vec_pretty(&record).unwrap_or_default(),
    )
    .and_then(|()| std::fs::rename(&temporary, &record_file))
    .map_err(io)?;
    println!(
        "{}",
        json!({"schema_version":1,"status":"installed","version":resolved.version,"platform":platform,
            "sha256":digest,"checksum_source":resolved.checksum_source,"files":files,
            "executable":executable.to_string_lossy(),"reported_version":reported,
            "missing_libraries":missing,
            "hint": if missing.is_empty() { Value::Null } else {
                json!("Install the listed shared libraries with your OS package manager (upstream runs `playwright install-deps`, which needs root)")
            },
            "side_effects": side_effects(&directory, &root)})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buffer = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut buffer);
        for (name, data) in entries {
            zip.start_file(
                *name,
                zip::write::SimpleFileOptions::default().unix_permissions(0o755),
            )
            .unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
        buffer.into_inner()
    }

    #[test]
    fn manifest_has_pinned_builds_with_digests() {
        for platform in ["linux64", "mac-arm64", "mac-x64", "win64"] {
            let resolved = pinned(platform, None).expect(platform);
            assert_eq!(resolved.sha256.as_ref().unwrap().len(), 64);
            assert!(resolved
                .url
                .starts_with("https://storage.googleapis.com/chrome-for-testing-public/"));
            assert!(
                resolved.url.contains(&resolved.version)
                    && resolved.url.ends_with(&format!("chrome-{platform}.zip"))
            );
        }
    }

    #[test]
    fn extraction_rejects_traversal_and_keeps_modes() {
        let dir = std::env::temp_dir().join(format!("rs-install-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ok = archive(&[("chrome-linux64/chrome", b"#!/bin/sh\necho x\n")]);
        assert_eq!(extract(&ok, &dir).ok(), Some(1));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("chrome-linux64/chrome"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o755);
        }
        let bad = archive(&[("../escape", b"x")]);
        let error = extract(&bad, &dir).err().unwrap();
        assert_eq!(error.kind, "archive");
        assert!(!dir.parent().unwrap().join("escape").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
