//! Detect whether the CLI runs inside an AI agent (port of detect-agent.ts,
//! including the `@vercel/detect-agent` 1.2.3 heuristics it wraps).

use crate::agents::AgentType;
use crate::sys;
use std::sync::OnceLock;

#[derive(Clone, Debug)]
pub struct AgentResult {
    /// `Some(name)` when running inside an agent.
    pub agent: Option<String>,
}

impl AgentResult {
    pub fn is_agent(&self) -> bool {
        self.agent.is_some()
    }
    pub fn name(&self) -> &str {
        self.agent.as_deref().unwrap_or("")
    }
}

/// `determineAgent()` from @vercel/detect-agent.
fn determine_agent() -> Option<String> {
    if let Some(raw) = sys::env("AI_AGENT") {
        let name = raw.trim().to_string();
        if !name.is_empty() {
            if name == "github-copilot" || name == "github-copilot-cli" {
                return Some("github-copilot".into());
            }
            return Some(name);
        }
    }
    if sys::env_truthy("CURSOR_TRACE_ID") {
        return Some("cursor".into());
    }
    if sys::env_truthy("CURSOR_AGENT")
        || sys::env_raw("CURSOR_EXTENSION_HOST_ROLE").as_deref() == Some("agent-exec")
    {
        return Some("cursor-cli".into());
    }
    if sys::env_truthy("GEMINI_CLI") {
        return Some("gemini".into());
    }
    if sys::env_truthy("CODEX_SANDBOX")
        || sys::env_truthy("CODEX_CI")
        || sys::env_truthy("CODEX_THREAD_ID")
    {
        return Some("codex".into());
    }
    if sys::env_truthy("ANTIGRAVITY_AGENT") {
        return Some("antigravity".into());
    }
    if sys::env_truthy("AUGMENT_AGENT") {
        return Some("augment-cli".into());
    }
    if sys::env_truthy("OPENCODE_CLIENT") {
        return Some("opencode".into());
    }
    if sys::env_truthy("CLAUDECODE") || sys::env_truthy("CLAUDE_CODE") {
        if sys::env_truthy("CLAUDE_CODE_IS_COWORK") {
            return Some("cowork".into());
        }
        return Some("claude".into());
    }
    if sys::env_truthy("REPL_ID") {
        return Some("replit".into());
    }
    if sys::env_truthy("COPILOT_MODEL")
        || sys::env_truthy("COPILOT_ALLOW_ALL")
        || sys::env_truthy("COPILOT_GITHUB_TOKEN")
    {
        return Some("github-copilot".into());
    }
    if std::path::Path::new("/opt/.devin").exists() {
        return Some("devin".into());
    }
    None
}

fn has_strong_cursor_agent_signal() -> bool {
    sys::env_trimmed("CURSOR_AGENT").is_some()
        || sys::env_raw("CURSOR_EXTENSION_HOST_ROLE").as_deref() == Some("agent-exec")
}

fn refine(agent: Option<String>) -> Option<String> {
    match agent.as_deref() {
        Some("cursor") | Some("cursor-cli") => {
            if !has_strong_cursor_agent_signal() {
                return None;
            }
            Some("cursor-cli".into())
        }
        _ => agent,
    }
}

/// Cached detection; also records the agent name for telemetry.
pub fn detect_agent() -> &'static AgentResult {
    static R: OnceLock<AgentResult> = OnceLock::new();
    R.get_or_init(|| {
        let agent = refine(determine_agent());
        if let Some(a) = &agent {
            crate::telemetry::set_detected_agent(Some(a));
        }
        AgentResult { agent }
    })
}

pub fn is_running_in_agent() -> bool {
    detect_agent().is_agent()
}

/// Map a detected agent name to a skills-cli agent type.
pub fn get_agent_type(name: &str) -> Option<AgentType> {
    Some(match name {
        "cursor" | "cursor-cli" => "cursor",
        "claude" | "cowork" => "claude-code",
        "devin" => "universal",
        "replit" => "replit",
        "gemini" => "gemini-cli",
        "codex" => "codex",
        "antigravity" => "antigravity",
        "augment-cli" => "augment",
        "opencode" => "opencode",
        "github-copilot" => "github-copilot",
        _ => return None,
    })
}
