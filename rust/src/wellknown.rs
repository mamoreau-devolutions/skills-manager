//! Well-known skills provider (RFC 8615), port of providers/wellknown.ts.
//!
//! Supports the v0.2.0 `$schema` + type/url/digest artifact index and the
//! legacy v0.1.0 name/description/files directory index, at
//! `/.well-known/agent-skills/` (preferred) or `/.well-known/skills/`.

use crate::archive::{read_zip_archive, ArchiveLimits};
use crate::blob::parallel_map;
use crate::frontmatter::parse_frontmatter;
use crate::sanitize::sanitize_metadata;
use crate::skills::should_install_internal_skills;
use crate::types::SnapshotFile;
use crate::urlutil;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::time::{Duration, Instant};

const DISCOVERY_SCHEMA_V2: &str = "https://schemas.agentskills.io/discovery/0.2.0/schema.json";
const MAX_ARCHIVE_UNPACKED_BYTES: u64 = 50 * 1024 * 1024;
const MAX_ARCHIVE_FILES: usize = 1000;
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);
const WELL_KNOWN_PATHS: [&str; 2] = [".well-known/agent-skills", ".well-known/skills"];
const INDEX_FILE: &str = "index.json";

#[derive(Clone, Debug)]
pub enum NormalizedEntry {
    V1 {
        name: String,
        description: String,
        files: Vec<String>,
        base_url: String,
        well_known_path: String,
        index_entry: Value,
    },
    V2 {
        name: String,
        description: String,
        kind: String,
        artifact_url: String,
        digest: String,
        index_entry: Value,
    },
}

impl NormalizedEntry {
    pub fn name(&self) -> &str {
        match self {
            NormalizedEntry::V1 { name, .. } | NormalizedEntry::V2 { name, .. } => name,
        }
    }
}

pub struct IndexResult {
    pub entries: Vec<NormalizedEntry>,
    pub resolved_base_url: String,
    pub resolved_well_known_path: String,
    pub index_url: String,
}

#[derive(Clone, Debug)]
pub struct WellKnownSkill {
    pub name: String,
    pub description: String,
    pub content: String,
    pub install_name: String,
    pub source_url: String,
    pub metadata: Option<Map<String, Value>>,
    /// Files keyed by relative path, in insertion order.
    pub files: Vec<SnapshotFile>,
    pub index_entry: Value,
}

#[derive(Debug)]
pub struct ScopeNotFoundError {
    pub scope_path: String,
    pub root_url: String,
}

impl std::fmt::Display for ScopeNotFoundError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "No skills found for the scoped path '{}' on {}. Not falling back to the root skills index because that would install every skill the host publishes. Check the URL, or run 'skills add {}' to install from the root index.",
            self.scope_path, self.root_url, self.root_url
        )
    }
}

fn is_valid_skill_name(v: Option<&Value>) -> bool {
    let Some(Value::String(name)) = v else {
        return false;
    };
    let len = crate::sanitize::js_len(name);
    (1..=64).contains(&len)
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
}

fn is_safe_legacy_file_path(v: &Value) -> bool {
    let Value::String(p) = v else { return false };
    !p.is_empty()
        && !p.starts_with('/')
        && !p.starts_with('\\')
        && !p.contains("..")
        && !p.contains('\0')
}

fn is_valid_entry_v1(e: &Value) -> bool {
    if !e.is_object() || !is_valid_skill_name(e.get("name")) {
        return false;
    }
    match e.get("description") {
        Some(Value::String(d)) if !d.is_empty() => {}
        _ => return false,
    }
    let Some(Value::Array(files)) = e.get("files") else {
        return false;
    };
    if files.is_empty() || !files.iter().all(is_safe_legacy_file_path) {
        return false;
    }
    files.iter().any(|f| {
        f.as_str()
            .map(|s| s.to_lowercase() == "skill.md")
            .unwrap_or(false)
    })
}

fn is_valid_entry_v2(e: &Value) -> bool {
    if !e.is_object() || !is_valid_skill_name(e.get("name")) {
        return false;
    }
    match e.get("description") {
        Some(Value::String(d)) if !d.is_empty() && crate::sanitize::js_len(d) <= 1024 => {}
        _ => return false,
    }
    match e.get("type").and_then(|t| t.as_str()) {
        Some("skill-md") | Some("archive") => {}
        _ => return false,
    }
    match e.get("url") {
        Some(Value::String(u)) if !u.is_empty() => {
            if urlutil::join("https://example.com/.well-known/agent-skills/index.json", u).is_none()
            {
                return false;
            }
        }
        _ => return false,
    }
    match e.get("digest").and_then(|d| d.as_str()) {
        Some(d) => {
            d.len() == 71
                && d.starts_with("sha256:")
                && d[7..]
                    .chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        }
        None => false,
    }
}

fn legacy_skill_base_url(index_url: &str, well_known_path: &str) -> String {
    let Some(u) = urlutil::parse(index_url) else {
        return String::new();
    };
    let marker = format!("/{}/{}", well_known_path, INDEX_FILE);
    let path = urlutil::pathname(&u);
    let base = if path.len() >= marker.len() {
        &path[..path.len() - marker.len()]
    } else {
        ""
    };
    format!("{}//{}{}", urlutil::protocol(&u), urlutil::host(&u), base)
}

fn normalize_index(
    raw: &Value,
    index_url: &str,
    well_known_path: &str,
) -> Option<Vec<NormalizedEntry>> {
    let record = raw.as_object()?;
    let skills = record.get("skills")?.as_array()?;
    match record.get("$schema") {
        Some(Value::String(s)) if s == DISCOVERY_SCHEMA_V2 => {
            let mut out = Vec::new();
            for e in skills {
                if !is_valid_entry_v2(e) {
                    continue;
                }
                let artifact_url = urlutil::join(index_url, e["url"].as_str().unwrap())?;
                out.push(NormalizedEntry::V2 {
                    name: e["name"].as_str().unwrap().to_string(),
                    description: e["description"].as_str().unwrap().to_string(),
                    kind: e["type"].as_str().unwrap().to_string(),
                    artifact_url,
                    digest: e["digest"].as_str().unwrap().to_string(),
                    index_entry: e.clone(),
                });
            }
            if out.is_empty() {
                None
            } else {
                Some(out)
            }
        }
        None => {
            let mut out = Vec::new();
            for e in skills {
                if !is_valid_entry_v1(e) {
                    return None;
                }
                out.push(NormalizedEntry::V1 {
                    name: e["name"].as_str().unwrap().to_string(),
                    description: e["description"].as_str().unwrap().to_string(),
                    files: e["files"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|f| f.as_str().unwrap().to_string())
                        .collect(),
                    base_url: legacy_skill_base_url(index_url, well_known_path),
                    well_known_path: well_known_path.to_string(),
                    index_entry: e.clone(),
                });
            }
            Some(out)
        }
        Some(_) => None,
    }
}

fn fetch_index_candidates(base_url: &str, update_check: bool) -> Vec<IndexResult> {
    let Some(parsed) = urlutil::parse(base_url) else {
        return Vec::new();
    };
    let pathname = urlutil::pathname(&parsed);
    let base_path = pathname.strip_suffix('/').unwrap_or(&pathname).to_string();
    let origin = format!("{}//{}", urlutil::protocol(&parsed), urlutil::host(&parsed));
    let deadline = Instant::now() + DISCOVERY_TIMEOUT;

    let mut urls: Vec<(String, String, &str)> = Vec::new();
    for wk in WELL_KNOWN_PATHS {
        urls.push((
            format!("{}{}/{}/{}", origin, base_path, wk, INDEX_FILE),
            format!("{}{}", origin, base_path),
            wk,
        ));
        if !base_path.is_empty() {
            urls.push((
                format!("{}/{}/{}", origin, wk, INDEX_FILE),
                origin.clone(),
                wk,
            ));
        }
    }

    let mut out = Vec::new();
    for (index_url, resolved_base, wk) in urls {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            continue;
        }
        let mut req = crate::http::get(&index_url).timeout(remaining);
        if update_check {
            req = req.header("X-Skills-Update-Check", "1");
        }
        let Ok(resp) = req.send() else { continue };
        if !resp.ok() {
            continue;
        }
        let Some(raw) = resp.json() else { continue };
        let Some(entries) = normalize_index(&raw, &index_url, wk) else {
            continue;
        };
        out.push(IndexResult {
            entries,
            resolved_base_url: resolved_base,
            resolved_well_known_path: wk.to_string(),
            index_url,
        });
    }
    out
}

/// First index found for `base_url`.
pub fn fetch_index(base_url: &str, update_check: bool) -> Option<IndexResult> {
    fetch_index_candidates(base_url, update_check)
        .into_iter()
        .next()
}

fn decode_text(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes).into_owned();
    s.strip_prefix('\u{feff}')
        .map(|x| x.to_string())
        .unwrap_or(s)
}

#[allow(clippy::too_many_arguments)]
fn create_skill(
    name: &str,
    description: &str,
    content: String,
    install_name: &str,
    source_url: &str,
    metadata: Option<&Value>,
    files: Vec<SnapshotFile>,
    index_entry: &Value,
) -> WellKnownSkill {
    WellKnownSkill {
        name: sanitize_metadata(name),
        description: sanitize_metadata(description),
        content,
        install_name: install_name.to_string(),
        source_url: source_url.to_string(),
        metadata: metadata.and_then(|m| m.as_object()).cloned(),
        files,
        index_entry: index_entry.clone(),
    }
}

fn set_file(files: &mut Vec<SnapshotFile>, path: &str, contents: Vec<u8>) {
    if let Some(f) = files.iter_mut().find(|f| f.path == path) {
        f.contents = contents;
    } else {
        files.push(SnapshotFile {
            path: path.to_string(),
            contents,
        });
    }
}

fn fetch_legacy(
    name: &str,
    files: &[String],
    base_url: &str,
    well_known_path: &str,
    index_entry: &Value,
) -> Option<WellKnownSkill> {
    let skill_base = format!(
        "{}/{}/{}",
        base_url.strip_suffix('/').unwrap_or(base_url),
        well_known_path,
        name
    );
    let skill_md_url = format!("{}/SKILL.md", skill_base);
    let resp = crate::http::get(&skill_md_url).send().ok()?;
    if !resp.ok() {
        return None;
    }
    let content = decode_text(&resp.body);
    let fm = parse_frontmatter(&content).ok()?;
    let (Some(Value::String(n)), Some(Value::String(d))) =
        (fm.data.get("name"), fm.data.get("description"))
    else {
        return None;
    };
    let mut out = vec![SnapshotFile {
        path: "SKILL.md".into(),
        contents: content.as_bytes().to_vec(),
    }];
    let others: Vec<String> = files
        .iter()
        .filter(|f| f.to_lowercase() != "skill.md")
        .cloned()
        .collect();
    let fetched = parallel_map(&others, |p| {
        let r = crate::http::get(&format!("{}/{}", skill_base, p))
            .send()
            .ok()?;
        if r.ok() {
            Some((p.clone(), r.body))
        } else {
            None
        }
    });
    for (p, body) in fetched.into_iter().flatten() {
        set_file(&mut out, &p, body);
    }
    Some(create_skill(
        n,
        d,
        content.clone(),
        name,
        &skill_md_url,
        fm.data.get("metadata"),
        out,
        index_entry,
    ))
}

fn compute_digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn normalize_tar_path(raw: &str) -> Option<String> {
    if raw.is_empty()
        || raw.contains('\0')
        || raw.starts_with('/')
        || raw.starts_with('\\')
        || raw.contains('\\')
    {
        return None;
    }
    let b = raw.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return None;
    }
    let parts: Vec<&str> = raw.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() || parts.iter().any(|p| *p == "." || *p == "..") {
        return None;
    }
    Some(parts.join("/"))
}

fn read_tar_string(buf: &[u8], offset: usize, len: usize) -> String {
    let slice = &buf[offset..offset + len];
    let end = slice.iter().position(|b| *b == 0).unwrap_or(slice.len());
    decode_text(&slice[..end])
}

fn extract_tar_gz(bytes: &[u8]) -> Result<Vec<SnapshotFile>, String> {
    let mut tar = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_end(&mut tar)
        .map_err(|e| e.to_string())?;
    let mut files: Vec<SnapshotFile> = Vec::new();
    let mut total: u64 = 0;
    let mut offset = 0usize;
    while offset + 512 <= tar.len() {
        let header = &tar[offset..offset + 512];
        if header.iter().all(|b| *b == 0) {
            break;
        }
        let name = read_tar_string(header, 0, 100);
        let size_text = read_tar_string(header, 124, 12).trim().to_string();
        let type_flag = header[156];
        let prefix = read_tar_string(header, 345, 155);
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{}/{}", prefix, name)
        };
        let size = if size_text.is_empty() {
            0
        } else {
            let digits: String = size_text
                .chars()
                .take_while(|c| ('0'..='7').contains(c))
                .collect();
            if digits.is_empty() {
                return Err("Invalid tar entry size".into());
            }
            u64::from_str_radix(&digits, 8).map_err(|_| "Invalid tar entry size".to_string())?
                as usize
        };
        offset += 512;
        if type_flag == b'2' || type_flag == b'1' {
            return Err("Archive links are not supported".into());
        }
        if type_flag == 0 || type_flag == b'0' {
            let end = (offset + size).min(tar.len());
            let content = tar[offset..end].to_vec();
            let normalized = normalize_tar_path(&path)
                .ok_or_else(|| format!("Unsafe archive path: {}", path))?;
            total += content.len() as u64;
            if total > MAX_ARCHIVE_UNPACKED_BYTES {
                return Err("Archive exceeds maximum unpacked size".into());
            }
            if files.len() >= MAX_ARCHIVE_FILES {
                return Err("Archive contains too many files".into());
            }
            set_file(&mut files, &normalized, content);
        }
        offset += size.div_ceil(512) * 512;
    }
    if !files.iter().any(|f| f.path == "SKILL.md") {
        return Err("Archive missing root SKILL.md".into());
    }
    Ok(files)
}

fn extract_archive(
    bytes: &[u8],
    url: &str,
    content_type: &str,
) -> Result<Vec<SnapshotFile>, String> {
    let lower = url.to_lowercase();
    let is_zip = content_type.contains("application/zip")
        || lower.ends_with(".zip")
        || (bytes.len() >= 2 && bytes[0] == 0x50 && bytes[1] == 0x4b);
    if is_zip {
        let files = read_zip_archive(
            bytes,
            &ArchiveLimits {
                max_extracted_bytes: MAX_ARCHIVE_UNPACKED_BYTES,
                max_entries: MAX_ARCHIVE_FILES as u64,
            },
        )
        .map_err(|e| e.to_string())?;
        return Ok(files
            .into_iter()
            .map(|(path, contents)| SnapshotFile { path, contents })
            .collect());
    }
    let is_tgz = content_type.contains("application/gzip")
        || content_type.contains("application/x-gzip")
        || lower.ends_with(".tar.gz")
        || lower.ends_with(".tgz")
        || (bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b);
    if is_tgz {
        return extract_tar_gz(bytes);
    }
    Err("Unsupported archive format".into())
}

fn fetch_artifact(
    name: &str,
    kind: &str,
    artifact_url: &str,
    digest: &str,
    index_entry: &Value,
) -> Option<WellKnownSkill> {
    let resp = crate::http::get(artifact_url).send().ok()?;
    if !resp.ok() {
        return None;
    }
    let content_type = resp.header("content-type").unwrap_or("").to_string();
    let bytes = resp.body;
    if compute_digest(&bytes) != digest {
        return None;
    }
    if kind == "skill-md" {
        let content = decode_text(&bytes);
        let fm = parse_frontmatter(&content).ok()?;
        let (Some(Value::String(n)), Some(Value::String(d))) =
            (fm.data.get("name"), fm.data.get("description"))
        else {
            return None;
        };
        let files = vec![SnapshotFile {
            path: "SKILL.md".into(),
            contents: content.as_bytes().to_vec(),
        }];
        return Some(create_skill(
            n,
            d,
            content.clone(),
            name,
            artifact_url,
            fm.data.get("metadata"),
            files,
            index_entry,
        ));
    }
    let mut files = extract_archive(&bytes, artifact_url, &content_type).ok()?;
    let skill_md = files.iter().find(|f| f.path == "SKILL.md")?;
    let content = decode_text(&skill_md.contents);
    set_file(&mut files, "SKILL.md", content.as_bytes().to_vec());
    let fm = parse_frontmatter(&content).ok()?;
    let (Some(Value::String(n)), Some(Value::String(d))) =
        (fm.data.get("name"), fm.data.get("description"))
    else {
        return None;
    };
    Some(create_skill(
        n,
        d,
        content.clone(),
        name,
        artifact_url,
        fm.data.get("metadata"),
        files,
        index_entry,
    ))
}

pub fn fetch_skill_by_entry(entry: &NormalizedEntry) -> Option<WellKnownSkill> {
    match entry {
        NormalizedEntry::V1 {
            name,
            files,
            base_url,
            well_known_path,
            index_entry,
            ..
        } => fetch_legacy(name, files, base_url, well_known_path, index_entry),
        NormalizedEntry::V2 {
            name,
            kind,
            artifact_url,
            digest,
            index_entry,
            ..
        } => fetch_artifact(name, kind, artifact_url, digest, index_entry),
    }
}

fn get_scope(url: &str) -> Option<(String, String)> {
    let u = urlutil::parse(url)?;
    let path = urlutil::pathname(&u);
    let scope = path.strip_suffix('/').unwrap_or(&path).to_string();
    if scope.is_empty() {
        return None;
    }
    Some((
        scope,
        format!("{}//{}", urlutil::protocol(&u), urlutil::host(&u)),
    ))
}

/// Fetch all skills. Scoped URLs never widen to the host's root index.
pub fn fetch_all_skills(
    url: &str,
    include_internal: bool,
) -> Result<Vec<WellKnownSkill>, ScopeNotFoundError> {
    let candidates = fetch_index_candidates(url, false);
    let scope = get_scope(url);
    let total = candidates.len();
    let scoped: Vec<IndexResult> = match &scope {
        Some((_, root)) => candidates
            .into_iter()
            .filter(|c| &c.resolved_base_url != root)
            .collect(),
        None => candidates,
    };
    let include_internal = include_internal || should_install_internal_skills();
    for result in &scoped {
        let skills: Vec<WellKnownSkill> = parallel_map(&result.entries, fetch_skill_by_entry)
            .into_iter()
            .flatten()
            .filter(|s| {
                include_internal
                    || s.metadata.as_ref().and_then(|m| m.get("internal"))
                        != Some(&Value::Bool(true))
            })
            .collect();
        if !skills.is_empty() {
            return Ok(skills);
        }
    }
    if let Some((scope_path, root)) = scope {
        if scoped.len() < total {
            return Err(ScopeNotFoundError {
                scope_path,
                root_url: root,
            });
        }
    }
    Ok(Vec::new())
}

/// Source identifier: hostname without `www.`.
pub fn get_source_identifier(url: &str) -> String {
    match urlutil::parse(url) {
        Some(u) => {
            let h = urlutil::hostname(&u);
            h.strip_prefix("www.").map(|s| s.to_string()).unwrap_or(h)
        }
        None => "unknown".into(),
    }
}

pub fn compute_well_known_skill_digest(skill: &WellKnownSkill) -> String {
    if let Some(d) = skill
        .index_entry
        .get("digest")
        .and_then(|d| d.as_str())
        .filter(|d| !d.is_empty())
    {
        return d.to_string();
    }
    let mut sorted: Vec<&SnapshotFile> = skill.files.iter().collect();
    sorted.sort_by(|a, b| a.path.encode_utf16().cmp(b.path.encode_utf16()));
    let mut h = Sha256::new();
    for f in sorted {
        h.update(f.path.as_bytes());
        h.update([0u8]);
        h.update(&f.contents);
        h.update([0u8]);
    }
    format!("sha256:{}", hex::encode(h.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalizes_v1_and_v2() {
        let v1 = json!({"skills": [{"name": "a-b", "description": "d", "files": ["SKILL.md", "x.txt"]}]});
        let e = normalize_index(
            &v1,
            "https://h.com/docs/.well-known/skills/index.json",
            ".well-known/skills",
        )
        .unwrap();
        match &e[0] {
            NormalizedEntry::V1 { base_url, .. } => assert_eq!(base_url, "https://h.com/docs"),
            _ => panic!(),
        }
        let bad_v1 =
            json!({"skills": [{"name": "a", "description": "d", "files": ["../x", "SKILL.md"]}]});
        assert!(normalize_index(
            &bad_v1,
            "https://h.com/.well-known/skills/index.json",
            ".well-known/skills"
        )
        .is_none());

        let digest = format!("sha256:{}", "a".repeat(64));
        let v2 = json!({"$schema": DISCOVERY_SCHEMA_V2, "skills": [{"name": "s", "type": "archive", "description": "d", "url": "s.zip", "digest": digest}, {"name": "Bad"}]});
        let e = normalize_index(
            &v2,
            "https://h.com/.well-known/agent-skills/index.json",
            ".well-known/agent-skills",
        )
        .unwrap();
        assert_eq!(e.len(), 1);
        match &e[0] {
            NormalizedEntry::V2 { artifact_url, .. } => {
                assert_eq!(artifact_url, "https://h.com/.well-known/agent-skills/s.zip")
            }
            _ => panic!(),
        }
        let unknown = json!({"$schema": "https://other", "skills": []});
        assert!(normalize_index(&unknown, "https://h.com/i.json", ".well-known/skills").is_none());
    }

    #[test]
    fn skill_names() {
        assert!(is_valid_skill_name(Some(&json!("my-skill"))));
        assert!(!is_valid_skill_name(Some(&json!("My-skill"))));
        assert!(!is_valid_skill_name(Some(&json!("a--b"))));
        assert!(!is_valid_skill_name(Some(&json!("-a"))));
    }

    #[test]
    fn digest_without_index_digest() {
        let s = WellKnownSkill {
            name: "n".into(),
            description: "d".into(),
            content: "c".into(),
            install_name: "n".into(),
            source_url: "u".into(),
            metadata: None,
            files: vec![
                SnapshotFile {
                    path: "b".into(),
                    contents: b"2".to_vec(),
                },
                SnapshotFile {
                    path: "a".into(),
                    contents: b"1".to_vec(),
                },
            ],
            index_entry: json!({}),
        };
        let mut h = Sha256::new();
        h.update(b"a\x001\x00b\x002\x00");
        assert_eq!(
            compute_well_known_skill_digest(&s),
            format!("sha256:{}", hex::encode(h.finalize()))
        );
        assert_eq!(
            get_source_identifier("https://www.example.com/x"),
            "example.com"
        );
    }
}
