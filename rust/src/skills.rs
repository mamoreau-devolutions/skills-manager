//! Skill discovery and SKILL.md parsing (port of skills.ts).

use crate::frontmatter::parse_frontmatter;
use crate::local_lock::read_local_lock;
use crate::paths::{self, join};
use crate::plugin_manifest::{get_plugin_groupings, get_plugin_skill_paths};
use crate::sanitize::{sanitize_metadata, strip_terminal_escapes};
use crate::types::{Skill, DEFAULT_SKILL_CONTAINER_DEPTH};
use serde_json::Value;
use std::collections::HashSet;

pub const SKIP_DIRS: &[&str] = &["node_modules", ".git", "dist", "build", "__pycache__"];

pub const AGENT_PROJECT_SKILL_DIRS: &[&str] = &[
    ".agents/skills",
    ".claude/skills",
    ".cline/skills",
    ".codebuddy/skills",
    ".codex/skills",
    ".commandcode/skills",
    ".continue/skills",
    ".factory/skills",
    ".github/skills",
    ".goose/skills",
    ".grok/skills",
    ".iflow/skills",
    ".junie/skills",
    ".kilo/skills",
    ".kilocode/skills",
    ".kimchi/skills",
    ".kiro/skills",
    ".minimax/skills",
    ".mux/skills",
    ".neovate/skills",
    ".opencode/skills",
    ".openhands/skills",
    ".pi/skills",
    ".posit/assistant/skills",
    ".qoder/skills",
    ".roo/skills",
    ".trae/skills",
    ".windsurf/skills",
    ".zcode/skills",
    ".zencoder/skills",
];

pub fn normalize_skill_name(name: &str) -> String {
    let lower = name.to_lowercase();
    let mut out = String::new();
    let mut in_run = false;
    for c in lower.chars() {
        if c.is_whitespace() || c == '_' {
            if !in_run {
                out.push('-');
                in_run = true;
            }
        } else {
            out.push(c);
            in_run = false;
        }
    }
    out
}

fn normalize_relative_path(p: &str) -> String {
    let joined = p.split(paths::SEP).collect::<Vec<_>>().join("/");
    let mut out = String::new();
    let mut prev_slash = false;
    for c in joined.chars() {
        if c == '/' {
            if !prev_slash {
                out.push(c);
            }
            prev_slash = true;
        } else {
            out.push(c);
            prev_slash = false;
        }
    }
    out
}

/// Internal skills are hidden unless INSTALL_INTERNAL_SKILLS=1|true.
pub fn should_install_internal_skills() -> bool {
    matches!(
        std::env::var("INSTALL_INTERNAL_SKILLS").as_deref(),
        Ok("1") | Ok("true")
    )
}

pub fn has_skill_md(dir: &str) -> bool {
    std::fs::metadata(join(&[dir, "SKILL.md"]))
        .map(|m| m.is_file())
        .unwrap_or(false)
}

fn warn_skipped(path: &str, reason: &str) {
    crate::errln!(
        "⚠ Skipped {} — {}",
        sanitize_metadata(path),
        strip_terminal_escapes(reason)
    );
}

/// JS truthiness of a JSON value (`undefined` → None → false).
pub fn js_truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0 && !f.is_nan()).unwrap_or(true),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

pub fn js_typeof(v: Option<&Value>) -> &'static str {
    match v {
        None => "undefined",
        Some(Value::Null) | Some(Value::Array(_)) | Some(Value::Object(_)) => "object",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
    }
}

pub fn read_utf8_lossy(path: &str) -> std::io::Result<String> {
    std::fs::read(path).map(|b| String::from_utf8_lossy(&b).into_owned())
}

pub fn parse_skill_md(skill_md_path: &str, include_internal: bool) -> Option<Skill> {
    let content = match read_utf8_lossy(skill_md_path) {
        Ok(c) => c,
        Err(e) => {
            warn_skipped(skill_md_path, &format!("failed to read file: {}", e));
            return None;
        }
    };
    let data = match parse_frontmatter(&content) {
        Ok(fm) => fm.data,
        Err(e) => {
            warn_skipped(skill_md_path, &format!("YAML parse error: {}", e));
            return None;
        }
    };
    let name = data.get("name");
    let description = data.get("description");
    if !js_truthy(name) || !js_truthy(description) {
        let mut missing = Vec::new();
        if !js_truthy(name) {
            missing.push("name");
        }
        if !js_truthy(description) {
            missing.push("description");
        }
        warn_skipped(
            skill_md_path,
            &format!(
                "missing required frontmatter field(s): {}",
                missing.join(", ")
            ),
        );
        return None;
    }
    let (Some(Value::String(name)), Some(Value::String(description))) = (name, description) else {
        warn_skipped(
            skill_md_path,
            &format!(
                "frontmatter \"name\" and \"description\" must be strings (got {} and {})",
                js_typeof(name),
                js_typeof(description)
            ),
        );
        return None;
    };
    let metadata = match data.get("metadata") {
        Some(Value::Object(m)) => Some(m.clone()),
        _ => None,
    };
    let is_internal = metadata.as_ref().and_then(|m| m.get("internal")) == Some(&Value::Bool(true));
    if is_internal && !should_install_internal_skills() && !include_internal {
        return None;
    }
    Some(Skill {
        name: sanitize_metadata(name),
        description: sanitize_metadata(description),
        path: paths::dirname(skill_md_path),
        raw_content: Some(content),
        plugin_name: None,
        metadata,
        blob: None,
    })
}

fn find_skill_dirs(dir: &str, depth: usize, max_depth: usize) -> Vec<String> {
    if depth > max_depth {
        return Vec::new();
    }
    let mut out = Vec::new();
    if has_skill_md(dir) {
        out.push(dir.to_string());
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let name = entry.file_name().to_string_lossy().to_string();
            if is_dir && !SKIP_DIRS.contains(&name.as_str()) {
                out.extend(find_skill_dirs(
                    &join(&[dir, name.as_str()]),
                    depth + 1,
                    max_depth,
                ));
            }
        }
    }
    out
}

#[derive(Default, Clone, Copy)]
pub struct DiscoverOptions {
    pub include_internal: bool,
    pub full_depth: bool,
    pub include_duplicate_names: bool,
}

/// Validate that `subpath` stays within `base_path`.
pub fn is_subpath_safe(base_path: &str, subpath: &str) -> bool {
    let base = paths::normalize(&paths::resolve1(base_path));
    let target = paths::normalize(&paths::resolve1(&join(&[base_path, subpath])));
    target.starts_with(&format!("{}{}", base, paths::SEP)) || target == base
}

struct Discovery<'a> {
    base_path: &'a str,
    options: DiscoverOptions,
    skills: Vec<Skill>,
    seen_names: HashSet<String>,
    parsed_paths: HashSet<String>,
    locked_names: HashSet<String>,
    groupings: std::collections::HashMap<String, String>,
}

impl<'a> Discovery<'a> {
    fn enhance(&self, mut skill: Skill) -> Skill {
        if let Some(name) = self.groupings.get(&paths::resolve1(&skill.path)) {
            skill.plugin_name = Some(name.clone());
        }
        skill
    }

    fn is_installed_project_skill(&self, skill: &Skill) -> bool {
        if self.locked_names.is_empty() {
            return false;
        }
        let rel = normalize_relative_path(&paths::relative(self.base_path, &skill.path));
        let is_agent_path = AGENT_PROJECT_SKILL_DIRS
            .iter()
            .any(|d| rel == *d || rel.starts_with(&format!("{}/", d)));
        if !is_agent_path {
            return false;
        }
        self.locked_names
            .contains(&normalize_skill_name(&skill.name))
            || self
                .locked_names
                .contains(&normalize_skill_name(&paths::basename(&skill.path)))
    }

    fn parse_at(&mut self, skill_dir: &str) -> Option<Skill> {
        let md = paths::resolve(&[skill_dir, "SKILL.md"]);
        if self.parsed_paths.contains(&md) {
            return None;
        }
        self.parsed_paths.insert(md.clone());
        parse_skill_md(&md, self.options.include_internal)
    }

    fn push(&mut self, skill: Skill) {
        let skill = self.enhance(skill);
        self.seen_names.insert(skill.name.clone());
        self.skills.push(skill);
    }

    fn try_add_at(&mut self, skill_dir: &str) -> bool {
        if !has_skill_md(skill_dir) {
            return false;
        }
        let Some(skill) = self.parse_at(skill_dir) else {
            return true;
        };
        if !self.options.include_duplicate_names && self.seen_names.contains(&skill.name) {
            return true;
        }
        if self.is_installed_project_skill(&skill) {
            return true;
        }
        self.push(skill);
        true
    }

    fn walk(&mut self, dir: &str, max_depth: usize, depth: usize) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let entries: Vec<_> = entries.flatten().collect();
        for entry in entries {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let child = join(&[dir, name.as_str()]);
            let found = self.try_add_at(&child);
            if found || depth >= max_depth || SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            self.walk(&child, max_depth, depth + 1);
        }
    }
}

pub fn discover_skills(
    base_path: &str,
    subpath: Option<&str>,
    options: DiscoverOptions,
) -> Result<Vec<Skill>, String> {
    let local_lock = read_local_lock(Some(base_path));
    let locked_names: HashSet<String> = local_lock
        .skills
        .keys()
        .map(|k| normalize_skill_name(k))
        .collect();

    let subpath = subpath.filter(|s| !s.is_empty());
    if let Some(sp) = subpath {
        if !is_subpath_safe(base_path, sp) {
            return Err(format!(
                "Invalid subpath: \"{}\" resolves outside the repository directory. Subpath must not contain \"..\" segments that escape the base path.",
                sp
            ));
        }
    }
    let search_path = match subpath {
        Some(sp) => join(&[base_path, sp]),
        None => base_path.to_string(),
    };

    let mut d = Discovery {
        base_path,
        options,
        skills: Vec::new(),
        seen_names: HashSet::new(),
        parsed_paths: HashSet::new(),
        locked_names,
        groupings: get_plugin_groupings(&search_path),
    };

    if has_skill_md(&search_path) {
        if let Some(skill) = d.parse_at(&search_path) {
            if !d.is_installed_project_skill(&skill) {
                d.push(skill);
                if !options.full_depth {
                    return Ok(d.skills);
                }
            }
        }
    }

    let mut priority: Vec<String> = vec![
        search_path.clone(),
        join(&[search_path.as_str(), "skills"]),
        join(&[search_path.as_str(), "skills/.curated"]),
        join(&[search_path.as_str(), "skills/.experimental"]),
        join(&[search_path.as_str(), "skills/.system"]),
    ];
    priority.extend(
        AGENT_PROJECT_SKILL_DIRS
            .iter()
            .map(|dir| join(&[search_path.as_str(), dir])),
    );
    let deep: HashSet<String> = priority[1..].iter().cloned().collect();
    priority.extend(get_plugin_skill_paths(&search_path));

    for dir in &priority {
        let max = if deep.contains(dir) {
            DEFAULT_SKILL_CONTAINER_DEPTH
        } else {
            1
        };
        d.walk(dir, max, 1);
    }

    if d.skills.is_empty() || options.full_depth {
        for skill_dir in find_skill_dirs(&search_path, 0, 5) {
            if let Some(skill) = d.parse_at(&skill_dir) {
                if (options.include_duplicate_names || !d.seen_names.contains(&skill.name))
                    && !d.is_installed_project_skill(&skill)
                {
                    d.push(skill);
                }
            }
        }
    }

    Ok(d.skills)
}

pub fn get_skill_display_name(skill: &Skill) -> String {
    if skill.name.is_empty() {
        paths::basename(&skill.path)
    } else {
        skill.name.clone()
    }
}

/// Case-insensitive exact match by name or display name.
pub fn filter_skills(skills: &[Skill], input_names: &[String]) -> Vec<Skill> {
    let inputs: Vec<String> = input_names.iter().map(|n| n.to_lowercase()).collect();
    skills
        .iter()
        .filter(|s| {
            let name = s.name.to_lowercase();
            let display = get_skill_display_name(s).to_lowercase();
            inputs.iter().any(|i| *i == name || *i == display)
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(dir: &str, name: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            join(&[dir, "SKILL.md"]),
            format!("---\nname: {}\ndescription: d {}\n---\nbody\n", name, name),
        )
        .unwrap();
    }

    #[test]
    fn root_skill_returns_early() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().to_string_lossy().to_string();
        write_skill(&root, "root");
        write_skill(&join(&[root.as_str(), "skills", "child"]), "child");
        let found = discover_skills(&root, None, DiscoverOptions::default()).unwrap();
        assert_eq!(found.len(), 1);
        let all = discover_skills(
            &root,
            None,
            DiscoverOptions {
                full_depth: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn nested_container_depth() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().to_string_lossy().to_string();
        write_skill(
            &join(&[root.as_str(), "skills", "cat", "sub", "deep"]),
            "deep",
        );
        write_skill(&join(&[root.as_str(), "examples", "foo"]), "example");
        let found = discover_skills(&root, None, DiscoverOptions::default()).unwrap();
        let names: Vec<_> = found.iter().map(|s| s.name.as_str()).collect();
        // examples/foo sits outside known containers and is only found by the
        // recursive fallback, which does not run once a skill was found.
        assert_eq!(names, vec!["deep"]);
    }

    #[test]
    fn internal_and_invalid_skills() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().to_string_lossy().to_string();
        let d = join(&[root.as_str(), "skills", "hidden"]);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            join(&[d.as_str(), "SKILL.md"]),
            "---\nname: hidden\ndescription: x\nmetadata:\n  internal: true\n---\n",
        )
        .unwrap();
        assert!(parse_skill_md(&join(&[d.as_str(), "SKILL.md"]), false).is_none());
        assert!(parse_skill_md(&join(&[d.as_str(), "SKILL.md"]), true).is_some());
        let bad = join(&[root.as_str(), "bad"]);
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(
            join(&[bad.as_str(), "SKILL.md"]),
            "---\nname: 5\ndescription: x\n---\n",
        )
        .unwrap();
        assert!(parse_skill_md(&join(&[bad.as_str(), "SKILL.md"]), false).is_none());
    }

    #[test]
    fn subpath_traversal_rejected() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().to_string_lossy().to_string();
        assert!(discover_skills(&root, Some("../outside"), DiscoverOptions::default()).is_err());
    }

    #[test]
    fn filter_matches_case_insensitively() {
        let s = Skill {
            name: "My Skill".into(),
            ..Default::default()
        };
        assert_eq!(
            filter_skills(std::slice::from_ref(&s), &["my skill".into()]).len(),
            1
        );
        assert_eq!(filter_skills(&[s], &["my".into()]).len(), 0);
    }

    #[test]
    fn normalize_names() {
        assert_eq!(normalize_skill_name("My_Skill  Name"), "my-skill-name");
    }
}
