//! Global lock file `~/.agents/.skill-lock.json` (port of skill-lock.ts).

use crate::paths::{self, join};
use serde_json::{Map, Value};

const LOCK_FILE: &str = ".skill-lock.json";
const CURRENT_VERSION: i64 = 3;

pub type Entry = Map<String, Value>;

/// The lock file, kept as an ordered JSON object so rewrites preserve key
/// order and unknown fields.
pub struct SkillLock {
    pub root: Map<String, Value>,
}

impl SkillLock {
    pub fn skills(&self) -> &Map<String, Value> {
        static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
        self.root
            .get("skills")
            .and_then(|v| v.as_object())
            .unwrap_or_else(|| EMPTY.get_or_init(Map::new))
    }

    pub fn skills_mut(&mut self) -> &mut Map<String, Value> {
        if !self
            .root
            .get("skills")
            .map(|v| v.is_object())
            .unwrap_or(false)
        {
            self.root.insert("skills".into(), Value::Object(Map::new()));
        }
        self.root
            .get_mut("skills")
            .unwrap()
            .as_object_mut()
            .unwrap()
    }
}

/// `$XDG_STATE_HOME/skills/.skill-lock.json` or `~/.agents/.skill-lock.json`.
pub fn get_skill_lock_path() -> String {
    if let Some(xdg) = crate::sys::env("XDG_STATE_HOME") {
        return join(&[xdg.as_str(), "skills", LOCK_FILE]);
    }
    join(&[crate::sys::homedir().as_str(), ".agents", LOCK_FILE])
}

fn empty() -> SkillLock {
    let mut root = Map::new();
    root.insert("version".into(), Value::from(CURRENT_VERSION));
    root.insert("skills".into(), Value::Object(Map::new()));
    root.insert("dismissed".into(), Value::Object(Map::new()));
    SkillLock { root }
}

/// Read the lock file; old versions or invalid content yield an empty lock.
pub fn read_skill_lock() -> SkillLock {
    let Ok(content) = std::fs::read_to_string(get_skill_lock_path()) else {
        return empty();
    };
    let Ok(Value::Object(root)) = serde_json::from_str::<Value>(&content) else {
        return empty();
    };
    let Some(version) = root.get("version").and_then(|v| v.as_f64()) else {
        return empty();
    };
    if !crate::skills::js_truthy(root.get("skills")) {
        return empty();
    }
    if version < CURRENT_VERSION as f64 {
        return empty();
    }
    let mut root = root;
    js_order_skills(&mut root);
    SkillLock { root }
}

/// Iterate/serialize `skills` in JS object key order (integer-like keys first).
fn js_order_skills(root: &mut Map<String, Value>) {
    if let Some(Value::Object(skills)) = root.get_mut("skills") {
        *skills = crate::local_lock::js_key_order(std::mem::take(skills));
    }
}

pub fn write_skill_lock(lock: &SkillLock) -> std::io::Result<()> {
    let path = get_skill_lock_path();
    std::fs::create_dir_all(paths::dirname(&path))?;
    let mut root = lock.root.clone();
    js_order_skills(&mut root);
    let content = serde_json::to_string_pretty(&Value::Object(root)).unwrap();
    std::fs::write(path, content)
}

/// `GITHUB_TOKEN` or `GH_TOKEN`. Stored GitHub CLI credentials are never
/// extracted; callers fall back to `gh api` or a normal git clone instead.
pub fn get_github_token() -> Option<String> {
    crate::sys::env("GITHUB_TOKEN").or_else(|| crate::sys::env("GH_TOKEN"))
}

pub fn add_skill_to_lock(name: &str, entry: Entry) -> std::io::Result<()> {
    let mut lock = read_skill_lock();
    let now = crate::sys::now_iso();
    let installed_at = lock
        .skills()
        .get(name)
        .and_then(|e| e.get("installedAt"))
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| Value::String(now.clone()));
    let mut full = entry;
    full.insert("installedAt".into(), installed_at);
    full.insert("updatedAt".into(), Value::String(now));
    // Assigning a fresh object to an existing JS key keeps its position.
    lock.skills_mut()
        .insert(name.to_string(), Value::Object(full));
    write_skill_lock(&lock)
}

pub fn remove_skill_from_lock(name: &str) -> std::io::Result<bool> {
    let mut lock = read_skill_lock();
    if !lock.skills().contains_key(name) {
        return Ok(false);
    }
    lock.skills_mut().shift_remove(name);
    write_skill_lock(&lock)?;
    Ok(true)
}

pub fn get_skill_from_lock(name: &str) -> Option<Value> {
    read_skill_lock().skills().get(name).cloned()
}

pub fn get_all_locked_skills() -> Map<String, Value> {
    read_skill_lock().skills().clone()
}

pub fn is_prompt_dismissed(key: &str) -> bool {
    read_skill_lock()
        .root
        .get("dismissed")
        .and_then(|d| d.get(key))
        == Some(&Value::Bool(true))
}

pub fn dismiss_prompt(key: &str) -> std::io::Result<()> {
    let mut lock = read_skill_lock();
    if !lock
        .root
        .get("dismissed")
        .map(|d| d.is_object())
        .unwrap_or(false)
    {
        lock.root
            .insert("dismissed".into(), Value::Object(Map::new()));
    }
    lock.root
        .get_mut("dismissed")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert(key.to_string(), Value::Bool(true));
    write_skill_lock(&lock)
}

pub fn get_last_selected_agents() -> Option<Vec<String>> {
    read_skill_lock()
        .root
        .get("lastSelectedAgents")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
}

pub fn save_selected_agents(agents: &[&str]) -> std::io::Result<()> {
    let mut lock = read_skill_lock();
    lock.root.insert(
        "lastSelectedAgents".into(),
        Value::Array(
            agents
                .iter()
                .map(|a| Value::String(a.to_string()))
                .collect(),
        ),
    );
    write_skill_lock(&lock)
}

/// Tree SHA for a skill folder via the GitHub Trees API.
pub fn fetch_skill_folder_hash(
    owner_repo: &str,
    skill_path: &str,
    use_token: bool,
    r#ref: Option<&str>,
) -> Option<String> {
    let tree = crate::blob::fetch_repo_tree(owner_repo, r#ref, use_token)?;
    crate::blob::get_skill_folder_hash_from_tree(&tree, skill_path)
}
