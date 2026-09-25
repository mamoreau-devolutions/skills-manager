//! Blob-based skill download (port of blob.ts).
//!
//! 1. GitHub Trees API → discover SKILL.md locations
//! 2. raw.githubusercontent.com → fetch frontmatter to get skill names
//! 3. skills.sh/api/download → fetch full file contents from a cached blob

use crate::collate::locale_compare;
use crate::frontmatter::parse_frontmatter;
use crate::github_host::get_github_host;
use crate::sanitize::sanitize_metadata;
use crate::skill_lock::get_github_token;
use crate::types::{BlobData, Skill, SnapshotFile, DEFAULT_SKILL_CONTAINER_DEPTH};
use crate::urlutil::encode_uri_component;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const GH_API_MAX_BUFFER: usize = 16 * 1024 * 1024;

fn download_base_url() -> String {
    crate::sys::env("SKILLS_DOWNLOAD_URL").unwrap_or_else(|| "https://skills.sh".to_string())
}

/// Repos that self-host their downloads on the blob fast-path.
pub fn blob_allowed_repo_download_url(owner_repo_lower: &str, slug: &str) -> Option<String> {
    match owner_repo_lower {
        "zapier/connectors" => Some(format!(
            "https://connectors-skills.zapier.com/download/{}/snapshot.json",
            encode_uri_component(slug)
        )),
        _ => None,
    }
}

pub fn is_blob_allowed_repo(owner_repo_lower: &str) -> bool {
    blob_allowed_repo_download_url(owner_repo_lower, "").is_some()
}

/// Must match the server-side `toSkillSlug()` exactly.
pub fn to_skill_slug(name: &str) -> String {
    let lower = name.to_lowercase();
    let mut s = String::new();
    let mut in_ws = false;
    for c in lower.chars() {
        if c.is_whitespace() || c == '_' {
            if !in_ws {
                s.push('-');
            }
            in_ws = true;
        } else {
            s.push(c);
            in_ws = false;
        }
    }
    let s: String = s
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .collect();
    let mut collapsed = String::new();
    for c in s.chars() {
        if c == '-' && collapsed.ends_with('-') {
            continue;
        }
        collapsed.push(c);
    }
    let collapsed = collapsed
        .strip_prefix('-')
        .unwrap_or(&collapsed)
        .to_string();
    collapsed
        .strip_suffix('-')
        .unwrap_or(&collapsed)
        .to_string()
}

#[derive(Clone, Debug)]
pub struct TreeEntry {
    pub path: String,
    pub kind: String,
    pub sha: String,
}

#[derive(Clone, Debug)]
pub struct RepoTree {
    pub sha: String,
    pub branch: String,
    pub tree: Vec<TreeEntry>,
}

static RATE_LIMITED: AtomicBool = AtomicBool::new(false);

struct BranchResult {
    tree: Option<RepoTree>,
    rate_limited: bool,
    auth_retryable: bool,
}

fn parse_tree(data: &Value, branch: &str) -> Option<RepoTree> {
    let sha = data.get("sha")?.as_str()?.to_string();
    let arr = data.get("tree")?.as_array()?;
    let tree = arr
        .iter()
        .map(|e| TreeEntry {
            path: e
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            kind: e
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            sha: e
                .get("sha")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        })
        .collect();
    Some(RepoTree {
        sha,
        branch: branch.to_string(),
        tree,
    })
}

fn fetch_tree_branch(owner_repo: &str, branch: &str, token: Option<&str>) -> BranchResult {
    let host = get_github_host();
    let api_base = if host == "github.com" {
        "https://api.github.com".to_string()
    } else {
        format!("https://{}/api/v3", host)
    };
    let url = format!(
        "{}/repos/{}/git/trees/{}?recursive=1",
        api_base,
        owner_repo,
        encode_uri_component(branch)
    );
    let mut req = crate::http::get(&url)
        .timeout(FETCH_TIMEOUT)
        .header("Accept", "application/vnd.github.v3+json")
        .header("User-Agent", "skills-cli");
    if let Some(t) = token {
        req = req.header("Authorization", &format!("Bearer {}", t));
    }
    match req.send() {
        Ok(resp) if resp.ok() => match resp.json().and_then(|d| parse_tree(&d, branch)) {
            Some(tree) => BranchResult {
                tree: Some(tree),
                rate_limited: false,
                auth_retryable: false,
            },
            None => BranchResult {
                tree: None,
                rate_limited: false,
                auth_retryable: false,
            },
        },
        Ok(resp) => BranchResult {
            tree: None,
            rate_limited: resp.status == 403 && resp.header("x-ratelimit-remaining") == Some("0"),
            auth_retryable: resp.status == 401 || resp.status == 404,
        },
        Err(_) => BranchResult {
            tree: None,
            rate_limited: false,
            auth_retryable: false,
        },
    }
}

fn fetch_tree_with_token(owner_repo: &str, branches: &[String]) -> Option<RepoTree> {
    let token = get_github_token()?;
    branches
        .iter()
        .find_map(|b| fetch_tree_branch(owner_repo, b, Some(&token)).tree)
}

fn fetch_tree_with_github_cli(owner_repo: &str, branches: &[String]) -> Option<RepoTree> {
    for branch in branches {
        let endpoint = format!(
            "repos/{}/git/trees/{}?recursive=1",
            owner_repo,
            encode_uri_component(branch)
        );
        let host = get_github_host();
        let out = crate::proc::command(
            "gh",
            &["api", &endpoint, "--method", "GET", "--hostname", &host],
        )
        .env("GH_PROMPT_DISABLED", "1")
        .timeout(FETCH_TIMEOUT)
        .max_output(GH_API_MAX_BUFFER)
        .hide_window()
        .output();
        let Ok(out) = out else { continue };
        if !out.success() {
            continue;
        }
        let Ok(data) = serde_json::from_slice::<Value>(&out.stdout) else {
            continue;
        };
        if let Some(tree) = parse_tree(&data, branch) {
            return Some(tree);
        }
    }
    None
}

fn fetch_tree_with_available_auth(
    owner_repo: &str,
    branches: &[String],
    use_token: bool,
) -> Option<RepoTree> {
    if use_token {
        if let Some(t) = fetch_tree_with_token(owner_repo, branches) {
            return Some(t);
        }
    }
    fetch_tree_with_github_cli(owner_repo, branches)
}

/// Fetch the full recursive tree for a GitHub repo. Anonymous first; then an
/// explicit token (when `use_token`) and `gh api` when the anonymous attempt
/// failed in a way credentials can fix (rate limit, or 401/404).
pub fn fetch_repo_tree(owner_repo: &str, r#ref: Option<&str>, use_token: bool) -> Option<RepoTree> {
    let branches: Vec<String> = match r#ref {
        Some(r) => vec![r.to_string()],
        None => vec!["HEAD".into(), "main".into(), "master".into()],
    };
    if RATE_LIMITED.load(Ordering::SeqCst) {
        return fetch_tree_with_available_auth(owner_repo, &branches, use_token);
    }
    let mut rate_limited = false;
    let mut auth_retryable = false;
    for b in &branches {
        let r = fetch_tree_branch(owner_repo, b, None);
        if r.tree.is_some() {
            return r.tree;
        }
        if r.rate_limited {
            rate_limited = true;
            break;
        }
        if r.auth_retryable {
            auth_retryable = true;
            break;
        }
    }
    if !(rate_limited || auth_retryable) {
        return None;
    }
    if rate_limited {
        RATE_LIMITED.store(true, Ordering::SeqCst);
    }
    fetch_tree_with_available_auth(owner_repo, &branches, use_token)
}

/// Folder tree SHA for a skill path (SKILL.md suffix optional).
pub fn get_skill_folder_hash_from_tree(tree: &RepoTree, skill_path: &str) -> Option<String> {
    let mut folder = skill_path.replace('\\', "/");
    let lower = folder.to_lowercase();
    if lower.ends_with("/skill.md") {
        folder.truncate(folder.len() - 9);
    } else if lower.ends_with("skill.md") {
        folder.truncate(folder.len() - 8);
    }
    if folder.ends_with('/') {
        folder.pop();
    }
    if folder.is_empty() {
        return Some(tree.sha.clone());
    }
    tree.tree
        .iter()
        .find(|e| e.kind == "tree" && e.path == folder)
        .map(|e| e.sha.clone())
}

const PRIORITY_PREFIXES: &[&str] = &[
    "",
    "skills/",
    "skills/.curated/",
    "skills/.experimental/",
    "skills/.system/",
    ".agents/skills/",
    ".claude/skills/",
    ".cline/skills/",
    ".codebuddy/skills/",
    ".codex/skills/",
    ".commandcode/skills/",
    ".continue/skills/",
    ".factory/skills/",
    ".github/skills/",
    ".goose/skills/",
    ".grok/skills/",
    ".iflow/skills/",
    ".junie/skills/",
    ".kilo/skills/",
    ".kilocode/skills/",
    ".kimchi/skills/",
    ".kiro/skills/",
    ".minimax/skills/",
    ".mux/skills/",
    ".neovate/skills/",
    ".opencode/skills/",
    ".openhands/skills/",
    ".pi/skills/",
    ".posit/assistant/skills/",
    ".qoder/skills/",
    ".roo/skills/",
    ".trae/skills/",
    ".windsurf/skills/",
    ".zcode/skills/",
    ".zencoder/skills/",
];

/// All SKILL.md paths in a tree, with the same priority-dir logic as
/// `discover_skills`.
pub fn find_skill_md_paths(tree: &RepoTree, subpath: Option<&str>) -> Vec<String> {
    let all: Vec<String> = tree
        .tree
        .iter()
        .filter(|e| e.kind == "blob" && e.path.to_lowercase().ends_with("skill.md"))
        .map(|e| e.path.clone())
        .collect();
    let prefix = match subpath.filter(|s| !s.is_empty()) {
        Some(s) if s.ends_with('/') => s.to_string(),
        Some(s) => format!("{}/", s),
        None => String::new(),
    };
    let filtered: Vec<String> = if prefix.is_empty() {
        all
    } else {
        all.into_iter()
            .filter(|p| p.starts_with(&prefix) || *p == format!("{}SKILL.md", prefix))
            .collect()
    };
    if filtered.is_empty() {
        return Vec::new();
    }
    let skip: HashSet<&str> = ["node_modules", ".git", "dist", "build", "__pycache__"]
        .into_iter()
        .collect();
    let lower_set: HashSet<String> = filtered.iter().map(|p| p.to_lowercase()).collect();
    let mut results: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for pp in PRIORITY_PREFIXES {
        let full_prefix = format!("{}{}", prefix, pp);
        let is_container = !pp.is_empty();
        for md in &filtered {
            if !md.starts_with(&full_prefix) {
                continue;
            }
            let rest = &md[full_prefix.len()..];
            if rest.to_lowercase() == "skill.md" {
                if seen.insert(md.clone()) {
                    results.push(md.clone());
                }
                continue;
            }
            let parts: Vec<&str> = rest.split('/').collect();
            if parts.len() == 2 && parts[1].to_lowercase() == "skill.md" {
                if seen.insert(md.clone()) {
                    results.push(md.clone());
                }
                continue;
            }
            let dirs = &parts[..parts.len() - 1];
            let has_ancestor = (0..dirs.len().saturating_sub(1)).any(|i| {
                let ancestor = dirs[..=i].join("/");
                lower_set.contains(&format!("{}{}/SKILL.md", full_prefix, ancestor).to_lowercase())
            });
            if is_container
                && parts.len() >= 3
                && parts.len() <= DEFAULT_SKILL_CONTAINER_DEPTH + 1
                && parts.last().unwrap().to_lowercase() == "skill.md"
                && dirs.iter().all(|d| !skip.contains(d))
                && !has_ancestor
                && seen.insert(md.clone())
            {
                results.push(md.clone());
            }
        }
    }
    if !results.is_empty() {
        return results;
    }
    filtered
        .into_iter()
        .filter(|p| p.split('/').count() <= 6)
        .collect()
}

fn fetch_skill_md_content(owner_repo: &str, branch: &str, path: &str) -> Option<String> {
    let url = format!(
        "https://raw.githubusercontent.com/{}/{}/{}",
        owner_repo, branch, path
    );
    let r = crate::http::get(&url).timeout(FETCH_TIMEOUT).send().ok()?;
    if !r.ok() {
        return None;
    }
    Some(r.text())
}

struct Download {
    files: Vec<SnapshotFile>,
    hash: String,
}

fn fetch_skill_download(source: &str, slug: &str) -> Option<Download> {
    let mut parts = source.split('/');
    let owner = parts.next().unwrap_or("");
    let repo = parts.next().unwrap_or("undefined");
    let default_url = format!(
        "{}/api/download/{}/{}/{}",
        download_base_url(),
        encode_uri_component(owner),
        encode_uri_component(repo),
        encode_uri_component(slug)
    );
    let url = blob_allowed_repo_download_url(&source.to_lowercase(), slug).unwrap_or(default_url);
    let r = crate::http::get(&url).timeout(FETCH_TIMEOUT).send().ok()?;
    if !r.ok() {
        return None;
    }
    let data = r.json()?;
    let files = data
        .get("files")?
        .as_array()?
        .iter()
        .map(|f| SnapshotFile {
            path: f
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            contents: f
                .get("contents")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .as_bytes()
                .to_vec(),
        })
        .collect();
    let hash = data
        .get("hash")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Some(Download { files, hash })
}

pub fn compute_snapshot_hash(files: &[SnapshotFile]) -> String {
    let mut sorted: Vec<&SnapshotFile> = files.iter().collect();
    sorted.sort_by(|a, b| locale_compare(&a.path, &b.path));
    let mut h = Sha256::new();
    for f in sorted {
        h.update(f.path.as_bytes());
        h.update(&f.contents);
    }
    hex::encode(h.finalize())
}

fn get_skill_folder_path(md: &str) -> String {
    let lower = md.to_lowercase();
    if lower.ends_with("/skill.md") {
        return md[..md.len() - 9].to_string();
    }
    if lower == "skill.md" {
        return String::new();
    }
    md[..md.len().saturating_sub(9)].to_string()
}

fn is_installable_snapshot_path(p: &str) -> bool {
    let parts: Vec<&str> = p.split('/').collect();
    let file = parts.last().copied().unwrap_or("");
    if file.is_empty() || file == "metadata.json" {
        return false;
    }
    parts[..parts.len() - 1]
        .iter()
        .all(|d| !matches!(*d, ".git" | "__pycache__" | "__pypackages__"))
}

fn has_complete_nested_snapshot(tree: &RepoTree, md: &str, files: &[SnapshotFile]) -> bool {
    let folder = get_skill_folder_path(md);
    if folder.is_empty() {
        return true;
    }
    let prefix = format!("{}/", folder);
    let paths: HashSet<&str> = files.iter().map(|f| f.path.as_str()).collect();
    tree.tree.iter().all(|e| {
        if e.kind != "blob" || !e.path.starts_with(&prefix) {
            return true;
        }
        let rel = &e.path[prefix.len()..];
        !is_installable_snapshot_path(rel) || paths.contains(rel)
    })
}

pub struct BlobInstallResult {
    pub skills: Vec<Skill>,
    pub tree: RepoTree,
}

/// Run `f` over `items` on scoped threads (bounded parallelism), preserving order.
pub fn parallel_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    const CHUNK: usize = 16;
    let mut out = Vec::with_capacity(items.len());
    for chunk in items.chunks(CHUNK) {
        let results: Vec<R> = std::thread::scope(|s| {
            let handles: Vec<_> = chunk.iter().map(|item| s.spawn(|| f(item))).collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("worker panicked"))
                .collect()
        });
        out.extend(results);
    }
    out
}

pub struct BlobOptions<'a> {
    pub subpath: Option<&'a str>,
    pub skill_filter: Option<&'a str>,
    pub r#ref: Option<&'a str>,
    pub use_token: bool,
    pub include_internal: bool,
}

/// Try to resolve skills from blob storage instead of cloning. `None` means
/// the caller should fall back to `git clone`.
pub fn try_blob_install(owner_repo: &str, options: &BlobOptions) -> Option<BlobInstallResult> {
    // Snapshots are ref-agnostic; an explicit ref must use the clone path.
    if options.r#ref.is_some() {
        return None;
    }
    let tree = fetch_repo_tree(owner_repo, None, options.use_token)?;
    let mut md_paths = find_skill_md_paths(&tree, options.subpath);
    if md_paths.is_empty() {
        return None;
    }
    if let Some(filter) = options.skill_filter {
        let slug = to_skill_slug(filter);
        let filtered: Vec<String> = md_paths
            .iter()
            .filter(|p| {
                let parts: Vec<&str> = p.split('/').collect();
                parts.len() >= 2 && to_skill_slug(parts[parts.len() - 2]) == slug
            })
            .cloned()
            .collect();
        if !filtered.is_empty() {
            md_paths = filtered;
        }
    }

    let contents = parallel_map(&md_paths, |p| {
        fetch_skill_md_content(owner_repo, &tree.branch, p)
    });

    struct Parsed {
        md_path: String,
        name: String,
        description: String,
        content: String,
        slug: String,
        metadata: Option<serde_json::Map<String, Value>>,
    }
    let mut parsed: Vec<Parsed> = Vec::new();
    for (md_path, content) in md_paths.iter().zip(contents) {
        let Some(content) = content else { continue };
        let Ok(fm) = parse_frontmatter(&content) else {
            return None;
        };
        let data = fm.data;
        let (Some(Value::String(name)), Some(Value::String(desc))) =
            (data.get("name"), data.get("description"))
        else {
            continue;
        };
        if name.is_empty() || desc.is_empty() {
            continue;
        }
        let metadata = data.get("metadata").and_then(|m| m.as_object()).cloned();
        let internal =
            data.get("metadata").and_then(|m| m.get("internal")) == Some(&Value::Bool(true));
        if internal && !options.include_internal {
            continue;
        }
        let safe_name = sanitize_metadata(name);
        parsed.push(Parsed {
            md_path: md_path.clone(),
            slug: to_skill_slug(&safe_name),
            name: safe_name,
            description: sanitize_metadata(desc),
            content,
            metadata,
        });
    }
    if parsed.is_empty() {
        return None;
    }
    if let Some(filter) = options.skill_filter {
        let slug = to_skill_slug(filter);
        let by_name: Vec<Parsed> = parsed
            .iter()
            .filter(|s| s.slug == slug)
            .map(|s| Parsed {
                md_path: s.md_path.clone(),
                name: s.name.clone(),
                description: s.description.clone(),
                content: s.content.clone(),
                slug: s.slug.clone(),
                metadata: s.metadata.clone(),
            })
            .collect();
        if !by_name.is_empty() {
            parsed = by_name;
        }
    }

    let source = owner_repo.to_lowercase();
    let downloads = parallel_map(&parsed, |s| fetch_skill_download(&source, &s.slug));
    if downloads.iter().any(|d| d.is_none()) {
        return None;
    }
    let downloads: Vec<Download> = downloads.into_iter().map(|d| d.unwrap()).collect();
    if !parsed
        .iter()
        .zip(&downloads)
        .all(|(s, d)| has_complete_nested_snapshot(&tree, &s.md_path, &d.files))
    {
        return None;
    }

    let skills = parsed
        .into_iter()
        .zip(downloads)
        .map(|(s, d)| {
            let folder = get_skill_folder_path(&s.md_path);
            let total = d.files.len();
            let files: Vec<SnapshotFile> = if folder.is_empty() {
                d.files
                    .into_iter()
                    .filter(|f| f.path.to_lowercase() == "skill.md")
                    .collect()
            } else {
                d.files
            };
            let snapshot_hash = if files.len() == total {
                d.hash
            } else {
                compute_snapshot_hash(&files)
            };
            Skill {
                name: s.name,
                description: s.description,
                path: String::new(),
                raw_content: Some(s.content),
                plugin_name: None,
                metadata: s.metadata,
                blob: Some(BlobData {
                    files,
                    snapshot_hash,
                    repo_path: s.md_path,
                }),
            }
        })
        .collect();
    Some(BlobInstallResult { skills, tree })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(paths: &[(&str, &str)]) -> RepoTree {
        RepoTree {
            sha: "root".into(),
            branch: "main".into(),
            tree: paths
                .iter()
                .map(|(p, k)| TreeEntry {
                    path: p.to_string(),
                    kind: k.to_string(),
                    sha: format!("sha-{}", p),
                })
                .collect(),
        }
    }

    #[test]
    fn slugs() {
        assert_eq!(to_skill_slug("My Cool_Skill!"), "my-cool-skill");
        assert_eq!(to_skill_slug("--a--b--"), "a-b");
    }

    #[test]
    fn folder_hash_lookup() {
        let t = tree(&[("skills/a", "tree"), ("skills/a/SKILL.md", "blob")]);
        assert_eq!(
            get_skill_folder_hash_from_tree(&t, "skills/a/SKILL.md").as_deref(),
            Some("sha-skills/a")
        );
        assert_eq!(
            get_skill_folder_hash_from_tree(&t, "SKILL.md").as_deref(),
            Some("root")
        );
        assert_eq!(
            get_skill_folder_hash_from_tree(&t, "missing/SKILL.md"),
            None
        );
    }

    #[test]
    fn priority_paths() {
        let t = tree(&[
            ("skills/a/SKILL.md", "blob"),
            ("skills/cat/b/SKILL.md", "blob"),
            ("skills/a/nested/SKILL.md", "blob"),
            ("examples/x/SKILL.md", "blob"),
        ]);
        assert_eq!(
            find_skill_md_paths(&t, None),
            vec!["skills/a/SKILL.md", "skills/cat/b/SKILL.md"]
        );
        let only_examples = tree(&[("examples/x/SKILL.md", "blob")]);
        assert_eq!(
            find_skill_md_paths(&only_examples, None),
            vec!["examples/x/SKILL.md"]
        );
        assert_eq!(
            find_skill_md_paths(&t, Some("skills/a")),
            vec!["skills/a/SKILL.md", "skills/a/nested/SKILL.md"]
        );
    }
}
