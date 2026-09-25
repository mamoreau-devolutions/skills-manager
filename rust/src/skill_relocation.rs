//! Resolve locked skills against their current locations
//! (port of skill-relocation.ts).

use serde_json::{Map, Value};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct DiscoveredSkillLocation {
    pub name: String,
    pub skill_path: String,
}

pub struct SkillLocationResolution {
    pub deleted_skills: Vec<String>,
    pub ambiguous_skills: Vec<String>,
    pub resolved_paths: HashMap<String, String>,
}

fn normalize_name(n: &str) -> String {
    crate::skills::normalize_skill_name(n)
}

fn normalize_path(p: &str) -> String {
    let s = p.replace('\\', "/");
    let mut out = String::new();
    for c in s.chars() {
        if c == '/' && out.ends_with('/') {
            continue;
        }
        out.push(c);
    }
    out
}

/// Exact paths win; a missing path is a relocation only when exactly one
/// discovered skill has the same normalized name. Ambiguity fails closed.
pub fn resolve_skill_locations(
    locked: &[String],
    lock_skills: &Map<String, Value>,
    discovered: &[DiscoveredSkillLocation],
) -> SkillLocationResolution {
    let discovered_paths: Vec<String> = discovered
        .iter()
        .map(|d| normalize_path(&d.skill_path))
        .collect();
    let mut by_name: HashMap<String, Vec<String>> = HashMap::new();
    for d in discovered {
        let list = by_name.entry(normalize_name(&d.name)).or_default();
        let p = normalize_path(&d.skill_path);
        if !list.contains(&p) {
            list.push(p);
        }
    }
    let mut out = SkillLocationResolution {
        deleted_skills: Vec::new(),
        ambiguous_skills: Vec::new(),
        resolved_paths: HashMap::new(),
    };
    for name in locked {
        let Some(locked_path) = lock_skills
            .get(name)
            .and_then(|e| e.get("skillPath"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let nlp = normalize_path(locked_path);
        let candidates = by_name
            .get(&normalize_name(name))
            .cloned()
            .unwrap_or_default();
        if candidates.len() > 1 {
            out.ambiguous_skills.push(name.clone());
            continue;
        }
        if discovered_paths.contains(&nlp) {
            out.resolved_paths.insert(name.clone(), nlp);
            continue;
        }
        if candidates.len() == 1 {
            out.resolved_paths
                .insert(name.clone(), candidates[0].clone());
        } else {
            out.deleted_skills.push(name.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolves_relocations() {
        let lock = json!({"a": {"skillPath": "skills/a/SKILL.md"}, "b": {"skillPath": "old/b/SKILL.md"}, "c": {"skillPath": "c/SKILL.md"}, "d": {"skillPath": "d/SKILL.md"}});
        let lock = lock.as_object().unwrap().clone();
        let disc = vec![
            DiscoveredSkillLocation {
                name: "a".into(),
                skill_path: "skills/a/SKILL.md".into(),
            },
            DiscoveredSkillLocation {
                name: "b".into(),
                skill_path: "new/b/SKILL.md".into(),
            },
            DiscoveredSkillLocation {
                name: "d".into(),
                skill_path: "x/d/SKILL.md".into(),
            },
            DiscoveredSkillLocation {
                name: "d".into(),
                skill_path: "y/d/SKILL.md".into(),
            },
        ];
        let names: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        let r = resolve_skill_locations(&names, &lock, &disc);
        assert_eq!(r.resolved_paths["a"], "skills/a/SKILL.md");
        assert_eq!(r.resolved_paths["b"], "new/b/SKILL.md");
        assert_eq!(r.deleted_skills, vec!["c"]);
        assert_eq!(r.ambiguous_skills, vec!["d"]);
    }
}
