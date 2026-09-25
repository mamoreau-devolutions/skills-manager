//! Project lock file `skills-lock.json` (port of local-lock.ts).
//!
//! Entries are kept as ordered JSON maps so rewriting a lock file preserves
//! existing key order exactly like the TS implementation.

use crate::collate::locale_compare;
use crate::paths::{self, join};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

const LOCAL_LOCK_FILE: &str = "skills-lock.json";
const CURRENT_VERSION: i64 = 1;

pub type Entry = Map<String, Value>;

pub struct LocalLock {
    pub version: Value,
    pub skills: Map<String, Value>,
}

pub fn entry_str<'a>(e: &'a Value, key: &str) -> Option<&'a str> {
    e.get(key).and_then(|v| v.as_str())
}

/// Truthy string field (JS `entry.x || ...` semantics).
pub fn entry_nonempty(e: &Value, key: &str) -> Option<String> {
    entry_str(e, key)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

pub fn entry_string_array(e: &Value, key: &str) -> Option<Vec<String>> {
    e.get(key).and_then(|v| v.as_array()).map(|a| {
        a.iter()
            .map(|x| x.as_str().unwrap_or("").to_string())
            .collect()
    })
}

/// Whether a key is a JS array index ("0", "42", but not "007" or "-1").
fn is_array_index(k: &str) -> bool {
    !k.is_empty()
        && k.bytes().all(|b| b.is_ascii_digit())
        && (k == "0" || !k.starts_with('0'))
        && k.parse::<u64>()
            .map(|v| v < u32::MAX as u64)
            .unwrap_or(false)
}

/// Reorder a map the way a JS object iterates: array-index keys first in
/// ascending numeric order, then all other keys in insertion order.
pub fn js_key_order(map: Map<String, Value>) -> Map<String, Value> {
    if !map.keys().any(|k| is_array_index(k)) {
        return map;
    }
    let (mut idx, rest): (Vec<_>, Vec<_>) = map.into_iter().partition(|(k, _)| is_array_index(k));
    idx.sort_by_key(|(k, _)| k.parse::<u64>().unwrap());
    idx.into_iter().chain(rest).collect()
}

pub fn get_local_lock_path(cwd: Option<&str>) -> String {
    let dir = cwd.map(|s| s.to_string()).unwrap_or_else(crate::sys::cwd);
    join(&[dir.as_str(), LOCAL_LOCK_FILE])
}

fn empty() -> LocalLock {
    LocalLock {
        version: Value::from(CURRENT_VERSION),
        skills: Map::new(),
    }
}

pub fn read_local_lock(cwd: Option<&str>) -> LocalLock {
    let lock_dir = cwd.map(|s| s.to_string()).unwrap_or_else(crate::sys::cwd);
    let lock_path = get_local_lock_path(Some(&lock_dir));
    let Ok(content) = std::fs::read_to_string(&lock_path) else {
        return empty();
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&content) else {
        return empty();
    };
    let version = match parsed.get("version").and_then(|v| v.as_f64()) {
        Some(v) => v,
        None => return empty(),
    };
    let mut skills = match parsed.get("skills") {
        Some(Value::Object(m)) => m.clone(),
        Some(Value::Null) | None | Some(Value::Bool(false)) => return empty(),
        Some(_) => Map::new(),
    };
    if version < CURRENT_VERSION as f64 {
        return empty();
    }
    for (_, entry) in skills.iter_mut() {
        if let Value::Object(e) = entry {
            if e.get("sourceType").and_then(|v| v.as_str()) == Some("local") {
                if let Some(src) = e.get("source").and_then(|v| v.as_str()) {
                    if !paths::is_absolute(src) {
                        let resolved = paths::resolve(&[lock_dir.as_str(), src]);
                        e.insert("source".into(), Value::String(resolved));
                    }
                }
            }
        }
    }
    let skills = js_key_order(skills);
    LocalLock {
        version: parsed
            .get("version")
            .cloned()
            .unwrap_or(Value::from(CURRENT_VERSION)),
        skills,
    }
}

fn portable_local_source(source: &str, lock_dir: &str) -> String {
    let absolute = if paths::is_absolute(source) {
        source.to_string()
    } else {
        paths::resolve(&[lock_dir, source])
    };
    let rel = paths::relative(lock_dir, &absolute);
    if paths::is_absolute(&rel) {
        return absolute.split(paths::SEP).collect::<Vec<_>>().join("/");
    }
    let portable = rel.split(paths::SEP).collect::<Vec<_>>().join("/");
    if portable.is_empty() {
        return ".".to_string();
    }
    if portable == ".." || portable.starts_with("../") {
        return portable;
    }
    format!("./{}", portable)
}

pub fn write_local_lock(lock: &LocalLock, cwd: Option<&str>) -> std::io::Result<()> {
    let lock_dir = cwd.map(|s| s.to_string()).unwrap_or_else(crate::sys::cwd);
    let lock_path = get_local_lock_path(Some(&lock_dir));
    let mut keys: Vec<&String> = lock.skills.keys().collect();
    keys.sort();
    let mut sorted = Map::new();
    for key in keys {
        let mut entry = lock.skills[key].clone();
        if let Value::Object(e) = &mut entry {
            if e.get("sourceType").and_then(|v| v.as_str()) == Some("local") {
                if let Some(src) = e
                    .get("source")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                {
                    e.insert(
                        "source".into(),
                        Value::String(portable_local_source(&src, &lock_dir)),
                    );
                }
            }
        }
        sorted.insert(key.clone(), entry);
    }
    let mut out = Map::new();
    out.insert("version".into(), lock.version.clone());
    out.insert("skills".into(), Value::Object(js_key_order(sorted)));
    let content = serde_json::to_string_pretty(&Value::Object(out)).unwrap() + "\n";
    std::fs::write(lock_path, content)
}

/// SHA-256 over all files in a skill directory, sorted with `localeCompare`
/// by relative path. `.git` and `node_modules` directories are skipped.
pub fn compute_skill_folder_hash(skill_dir: &str) -> std::io::Result<String> {
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    collect_files(skill_dir, skill_dir, &mut files)?;
    files.sort_by(|a, b| locale_compare(&a.0, &b.0));
    let mut hasher = Sha256::new();
    for (rel, content) in &files {
        hasher.update(rel.as_bytes());
        hasher.update(content);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn collect_files(
    base: &str,
    current: &str,
    out: &mut Vec<(String, Vec<u8>)>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(current)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        let full = join(&[current, name.as_str()]);
        let ft = entry.file_type()?;
        if ft.is_dir() {
            if name == ".git" || name == "node_modules" {
                continue;
            }
            collect_files(base, &full, out)?;
        } else if ft.is_file() {
            let content = std::fs::read(&full)?;
            let rel = paths::relative(base, &full).replace('\\', "/");
            out.push((rel, content));
        }
    }
    Ok(())
}

pub fn add_skill_to_local_lock(name: &str, entry: Entry, cwd: Option<&str>) -> std::io::Result<()> {
    let mut lock = read_local_lock(cwd);
    lock.skills.insert(name.to_string(), Value::Object(entry));
    write_local_lock(&lock, cwd)
}

pub fn remove_skill_from_local_lock(name: &str, cwd: Option<&str>) -> std::io::Result<bool> {
    let mut lock = read_local_lock(cwd);
    if lock.skills.shift_remove(name).is_none() {
        return Ok(false);
    }
    write_local_lock(&lock, cwd)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_sorting() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().to_string_lossy().to_string();
        let mut e = Entry::new();
        e.insert("source".into(), "owner/repo".into());
        e.insert("sourceType".into(), "github".into());
        e.insert("computedHash".into(), "abc".into());
        add_skill_to_local_lock("zeta", e.clone(), Some(&d)).unwrap();
        add_skill_to_local_lock("alpha", e, Some(&d)).unwrap();
        let text = std::fs::read_to_string(get_local_lock_path(Some(&d))).unwrap();
        assert!(text.find("\"alpha\"").unwrap() < text.find("\"zeta\"").unwrap());
        assert!(text.ends_with("}\n"));
        assert!(remove_skill_from_local_lock("alpha", Some(&d)).unwrap());
        assert_eq!(read_local_lock(Some(&d)).skills.len(), 1);
    }

    #[test]
    fn integer_like_keys_iterate_first() {
        let mut m = Map::new();
        for k in ["alpha", "10", "007", "9", "beta"] {
            m.insert(k.into(), Value::Null);
        }
        let keys: Vec<String> = js_key_order(m).keys().cloned().collect();
        assert_eq!(keys, vec!["9", "10", "alpha", "007", "beta"]);
    }

    #[test]
    fn local_sources_are_portable() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().to_string_lossy().to_string();
        let mut e = Entry::new();
        e.insert("source".into(), join(&[d.as_str(), "skills", "x"]).into());
        e.insert("sourceType".into(), "local".into());
        e.insert("computedHash".into(), "h".into());
        add_skill_to_local_lock("x", e, Some(&d)).unwrap();
        let text = std::fs::read_to_string(get_local_lock_path(Some(&d))).unwrap();
        assert!(text.contains("\"./skills/x\""));
        let lock = read_local_lock(Some(&d));
        assert!(paths::is_absolute(
            entry_str(&lock.skills["x"], "source").unwrap()
        ));
    }

    #[test]
    fn folder_hash_is_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().to_string_lossy().to_string();
        std::fs::write(join(&[d.as_str(), "SKILL.md"]), "a").unwrap();
        std::fs::create_dir_all(join(&[d.as_str(), "references"])).unwrap();
        std::fs::write(join(&[d.as_str(), "references", "x.md"]), "b").unwrap();
        // localeCompare order: references/x.md before SKILL.md
        let mut h = Sha256::new();
        h.update(b"references/x.md");
        h.update(b"b");
        h.update(b"SKILL.md");
        h.update(b"a");
        assert_eq!(
            compute_skill_folder_hash(&d).unwrap(),
            hex::encode(h.finalize())
        );
    }
}
