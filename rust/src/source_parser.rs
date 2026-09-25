//! Parse git URLs, GitHub shorthand, local paths, well-known and download
//! URLs (port of source-parser.ts).

use crate::github_host::{get_github_host, is_github_host};
use crate::paths;
use crate::types::ParsedSource;
use crate::urlutil::{self, decode_uri_component, encode_uri_component};
use regex::Regex;
use std::sync::OnceLock;
use std::time::Duration;

macro_rules! re {
    ($name:ident, $pat:expr) => {
        fn $name() -> &'static Regex {
            static R: OnceLock<Regex> = OnceLock::new();
            R.get_or_init(|| Regex::new($pat).unwrap())
        }
    };
}

re!(re_ssh, r"^git@[^:]+:(.+)$");
re!(re_dotgit_end, r"\.git$");
re!(re_owner_repo, r"^([^/]+)/([^/]+)$");
re!(re_azure_version, r"(?i)^(?:GB|GT)(.+)$");
re!(re_win_drive, r"^[a-zA-Z]:[/\\]");
re!(re_ssh_git, r"(?i)^ssh://.+\.git(?:$|[/?])");
re!(
    re_gh_repo_tree_path,
    r"^/[^/]+/[^/]+(?:\.git)?(?:/tree/[^/]+(?:/.*)?)?/?$"
);
re!(
    re_gl_repo_tree_path,
    r"^/.+?/[^/]+(?:\.git)?(?:/-/tree/[^/]+(?:/.*)?)?/?$"
);
re!(re_http_git, r"(?i)^https?://.+\.git(?:$|[/?])");
re!(re_shorthand_like, r"^([^/]+)/([^/]+)(?:/(.+)|@(.+))?$");
re!(
    re_gh_artifact,
    r"^/[^/]+/[^/]+/(?:archive/|raw/|releases/(?:download/|latest/download/))"
);
re!(re_gl_artifact, r"/-/(?:archive|raw)/");
re!(re_github_prefix, r"(?s)^github:(.+)$");
re!(re_gitlab_prefix, r"(?s)^gitlab:(.+)$");
re!(re_http_prefix, r"^https?://");
re!(
    re_gh_tree_with_path,
    r"github\.com/([^/]+)/([^/]+)/tree/([^/]+)/(.+)"
);
re!(re_gh_tree, r"github\.com/([^/]+)/([^/]+)/tree/([^/]+)$");
re!(re_gh_repo, r"github\.com/([^/]+)/([^/]+)");
re!(
    re_gl_tree_with_path,
    r"^(https?)://([^/]+)/(.+?)/-/tree/([^/]+)/(.+)"
);
re!(re_gl_tree, r"^(https?)://([^/]+)/(.+?)/-/tree/([^/]+)$");
re!(re_gl_repo, r"gitlab\.com/(.+?)(?:\.git)?/?$");
re!(re_at_skill, r"^([^/]+)/([^/@]+)@(.+)$");
re!(re_shorthand, r"^([^/]+)/([^/]+)(?:/(.+?))?/?$");

fn strip_dot_git(s: &str) -> String {
    re_dotgit_end().replace(s, "").into_owned()
}

/// Extract owner/repo (or group/subgroup/repo) for lockfile tracking and
/// telemetry. `None` for local paths, downloads or unparseable sources.
pub fn get_owner_repo(parsed: &ParsedSource) -> Option<String> {
    if parsed.kind == "local" || parsed.kind == "download" {
        return None;
    }
    if let Some(c) = re_ssh().captures(&parsed.url) {
        let path = strip_dot_git(&c[1]);
        return if path.contains('/') { Some(path) } else { None };
    }
    if parsed.url.starts_with("ssh://") {
        let u = urlutil::parse(&parsed.url)?;
        let p = urlutil::pathname(&u);
        let path = strip_dot_git(p.get(1..).unwrap_or(""));
        return if path.contains('/') { Some(path) } else { None };
    }
    if !parsed.url.starts_with("http://") && !parsed.url.starts_with("https://") {
        return None;
    }
    let u = urlutil::parse(&parsed.url)?;
    let p = urlutil::pathname(&u);
    let path = strip_dot_git(p.get(1..).unwrap_or(""));
    if path.contains('/') {
        Some(path)
    } else {
        None
    }
}

/// Split an owner/repo string.
pub fn parse_owner_repo(owner_repo: &str) -> Option<(String, String)> {
    re_owner_repo()
        .captures(owner_repo)
        .map(|c| (c[1].to_string(), c[2].to_string()))
}

/// Whether a GitHub repository is private: `Some(true)` private,
/// `Some(false)` public, `None` when it cannot be determined.
pub fn is_repo_private(owner: &str, repo: &str) -> Option<bool> {
    let url = format!("https://api.github.com/repos/{}/{}", owner, repo);
    let res = crate::http::get(&url)
        .timeout(Duration::from_secs(30))
        .send()
        .ok()?;
    if !res.ok() {
        return None;
    }
    let data = res.json()?;
    Some(data.get("private") == Some(&serde_json::Value::Bool(true)))
}

/// Reject subpaths containing `..` segments.
pub fn sanitize_subpath(subpath: &str) -> Result<String, String> {
    let normalized = subpath.replace('\\', "/");
    if normalized.split('/').any(|s| s == "..") {
        return Err(format!(
            "Unsafe subpath: \"{}\" contains path traversal segments. Subpaths must not contain \"..\" components.",
            subpath
        ));
    }
    Ok(subpath.to_string())
}

fn azure_version_to_ref(version: Option<String>) -> Option<String> {
    let v = version?;
    if v.is_empty() {
        return None;
    }
    re_azure_version()
        .captures(&v)
        .map(|c| c[1].to_string())
        .filter(|s| !s.is_empty())
}

fn parse_azure_repos_source(
    input: &str,
    fragment_ref: &Option<String>,
    fragment_skill_filter: &Option<String>,
) -> Result<Option<ParsedSource>, String> {
    if !input.starts_with("http://") && !input.starts_with("https://") {
        return Ok(None);
    }
    let Some(parsed) = urlutil::parse(input) else {
        return Ok(None);
    };
    let pathname = urlutil::pathname(&parsed);
    let segments: Vec<&str> = pathname.split('/').filter(|s| !s.is_empty()).collect();
    let git_index = match segments.iter().position(|s| *s == "_git") {
        Some(i) if i != segments.len() - 1 => i,
        _ => return Ok(None),
    };
    let repo = strip_dot_git(segments[git_index + 1]);
    if repo.is_empty() {
        return Ok(None);
    }
    let mut parts: Vec<String> = segments[..git_index]
        .iter()
        .map(|s| s.to_string())
        .collect();
    parts.push("_git".to_string());
    parts.push(repo);
    let clone_path = format!(
        "/{}",
        parts
            .iter()
            .map(|seg| match decode_uri_component(seg) {
                Ok(d) => encode_uri_component(&d),
                Err(_) => encode_uri_component(seg),
            })
            .collect::<Vec<_>>()
            .join("/")
    );
    let clone_url = format!(
        "{}//{}{}",
        urlutil::protocol(&parsed),
        urlutil::host(&parsed),
        clone_path
    );
    let r = azure_version_to_ref(urlutil::search_param(&parsed, "version"))
        .or_else(|| fragment_ref.clone());
    let subpath = match urlutil::search_param(&parsed, "path") {
        Some(p) if !p.is_empty() => {
            let trimmed = p.trim_start_matches('/').to_string();
            Some(sanitize_subpath(&trimmed)?).filter(|s| !s.is_empty())
        }
        _ => None,
    };
    let mut out = ParsedSource::new("git", &clone_url);
    out.r#ref = r;
    out.subpath = subpath;
    out.skill_filter = fragment_skill_filter.clone().filter(|s| !s.is_empty());
    Ok(Some(out))
}

fn is_local_path(input: &str) -> bool {
    paths::is_absolute(input)
        || input.starts_with("./")
        || input.starts_with("../")
        || input == "."
        || input == ".."
        || re_win_drive().is_match(input)
}

fn source_alias(input: &str) -> Option<&'static str> {
    match input {
        "coinbase/agentWallet" => Some("coinbase/agentic-wallet-skills"),
        "vercel-labs/vercel-skills" => Some("vercel-labs/agent-skills"),
        _ => None,
    }
}

fn decode_fragment_value(v: &str) -> String {
    decode_uri_component(v).unwrap_or_else(|_| v.to_string())
}

fn looks_like_git_source(input: &str) -> bool {
    if input.starts_with("github:") || input.starts_with("gitlab:") || input.starts_with("git@") {
        return true;
    }
    if re_ssh_git().is_match(input) {
        return true;
    }
    if input.starts_with("http://") || input.starts_with("https://") {
        if let Some(parsed) = urlutil::parse(input) {
            let pathname = urlutil::pathname(&parsed);
            if is_github_host(&urlutil::host(&parsed)) {
                return re_gh_repo_tree_path().is_match(&pathname);
            }
            if urlutil::hostname(&parsed) == "gitlab.com" {
                return re_gl_repo_tree_path().is_match(&pathname);
            }
            let segs: Vec<&str> = pathname.split('/').filter(|s| !s.is_empty()).collect();
            if let Some(i) = segs.iter().position(|s| *s == "_git") {
                if i < segs.len() - 1 {
                    return true;
                }
            }
        }
    }
    if re_http_git().is_match(input) {
        return true;
    }
    !input.contains(':')
        && !input.starts_with('.')
        && !input.starts_with('/')
        && re_shorthand_like().is_match(input)
}

struct FragmentRef {
    input: String,
    r#ref: Option<String>,
    skill_filter: Option<String>,
}

fn parse_fragment_ref(input: &str) -> FragmentRef {
    let Some(hash) = input.find('#') else {
        return FragmentRef {
            input: input.to_string(),
            r#ref: None,
            skill_filter: None,
        };
    };
    let without = &input[..hash];
    let fragment = &input[hash + 1..];
    if fragment.is_empty() || !looks_like_git_source(without) {
        return FragmentRef {
            input: input.to_string(),
            r#ref: None,
            skill_filter: None,
        };
    }
    match fragment.find('@') {
        None => FragmentRef {
            input: without.to_string(),
            r#ref: Some(decode_fragment_value(fragment)),
            skill_filter: None,
        },
        Some(at) => {
            let r = &fragment[..at];
            let sf = &fragment[at + 1..];
            FragmentRef {
                input: without.to_string(),
                r#ref: if r.is_empty() {
                    None
                } else {
                    Some(decode_fragment_value(r))
                },
                skill_filter: if sf.is_empty() {
                    None
                } else {
                    Some(decode_fragment_value(sf))
                },
            }
        }
    }
}

fn append_fragment_ref(input: &str, r: &Option<String>, skill_filter: &Option<String>) -> String {
    match r {
        None => input.to_string(),
        Some(r) if r.is_empty() => input.to_string(),
        Some(r) => format!(
            "{}#{}{}",
            input,
            r,
            skill_filter
                .as_ref()
                .filter(|s| !s.is_empty())
                .map(|s| format!("@{}", s))
                .unwrap_or_default()
        ),
    }
}

fn is_hosted_artifact_url(input: &str) -> bool {
    let Some(parsed) = urlutil::parse(input) else {
        return false;
    };
    let host = urlutil::hostname(&parsed).to_lowercase();
    if host == "raw.githubusercontent.com"
        || host == "codeload.github.com"
        || host == "objects.githubusercontent.com"
    {
        return true;
    }
    if host == "github.com" {
        return re_gh_artifact().is_match(&urlutil::pathname(&parsed));
    }
    if host == "gitlab.com" {
        return re_gl_artifact().is_match(&urlutil::pathname(&parsed));
    }
    false
}

fn is_well_known_url(input: &str) -> bool {
    if !input.starts_with("http://") && !input.starts_with("https://") {
        return false;
    }
    let Some(parsed) = urlutil::parse(input) else {
        return false;
    };
    let hostname = urlutil::hostname(&parsed);
    if ["github.com", "gitlab.com", "raw.githubusercontent.com"].contains(&hostname.as_str()) {
        return false;
    }
    !input.ends_with(".git")
}

fn opt(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

fn opt_subpath(s: &str) -> Result<Option<String>, String> {
    if s.is_empty() {
        Ok(None)
    } else {
        Ok(Some(sanitize_subpath(s)?))
    }
}

/// Parse a source string into a structured form. Errors are unsafe subpaths.
pub fn parse_source(input: &str) -> Result<ParsedSource, String> {
    if is_local_path(input) {
        let resolved = paths::resolve1(input);
        let mut p = ParsedSource::new("local", &resolved);
        p.local_path = Some(resolved);
        return Ok(p);
    }

    let frag = parse_fragment_ref(input);
    let fragment_ref = frag.r#ref;
    let fragment_skill_filter = frag.skill_filter;
    let mut input = frag.input;

    if let Some(alias) = source_alias(&input) {
        input = alias.to_string();
    }

    if let Some(c) = re_github_prefix().captures(&input) {
        return parse_source(&append_fragment_ref(
            &c[1],
            &fragment_ref,
            &fragment_skill_filter,
        ));
    }
    if let Some(c) = re_gitlab_prefix().captures(&input) {
        return parse_source(&append_fragment_ref(
            &format!("https://gitlab.com/{}", &c[1]),
            &fragment_ref,
            &fragment_skill_filter,
        ));
    }

    if is_hosted_artifact_url(&input) {
        return Ok(ParsedSource::new("download", &input));
    }

    // Explicit GitHub Enterprise URL → generic git handling.
    if get_github_host() != "github.com" && re_http_prefix().is_match(&input) {
        if let Some(parsed_url) = urlutil::parse(&input) {
            let host = urlutil::host(&parsed_url);
            if is_github_host(&host) && host != "github.com" {
                let pathname = urlutil::pathname(&parsed_url);
                let segments: Vec<&str> = pathname.split('/').filter(|s| !s.is_empty()).collect();
                if segments.len() >= 2 {
                    let owner = segments[0];
                    let repo = strip_dot_git(segments[1]);
                    let marker = segments.get(2).copied();
                    let r = segments.get(3).copied();
                    let is_tree = marker == Some("tree") && r.is_some();
                    let mut out = ParsedSource::new(
                        "git",
                        &format!(
                            "{}//{}/{}/{}.git",
                            urlutil::protocol(&parsed_url),
                            host,
                            owner,
                            repo
                        ),
                    );
                    if is_tree {
                        out.r#ref = r.map(|s| s.to_string());
                        if segments.len() > 4 {
                            out.subpath = Some(sanitize_subpath(&segments[4..].join("/"))?);
                        }
                    } else if fragment_ref.is_some() {
                        out.r#ref = fragment_ref.clone();
                    }
                    return Ok(out);
                }
            }
        }
    }

    if let Some(c) = re_gh_tree_with_path().captures(&input) {
        let mut out = ParsedSource::new(
            "github",
            &format!("https://github.com/{}/{}.git", &c[1], &c[2]),
        );
        out.r#ref = opt(&c[3]).or_else(|| fragment_ref.clone());
        out.subpath = opt_subpath(&c[4])?;
        return Ok(out);
    }

    if let Some(c) = re_gh_tree().captures(&input) {
        let mut out = ParsedSource::new(
            "github",
            &format!("https://github.com/{}/{}.git", &c[1], &c[2]),
        );
        out.r#ref = opt(&c[3]).or_else(|| fragment_ref.clone());
        return Ok(out);
    }

    if let Some(c) = re_gh_repo().captures(&input) {
        let clean = strip_dot_git(&c[2]);
        let mut out = ParsedSource::new(
            "github",
            &format!("https://github.com/{}/{}.git", &c[1], clean),
        );
        out.r#ref = fragment_ref.clone();
        return Ok(out);
    }

    if let Some(c) = re_gl_tree_with_path().captures(&input) {
        let (protocol, hostname, repo_path, r, subpath) = (&c[1], &c[2], &c[3], &c[4], &c[5]);
        if hostname != "github.com" && !repo_path.is_empty() {
            let mut out = ParsedSource::new(
                "gitlab",
                &format!(
                    "{}://{}/{}.git",
                    protocol,
                    hostname,
                    strip_dot_git(repo_path)
                ),
            );
            out.r#ref = opt(r).or_else(|| fragment_ref.clone());
            out.subpath = opt_subpath(subpath)?;
            return Ok(out);
        }
    }

    if let Some(c) = re_gl_tree().captures(&input) {
        let (protocol, hostname, repo_path, r) = (&c[1], &c[2], &c[3], &c[4]);
        if hostname != "github.com" && !repo_path.is_empty() {
            let mut out = ParsedSource::new(
                "gitlab",
                &format!(
                    "{}://{}/{}.git",
                    protocol,
                    hostname,
                    strip_dot_git(repo_path)
                ),
            );
            out.r#ref = opt(r).or_else(|| fragment_ref.clone());
            return Ok(out);
        }
    }

    if let Some(c) = re_gl_repo().captures(&input) {
        let repo_path = &c[1];
        if repo_path.contains('/') {
            let mut out =
                ParsedSource::new("gitlab", &format!("https://gitlab.com/{}.git", repo_path));
            out.r#ref = fragment_ref.clone();
            return Ok(out);
        }
    }

    if let Some(azure) = parse_azure_repos_source(&input, &fragment_ref, &fragment_skill_filter)? {
        return Ok(azure);
    }

    let github_host = get_github_host();
    let shorthand_type = if github_host == "github.com" {
        "github"
    } else {
        "git"
    };
    let plain = !input.contains(':') && !input.starts_with('.') && !input.starts_with('/');

    if let Some(c) = re_at_skill().captures(&input) {
        if plain {
            let mut out = ParsedSource::new(
                shorthand_type,
                &format!("https://{}/{}/{}.git", github_host, &c[1], &c[2]),
            );
            out.r#ref = fragment_ref.clone();
            out.skill_filter = fragment_skill_filter.clone().or_else(|| opt(&c[3]));
            return Ok(out);
        }
    }

    if let Some(c) = re_shorthand().captures(&input) {
        if plain {
            let mut out = ParsedSource::new(
                shorthand_type,
                &format!("https://{}/{}/{}.git", github_host, &c[1], &c[2]),
            );
            out.r#ref = fragment_ref.clone();
            out.subpath = match c.get(3) {
                Some(m) => opt_subpath(m.as_str())?,
                None => None,
            };
            out.skill_filter = fragment_skill_filter.clone();
            return Ok(out);
        }
    }

    if is_well_known_url(&input) {
        return Ok(ParsedSource::new("well-known", &input));
    }

    let mut out = ParsedSource::new("git", &input);
    out.r#ref = fragment_ref;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> ParsedSource {
        parse_source(s).unwrap()
    }

    #[test]
    fn github_shorthand() {
        let s = p("vercel-labs/agent-skills");
        assert_eq!(s.kind, "github");
        assert_eq!(s.url, "https://github.com/vercel-labs/agent-skills.git");
        assert_eq!(s.subpath, None);

        let s = p("owner/repo/skills/my-skill");
        assert_eq!(s.subpath.as_deref(), Some("skills/my-skill"));

        let s = p("owner/repo@my-skill");
        assert_eq!(s.skill_filter.as_deref(), Some("my-skill"));

        let s = p("github:owner/repo");
        assert_eq!(s.url, "https://github.com/owner/repo.git");

        let s = p("vercel-labs/vercel-skills");
        assert_eq!(s.url, "https://github.com/vercel-labs/agent-skills.git");
    }

    #[test]
    fn github_urls() {
        let s = p("https://github.com/owner/repo/tree/main/skills/x");
        assert_eq!(s.kind, "github");
        assert_eq!(s.r#ref.as_deref(), Some("main"));
        assert_eq!(s.subpath.as_deref(), Some("skills/x"));

        let s = p("https://github.com/owner/repo/tree/dev");
        assert_eq!(s.r#ref.as_deref(), Some("dev"));

        let s = p("https://github.com/owner/repo.git#v1.2@skill");
        assert_eq!(s.url, "https://github.com/owner/repo.git");
        assert_eq!(s.r#ref.as_deref(), Some("v1.2"));
    }

    #[test]
    fn fragments() {
        let s = p("owner/repo#feature%2Fx");
        assert_eq!(s.r#ref.as_deref(), Some("feature/x"));
        let s = p("owner/repo#main@my-skill");
        assert_eq!(s.r#ref.as_deref(), Some("main"));
        assert_eq!(s.skill_filter.as_deref(), Some("my-skill"));
        // Well-known URLs keep their fragment
        let s = p("https://example.com/docs#section");
        assert_eq!(s.kind, "well-known");
        assert_eq!(s.url, "https://example.com/docs#section");
    }

    #[test]
    fn gitlab_and_azure() {
        let s = p("https://gitlab.com/group/sub/repo");
        assert_eq!(s.kind, "gitlab");
        assert_eq!(s.url, "https://gitlab.com/group/sub/repo.git");

        let s = p("https://gitlab.example.com/g/r/-/tree/main/skills");
        assert_eq!(s.kind, "gitlab");
        assert_eq!(s.url, "https://gitlab.example.com/g/r.git");
        assert_eq!(s.subpath.as_deref(), Some("skills"));

        let s = p("gitlab:group/repo");
        assert_eq!(s.url, "https://gitlab.com/group/repo.git");

        let s = p("https://dev.azure.com/org/proj/_git/repo?path=/skills&version=GBmain");
        assert_eq!(s.kind, "git");
        assert_eq!(s.url, "https://dev.azure.com/org/proj/_git/repo");
        assert_eq!(s.r#ref.as_deref(), Some("main"));
        assert_eq!(s.subpath.as_deref(), Some("skills"));
    }

    #[test]
    fn downloads_wellknown_git() {
        assert_eq!(
            p("https://raw.githubusercontent.com/o/r/main/SKILL.md").kind,
            "download"
        );
        assert_eq!(
            p("https://github.com/o/r/archive/refs/heads/main.zip").kind,
            "download"
        );
        assert_eq!(p("https://mintlify.com/docs").kind, "well-known");
        let s = p("git@github.com:owner/repo.git");
        assert_eq!(s.kind, "git");
        assert_eq!(get_owner_repo(&s).as_deref(), Some("owner/repo"));
        let s = p("https://git.example.com/team/repo.git");
        assert_eq!(s.kind, "git");
    }

    #[test]
    fn local_paths() {
        let s = p("./skills");
        assert_eq!(s.kind, "local");
        assert!(paths::is_absolute(s.local_path.as_ref().unwrap()));
    }

    #[test]
    fn rejects_traversal() {
        assert!(parse_source("owner/repo/../../etc").is_err());
        assert!(parse_source("https://github.com/o/r/tree/main/../x").is_err());
    }

    #[test]
    fn owner_repo_extraction() {
        assert_eq!(
            get_owner_repo(&p("https://gitlab.com/g/s/r")).as_deref(),
            Some("g/s/r")
        );
        let ssh = ParsedSource::new("git", "ssh://git@host:7999/owner/repo.git");
        assert_eq!(get_owner_repo(&ssh).as_deref(), Some("owner/repo"));
        assert_eq!(parse_owner_repo("a/b"), Some(("a".into(), "b".into())));
        assert_eq!(parse_owner_repo("a/b/c"), None);
    }
}
