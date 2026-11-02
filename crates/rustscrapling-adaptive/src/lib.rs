//! Adaptive relocation for RustScrapling: element fingerprints,
//! a bit-exact port of Python's difflib SequenceMatcher, similarity
//! scoring, `relocate` / `find_similar`, and the SQLite storage that is
//! file-compatible with Scrapling's.

pub mod dict;
pub mod scoring;
pub mod seqmatch;
pub mod storage;

pub use dict::{element_to_dict, ElementDict, SibTag};
pub use scoring::{calculate_similarity_score, find_similar, relocate};
pub use seqmatch::SequenceMatcher;
pub use storage::{base_url, SqliteStorage};
