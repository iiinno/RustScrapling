//! Location of browsers installed by `rustscrapling install` (Plan C-05).
//!
//! The installer writes `<browsers_dir>/current.json`; every component that
//! looks for Chrome (CLI fetch tiers, MCP, shell) consults it after explicit
//! configuration and before system-wide installations.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Record of the active installation (`current.json`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstallRecord {
    pub product: String,
    pub version: String,
    pub platform: String,
    pub url: String,
    pub sha256: String,
    /// How the checksum was established: `pinned`, `user` or `unpinned`.
    pub checksum_source: String,
    pub directory: PathBuf,
    pub executable: PathBuf,
}

/// `RUSTSCRAPLING_BROWSERS_PATH`, else the per-user data directory.
pub fn browsers_dir() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("RUSTSCRAPLING_BROWSERS_PATH").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path));
    }
    let home = || {
        std::env::var_os("HOME")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
    };
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        home().map(|h| h.join("Library").join("Application Support"))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .or_else(|| home().map(|h| h.join(".local").join("share")))
    }?;
    Some(base.join("rustscrapling").join("browsers"))
}

pub fn read_record(dir: &Path) -> Option<InstallRecord> {
    let text = std::fs::read_to_string(dir.join("current.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// Executable of the active installation, when it still exists.
pub fn installed_chrome() -> Option<PathBuf> {
    let record = read_record(&browsers_dir()?)?;
    record.executable.is_file().then_some(record.executable)
}
