//! GitHub host selection via `GH_HOST` (port of github-host.ts).

use crate::urlutil;

const DEFAULT_GITHUB_HOST: &str = "github.com";

/// The GitHub host selected by the GitHub CLI. `GH_HOST` must be a bare
/// hostname; anything else falls back to github.com.
pub fn get_github_host() -> String {
    let configured = match crate::sys::env_trimmed("GH_HOST") {
        Some(h) => h,
        None => return DEFAULT_GITHUB_HOST.to_string(),
    };
    let parsed = match urlutil::parse(&format!("https://{}", configured)) {
        Some(u) => u,
        None => return DEFAULT_GITHUB_HOST.to_string(),
    };
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port().is_some()
        || parsed.path() != "/"
        || parsed.query().map(|q| !q.is_empty()).unwrap_or(false)
        || parsed.fragment().map(|f| !f.is_empty()).unwrap_or(false)
    {
        return DEFAULT_GITHUB_HOST.to_string();
    }
    urlutil::hostname(&parsed)
}

/// Whether a host is GitHub.com or the configured GitHub Enterprise host.
pub fn is_github_host(host: &str) -> bool {
    let h = host.to_lowercase();
    h == DEFAULT_GITHUB_HOST || h == get_github_host().to_lowercase()
}
