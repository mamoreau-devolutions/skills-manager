//! Latest GitHub release lookup, used by `skills add --pin latest` and the
//! `gh skill` update check (extension; not in the reference CLI).
//!
//! Auth follows `blob::fetch_repo_tree`: an anonymous request first; when it
//! fails in a way credentials can fix (401, 404, or a 403 rate limit), retry
//! with `GITHUB_TOKEN`/`GH_TOKEN`, then with `gh api`.

use crate::blob::github_api_base;
use crate::github_host::get_github_host;
use crate::skill_lock::get_github_token;
use serde_json::Value;
use std::time::Duration;

const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const GH_API_MAX_BUFFER: usize = 16 * 1024 * 1024;

/// `tag_name` of a release object, when present and non-empty.
pub fn release_tag(data: &Value) -> Option<String> {
    data.get("tag_name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

enum Attempt {
    Found(Value),
    Status { status: u16, retryable: bool },
    Failed(String),
}

fn fetch(owner_repo: &str, token: Option<&str>) -> Attempt {
    let url = format!("{}/repos/{}/releases/latest", github_api_base(), owner_repo);
    let mut req = crate::http::get(&url)
        .timeout(FETCH_TIMEOUT)
        .header("Accept", "application/vnd.github.v3+json")
        .header("User-Agent", "skills-cli");
    if let Some(t) = token {
        req = req.header("Authorization", &format!("Bearer {}", t));
    }
    match req.send() {
        Ok(resp) if resp.ok() => match resp.json() {
            Some(v) => Attempt::Found(v),
            None => Attempt::Failed("invalid response from GitHub".into()),
        },
        Ok(resp) => Attempt::Status {
            status: resp.status,
            retryable: resp.status == 401
                || resp.status == 404
                || (resp.status == 403 && resp.header("x-ratelimit-remaining") == Some("0")),
        },
        Err(e) => Attempt::Failed(e),
    }
}

fn fetch_with_github_cli(owner_repo: &str) -> Option<Value> {
    let endpoint = format!("repos/{}/releases/latest", owner_repo);
    let host = get_github_host();
    let out = crate::proc::command(
        "gh",
        &["api", &endpoint, "--method", "GET", "--hostname", &host],
    )
    .env("GH_PROMPT_DISABLED", "1")
    .timeout(FETCH_TIMEOUT)
    .max_output(GH_API_MAX_BUFFER)
    .hide_window()
    .output()
    .ok()?;
    if !out.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

fn status_error(status: u16) -> String {
    format!("GitHub API returned HTTP {}", status)
}

/// The newest release tag of `owner/repo`: `Ok(Some(tag))`, `Ok(None)` when
/// the repository has no releases (HTTP 404 or no tag), `Err(reason)` on any
/// other failure.
pub fn resolve_latest_release(owner_repo: &str) -> Result<Option<String>, String> {
    let mut last_status = match fetch(owner_repo, None) {
        Attempt::Found(v) => return Ok(release_tag(&v)),
        Attempt::Failed(e) => return Err(e),
        Attempt::Status {
            status,
            retryable: false,
        } => return Err(status_error(status)),
        Attempt::Status { status, .. } => status,
    };
    if let Some(token) = get_github_token() {
        match fetch(owner_repo, Some(&token)) {
            Attempt::Found(v) => return Ok(release_tag(&v)),
            Attempt::Status { status, .. } => last_status = status,
            Attempt::Failed(_) => {}
        }
    }
    if let Some(v) = fetch_with_github_cli(owner_repo) {
        return Ok(release_tag(&v));
    }
    if last_status == 404 {
        Ok(None)
    } else {
        Err(status_error(last_status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_release_tag() {
        assert_eq!(
            release_tag(&json!({"tag_name": "v1.2.0"})).as_deref(),
            Some("v1.2.0")
        );
        assert_eq!(release_tag(&json!({"tag_name": ""})), None);
        assert_eq!(release_tag(&json!({})), None);
    }
}
