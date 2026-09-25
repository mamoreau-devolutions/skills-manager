//! Skills installed by the GitHub CLI's `gh skill install` (extension; not in
//! the reference CLI).
//!
//! `gh skill` copies skills into the same agent directories but records their
//! origin in SKILL.md frontmatter (`metadata.github-repo`, `github-ref`,
//! `github-tree-sha`, `github-path`, `github-pinned`, or `local-path`)
//! instead of a lock file. `skills list` shows that origin, and
//! `skills update` reports available updates but leaves reinstalling to gh.

use crate::agents::agents;
use crate::blob::{fetch_repo_tree, get_skill_folder_hash_from_tree};
use crate::collate::ordinal_cmp;
use crate::color::ansi::{DIM, RESET};
use crate::frontmatter::parse_frontmatter;
use crate::github_release::resolve_latest_release;
use crate::installer::{get_canonical_skills_dir, sanitize_name};
use crate::paths::join;
use crate::sanitize::sanitize_metadata;
use crate::{outln, sys};
use serde_json::{Map, Value};

#[derive(Clone, Debug, PartialEq)]
pub enum GhOrigin {
    GitHub {
        /// `metadata.github-repo` as written.
        url: String,
        host: String,
        owner: String,
        repo: String,
        /// `metadata.github-ref` (e.g. `refs/heads/main`, `refs/tags/v1`, a SHA).
        git_ref: Option<String>,
        tree_sha: Option<String>,
        path: Option<String>,
        /// `metadata.github-pinned`; `None` when absent or empty.
        pinned: Option<String>,
    },
    Local {
        path: String,
    },
}

impl GhOrigin {
    /// `owner/repo` (github.com), `host/owner/repo`, or the local path.
    pub fn source(&self) -> String {
        match self {
            GhOrigin::GitHub {
                host, owner, repo, ..
            } => {
                if host.eq_ignore_ascii_case("github.com") {
                    format!("{}/{}", owner, repo)
                } else {
                    format!("{}/{}/{}", host, owner, repo)
                }
            }
            GhOrigin::Local { path } => path.clone(),
        }
    }

    /// The `Source:` value shown by `skills list`.
    pub fn list_label(&self) -> String {
        match self {
            GhOrigin::GitHub {
                pinned: Some(p), ..
            } => format!("{} (gh skill, pinned {})", self.source(), p),
            _ => format!("{} (gh skill)", self.source()),
        }
    }
}

/// `https://<host>/<owner>/<repo>` (trailing `/` and `.git` ignored).
pub fn parse_repo_url(url: &str) -> Option<(String, String, String)> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let rest = rest.trim_end_matches('/');
    let parts: Vec<&str> = rest.split('/').collect();
    if parts.len() != 3 {
        return None;
    }
    let repo = parts[2].strip_suffix(".git").unwrap_or(parts[2]);
    if parts[0].is_empty() || parts[1].is_empty() || repo.is_empty() {
        return None;
    }
    Some((parts[0].to_string(), parts[1].to_string(), repo.to_string()))
}

/// `github-ref` without a `refs/heads/` or `refs/tags/` prefix.
pub fn short_ref(r: &str) -> String {
    r.strip_prefix("refs/heads/")
        .or_else(|| r.strip_prefix("refs/tags/"))
        .unwrap_or(r)
        .to_string()
}

fn meta_str(m: &Map<String, Value>, key: &str) -> Option<String> {
    m.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// The gh origin recorded in a SKILL.md's frontmatter, if any.
pub fn parse_gh_origin(skill_md: &str) -> Option<GhOrigin> {
    let fm = parse_frontmatter(skill_md).ok()?;
    let Some(Value::Object(m)) = fm.data.get("metadata") else {
        return None;
    };
    if let Some((host, owner, repo)) = meta_str(m, "github-repo").and_then(|u| parse_repo_url(&u)) {
        return Some(GhOrigin::GitHub {
            url: meta_str(m, "github-repo").unwrap_or_default(),
            host,
            owner,
            repo,
            git_ref: meta_str(m, "github-ref"),
            tree_sha: meta_str(m, "github-tree-sha"),
            path: meta_str(m, "github-path"),
            pinned: meta_str(m, "github-pinned"),
        });
    }
    meta_str(m, "local-path").map(|path| GhOrigin::Local { path })
}

pub fn read_gh_origin(skill_dir: &str) -> Option<GhOrigin> {
    let raw = crate::skills::read_utf8_lossy(&join(&[skill_dir, "SKILL.md"])).ok()?;
    parse_gh_origin(&raw)
}

/// Whether a lock has an entry for `name` (exact, else by sanitized name, as
/// `skills list` matches them).
pub fn lock_has_skill(locked: &Map<String, Value>, name: &str) -> bool {
    if locked.contains_key(name) {
        return true;
    }
    let s = sanitize_name(name);
    locked.keys().any(|k| sanitize_name(k) == s)
}

#[derive(Clone, Debug)]
pub struct GhSkill {
    pub name: String,
    pub dir: String,
    pub origin: GhOrigin,
}

fn scan_roots(global: bool) -> Vec<String> {
    let mut roots = vec![get_canonical_skills_dir(global, None)];
    let cwd = sys::cwd();
    for a in agents() {
        let dir = if global {
            match &a.global_skills_dir {
                Some(d) => d.clone(),
                None => continue,
            }
        } else {
            join(&[cwd.as_str(), a.skills_dir])
        };
        if !roots.contains(&dir) {
            roots.push(dir);
        }
    }
    roots
}

/// Name of a valid installed skill (frontmatter `name` and `description`
/// non-empty strings), parsed without printing warnings.
fn installed_skill_name(raw: &str) -> Option<String> {
    let fm = parse_frontmatter(raw).ok()?;
    match (fm.data.get("name"), fm.data.get("description")) {
        (Some(Value::String(n)), Some(Value::String(d))) if !n.is_empty() && !d.is_empty() => {
            Some(sanitize_metadata(n))
        }
        _ => None,
    }
}

/// gh-installed skills of one scope that the lock does not track. Directories
/// are scanned in order (canonical `.agents/skills`, then each agent's
/// directory in agents-table order), entries ordinal by directory name; the
/// first skill with a given name wins.
pub fn scan_gh_skills(global: bool, locked: &Map<String, Value>) -> Vec<GhSkill> {
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for root in scan_roots(global) {
        let Ok(entries) = crate::sys::read_dir(&root) else {
            continue;
        };
        let mut names: Vec<String> = entries
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        names.sort_by(|a, b| ordinal_cmp(a, b));
        for n in names {
            let dir = join(&[root.as_str(), n.as_str()]);
            if !std::fs::metadata(&dir).map(|m| m.is_dir()).unwrap_or(false) {
                continue;
            }
            let Ok(raw) = crate::skills::read_utf8_lossy(&join(&[dir.as_str(), "SKILL.md"])) else {
                continue;
            };
            let Some(name) = installed_skill_name(&raw) else {
                continue;
            };
            if seen.contains(&name) {
                continue;
            }
            seen.push(name.clone());
            if lock_has_skill(locked, &name) {
                continue;
            }
            if let Some(origin) = parse_gh_origin(&raw) {
                out.push(GhSkill { name, dir, origin });
            }
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq)]
pub enum GhStatus {
    UpdateAvailable,
    Pinned(String),
}

/// SKILL.md path for the tree lookup: `github-path` itself when it names the
/// file, else `<github-path>/SKILL.md`.
pub fn gh_skill_md_path(path: &str) -> String {
    if path.ends_with("SKILL.md") {
        path.to_string()
    } else {
        format!("{}/SKILL.md", path.trim_end_matches('/'))
    }
}

/// Check gh-installed skills the way `gh skill update` does: against the
/// latest release, else the default branch. Only github.com skills with a
/// tree SHA and path are checked; failures are ignored. Results are in input
/// order; each repository is fetched once.
pub fn check_gh_skills(skills: &[GhSkill]) -> Vec<(usize, GhStatus)> {
    let mut results: Vec<(usize, GhStatus)> = Vec::new();
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (i, s) in skills.iter().enumerate() {
        let GhOrigin::GitHub {
            host,
            owner,
            repo,
            tree_sha: Some(_),
            path: Some(_),
            pinned,
            ..
        } = &s.origin
        else {
            continue;
        };
        if !host.eq_ignore_ascii_case("github.com") {
            continue;
        }
        if let Some(p) = pinned {
            results.push((i, GhStatus::Pinned(p.clone())));
            continue;
        }
        let key = format!("{}/{}", owner, repo);
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, v)) => v.push(i),
            None => groups.push((key, vec![i])),
        }
    }
    for (owner_repo, idxs) in &groups {
        let r#ref = match resolve_latest_release(owner_repo) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let Some(tree) = fetch_repo_tree(owner_repo, r#ref.as_deref(), true) else {
            continue;
        };
        for &i in idxs {
            let GhOrigin::GitHub {
                tree_sha: Some(sha),
                path: Some(path),
                ..
            } = &skills[i].origin
            else {
                continue;
            };
            if let Some(latest) = get_skill_folder_hash_from_tree(&tree, &gh_skill_md_path(path)) {
                if latest != *sha {
                    results.push((i, GhStatus::UpdateAvailable));
                }
            }
        }
    }
    results.sort_by_key(|(i, _)| *i);
    results
}

/// For `skills update`: check the gh-installed skills of one scope that match
/// the name filter and print the `Managed by gh skill` notice when any has an
/// update or is pinned. Returns how many gh skills matched the filter.
pub fn report_gh_skills(
    global: bool,
    locked: &Map<String, Value>,
    filter: &Option<Vec<String>>,
) -> usize {
    let skills: Vec<GhSkill> = scan_gh_skills(global, locked)
        .into_iter()
        .filter(|s| crate::update::matches_skill_filter(&s.name, filter))
        .collect();
    if skills.is_empty() {
        return 0;
    }
    let results = check_gh_skills(&skills);
    if !results.is_empty() {
        outln!();
        outln!(
            "{}Managed by gh skill (update them with gh skill update):{}",
            DIM,
            RESET
        );
        for (i, status) in &results {
            let s = &skills[*i];
            let tail = match status {
                GhStatus::UpdateAvailable => "update available".to_string(),
                GhStatus::Pinned(p) => format!("pinned to {}", sanitize_metadata(p)),
            };
            outln!(
                "  • {} {}({}){} {}",
                sanitize_metadata(&s.name),
                DIM,
                sanitize_metadata(&s.origin.source()),
                RESET,
                tail
            );
        }
    }
    skills.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_repo_urls() {
        assert_eq!(
            parse_repo_url("https://github.com/o/r.git/"),
            Some(("github.com".into(), "o".into(), "r".into()))
        );
        assert_eq!(parse_repo_url("https://github.com/o"), None);
        assert_eq!(parse_repo_url("https://github.com/o/r/tree/main"), None);
        assert_eq!(parse_repo_url("git@github.com:o/r"), None);
        assert_eq!(short_ref("refs/heads/main"), "main");
        assert_eq!(short_ref("refs/tags/v1.0"), "v1.0");
        assert_eq!(short_ref("abc123"), "abc123");
    }

    #[test]
    fn parses_gh_metadata() {
        let md = "---\r\nname: foo\r\ndescription: d\r\nmetadata:\r\n  github-repo: https://github.com/acme/skills\r\n  github-ref: refs/tags/v1.0\r\n  github-tree-sha: abc\r\n  github-path: skills/foo\r\n  github-pinned: v1.0\r\n---\r\nbody\r\n";
        let o = parse_gh_origin(md).unwrap();
        assert_eq!(o.source(), "acme/skills");
        assert_eq!(o.list_label(), "acme/skills (gh skill, pinned v1.0)");
        let GhOrigin::GitHub {
            git_ref, tree_sha, ..
        } = &o
        else {
            panic!()
        };
        assert_eq!(git_ref.as_deref(), Some("refs/tags/v1.0"));
        assert_eq!(tree_sha.as_deref(), Some("abc"));

        let ghe = "---\nname: x\ndescription: d\nmetadata:\n  github-repo: https://ghe.corp/o/r\n  github-pinned: ''\n---\n";
        let o = parse_gh_origin(ghe).unwrap();
        assert_eq!(o.list_label(), "ghe.corp/o/r (gh skill)");

        let local = "---\nname: x\ndescription: d\nmetadata:\n  local-path: /src/x\n---\n";
        assert_eq!(
            parse_gh_origin(local),
            Some(GhOrigin::Local {
                path: "/src/x".into()
            })
        );
        assert_eq!(
            parse_gh_origin("---\nname: x\nmetadata:\n  author: me\n---\n"),
            None
        );
        assert_eq!(parse_gh_origin("no frontmatter"), None);
        assert_eq!(parse_gh_origin("---\nname: [\n---\n"), None);
    }

    #[test]
    fn skill_md_paths() {
        assert_eq!(gh_skill_md_path("skills/foo"), "skills/foo/SKILL.md");
        assert_eq!(gh_skill_md_path("skills/foo/"), "skills/foo/SKILL.md");
        assert_eq!(
            gh_skill_md_path("skills/foo/SKILL.md"),
            "skills/foo/SKILL.md"
        );
    }

    #[test]
    fn lock_matching() {
        let mut m = Map::new();
        m.insert("My Skill".into(), Value::Null);
        assert!(lock_has_skill(&m, "My Skill"));
        assert!(lock_has_skill(&m, "my-skill"));
        assert!(!lock_has_skill(&m, "other"));
    }
}
