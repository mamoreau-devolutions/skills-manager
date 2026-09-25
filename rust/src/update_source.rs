//! Build `skills add` source arguments for updates (port of update-source.ts).

use crate::urlutil;
use serde_json::Value;

#[derive(Clone, Debug, Default)]
pub struct UpdateSourceEntry {
    pub source: String,
    pub source_url: Option<String>,
    pub source_type: Option<String>,
    pub r#ref: Option<String>,
    pub skill_path: Option<String>,
}

fn truthy(v: Option<&Value>) -> Option<String> {
    v.and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

impl UpdateSourceEntry {
    /// Build from a lock-file JSON entry (empty strings count as absent).
    pub fn from_json(e: &Value) -> Self {
        UpdateSourceEntry {
            source: e
                .get("source")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            source_url: truthy(e.get("sourceUrl")),
            source_type: truthy(e.get("sourceType")),
            r#ref: truthy(e.get("ref")),
            skill_path: truthy(e.get("skillPath")),
        }
    }
}

pub fn format_source_input(source_url: &str, r#ref: Option<&str>) -> String {
    match r#ref.filter(|r| !r.is_empty()) {
        None => source_url.to_string(),
        Some(r) => format!("{}#{}", source_url, r),
    }
}

fn derive_skill_folder(skill_path: &str) -> String {
    let mut folder = skill_path.to_string();
    if folder.ends_with("/SKILL.md") {
        folder.truncate(folder.len() - 9);
    } else if folder.ends_with("SKILL.md") {
        folder.truncate(folder.len() - 8);
    }
    if folder.ends_with('/') {
        folder.pop();
    }
    folder
}

fn supports_appended_subpath(source: &str) -> bool {
    if source.starts_with("git@") || source.starts_with("ssh://") || source.ends_with(".git") {
        return false;
    }
    if source.starts_with("http://") || source.starts_with("https://") {
        return match urlutil::parse(source) {
            Some(u) => {
                let h = urlutil::hostname(&u);
                h == "github.com" || h == "gitlab.com"
            }
            None => false,
        };
    }
    true
}

fn is_bare_shorthand(source: &str) -> bool {
    !source.contains(':') && !source.starts_with('.') && !source.starts_with('/')
}

fn get_local_source(e: &UpdateSourceEntry) -> Option<String> {
    if let Some(u) = &e.source_url {
        return Some(u.clone());
    }
    let requires_url = matches!(e.source_type.as_deref(), Some("git") | Some("gitlab"));
    if requires_url && is_bare_shorthand(&e.source) {
        return None;
    }
    Some(e.source.clone())
}

/// Cloneable repository URL for project update checks.
pub fn build_local_clone_source(e: &UpdateSourceEntry) -> Option<String> {
    let source = get_local_source(e)?;
    if e.source_type.as_deref() == Some("github") && is_bare_shorthand(&source) {
        return Some(format!(
            "https://github.com/{}.git",
            source.strip_suffix(".git").unwrap_or(&source)
        ));
    }
    Some(source)
}

pub fn should_use_full_depth_for_update(e: &UpdateSourceEntry) -> bool {
    if e.skill_path.is_none() {
        return false;
    }
    let source = match e.source_type.as_deref() {
        Some(t) if t != "github" => get_local_source(e),
        _ => Some(e.source.clone()),
    };
    source
        .map(|s| !supports_appended_subpath(&s))
        .unwrap_or(false)
}

fn append_folder_and_ref(source: &str, skill_path: &str, r#ref: Option<&str>) -> String {
    if !supports_appended_subpath(source) {
        return format_source_input(source, r#ref);
    }
    let folder = derive_skill_folder(skill_path);
    let with_folder = if folder.is_empty() {
        source.to_string()
    } else {
        format!("{}/{}", source, folder)
    };
    format_source_input(&with_folder, r#ref)
}

/// Source argument for `skills add` during a global update.
pub fn build_update_install_source(e: &UpdateSourceEntry) -> Option<String> {
    let non_github = matches!(e.source_type.as_deref(), Some(t) if t != "github");
    match &e.skill_path {
        None => {
            let source = if non_github {
                get_local_source(e)
            } else {
                Some(e.source_url.clone().unwrap_or_else(|| e.source.clone()))
                    .filter(|s| !s.is_empty())
            };
            Some(format_source_input(&source?, e.r#ref.as_deref()))
        }
        Some(sp) => {
            let source = if non_github {
                get_local_source(e)
            } else {
                Some(e.source.clone())
            };
            let source = source.filter(|s| !s.is_empty())?;
            Some(append_folder_and_ref(&source, sp, e.r#ref.as_deref()))
        }
    }
}

/// Source argument for `skills add` during a project update.
pub fn build_local_update_source(e: &UpdateSourceEntry) -> Option<String> {
    let source = get_local_source(e)?;
    match &e.skill_path {
        None => Some(format_source_input(&source, e.r#ref.as_deref())),
        Some(sp) => Some(append_folder_and_ref(&source, sp, e.r#ref.as_deref())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        source: &str,
        url: Option<&str>,
        t: &str,
        r: Option<&str>,
        sp: Option<&str>,
    ) -> UpdateSourceEntry {
        UpdateSourceEntry {
            source: source.into(),
            source_url: url.map(|s| s.into()),
            source_type: Some(t.into()),
            r#ref: r.map(|s| s.into()),
            skill_path: sp.map(|s| s.into()),
        }
    }

    #[test]
    fn github_shorthand_with_folder_and_ref() {
        let e = entry(
            "o/r",
            Some("https://github.com/o/r.git"),
            "github",
            Some("v1"),
            Some("skills/x/SKILL.md"),
        );
        assert_eq!(
            build_update_install_source(&e).as_deref(),
            Some("o/r/skills/x#v1")
        );
        assert!(!should_use_full_depth_for_update(&e));
        let root = entry("o/r", None, "github", None, Some("SKILL.md"));
        assert_eq!(build_update_install_source(&root).as_deref(), Some("o/r"));
    }

    #[test]
    fn generic_git_sources() {
        let e = entry(
            "team/repo",
            Some("https://git.example.com/team/repo.git"),
            "git",
            None,
            Some("skills/x/SKILL.md"),
        );
        assert_eq!(
            build_local_update_source(&e).as_deref(),
            Some("https://git.example.com/team/repo.git")
        );
        assert!(should_use_full_depth_for_update(&e));
        let legacy = entry("team/repo", None, "git", None, None);
        assert_eq!(build_local_update_source(&legacy), None);
        assert_eq!(build_local_clone_source(&legacy), None);
        let gh = entry("o/r", None, "github", None, None);
        assert_eq!(
            build_local_clone_source(&gh).as_deref(),
            Some("https://github.com/o/r.git")
        );
        let ssh = entry(
            "git@github.com:o/r.git",
            None,
            "git",
            Some("main"),
            Some("a/SKILL.md"),
        );
        assert_eq!(
            build_local_update_source(&ssh).as_deref(),
            Some("git@github.com:o/r.git#main")
        );
    }
}
