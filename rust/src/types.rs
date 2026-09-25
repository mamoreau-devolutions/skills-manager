//! Shared data types (port of types.ts and friends).

use serde_json::{Map, Value};

pub const AGENTS_DIR: &str = ".agents";
pub const SKILLS_SUBDIR: &str = "skills";
/// Maximum skill-directory depth searched inside a known container by default.
pub const DEFAULT_SKILL_CONTAINER_DEPTH: usize = 3;

/// A file captured in memory (blob snapshot or well-known download).
#[derive(Clone, Debug)]
pub struct SnapshotFile {
    pub path: String,
    pub contents: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// Directory on disk ('' for blob skills).
    pub path: String,
    pub raw_content: Option<String>,
    pub plugin_name: Option<String>,
    pub metadata: Option<Map<String, Value>>,
    /// Blob snapshot data (only for skills resolved from the blob fast path).
    pub blob: Option<BlobData>,
}

#[derive(Clone, Debug)]
pub struct BlobData {
    pub files: Vec<SnapshotFile>,
    pub snapshot_hash: String,
    /// Path of the SKILL.md within the repo
    pub repo_path: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParsedSource {
    /// github | gitlab | git | local | well-known | download
    pub kind: String,
    pub url: String,
    pub subpath: Option<String>,
    pub local_path: Option<String>,
    pub r#ref: Option<String>,
    pub skill_filter: Option<String>,
}

impl ParsedSource {
    pub fn new(kind: &str, url: &str) -> Self {
        ParsedSource {
            kind: kind.to_string(),
            url: url.to_string(),
            ..Default::default()
        }
    }
}
