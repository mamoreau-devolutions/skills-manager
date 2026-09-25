//! Git clone operations (port of git.ts). Invokes the `git` binary directly
//! with the same config overrides and environment the TS CLI passes through
//! simple-git.

use crate::github_host::is_github_host;
use crate::paths::{self, join};
use crate::proc::{self, ProcError};
use crate::sys;
use crate::urlutil;
use regex::Regex;
use std::time::Duration;

const DEFAULT_CLONE_TIMEOUT_MS: u64 = 300_000;
const ALLOWED_GIT_PROTOCOLS: &str = "https:http:ssh:git:file";

fn clone_timeout_ms() -> u64 {
    match sys::env("SKILLS_CLONE_TIMEOUT_MS").and_then(|r| parse_int_prefix(&r)) {
        Some(v) if v > 0 => v as u64,
        _ => DEFAULT_CLONE_TIMEOUT_MS,
    }
}

/// `Number.parseInt(s, 10)` returning `None` for NaN.
pub fn parse_int_prefix(s: &str) -> Option<i64> {
    let t = s.trim_start();
    let (sign, rest) = match t.strip_prefix('-') {
        Some(r) => (-1, r),
        None => (1, t.strip_prefix('+').unwrap_or(t)),
    };
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<i64>().ok().map(|v| v * sign)
}

#[derive(Debug, Clone)]
pub struct GitCloneError {
    pub message: String,
    pub url: String,
    pub is_timeout: bool,
    pub is_auth_error: bool,
}

impl std::fmt::Display for GitCloneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

pub fn is_commit_sha(r: &str) -> bool {
    r.len() == 40 && r.chars().all(|c| c.is_ascii_hexdigit())
}

pub fn is_missing_ref_error(message: &str) -> bool {
    let lower = message.to_lowercase();
    Regex::new(r"(?i)Remote branch .* not found in upstream origin")
        .unwrap()
        .is_match(message)
        || lower.contains("couldn't find remote ref")
        || lower.contains("upload-pack: not our ref")
}

pub struct GitHubRepoInfo {
    pub owner: String,
    pub repo: String,
    pub slug: String,
    pub ssh_url: String,
}

pub fn parse_github_repo_url(url: &str) -> Option<GitHubRepoInfo> {
    let ssh = Regex::new(r"(?i)^git@([^:]+):([^/]+)/([^/]+?)(?:\.git)?$").unwrap();
    if let Some(c) = ssh.captures(url) {
        if is_github_host(&c[1]) {
            return Some(GitHubRepoInfo {
                owner: c[2].to_string(),
                repo: c[3].to_string(),
                slug: format!("{}/{}", &c[2], &c[3]),
                ssh_url: format!("git@{}:{}/{}.git", &c[1], &c[2], &c[3]),
            });
        }
    }
    let parsed = urlutil::parse(url)?;
    let host = urlutil::host(&parsed);
    if !is_github_host(&host) {
        return None;
    }
    let path_re = Regex::new(r"^/([^/]+)/([^/]+?)(?:\.git)?/?$").unwrap();
    let c = path_re.captures(parsed.path())?;
    Some(GitHubRepoInfo {
        owner: c[1].to_string(),
        repo: c[2].to_string(),
        slug: format!("{}/{}", &c[1], &c[2]),
        ssh_url: format!("git@{}:{}/{}.git", host, &c[1], &c[2]),
    })
}

pub fn is_github_https_clone_url(url: &str) -> bool {
    urlutil::parse(url)
        .map(|u| u.scheme() == "https" && is_github_host(&urlutil::host(&u)))
        .unwrap_or(false)
}

pub fn is_github_sso_auth_error(message: &str) -> bool {
    let l = message.to_lowercase();
    l.contains("saml sso")
        || l.contains("enforced sso")
        || l.contains("enabled or enforced saml")
        || l.contains("re-authorize the oauth application")
}

fn is_auth_failure(message: &str) -> bool {
    message.contains("Authentication failed")
        || message.contains("could not read Username")
        || message.contains("Permission denied")
        || message.contains("Repository not found")
        || message.contains("requested URL returned error: 403")
        || is_github_sso_auth_error(message)
}

/// Run a git command with the LFS-disabling config and safe environment.
fn run_git(args: &[&str], cwd: Option<&str>, extra_env: &[(&str, &str)]) -> Result<(), String> {
    let mut full: Vec<&str> = vec![
        "-c",
        "filter.lfs.required=false",
        "-c",
        "filter.lfs.smudge=",
        "-c",
        "filter.lfs.clean=",
        "-c",
        "filter.lfs.process=",
    ];
    let cwd_owned;
    if let Some(dir) = cwd {
        cwd_owned = dir.to_string();
        full.push("-C");
        full.push(&cwd_owned);
    }
    full.extend_from_slice(args);
    let mut cmd = proc::command("git", &full)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ALLOW_PROTOCOL", ALLOWED_GIT_PROTOCOLS)
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .timeout(Duration::from_millis(clone_timeout_ms()));
    for (k, v) in extra_env {
        cmd = cmd.env(k, v);
    }
    match cmd.output() {
        Ok(out) if out.success() => Ok(()),
        Ok(out) => {
            let stderr = out.stderr_str();
            let msg = stderr.trim();
            Err(if msg.is_empty() {
                format!("git exited with code {}", out.status.unwrap_or(-1))
            } else {
                msg.to_string()
            })
        }
        Err(ProcError::Timeout) => Err("block timeout reached".to_string()),
        Err(ProcError::NotFound) => {
            Err("spawn git ENOENT: git is not installed or not on PATH".to_string())
        }
        Err(e) => Err(e.to_string()),
    }
}

fn clone_args<'a>(url: &'a str, dir: &'a str, r#ref: Option<&'a str>) -> Vec<&'a str> {
    let mut v = vec!["clone", "--depth", "1"];
    if let Some(r) = r#ref {
        v.push("--branch");
        v.push(r);
    }
    v.push("--");
    v.push(url);
    v.push(dir);
    v
}

fn clone_at_sha(url: &str, sha: &str, dir: &str, extra_env: &[(&str, &str)]) -> Result<(), String> {
    run_git(&["init"], Some(dir), extra_env)?;
    run_git(&["remote", "add", "origin", url], Some(dir), extra_env)?;
    run_git(
        &["fetch", "--depth", "1", "origin", sha],
        Some(dir),
        extra_env,
    )?;
    run_git(&["checkout", "FETCH_HEAD"], Some(dir), extra_env)?;
    Ok(())
}

fn reset_temp_dir(dir: &str) {
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::create_dir_all(dir);
}

fn try_gh_clone(repo: &GitHubRepoInfo, dir: &str, r#ref: Option<&str>) -> Result<bool, String> {
    let host = Regex::new(r"^git@([^:]+):")
        .unwrap()
        .captures(&repo.ssh_url)
        .map(|c| c[1].to_string())
        .unwrap_or_else(|| "github.com".into());
    let status = proc::command("gh", &["auth", "status", "-h", &host])
        .env("GIT_TERMINAL_PROMPT", "0")
        .timeout(Duration::from_secs(5))
        .output();
    let mut target = repo.slug.clone();
    match status {
        Ok(out) if out.success() => {
            let text = format!("{}{}", out.stdout_str(), out.stderr_str());
            if Regex::new(r"(?i)Git operations protocol:\s+ssh")
                .unwrap()
                .is_match(&text)
            {
                target = repo.ssh_url.clone();
            }
        }
        _ => return Ok(false),
    }
    let mut args: Vec<&str> = vec!["repo", "clone", &target, dir, "--", "--depth=1"];
    if let Some(r) = r#ref {
        args.push("--branch");
        args.push(r);
    }
    let out = proc::command("gh", &args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ALLOW_PROTOCOL", ALLOWED_GIT_PROTOCOLS)
        .timeout(Duration::from_millis(clone_timeout_ms()))
        .output()
        .map_err(|e| e.to_string())?;
    if out.success() {
        Ok(true)
    } else {
        Err(out.stderr_str())
    }
}

fn build_github_auth_error(url: &str, repo: Option<&GitHubRepoInfo>, message: &str) -> String {
    let host = repo
        .and_then(|r| {
            Regex::new(r"^git@([^:]+):")
                .unwrap()
                .captures(&r.ssh_url)
                .map(|c| c[1].to_string())
        })
        .unwrap_or_else(|| "github.com".into());
    if let Some(repo) = repo {
        if is_github_sso_auth_error(message) {
            return format!(
                "GitHub blocked HTTPS access to {} because the organization enforces SAML SSO.\n  skills tried your existing git credentials and available fallbacks, but none succeeded.\n  - Re-authorize your GitHub credentials/app for that org's SSO policy\n  - Or rerun with SSH: skills add {}\n  - Verify access with: gh auth status -h {} or ssh -T git@{}",
                url, repo.ssh_url, host, host
            );
        }
        return format!(
            "Authentication failed for {}.\n  - For private repos, ensure you have access\n  - Retry with SSH: skills add {}\n  - Check access with: gh auth status -h {} or ssh -T git@{}",
            url, repo.ssh_url, host, host
        );
    }
    format!(
        "Authentication failed for {}.\n  - For private repos, ensure you have access\n  - For SSH: Check your keys with 'ssh -T git@github.com'\n  - For HTTPS: Run 'gh auth login' or configure git credentials",
        url
    )
}

/// Shallow-clone `url` (optionally at `ref`) into a fresh temp directory.
pub fn clone_repo(url: &str, r#ref: Option<&str>) -> Result<String, GitCloneError> {
    let err = |message: String, timeout: bool, auth: bool| GitCloneError {
        message,
        url: url.to_string(),
        is_timeout: timeout,
        is_auth_error: auth,
    };
    if url.to_lowercase().starts_with("ext::") {
        return Err(err("Unsupported Git transport: ext".into(), false, false));
    }
    let temp_dir = sys::mkdtemp("skills-").map_err(|e| err(e.to_string(), false, false))?;
    let ref_can_be_sha = r#ref.map(is_commit_sha).unwrap_or(false);
    let repo = parse_github_repo_url(url);

    let error_message = match run_git(&clone_args(url, &temp_dir, r#ref), None, &[]) {
        Ok(()) => return Ok(temp_dir),
        Err(e) => e,
    };

    if ref_can_be_sha && is_missing_ref_error(&error_message) {
        reset_temp_dir(&temp_dir);
        if clone_at_sha(url, r#ref.unwrap(), &temp_dir, &[]).is_ok() {
            return Ok(temp_dir);
        }
    }

    let is_timeout = error_message.contains("block timeout") || error_message.contains("timed out");
    let is_auth = is_auth_failure(&error_message);

    if is_timeout {
        let _ = std::fs::remove_dir_all(&temp_dir);
        let seconds = (clone_timeout_ms() as f64 / 1000.0).round() as u64;
        return Err(err(
            format!(
                "Clone timed out after {}s. Common causes:\n  - Large repository: raise the timeout with SKILLS_CLONE_TIMEOUT_MS=600000 (10m)\n  - Slow network: retry, or clone manually and pass the local path to 'skills add'\n  - Private repo without credentials: ensure auth is configured\n      - For SSH: ssh-add -l (to check loaded keys)\n      - For HTTPS: gh auth status (if using GitHub CLI)",
                seconds
            ),
            true,
            false,
        ));
    }

    if is_auth && is_github_https_clone_url(url) {
        if let Some(repo) = &repo {
            reset_temp_dir(&temp_dir);
            if let Ok(true) = try_gh_clone(repo, &temp_dir, r#ref) {
                return Ok(temp_dir);
            }
            reset_temp_dir(&temp_dir);
            let ssh_cmd =
                sys::env_raw("GIT_SSH_COMMAND").unwrap_or_else(|| "ssh -o BatchMode=yes".into());
            let env = [("GIT_SSH_COMMAND", ssh_cmd.as_str())];
            let ssh_result = match run_git(&clone_args(&repo.ssh_url, &temp_dir, r#ref), None, &env)
            {
                Ok(()) => Ok(()),
                Err(ssh_msg) if ref_can_be_sha && is_missing_ref_error(&ssh_msg) => {
                    reset_temp_dir(&temp_dir);
                    clone_at_sha(&repo.ssh_url, r#ref.unwrap(), &temp_dir, &env)
                }
                Err(e) => Err(e),
            };
            if ssh_result.is_ok() {
                return Ok(temp_dir);
            }
        }
    }

    let _ = std::fs::remove_dir_all(&temp_dir);
    if is_auth {
        return Err(err(
            build_github_auth_error(url, repo.as_ref(), &error_message),
            false,
            true,
        ));
    }
    Err(err(
        format!("Failed to clone {}: {}", url, error_message),
        false,
        false,
    ))
}

/// Git tree object for a locked skill path (matches the GitHub Trees API SHA).
pub fn get_git_tree_hash(repo_dir: &str, skill_path: &str) -> Option<String> {
    let normalized = skill_path.replace('\\', "/");
    let mut segments: Vec<&str> = normalized.split('/').collect();
    segments.pop();
    let folder = segments.join("/");
    let revision = if folder.is_empty() {
        "HEAD^{tree}".to_string()
    } else {
        format!("HEAD:{}", folder)
    };
    let out = proc::command(
        "git",
        &[
            "-C",
            repo_dir,
            "rev-parse",
            "--verify",
            "--end-of-options",
            &revision,
        ],
    )
    .env("GIT_OPTIONAL_LOCKS", "0")
    .env("GIT_TERMINAL_PROMPT", "0")
    .timeout(Duration::from_millis(clone_timeout_ms()))
    .output()
    .ok()?;
    if !out.success() {
        return None;
    }
    let hash = out.stdout_str().trim().to_string();
    if hash.len() == 40 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(hash.to_lowercase())
    } else {
        None
    }
}

/// Remove a temp directory, refusing paths outside the system temp dir.
pub fn cleanup_temp_dir(dir: &str) -> Result<(), String> {
    let normalized = paths::normalize(&paths::resolve1(dir));
    let tmp = paths::normalize(&paths::resolve1(&sys::tmpdir()));
    if !normalized.starts_with(&format!("{}{}", tmp, paths::SEP)) && normalized != tmp {
        return Err("Attempted to clean up directory outside of temp directory".into());
    }
    remove_all(dir).map_err(|e| e.to_string())
}

/// `rm -rf` that tolerates missing paths and read-only files (git objects on
/// Windows are read-only, which makes `remove_dir_all` fail).
pub fn remove_all(path: &str) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
        Ok(m) => {
            if !m.is_dir() || m.file_type().is_symlink() {
                return remove_file_or_link(path, &m);
            }
        }
    }
    if std::fs::remove_dir_all(path).is_ok() {
        return Ok(());
    }
    clear_readonly(path);
    match std::fs::remove_dir_all(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        r => r,
    }
}

/// Whether an entry must be deleted with `remove_dir`. On Windows, directory
/// symlinks and junctions carry the directory attribute and are removed with
/// RemoveDirectory (which deletes the link, never the target); `is_dir()` is
/// false for them because `symlink_metadata` reports them as links.
fn is_dir_entry(m: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
        m.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0
    }
    #[cfg(not(windows))]
    {
        m.is_dir()
    }
}

fn remove_file_or_link(path: &str, m: &std::fs::Metadata) -> std::io::Result<()> {
    let dir_like = is_dir_entry(m);
    let r = if dir_like {
        std::fs::remove_dir(path)
    } else {
        std::fs::remove_file(path)
    };
    match r {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => {
            let mut perms = m.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            let _ = std::fs::set_permissions(path, perms);
            if dir_like {
                std::fs::remove_dir(path)
            } else {
                std::fs::remove_file(path)
            }
        }
    }
}

fn clear_readonly(path: &str) {
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for e in entries.flatten() {
        let p = join(&[path, e.file_name().to_string_lossy().as_ref()]);
        if let Ok(m) = std::fs::symlink_metadata(&p) {
            if m.is_dir() && !m.file_type().is_symlink() {
                clear_readonly(&p);
            } else if m.permissions().readonly() {
                let mut perms = m.permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                perms.set_readonly(false);
                let _ = std::fs::set_permissions(&p, perms);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha_and_ref_errors() {
        assert!(is_commit_sha(&"a".repeat(40)));
        assert!(!is_commit_sha("deadbeef"));
        assert!(is_missing_ref_error(
            "warning: Remote branch abc not found in upstream origin"
        ));
        assert!(is_missing_ref_error("fatal: couldn't find remote ref abc"));
        assert!(!is_missing_ref_error("fatal: repository not found"));
    }

    #[test]
    fn github_repo_urls() {
        let r = parse_github_repo_url("https://github.com/o/r.git").unwrap();
        assert_eq!(r.slug, "o/r");
        assert_eq!(r.ssh_url, "git@github.com:o/r.git");
        let s = parse_github_repo_url("git@github.com:o/r.git").unwrap();
        assert_eq!(s.repo, "r");
        assert!(parse_github_repo_url("https://gitlab.com/o/r.git").is_none());
    }

    #[test]
    fn rejects_ext_transport() {
        assert!(clone_repo("ext::sh -c touch% /tmp/pwned", None).is_err());
    }

    #[test]
    fn cleanup_refuses_outside_tmp() {
        let home = sys::homedir();
        assert!(cleanup_temp_dir(&home).is_err());
    }

    #[test]
    fn parse_int() {
        assert_eq!(parse_int_prefix("600000ms"), Some(600000));
        assert_eq!(parse_int_prefix("abc"), None);
    }
}
