//! Agent definitions and detection (port of agents.ts).
//!
//! Agents are kept in the same order as the TS `agents` record, because that
//! order is observable (`Object.keys(agents)` drives prompts, `--agent '*'`,
//! error messages and lock-file content).

use crate::paths::join;
use crate::sys;
use std::path::Path;
use std::sync::OnceLock;

pub type AgentType = &'static str;

pub struct AgentConfig {
    pub name: AgentType,
    pub display_name: &'static str,
    pub skills_dir: &'static str,
    /// `None` when the agent does not support global installation.
    pub global_skills_dir: Option<String>,
    pub show_in_universal_list: bool,
    pub show_in_universal_prompt: bool,
    pub create_project_skills_dir_by_default: bool,
}

struct Homes {
    home: String,
    config_home: String,
    codex_home: String,
    claude_home: String,
    vibe_home: String,
    hermes_home: String,
    autohand_home: String,
    grok_home: String,
    sarvam_home: String,
    zed_app_data: Option<String>,
    zed_flatpak_config: Option<String>,
}

fn homes() -> &'static Homes {
    static H: OnceLock<Homes> = OnceLock::new();
    H.get_or_init(|| {
        let home = sys::homedir();
        let config_home =
            sys::env("XDG_CONFIG_HOME").unwrap_or_else(|| join(&[home.as_str(), ".config"]));
        let or_home = |var: &str, dir: &str| {
            sys::env_trimmed(var).unwrap_or_else(|| join(&[home.as_str(), dir]))
        };
        Homes {
            codex_home: or_home("CODEX_HOME", ".codex"),
            claude_home: or_home("CLAUDE_CONFIG_DIR", ".claude"),
            vibe_home: or_home("VIBE_HOME", ".vibe"),
            hermes_home: or_home("HERMES_HOME", ".hermes"),
            autohand_home: or_home("AUTOHAND_HOME", ".autohand"),
            grok_home: or_home("GROK_HOME", ".grok"),
            sarvam_home: or_home("SARVAM_HOME", ".sarvam"),
            zed_app_data: sys::env_trimmed("APPDATA"),
            zed_flatpak_config: sys::env_trimmed("FLATPAK_XDG_CONFIG_HOME"),
            home,
            config_home,
        }
    })
}

pub fn home() -> &'static str {
    &homes().home
}

fn exists(p: &str) -> bool {
    Path::new(p).exists()
}

fn h(rel: &str) -> String {
    join(&[homes().home.as_str(), rel])
}

fn c(rel: &str) -> String {
    join(&[homes().config_home.as_str(), rel])
}

fn cwd_path(rel: &str) -> String {
    join(&[sys::cwd().as_str(), rel])
}

pub fn get_openclaw_global_skills_dir(
    home_dir: &str,
    path_exists: &dyn Fn(&str) -> bool,
) -> String {
    if path_exists(&join(&[home_dir, ".openclaw"])) {
        return join(&[home_dir, ".openclaw/skills"]);
    }
    if path_exists(&join(&[home_dir, ".clawdbot"])) {
        return join(&[home_dir, ".clawdbot/skills"]);
    }
    if path_exists(&join(&[home_dir, ".moltbot"])) {
        return join(&[home_dir, ".moltbot/skills"]);
    }
    join(&[home_dir, ".openclaw/skills"])
}

pub fn is_zcode_installed(home_dir: &str, path_exists: &dyn Fn(&str) -> bool) -> bool {
    path_exists(&join(&[home_dir, ".zcode"])) || path_exists("/Applications/ZCode.app")
}

pub fn is_kimchi_installed(home_dir: &str, path_exists: &dyn Fn(&str) -> bool) -> bool {
    path_exists(&join(&[home_dir, ".config", "kimchi"]))
}

pub fn is_minimax_code_installed(home_dir: &str, path_exists: &dyn Fn(&str) -> bool) -> bool {
    path_exists(&join(&[home_dir, ".minimax"])) || path_exists("/Applications/MiniMax Code.app")
}

pub fn is_posit_assistant_installed(home_dir: &str, path_exists: &dyn Fn(&str) -> bool) -> bool {
    path_exists(&join(&[home_dir, ".posit/assistant"]))
        || path_exists(&join(&[home_dir, ".positai"]))
}

fn package_json_has_dependency(package_json_path: &str, dep: &str) -> bool {
    let Ok(content) = std::fs::read_to_string(package_json_path) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) else {
        return false;
    };
    let truthy = |x: Option<&serde_json::Value>| match x {
        None | Some(serde_json::Value::Null) | Some(serde_json::Value::Bool(false)) => false,
        Some(serde_json::Value::String(s)) => !s.is_empty(),
        Some(serde_json::Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Some(_) => true,
    };
    truthy(v.get("dependencies").and_then(|d| d.get(dep)))
        || truthy(v.get("devDependencies").and_then(|d| d.get(dep)))
}

struct Def {
    name: AgentType,
    display: &'static str,
    skills_dir: &'static str,
    global: Option<String>,
    universal_list: bool,
    universal_prompt: bool,
    create_default: bool,
}

fn def(
    name: AgentType,
    display: &'static str,
    skills_dir: &'static str,
    global: Option<String>,
) -> Def {
    Def {
        name,
        display,
        skills_dir,
        global,
        universal_list: true,
        universal_prompt: true,
        create_default: false,
    }
}

impl Def {
    fn hide_prompt(mut self) -> Self {
        self.universal_prompt = false;
        self
    }
    fn hide_list(mut self) -> Self {
        self.universal_list = false;
        self
    }
    fn create_default(mut self) -> Self {
        self.create_default = true;
        self
    }
}

pub fn agents() -> &'static [AgentConfig] {
    static A: OnceLock<Vec<AgentConfig>> = OnceLock::new();
    A.get_or_init(|| {
        let hm = homes();
        let defs = vec![
            def(
                "aider-desk",
                "AiderDesk",
                ".aider-desk/skills",
                Some(h(".aider-desk/skills")),
            ),
            def("amp", "Amp", ".agents/skills", Some(c("agents/skills"))),
            def(
                "antigravity",
                "Antigravity",
                ".agents/skills",
                Some(h(".gemini/antigravity/skills")),
            )
            .hide_prompt(),
            def(
                "antigravity-cli",
                "Antigravity CLI",
                ".agents/skills",
                Some(h(".gemini/antigravity-cli/skills")),
            )
            .hide_prompt(),
            def(
                "astrbot",
                "AstrBot",
                "data/skills",
                Some(h(".astrbot/data/skills")),
            ),
            def(
                "autohand-code",
                "Autohand Code CLI",
                ".autohand/skills",
                Some(join(&[hm.autohand_home.as_str(), "skills"])),
            ),
            def(
                "augment",
                "Augment",
                ".augment/skills",
                Some(h(".augment/skills")),
            ),
            def("bob", "IBM Bob", ".bob/skills", Some(h(".bob/skills"))),
            def(
                "claude-code",
                "Claude Code",
                ".claude/skills",
                Some(join(&[hm.claude_home.as_str(), "skills"])),
            )
            .create_default(),
            def(
                "openclaw",
                "OpenClaw",
                "skills",
                Some(get_openclaw_global_skills_dir(&hm.home, &exists)),
            ),
            def(
                "cline",
                "Cline",
                ".agents/skills",
                Some(join(&[hm.home.as_str(), ".agents", "skills"])),
            ),
            def(
                "codearts-agent",
                "CodeArts Agent",
                ".codeartsdoer/skills",
                Some(h(".codeartsdoer/skills")),
            ),
            def(
                "codebuddy",
                "CodeBuddy",
                ".codebuddy/skills",
                Some(h(".codebuddy/skills")),
            ),
            def(
                "codemaker",
                "Codemaker",
                ".codemaker/skills",
                Some(h(".codemaker/skills")),
            ),
            def(
                "codestudio",
                "Code Studio",
                ".codestudio/skills",
                Some(h(".codestudio/skills")),
            ),
            def(
                "codex",
                "Codex",
                ".agents/skills",
                Some(join(&[hm.codex_home.as_str(), "skills"])),
            ),
            def(
                "command-code",
                "Command Code",
                ".commandcode/skills",
                Some(h(".commandcode/skills")),
            ),
            def(
                "continue",
                "Continue",
                ".continue/skills",
                Some(h(".continue/skills")),
            ),
            def(
                "cortex",
                "Cortex Code",
                ".cortex/skills",
                Some(h(".snowflake/cortex/skills")),
            ),
            def(
                "crush",
                "Crush",
                ".crush/skills",
                Some(h(".config/crush/skills")),
            ),
            def(
                "cursor",
                "Cursor",
                ".agents/skills",
                Some(h(".cursor/skills")),
            ),
            def(
                "deepagents",
                "Deep Agents",
                ".agents/skills",
                Some(h(".deepagents/agent/skills")),
            )
            .hide_prompt(),
            def(
                "devin",
                "Devin for Terminal",
                ".devin/skills",
                Some(c("devin/skills")),
            ),
            def(
                "dexto",
                "Dexto",
                ".agents/skills",
                Some(h(".agents/skills")),
            )
            .hide_prompt(),
            def(
                "droid",
                "Droid",
                ".agents/skills",
                Some(h(".factory/skills")),
            ),
            def("eve", "Eve", "agent/skills", None),
            def(
                "firebender",
                "Firebender",
                ".agents/skills",
                Some(h(".firebender/skills")),
            )
            .hide_prompt(),
            def(
                "forgecode",
                "ForgeCode",
                ".forge/skills",
                Some(h(".forge/skills")),
            ),
            def("fx", "fx", ".fx/skills", Some(h(".fx/skills"))),
            def(
                "gemini-cli",
                "Gemini CLI",
                ".agents/skills",
                Some(h(".gemini/skills")),
            ),
            def(
                "github-copilot",
                "GitHub Copilot",
                ".agents/skills",
                Some(h(".copilot/skills")),
            ),
            def("goose", "Goose", ".goose/skills", Some(c("goose/skills"))),
            def(
                "grok",
                "Grok Build",
                ".grok/skills",
                Some(join(&[hm.grok_home.as_str(), "skills"])),
            ),
            def(
                "hermes-agent",
                "Hermes Agent",
                ".hermes/skills",
                Some(join(&[hm.hermes_home.as_str(), "skills"])),
            ),
            def(
                "inference-sh",
                "inference.sh",
                ".inferencesh/skills",
                Some(h(".inferencesh/skills")),
            ),
            def("jazz", "Jazz", ".jazz/skills", Some(h(".jazz/skills"))),
            def("junie", "Junie", ".junie/skills", Some(h(".junie/skills"))),
            def(
                "iflow-cli",
                "iFlow CLI",
                ".iflow/skills",
                Some(h(".iflow/skills")),
            ),
            def(
                "kilo",
                "Kilo Code",
                ".agents/skills",
                Some(h(".kilo/skills")),
            ),
            def(
                "kimchi",
                "Kimchi",
                ".kimchi/skills",
                Some(join(&[
                    hm.home.as_str(),
                    ".config",
                    "kimchi",
                    "harness",
                    "skills",
                ])),
            ),
            def(
                "kimi-code-cli",
                "Kimi Code CLI",
                ".agents/skills",
                Some(h(".agents/skills")),
            ),
            def(
                "kiro-cli",
                "Kiro CLI",
                ".kiro/skills",
                Some(h(".kiro/skills")),
            ),
            def("kode", "Kode", ".kode/skills", Some(h(".kode/skills"))),
            def(
                "lingma",
                "Lingma",
                ".lingma/skills",
                Some(h(".lingma/skills")),
            ),
            def("loaf", "Loaf", ".agents/skills", Some(h(".agents/skills"))).hide_prompt(),
            def(
                "mcpjam",
                "MCPJam",
                ".mcpjam/skills",
                Some(h(".mcpjam/skills")),
            ),
            def(
                "minimax-code",
                "MiniMax Code",
                ".minimax/skills",
                Some(h(".minimax/skills")),
            ),
            def(
                "mistral-vibe",
                "Mistral Vibe",
                ".vibe/skills",
                Some(join(&[hm.vibe_home.as_str(), "skills"])),
            ),
            def("moxby", "Moxby", ".moxby/skills", Some(h(".moxby/skills"))),
            def("mux", "Mux", ".mux/skills", Some(h(".mux/skills"))),
            def(
                "opencode",
                "OpenCode",
                ".agents/skills",
                Some(c("opencode/skills")),
            ),
            def(
                "openhands",
                "OpenHands",
                ".openhands/skills",
                Some(h(".openhands/skills")),
            ),
            def("ona", "Ona", ".ona/skills", Some(h(".ona/skills"))),
            def("pi", "Pi", ".pi/skills", Some(h(".pi/agent/skills"))),
            def(
                "posit-assistant",
                "Posit Assistant",
                ".posit/assistant/skills",
                Some(h(".posit/assistant/skills")),
            ),
            def("qoder", "Qoder", ".qoder/skills", Some(h(".qoder/skills"))),
            def(
                "qoder-cn",
                "Qoder CN",
                ".qoder/skills",
                Some(h(".qoder-cn/skills")),
            ),
            def(
                "qwen-code",
                "Qwen Code",
                ".qwen/skills",
                Some(h(".qwen/skills")),
            ),
            def(
                "replit",
                "Replit",
                ".agents/skills",
                Some(c("agents/skills")),
            )
            .hide_list(),
            def(
                "reasonix",
                "Reasonix",
                ".reasonix/skills",
                Some(h(".reasonix/skills")),
            ),
            def(
                "rovodev",
                "Rovo Dev",
                ".rovodev/skills",
                Some(h(".rovodev/skills")),
            ),
            def("roo", "Roo Code", ".roo/skills", Some(h(".roo/skills"))),
            def(
                "sarvam-code",
                "Sarvam Code",
                ".agents/skills",
                Some(h(".agents/skills")),
            )
            .hide_prompt(),
            def(
                "tabnine-cli",
                "Tabnine CLI",
                ".tabnine/agent/skills",
                Some(h(".tabnine/agent/skills")),
            ),
            def(
                "terramind",
                "Terramind",
                ".terramind/skills",
                Some(h(".terramind/skills")),
            ),
            def(
                "tinycloud",
                "Tinycloud",
                ".tinycloud/skills",
                Some(h(".tinycloud/skills")),
            ),
            def("trae", "Trae", ".trae/skills", Some(h(".trae/skills"))),
            def(
                "trae-cn",
                "Trae CN",
                ".trae/skills",
                Some(h(".trae-cn/skills")),
            ),
            def("warp", "Warp", ".agents/skills", Some(h(".agents/skills"))),
            def(
                "windsurf",
                "Windsurf",
                ".windsurf/skills",
                Some(h(".codeium/windsurf/skills")),
            ),
            def("zed", "Zed", ".agents/skills", Some(h(".agents/skills"))),
            def("zcode", "ZCode", ".zcode/skills", Some(h(".zcode/skills"))),
            def(
                "zencoder",
                "Zencoder",
                ".zencoder/skills",
                Some(h(".zencoder/skills")),
            ),
            def(
                "zenflow",
                "Zenflow",
                ".zencoder/skills",
                Some(h(".zencoder/skills")),
            ),
            def(
                "neovate",
                "Neovate",
                ".neovate/skills",
                Some(h(".neovate/skills")),
            ),
            def("pochi", "Pochi", ".pochi/skills", Some(h(".pochi/skills"))),
            def("promptscript", "PromptScript", ".agents/skills", None).hide_prompt(),
            def("adal", "AdaL", ".adal/skills", Some(h(".adal/skills"))),
            def(
                "universal",
                "Universal",
                ".agents/skills",
                Some(c("agents/skills")),
            )
            .hide_list(),
        ];
        defs.into_iter()
            .map(|d| AgentConfig {
                name: d.name,
                display_name: d.display,
                skills_dir: d.skills_dir,
                global_skills_dir: d.global,
                show_in_universal_list: d.universal_list,
                show_in_universal_prompt: d.universal_prompt,
                create_project_skills_dir_by_default: d.create_default,
            })
            .collect()
    })
}

/// Look up an agent by name.
pub fn get(name: &str) -> Option<&'static AgentConfig> {
    agents().iter().find(|a| a.name == name)
}

/// Look up an agent that is known to exist.
pub fn agent(name: &str) -> &'static AgentConfig {
    get(name).unwrap_or_else(|| panic!("unknown agent: {}", name))
}

/// Intern a user-provided agent name into the static agent list.
pub fn to_agent_type(name: &str) -> Option<AgentType> {
    get(name).map(|a| a.name)
}

pub fn all_agent_names() -> Vec<AgentType> {
    agents().iter().map(|a| a.name).collect()
}

/// `config.detectInstalled()`
pub fn detect_installed(name: &str) -> bool {
    let hm = homes();
    let home = hm.home.as_str();
    match name {
        "aider-desk" => exists(&h(".aider-desk")),
        "amp" => exists(&c("amp")),
        "antigravity" => exists(&h(".gemini/antigravity")),
        "antigravity-cli" => exists(&h(".gemini/antigravity-cli")),
        "astrbot" => exists(&cwd_path("data/skills")) || exists(&h(".astrbot")),
        "autohand-code" => exists(&hm.autohand_home),
        "augment" => exists(&h(".augment")),
        "bob" => exists(&h(".bob")),
        "claude-code" => exists(&hm.claude_home),
        "openclaw" => exists(&h(".openclaw")) || exists(&h(".clawdbot")) || exists(&h(".moltbot")),
        "cline" => exists(&h(".cline")),
        "codearts-agent" => exists(&h(".codeartsdoer")),
        "codebuddy" => exists(&cwd_path(".codebuddy")) || exists(&h(".codebuddy")),
        "codemaker" => exists(&h(".codemaker")),
        "codestudio" => exists(&h(".codestudio")),
        "codex" => exists(&hm.codex_home) || exists("/etc/codex"),
        "command-code" => exists(&h(".commandcode")),
        "continue" => exists(&cwd_path(".continue")) || exists(&h(".continue")),
        "cortex" => exists(&h(".snowflake/cortex")),
        "crush" => exists(&h(".config/crush")),
        "cursor" => exists(&h(".cursor")),
        "deepagents" => exists(&h(".deepagents")),
        "devin" => exists(&c("devin")),
        "dexto" => exists(&h(".dexto")),
        "droid" => exists(&h(".factory")),
        "eve" => {
            let cwd = sys::cwd();
            exists(&join(&[cwd.as_str(), "agent"]))
                && package_json_has_dependency(&join(&[cwd.as_str(), "package.json"]), "eve")
        }
        "firebender" => exists(&h(".firebender")),
        "forgecode" => exists(&h(".forge")),
        "fx" => exists(&h(".fx")),
        "gemini-cli" => exists(&h(".gemini")),
        "github-copilot" => exists(&h(".copilot")),
        "goose" => exists(&c("goose")),
        "grok" => exists(&hm.grok_home),
        "hermes-agent" => exists(&hm.hermes_home),
        "inference-sh" => exists(&h(".inferencesh")),
        "jazz" => exists(&h(".jazz")) || exists(&cwd_path(".jazz")),
        "junie" => exists(&h(".junie")),
        "iflow-cli" => exists(&h(".iflow")),
        "kilo" => exists(&h(".kilo")) || exists(&h(".kilocode")),
        "kimchi" => is_kimchi_installed(home, &exists),
        "kimi-code-cli" => exists(&h(".kimi-code")) || exists(&h(".kimi")),
        "kiro-cli" => exists(&h(".kiro")),
        "kode" => exists(&h(".kode")),
        "lingma" => exists(&h(".lingma")),
        "loaf" => exists(&h(".loaf")),
        "mcpjam" => exists(&h(".mcpjam")),
        "minimax-code" => is_minimax_code_installed(home, &exists),
        "mistral-vibe" => exists(&hm.vibe_home),
        "moxby" => exists(&h(".moxby")),
        "mux" => exists(&h(".mux")),
        "opencode" => exists(&c("opencode")),
        "openhands" => exists(&h(".openhands")),
        "ona" => exists(&h(".ona")),
        "pi" => exists(&h(".pi/agent")),
        "posit-assistant" => is_posit_assistant_installed(home, &exists),
        "qoder" => exists(&h(".qoder")),
        "qoder-cn" => exists(&h(".qoder-cn")),
        "qwen-code" => exists(&h(".qwen")),
        "replit" => exists(&cwd_path(".replit")),
        "reasonix" => exists(&h(".reasonix")),
        "rovodev" => exists(&h(".rovodev")),
        "roo" => exists(&h(".roo")),
        "sarvam-code" => exists(&hm.sarvam_home),
        "tabnine-cli" => exists(&h(".tabnine")),
        "terramind" => exists(&h(".terramind")),
        "tinycloud" => exists(&h(".tinycloud")),
        "trae" => exists(&h(".trae")),
        "trae-cn" => exists(&h(".trae-cn")),
        "warp" => exists(&h(".warp")),
        "windsurf" => exists(&h(".codeium/windsurf")),
        "zed" => {
            exists(&c("zed"))
                || hm
                    .zed_app_data
                    .as_ref()
                    .map(|d| exists(&join(&[d.as_str(), "Zed"])))
                    .unwrap_or(false)
                || hm
                    .zed_flatpak_config
                    .as_ref()
                    .map(|d| exists(&join(&[d.as_str(), "zed"])))
                    .unwrap_or(false)
        }
        "zcode" => is_zcode_installed(home, &exists),
        "zencoder" | "zenflow" => exists(&h(".zencoder")),
        "neovate" => exists(&h(".neovate")),
        "pochi" => exists(&h(".pochi")),
        "promptscript" => {
            exists(&cwd_path(".promptscript")) || exists(&cwd_path("promptscript.yaml"))
        }
        "adal" => exists(&h(".adal")),
        _ => false,
    }
}

pub fn detect_installed_agents() -> Vec<AgentType> {
    agents()
        .iter()
        .filter(|a| detect_installed(a.name))
        .map(|a| a.name)
        .collect()
}

/// `join('agent', 'subagents')`
pub fn eve_subagents_dir() -> String {
    join(&["agent", "subagents"])
}

/// Names of Eve subagent directories under `agent/subagents/`, sorted.
pub fn get_eve_subagents(cwd: &str) -> Vec<String> {
    let dir = join(&[cwd, eve_subagents_dir().as_str()]);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

pub fn get_universal_agents() -> Vec<AgentType> {
    agents()
        .iter()
        .filter(|a| a.skills_dir == ".agents/skills" && a.show_in_universal_list)
        .map(|a| a.name)
        .collect()
}

pub fn get_visible_universal_agents() -> Vec<AgentType> {
    agents()
        .iter()
        .filter(|a| {
            a.skills_dir == ".agents/skills"
                && a.show_in_universal_list
                && a.show_in_universal_prompt
        })
        .map(|a| a.name)
        .collect()
}

pub fn get_non_universal_agents() -> Vec<AgentType> {
    agents()
        .iter()
        .filter(|a| a.skills_dir != ".agents/skills")
        .map(|a| a.name)
        .collect()
}

pub fn is_universal_agent(name: &str) -> bool {
    get(name)
        .map(|a| a.skills_dir == ".agents/skills")
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_shape() {
        assert_eq!(agents().len(), 79);
        assert_eq!(agents()[0].name, "aider-desk");
        assert_eq!(agents().last().unwrap().name, "universal");
        assert!(get_universal_agents().contains(&"codex"));
        assert!(!get_universal_agents().contains(&"replit"));
        assert!(!get_visible_universal_agents().contains(&"antigravity"));
        assert!(get_non_universal_agents().contains(&"claude-code"));
        assert!(agent("eve").global_skills_dir.is_none());
    }

    #[test]
    fn openclaw_global_dir_fallbacks() {
        let none = |_: &str| false;
        assert!(get_openclaw_global_skills_dir("/h", &none).ends_with("skills"));
        let clawd = |p: &str| p.ends_with(".clawdbot");
        assert!(get_openclaw_global_skills_dir("/h", &clawd).contains(".clawdbot"));
    }
}
