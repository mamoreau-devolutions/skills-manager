//! Claude plugin manifest discovery (port of plugin-manifest.ts).

use crate::paths::{self, join};
use serde_json::Value;
use std::collections::HashMap;

fn is_contained_in(target: &str, base: &str) -> bool {
    paths::is_path_safe(base, target)
}

fn is_valid_relative_path(p: &str) -> bool {
    p.starts_with("./")
}

fn read_json(path: &str) -> Option<Value> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn string_array(v: Option<&Value>) -> Option<Vec<String>> {
    match v {
        Some(Value::Array(a)) => Some(
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect(),
        ),
        _ => None,
    }
}

struct MarketplacePlugin {
    source: Option<String>,
    skills: Option<Vec<String>>,
    name: Option<String>,
}

/// Parse marketplace plugins; returns `(pluginRoot, plugins)` when the
/// pluginRoot is valid. Remote (object) sources are filtered out.
fn marketplace(base: &str) -> Option<(String, Vec<MarketplacePlugin>)> {
    let manifest = read_json(&join(&[base, ".claude-plugin/marketplace.json"]))?;
    let plugin_root = manifest.get("metadata").and_then(|m| m.get("pluginRoot"));
    let plugin_root = match plugin_root {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => return Some((String::new(), Vec::new())),
    };
    if let Some(r) = &plugin_root {
        if !is_valid_relative_path(r) {
            return Some((String::new(), Vec::new()));
        }
    }
    let mut plugins = Vec::new();
    if let Some(Value::Array(list)) = manifest.get("plugins") {
        for p in list {
            let source = match p.get("source") {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) => Some(s.clone()),
                Some(_) => continue,
            };
            if let Some(s) = &source {
                if !is_valid_relative_path(s) {
                    continue;
                }
            }
            plugins.push(MarketplacePlugin {
                source,
                skills: string_array(p.get("skills")),
                name: p
                    .get("name")
                    .and_then(|n| n.as_str())
                    .map(|s| s.to_string())
                    .filter(|s| !s.is_empty()),
            });
        }
    }
    Some((plugin_root.unwrap_or_default(), plugins))
}

/// Directories that contain skills, derived from plugin manifests.
pub fn get_plugin_skill_paths(base_path: &str) -> Vec<String> {
    let mut search_dirs = Vec::new();
    let mut add = |plugin_base: &str, skills: &Option<Vec<String>>| {
        if !is_contained_in(plugin_base, base_path) {
            return;
        }
        if let Some(skills) = skills {
            for skill_path in skills {
                if !is_valid_relative_path(skill_path) {
                    continue;
                }
                let skill_dir = paths::dirname(&join(&[plugin_base, skill_path.as_str()]));
                if is_contained_in(&skill_dir, base_path) {
                    search_dirs.push(skill_dir);
                }
            }
        }
        search_dirs.push(join(&[plugin_base, "skills"]));
    };

    if let Some((root, plugins)) = marketplace(base_path) {
        for plugin in &plugins {
            let plugin_base = join(&[
                base_path,
                root.as_str(),
                plugin.source.as_deref().unwrap_or(""),
            ]);
            add(&plugin_base, &plugin.skills);
        }
    }

    if let Some(manifest) = read_json(&join(&[base_path, ".claude-plugin/plugin.json"])) {
        if manifest.is_object() {
            add(base_path, &string_array(manifest.get("skills")));
        }
    }
    search_dirs
}

/// Map of absolute skill directory → plugin name.
pub fn get_plugin_groupings(base_path: &str) -> HashMap<String, String> {
    let mut groupings = HashMap::new();
    if let Some((root, plugins)) = marketplace(base_path) {
        for plugin in &plugins {
            let Some(name) = &plugin.name else { continue };
            let plugin_base = join(&[
                base_path,
                root.as_str(),
                plugin.source.as_deref().unwrap_or(""),
            ]);
            if !is_contained_in(&plugin_base, base_path) {
                continue;
            }
            if let Some(skills) = &plugin.skills {
                for skill_path in skills {
                    if !is_valid_relative_path(skill_path) {
                        continue;
                    }
                    let skill_dir = join(&[plugin_base.as_str(), skill_path.as_str()]);
                    if is_contained_in(&skill_dir, base_path) {
                        groupings.insert(paths::resolve1(&skill_dir), name.clone());
                    }
                }
            }
        }
    }
    if let Some(manifest) = read_json(&join(&[base_path, ".claude-plugin/plugin.json"])) {
        let name = manifest
            .get("name")
            .and_then(|n| n.as_str())
            .filter(|s| !s.is_empty());
        let skills = string_array(manifest.get("skills"));
        if let (Some(name), Some(skills)) = (name, skills) {
            for skill_path in &skills {
                if !is_valid_relative_path(skill_path) {
                    continue;
                }
                let skill_dir = join(&[base_path, skill_path.as_str()]);
                if is_contained_in(&skill_dir, base_path) {
                    groupings.insert(paths::resolve1(&skill_dir), name.to_string());
                }
            }
        }
    }
    groupings
}
