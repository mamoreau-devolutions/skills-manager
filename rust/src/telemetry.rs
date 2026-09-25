//! Anonymous usage tracking and security audit lookups (port of telemetry.ts).
//!
//! Events are fired on background threads and awaited (up to 5s) by
//! [`flush_telemetry`] at normal CLI exit, like the TS pending-promise list.

use crate::urlutil::search_params;
use serde_json::Value;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const TELEMETRY_URL: &str = "https://add-skill.vercel.sh/t";
const AUDIT_URL: &str = "https://add-skill.vercel.sh/audit";

static VERSION: Mutex<Option<String>> = Mutex::new(None);
static DETECTED_AGENT: Mutex<Option<String>> = Mutex::new(None);
static PENDING: Mutex<Vec<std::thread::JoinHandle<()>>> = Mutex::new(Vec::new());

pub fn set_version(v: &str) {
    *VERSION.lock().unwrap() = Some(v.to_string());
}

pub fn set_detected_agent(name: Option<&str>) {
    *DETECTED_AGENT.lock().unwrap() = name.map(|s| s.to_string());
}

fn is_ci() -> bool {
    [
        "CI",
        "GITHUB_ACTIONS",
        "GITLAB_CI",
        "CIRCLECI",
        "TRAVIS",
        "BUILDKITE",
        "JENKINS_URL",
        "TEAMCITY_VERSION",
    ]
    .iter()
    .any(|v| crate::sys::env_truthy(v))
}

pub fn is_enabled() -> bool {
    !crate::sys::env_truthy("DISABLE_TELEMETRY") && !crate::sys::env_truthy("DO_NOT_TRACK")
}

/// Partner audit data: `{ skillSlug: { partner: { risk, alerts?, ... } } }`
pub type AuditResponse = serde_json::Map<String, Value>;

/// Fetch security audit results; `None` on any error or timeout.
pub fn fetch_audit_data(
    source: &str,
    skill_slugs: &[String],
    timeout_ms: u64,
) -> Option<AuditResponse> {
    if !is_enabled() || skill_slugs.is_empty() {
        return None;
    }
    let skills = skill_slugs.join(",");
    let params = search_params(&[("source", source), ("skills", &skills)]);
    let resp = crate::http::get(&format!("{}?{}", AUDIT_URL, params))
        .timeout(Duration::from_millis(timeout_ms))
        .send()
        .ok()?;
    if !resp.ok() {
        return None;
    }
    match resp.json()? {
        Value::Object(m) => Some(m),
        _ => None,
    }
}

/// Fire-and-forget event. `data` is an ordered list of (key, value) pairs;
/// `None` values are omitted (as `undefined` is in the TS implementation).
pub fn track(data: &[(&str, Option<String>)]) {
    if !is_enabled() {
        return;
    }
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut set = |k: &str, v: String| {
        // URLSearchParams.set replaces an existing key in place
        if let Some(p) = pairs.iter_mut().find(|(pk, _)| pk == k) {
            p.1 = v;
        } else {
            pairs.push((k.to_string(), v));
        }
    };
    if let Some(v) = VERSION.lock().unwrap().clone() {
        set("v", v);
    }
    if is_ci() {
        set("ci", "1".into());
    }
    if let Some(a) = DETECTED_AGENT.lock().unwrap().clone() {
        set("agent", a);
    }
    for (k, v) in data {
        if let Some(v) = v {
            set(k, v.clone());
        }
    }
    let refs: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let url = format!("{}?{}", TELEMETRY_URL, search_params(&refs));
    let handle = std::thread::spawn(move || {
        let _ = crate::http::get(&url)
            .timeout(Duration::from_secs(10))
            .send();
    });
    PENDING.lock().unwrap().push(handle);
}

/// Wait (bounded) for in-flight telemetry requests.
pub fn flush_telemetry(timeout: Duration) {
    let handles: Vec<_> = std::mem::take(&mut *PENDING.lock().unwrap());
    if handles.is_empty() {
        return;
    }
    let deadline = Instant::now() + timeout;
    for h in handles {
        while !h.is_finished() {
            if Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = h.join();
    }
}
