//! `skills add` (port of add.ts).

use crate::agents::{
    self, agent, detect_installed_agents, get_eve_subagents, get_non_universal_agents,
    get_universal_agents, get_visible_universal_agents, is_universal_agent, AgentType,
};
use crate::blob::{
    fetch_repo_tree, get_skill_folder_hash_from_tree, is_blob_allowed_repo, try_blob_install,
    BlobInstallResult, BlobOptions,
};
use crate::color::pc;
use crate::detect_agent::{detect_agent, get_agent_type};
use crate::download_source::{download_source, DownloadKind, DownloadOptions};
use crate::git::{cleanup_temp_dir, clone_repo, GitCloneError};
use crate::installer::{
    get_canonical_path, install_blob_skill_for_agent, install_skill_for_agent,
    install_well_known_skill_for_agent, is_skill_installed, InstallMode, InstallOptions,
    InstallResult,
};
use crate::local_lock::{add_skill_to_local_lock, compute_skill_folder_hash};
use crate::paths::{self, join};
use crate::sanitize::{js_len, js_slice_to, strip_terminal_escapes};
use crate::search_multiselect::{search_multiselect, LockedSection, Options, SearchItem};
use crate::skill_lock::{
    add_skill_to_lock, dismiss_prompt, get_last_selected_agents, is_prompt_dismissed,
    save_selected_agents,
};
use crate::skills::{discover_skills, filter_skills, get_skill_display_name, DiscoverOptions};
use crate::source_parser::{get_owner_repo, is_repo_private, parse_owner_repo, parse_source};
use crate::telemetry::{fetch_audit_data, track, AuditResponse};
use crate::types::{ParsedSource, Skill};
use crate::ui::{self, log, SelectOption};
use crate::urlutil;
use crate::wellknown::{self, compute_well_known_skill_digest, WellKnownSkill};
use crate::{errln, outln, sys};
use serde_json::{json, Map, Value};
use std::sync::{Arc, Condvar, Mutex};

const EVE_AGENT_LABEL: &str = "eve agent";
const BLOB_ALLOWED_OWNERS: &[&str] = &["vercel", "vercel-labs", "heygen-com", "remotion-dev"];

#[derive(Default, Clone, Debug)]
pub struct AddOptions {
    /// `None` means "not specified" (the scope prompt may be shown).
    pub global: Option<bool>,
    pub agent: Option<Vec<String>>,
    pub yes: bool,
    pub skill: Option<Vec<String>>,
    pub metadata: Option<String>,
    pub list: bool,
    pub all: bool,
    pub full_depth: bool,
    pub copy: bool,
    pub subagent: Option<Vec<String>>,
    pub json: bool,
}

/// A value computed on a background thread (stands in for an early-started Promise).
#[derive(Clone)]
struct Shared<T: Clone + Send + 'static>(Arc<(Mutex<Option<T>>, Condvar)>);

impl<T: Clone + Send + 'static> Shared<T> {
    fn spawn(f: impl FnOnce() -> T + Send + 'static) -> Self {
        let s = Shared(Arc::new((Mutex::new(None), Condvar::new())));
        let c = s.clone();
        std::thread::spawn(move || {
            let v = f();
            *c.0 .0.lock().unwrap() = Some(v);
            c.0 .1.notify_all();
        });
        s
    }
    fn ready(v: T) -> Self {
        Shared(Arc::new((Mutex::new(Some(v)), Condvar::new())))
    }
    fn wait(&self) -> T {
        let mut g = self.0 .0.lock().unwrap();
        while g.is_none() {
            g = self.0 .1.wait(g).unwrap();
        }
        g.clone().unwrap()
    }
}

pub fn get_lock_source(parsed_url: &str, normalized: Option<&str>) -> Option<String> {
    if parsed_url.starts_with("git@") || parsed_url.starts_with("ssh://") {
        return Some(parsed_url.to_string());
    }
    if parsed_url.starts_with("http://") || parsed_url.starts_with("https://") {
        match urlutil::parse(parsed_url) {
            Some(u) => {
                if urlutil::hostname(&u) != "github.com" {
                    return Some(parsed_url.to_string());
                }
            }
            None => return normalized.map(|s| s.to_string()),
        }
    }
    normalized.map(|s| s.to_string())
}

pub fn get_project_lock_source_url(source_type: &str, source_url: &str) -> Option<String> {
    if source_type == "git" || source_type == "gitlab" {
        Some(source_url.to_string())
    } else {
        None
    }
}

// ─── Security advisory ───

fn risk_label(risk: &str) -> String {
    match risk {
        "critical" => pc::red(pc::bold("Critical Risk")),
        "high" => pc::red("High Risk"),
        "medium" => pc::yellow("Med Risk"),
        "low" => pc::green("Low Risk"),
        "safe" => pc::green("Safe"),
        _ => pc::dim("--"),
    }
}

fn socket_label(audit: &Value) -> String {
    let count = audit.get("alerts").and_then(|v| v.as_f64()).unwrap_or(0.0) as i64;
    if count > 0 {
        pc::red(format!(
            "{} alert{}",
            count,
            if count != 1 { "s" } else { "" }
        ))
    } else {
        pc::green("0 alerts")
    }
}

fn pad_end(s: &str, width: usize) -> String {
    let visible = js_len(&strip_terminal_escapes(s));
    format!("{}{}", s, " ".repeat(width.saturating_sub(visible)))
}

fn partner<'a>(data: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    data.and_then(|d| d.get(key))
        .filter(|v| crate::skills::js_truthy(Some(v)))
}

fn build_security_lines(
    audit: Option<&AuditResponse>,
    skills: &[String],
    source: &str,
) -> Vec<String> {
    let Some(audit) = audit else {
        return Vec::new();
    };
    let has_any = skills.iter().any(|s| {
        audit
            .get(s)
            .and_then(|d| d.as_object())
            .map(|o| !o.is_empty())
            .unwrap_or(false)
    });
    if !has_any {
        return Vec::new();
    }
    let name_width = skills.iter().map(|s| js_len(s)).max().unwrap_or(0).min(36);
    let mut lines = vec![format!(
        "{}{}{}{}",
        pad_end("", name_width + 2),
        pad_end(&pc::dim("Gen"), 18),
        pad_end(&pc::dim("Socket"), 18),
        pc::dim("Snyk")
    )];
    for skill in skills {
        let data = audit.get(skill);
        let name = if js_len(skill) > name_width {
            format!(
                "{}\u{2026}",
                js_slice_to(skill, name_width.saturating_sub(1))
            )
        } else {
            skill.clone()
        };
        let risk = |k: &str| {
            partner(data, k)
                .map(|p| risk_label(p.get("risk").and_then(|r| r.as_str()).unwrap_or("")))
                .unwrap_or_else(|| pc::dim("--"))
        };
        let ath = risk("ath");
        let socket = partner(data, "socket")
            .map(socket_label)
            .unwrap_or_else(|| pc::dim("--"));
        let snyk = risk("snyk");
        lines.push(format!(
            "{}{}{}{}",
            pad_end(&pc::cyan(&name), name_width + 2),
            pad_end(&ath, 18),
            pad_end(&socket, 18),
            snyk
        ));
    }
    lines.push(String::new());
    lines.push(format!(
        "{} {}",
        pc::dim("Details:"),
        pc::dim(format!("https://skills.sh/{}", source))
    ));
    lines
}

fn build_json_security(audit: Option<&AuditResponse>, skill: &str, source: Option<&str>) -> Value {
    let Some(data) = audit.and_then(|a| a.get(skill)) else {
        return Value::Null;
    };
    if data.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        return Value::Null;
    }
    let mut out = Map::new();
    if let Some(a) = partner(Some(data), "ath") {
        out.insert("gen".into(), a.get("risk").cloned().unwrap_or(Value::Null));
    }
    if let Some(s) = partner(Some(data), "socket") {
        let n = s.get("alerts").and_then(|v| v.as_f64()).unwrap_or(0.0) as i64;
        out.insert(
            "socket".into(),
            Value::String(format!("{} alert{}", n, if n != 1 { "s" } else { "" })),
        );
    }
    if let Some(s) = partner(Some(data), "snyk") {
        out.insert("snyk".into(), s.get("risk").cloned().unwrap_or(Value::Null));
    }
    if let Some(src) = source.filter(|s| !s.is_empty()) {
        out.insert(
            "details".into(),
            Value::String(format!("https://skills.sh/{}", src)),
        );
    }
    Value::Object(out)
}

/// Replace homedir with `~` and cwd with `.` on whole path segments.
pub fn shorten_path(full: &str, cwd: &str) -> String {
    crate::sync::shorten_path(full, cwd)
}

pub fn format_list(items: &[String], max_show: usize) -> String {
    crate::list::format_list(items, max_show)
}

fn format_skill_prompt_subject(skills: &[&Skill]) -> String {
    let names: Vec<String> = skills
        .iter()
        .map(|s| pc::cyan(get_skill_display_name(s)))
        .collect();
    let subject = format_list(&names, 3);
    if js_len(&strip_terminal_escapes(&subject)) <= 80 {
        subject
    } else {
        format!("{} selected skills", skills.len())
    }
}

pub fn format_eve_install_prompt_message(skills: &[&Skill]) -> String {
    format!(
        "Detected an eve project. Install {} for your {} to use?",
        format_skill_prompt_subject(skills),
        EVE_AGENT_LABEL
    )
}

fn split_agents_by_type(list: &[AgentType]) -> (Vec<String>, Vec<String>) {
    let mut universal = Vec::new();
    let mut symlinked = Vec::new();
    for a in list {
        if is_universal_agent(a) {
            universal.push(agent(a).display_name.to_string());
        } else {
            symlinked.push(agent(a).display_name.to_string());
        }
    }
    (universal, symlinked)
}

fn build_agent_summary_lines(targets: &[AgentType], mode: InstallMode) -> Vec<String> {
    let mut lines = Vec::new();
    let (universal, symlinked) = split_agents_by_type(targets);
    if mode == InstallMode::Symlink {
        if !universal.is_empty() {
            lines.push(format!(
                "  {} {}",
                pc::green("universal:"),
                format_list(&universal, 5)
            ));
        }
        if !symlinked.is_empty() {
            lines.push(format!(
                "  {} {}",
                pc::dim("symlink →"),
                format_list(&symlinked, 5)
            ));
        }
    } else {
        let all: Vec<String> = targets
            .iter()
            .map(|a| agent(a).display_name.to_string())
            .collect();
        lines.push(format!("  {} {}", pc::dim("copy →"), format_list(&all, 5)));
    }
    lines
}

#[derive(Clone, Debug)]
struct InstallTarget {
    agent: AgentType,
    subagent: Option<String>,
}

fn target_display_name(t: &InstallTarget) -> String {
    let base = agent(t.agent).display_name;
    match &t.subagent {
        Some(s) => format!("{} ({})", base, s),
        None => base.to_string(),
    }
}

fn build_install_targets(targets: &[AgentType], eve: &[Option<String>]) -> Vec<InstallTarget> {
    let mut out = Vec::new();
    for a in targets {
        if *a == "eve" {
            for s in eve {
                out.push(InstallTarget {
                    agent: a,
                    subagent: s.clone(),
                });
            }
        } else {
            out.push(InstallTarget {
                agent: a,
                subagent: None,
            });
        }
    }
    out
}

fn build_target_summary_lines(targets: &[InstallTarget], mode: InstallMode) -> Vec<String> {
    let mut lines = Vec::new();
    let root: Vec<AgentType> = targets
        .iter()
        .filter(|t| t.subagent.is_none())
        .map(|t| t.agent)
        .collect();
    let subs: Vec<String> = targets
        .iter()
        .filter(|t| t.subagent.is_some())
        .map(target_display_name)
        .collect();
    let (universal, symlinked) = split_agents_by_type(&root);
    if mode == InstallMode::Symlink {
        if !universal.is_empty() {
            lines.push(format!(
                "  {} {}",
                pc::green("universal:"),
                format_list(&universal, 5)
            ));
        }
        if !symlinked.is_empty() {
            lines.push(format!(
                "  {} {}",
                pc::dim("symlink →"),
                format_list(&symlinked, 5)
            ));
        }
        if !subs.is_empty() {
            lines.push(format!("  {} {}", pc::dim("copy →"), format_list(&subs, 5)));
        }
    } else {
        let all: Vec<String> = targets.iter().map(target_display_name).collect();
        lines.push(format!("  {} {}", pc::dim("copy →"), format_list(&all, 5)));
    }
    lines
}

fn ensure_universal_agents(targets: &[AgentType]) -> Vec<AgentType> {
    let mut out = targets.to_vec();
    for ua in get_universal_agents() {
        if !out.contains(&ua) {
            out.push(ua);
        }
    }
    out
}

struct AddResult {
    skill: String,
    agent: String,
    plugin_name: Option<String>,
    r: InstallResult,
}

fn build_result_lines(results: &[&AddResult], targets: &[AgentType]) -> Vec<String> {
    let mut lines = Vec::new();
    let (universal, symlink_agents) = split_agents_by_type(targets);
    let ok: Vec<String> = results
        .iter()
        .filter(|r| !r.r.symlink_failed && !r.r.skipped && !universal.contains(&r.agent))
        .map(|r| r.agent.clone())
        .collect();
    let failed: Vec<String> = results
        .iter()
        .filter(|r| r.r.symlink_failed && !r.r.skipped)
        .map(|r| r.agent.clone())
        .collect();
    let skipped: Vec<String> = results
        .iter()
        .filter(|r| {
            r.r.skipped
                && r.r.skip_reason == Some("missing-agent-project-directory")
                && symlink_agents.contains(&r.agent)
        })
        .map(|r| r.agent.clone())
        .collect();
    if !universal.is_empty() {
        lines.push(format!(
            "  {} {}",
            pc::green("universal:"),
            format_list(&universal, 5)
        ));
    }
    if !ok.is_empty() {
        lines.push(format!(
            "  {} {}",
            pc::dim("symlinked:"),
            format_list(&ok, 5)
        ));
    }
    if !failed.is_empty() {
        lines.push(format!(
            "  {} {}",
            pc::yellow("copied:"),
            format_list(&failed, 5)
        ));
    }
    if !skipped.is_empty() {
        lines.push(format!(
            "  {} {} {}",
            pc::yellow("skipped:"),
            format_list(&skipped, 5),
            pc::dim("(project directory not found)")
        ));
    }
    lines
}

/// Exit after a prompt was cancelled before anything was installed.
fn exit_installation_cancelled() -> ! {
    ui::cancel("Installation cancelled");
    if !sys::stdin_is_tty() {
        errln!("Interactive prompt required but stdin is not a TTY. Nothing was installed. Use --agent <name> (or --agent '*') and -y to run non-interactively.");
        sys::exit(1);
    }
    sys::exit(0);
}

fn agent_item(a: AgentType, hint: Option<String>) -> SearchItem<AgentType> {
    SearchItem::new(a, a, agent(a).display_name).hint(hint)
}

/// Search prompt over `choices`, pre-selecting the last used agents.
pub fn prompt_for_agents(message: &str, choices: Vec<AgentType>) -> Option<Vec<AgentType>> {
    let last = get_last_selected_agents();
    let defaults: Vec<AgentType> = ["claude-code", "opencode", "codex"]
        .into_iter()
        .filter(|a| choices.contains(a))
        .collect();
    let mut initial: Vec<AgentType> = match &last {
        Some(l) if !l.is_empty() => l
            .iter()
            .filter_map(|a| choices.iter().find(|c| **c == a.as_str()).copied())
            .collect(),
        _ => Vec::new(),
    };
    if initial.is_empty() {
        initial = defaults;
    }
    let items: Vec<SearchItem<AgentType>> = choices.iter().map(|a| agent_item(a, None)).collect();
    let mut o = Options::new(message, items);
    o.initial_selected = initial
        .iter()
        .filter_map(|a| choices.iter().position(|c| c == a))
        .collect();
    o.required = true;
    let selected = search_multiselect(o)?;
    let _ = save_selected_agents(&selected);
    Some(selected)
}

fn select_agents_interactive(global: bool) -> Option<Vec<AgentType>> {
    let supports = |a: &AgentType| !global || agent(a).global_skills_dir.is_some();
    let universal: Vec<AgentType> = get_universal_agents()
        .into_iter()
        .filter(supports)
        .collect();
    let visible: Vec<AgentType> = get_visible_universal_agents()
        .into_iter()
        .filter(supports)
        .collect();
    let others: Vec<AgentType> = get_non_universal_agents()
        .into_iter()
        .filter(|a| *a != "eve" && supports(a))
        .collect();
    let items: Vec<SearchItem<AgentType>> = others
        .iter()
        .map(|a| {
            agent_item(
                a,
                Some(if global {
                    agent(a).global_skills_dir.clone().unwrap_or_default()
                } else {
                    agent(a).skills_dir.to_string()
                }),
            )
        })
        .collect();
    let last = get_last_selected_agents();
    let initial: Vec<usize> = match &last {
        Some(l) => l
            .iter()
            .filter(|a| others.contains(&a.as_str()) && !universal.contains(&a.as_str()))
            .filter_map(|a| others.iter().position(|o| *o == a.as_str()))
            .collect(),
        None => Vec::new(),
    };
    let mut o = Options::new("Which agents do you want to install to?", items);
    o.initial_selected = initial;
    o.locked_section = Some(LockedSection {
        title: "Universal (.agents/skills)".into(),
        items: visible.iter().map(|a| agent_item(a, None)).collect(),
        hidden_count: universal.len() - visible.len(),
    });
    let selected = search_multiselect(o)?;
    let _ = save_selected_agents(&selected);
    Some(selected)
}

fn is_skills_sh_pack_url(url: &str) -> bool {
    let Some(u) = urlutil::parse(url) else {
        return false;
    };
    let host = urlutil::hostname(&u);
    let host = host.strip_prefix("www.").unwrap_or(&host);
    host == "skills.sh" && regex::Regex::new(r"^/p/[^/]+").unwrap().is_match(u.path())
}

fn log_auto_selected_skills(entries: &[(String, String)]) {
    if entries.len() != 1 {
        log::info(&format!("Installing all {} skills", entries.len()));
        return;
    }
    log::info(&format!("Skill: {}", pc::cyan(&entries[0].0)));
    if !entries[0].1.is_empty() {
        log::message(&pc::dim(&entries[0].1));
    }
}

fn validate_agents(list: &[String]) -> Result<Vec<AgentType>, Vec<String>> {
    let invalid: Vec<String> = list
        .iter()
        .filter(|a| agents::get(a).is_none())
        .cloned()
        .collect();
    if !invalid.is_empty() {
        return Err(invalid);
    }
    Ok(list
        .iter()
        .map(|a| agents::to_agent_type(a).unwrap())
        .collect())
}

fn is_source_private(source: &str) -> Option<bool> {
    match parse_owner_repo(source) {
        None => Some(false),
        Some((o, r)) => is_repo_private(&o, &r),
    }
}

fn select_scope() -> bool {
    let options = vec![
        SelectOption::new(
            false,
            "Project",
            Some("Install in current directory (committed with your project)"),
        ),
        SelectOption::new(
            true,
            "Global",
            Some("Install in home directory (available across all projects)"),
        ),
    ];
    match ui::select("Installation scope", &options, 0) {
        Some(v) => v,
        None => exit_installation_cancelled(),
    }
}

fn select_mode() -> Option<InstallMode> {
    let options = vec![
        SelectOption::new(
            InstallMode::Symlink,
            "Symlink (Recommended)",
            Some("Single source of truth, easy updates"),
        ),
        SelectOption::new(
            InstallMode::Copy,
            "Copy to all agents",
            Some("Independent copies for each agent"),
        ),
    ];
    ui::select("Installation method", &options, 0)
}

/// Well-known discovery flow. Returns `false` to fall back to a direct download.
fn handle_well_known_skills(url: &str, options: &AddOptions, spinner: &mut ui::Spinner) -> bool {
    spinner.start("Discovering skills from well-known endpoint...");
    let include_internal = options
        .skill
        .as_ref()
        .map(|s| !s.is_empty() && !s.iter().any(|x| x == "*"))
        .unwrap_or(false);
    let skills: Vec<WellKnownSkill> = match wellknown::fetch_all_skills(url, include_internal) {
        Ok(s) => s,
        Err(e) => {
            spinner.stop(&pc::red("No matching skills"));
            log::error(&e.to_string());
            sys::exit(1);
        }
    };
    if skills.is_empty() {
        spinner.stop(&pc::dim(
            "No well-known skills found; trying direct download...",
        ));
        return false;
    }
    spinner.stop(&format!(
        "Found {} skill{}",
        pc::green(skills.len().to_string()),
        if skills.len() > 1 { "s" } else { "" }
    ));

    for s in &skills {
        log::info(&format!("Skill: {}", pc::cyan(&s.install_name)));
        log::message(&pc::dim(&s.description));
        if s.files.len() > 1 {
            log::message(&pc::dim(format!(
                "  Files: {}",
                s.files
                    .iter()
                    .map(|f| f.path.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
    }

    if options.list {
        outln!();
        log::step(&pc::bold("Available Skills"));
        for s in &skills {
            log::message(&format!("  {}", pc::cyan(&s.install_name)));
            log::message(&format!("    {}", pc::dim(&s.description)));
            if s.files.len() > 1 {
                log::message(&format!(
                    "    {}",
                    pc::dim(format!("Files: {}", s.files.len()))
                ));
            }
        }
        outln!();
        ui::outro("Run without --list to install");
        sys::exit(0);
    }

    let log_chosen = |chosen: &[&WellKnownSkill]| {
        log_auto_selected_skills(
            &chosen
                .iter()
                .map(|s| (s.install_name.clone(), s.description.clone()))
                .collect::<Vec<_>>(),
        );
    };
    let selected: Vec<&WellKnownSkill> = if options
        .skill
        .as_ref()
        .map(|s| s.iter().any(|x| x == "*"))
        .unwrap_or(false)
    {
        let all: Vec<&WellKnownSkill> = skills.iter().collect();
        log_chosen(&all);
        all
    } else if let Some(names) = options.skill.as_ref().filter(|s| !s.is_empty()) {
        let sel: Vec<&WellKnownSkill> = skills
            .iter()
            .filter(|s| {
                names.iter().any(|n| {
                    s.install_name.to_lowercase() == n.to_lowercase()
                        || s.name.to_lowercase() == n.to_lowercase()
                })
            })
            .collect();
        if sel.is_empty() {
            log::error(&format!(
                "No matching skills found for: {}",
                names.join(", ")
            ));
            log::info("Available skills:");
            for s in &skills {
                log::message(&format!("  - {}", s.install_name));
            }
            sys::exit(1);
        }
        sel
    } else if skills.len() == 1 || options.yes {
        let all: Vec<&WellKnownSkill> = skills.iter().collect();
        log_chosen(&all);
        all
    } else {
        let items: Vec<SearchItem<usize>> = skills
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let hint = if js_len(&s.description) > 60 {
                    format!("{}…", js_slice_to(&s.description, 57))
                } else {
                    s.description.clone()
                };
                SearchItem::new(i, "[object Object]", &s.install_name).hint(Some(hint))
            })
            .collect();
        let mut o = Options::new("Select skills to install", items);
        if is_skills_sh_pack_url(url) {
            o.initial_selected = (0..skills.len()).collect();
        }
        o.required = true;
        o.max_visible = 20;
        o.select_all = true;
        match search_multiselect(o) {
            Some(idx) => idx.into_iter().map(|i| &skills[i]).collect(),
            None => exit_installation_cancelled(),
        }
    };

    let target_agents: Vec<AgentType> = if options
        .agent
        .as_ref()
        .map(|a| a.iter().any(|x| x == "*"))
        .unwrap_or(false)
    {
        let all = agents::all_agent_names();
        log::info(&format!("Installing to all {} agents", all.len()));
        all
    } else if let Some(list) = options.agent.as_ref().filter(|l| !l.is_empty()) {
        match validate_agents(list) {
            Ok(v) => v,
            Err(invalid) => {
                log::error(&format!("Invalid agents: {}", invalid.join(", ")));
                log::info(&format!(
                    "Valid agents: {}",
                    agents::all_agent_names().join(", ")
                ));
                sys::exit(1);
            }
        }
    } else {
        spinner.start("Loading agents…");
        let installed = detect_installed_agents();
        spinner.stop(&format!("{} agents", agents::agents().len()));
        if installed.is_empty() {
            if options.yes {
                log::info("Installing to all agents");
                agents::all_agent_names()
            } else {
                log::info("Select agents to install skills to");
                match prompt_for_agents(
                    "Which agents do you want to install to?",
                    agents::all_agent_names(),
                ) {
                    Some(v) => v,
                    None => exit_installation_cancelled(),
                }
            }
        } else if installed.len() == 1 || options.yes {
            if installed.len() == 1 {
                log::info(&format!(
                    "Installing to: {}",
                    pc::cyan(agent(installed[0]).display_name)
                ));
            } else {
                log::info(&format!(
                    "Installing to: {}",
                    installed
                        .iter()
                        .map(|a| pc::cyan(agent(a).display_name))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            ensure_universal_agents(&installed)
        } else {
            match select_agents_interactive(options.global.unwrap_or(false)) {
                Some(v) => v,
                None => exit_installation_cancelled(),
            }
        }
    };

    let mut install_globally = options.global.unwrap_or(false);
    let supports_global = target_agents
        .iter()
        .any(|a| agent(a).global_skills_dir.is_some());
    if options.global.is_none() && !options.yes && supports_global {
        install_globally = select_scope();
    }

    let mut mode = if options.copy {
        InstallMode::Copy
    } else {
        InstallMode::Symlink
    };
    let unique_dirs: std::collections::HashSet<&str> =
        target_agents.iter().map(|a| agent(a).skills_dir).collect();
    if !options.copy && !options.yes && unique_dirs.len() > 1 {
        mode = select_mode().unwrap_or_else(|| exit_installation_cancelled());
    } else if unique_dirs.len() <= 1 {
        mode = InstallMode::Copy;
    }

    let cwd = sys::cwd();
    let mut summary: Vec<String> = Vec::new();
    for s in &selected {
        if !summary.is_empty() {
            summary.push(String::new());
        }
        let canonical = get_canonical_path(&s.install_name, install_globally, None, None, None)
            .unwrap_or_default();
        summary.push(pc::cyan(shorten_path(&canonical, &cwd)));
        summary.extend(build_agent_summary_lines(&target_agents, mode));
        if s.files.len() > 1 {
            summary.push(format!("  {} {}", pc::dim("files:"), s.files.len()));
        }
        let overwrites: Vec<String> = target_agents
            .iter()
            .filter(|a| is_skill_installed(&s.install_name, a, install_globally, None, None))
            .map(|a| agent(a).display_name.to_string())
            .collect();
        if !overwrites.is_empty() {
            summary.push(format!(
                "  {} {}",
                pc::yellow("overwrites:"),
                format_list(&overwrites, 5)
            ));
        }
    }
    outln!();
    ui::note(&summary.join("\n"), "Installation Summary");

    if !options.yes {
        match ui::confirm("Proceed with installation?", true) {
            Some(true) => {}
            _ => exit_installation_cancelled(),
        }
    }

    let source_identifier = wellknown::get_source_identifier(url);
    let sid = source_identifier.clone();
    let privacy = Shared::spawn(move || is_source_private(&sid));

    spinner.start("Installing skills…");
    let mut results: Vec<AddResult> = Vec::new();
    for s in &selected {
        for a in &target_agents {
            let r = install_well_known_skill_for_agent(
                &s.install_name,
                &s.files,
                a,
                &InstallOptions {
                    global: install_globally,
                    mode: Some(mode),
                    ..Default::default()
                },
            );
            results.push(AddResult {
                skill: s.install_name.clone(),
                agent: agent(a).display_name.to_string(),
                plugin_name: None,
                r,
            });
        }
    }
    spinner.stop("Installation complete");
    outln!();

    let successful: Vec<&AddResult> = results.iter().filter(|r| r.r.success).collect();
    let failed: Vec<&AddResult> = results.iter().filter(|r| !r.r.success).collect();
    let ok_names: Vec<String> = successful.iter().map(|r| r.skill.clone()).collect();

    let mut skill_files = Map::new();
    for s in &selected {
        skill_files.insert(s.install_name.clone(), Value::String(s.source_url.clone()));
    }

    if privacy.wait() != Some(true) {
        track(&[
            ("event", Some("install".into())),
            ("source", Some(source_identifier.clone())),
            (
                "skills",
                Some(
                    selected
                        .iter()
                        .map(|s| s.install_name.clone())
                        .collect::<Vec<_>>()
                        .join(","),
                ),
            ),
            ("agents", Some(target_agents.join(","))),
            (
                "global",
                if install_globally {
                    Some("1".into())
                } else {
                    None
                },
            ),
            (
                "skillFiles",
                Some(serde_json::to_string(&skill_files).unwrap()),
            ),
            ("installUrl", Some(url.to_string())),
            ("metadata", options.metadata.clone()),
            ("sourceType", Some("well-known".into())),
        ]);
    }

    if !successful.is_empty() && install_globally {
        for s in &selected {
            if ok_names.contains(&s.install_name) {
                let mut e = Map::new();
                e.insert("source".into(), Value::String(source_identifier.clone()));
                e.insert("sourceType".into(), Value::String("well-known".into()));
                e.insert("sourceUrl".into(), Value::String(s.source_url.clone()));
                e.insert("skillFolderHash".into(), Value::String(String::new()));
                e.insert("sourceBaseUrl".into(), Value::String(url.to_string()));
                e.insert(
                    "wellKnownDigest".into(),
                    Value::String(compute_well_known_skill_digest(s)),
                );
                let _ = add_skill_to_lock(&s.install_name, e);
            }
        }
    }

    if !successful.is_empty() && !install_globally {
        for s in &selected {
            if !ok_names.contains(&s.install_name) {
                continue;
            }
            let Some(m) = successful.iter().find(|r| r.skill == s.install_name) else {
                continue;
            };
            let dir =
                m.r.canonical_path
                    .clone()
                    .filter(|p| !p.is_empty())
                    .unwrap_or_else(|| m.r.path.clone());
            if dir.is_empty() {
                continue;
            }
            if let Ok(hash) = compute_skill_folder_hash(&dir) {
                let mut e = Map::new();
                e.insert("source".into(), Value::String(source_identifier.clone()));
                e.insert("sourceUrl".into(), Value::String(url.to_string()));
                e.insert("sourceType".into(), Value::String("well-known".into()));
                e.insert("computedHash".into(), Value::String(hash));
                e.insert(
                    "wellKnownDigest".into(),
                    Value::String(compute_well_known_skill_digest(s)),
                );
                let _ = add_skill_to_local_lock(&s.install_name, e, Some(&cwd));
            }
        }
    }

    if !successful.is_empty() {
        let mut by_skill: Vec<(String, Vec<&AddResult>)> = Vec::new();
        for r in &successful {
            match by_skill.iter_mut().find(|(k, _)| *k == r.skill) {
                Some((_, v)) => v.push(r),
                None => by_skill.push((r.skill.clone(), vec![r])),
            }
        }
        let symlink_failures: Vec<&&AddResult> = successful
            .iter()
            .filter(|r| r.r.mode == InstallMode::Symlink && r.r.symlink_failed)
            .collect();
        let mut lines: Vec<String> = Vec::new();
        for (name, rs) in &by_skill {
            let first = rs[0];
            if first.r.mode == InstallMode::Copy {
                lines.push(format!(
                    "{} {} {}",
                    pc::green("✓"),
                    name,
                    pc::dim("(copied)")
                ));
                let mut seen: Vec<String> = Vec::new();
                for r in rs {
                    let sp = shorten_path(&r.r.path, &cwd);
                    if !seen.contains(&sp) {
                        lines.push(format!("  {} {}", pc::dim("→"), sp));
                        seen.push(sp);
                    }
                }
            } else {
                match &first.r.canonical_path {
                    Some(c) if !c.is_empty() => {
                        lines.push(format!("{} {}", pc::green("✓"), shorten_path(c, &cwd)))
                    }
                    _ => lines.push(format!("{} {}", pc::green("✓"), name)),
                }
                lines.extend(build_result_lines(rs, &target_agents));
            }
        }
        let n = by_skill.len();
        ui::note(
            &lines.join("\n"),
            &pc::green(format!(
                "Installed {} skill{}",
                n,
                if n != 1 { "s" } else { "" }
            )),
        );
        if !symlink_failures.is_empty() {
            let agents_list: Vec<String> =
                symlink_failures.iter().map(|r| r.agent.clone()).collect();
            log::warn(&pc::yellow(format!(
                "Symlinks failed for: {}",
                format_list(&agents_list, 5)
            )));
            log::message(&pc::dim("  Files were copied instead. On Windows, enable Developer Mode for symlink support."));
        }
    }

    if !failed.is_empty() {
        outln!();
        log::error(&pc::red(format!("Failed to install {}", failed.len())));
        for r in &failed {
            log::message(&format!(
                "  {} {} → {}: {}",
                pc::red("✗"),
                r.skill,
                r.agent,
                pc::dim(r.r.error.clone().unwrap_or_default())
            ));
        }
    }

    outln!();
    ui::outro(&format!(
        "{}{}",
        pc::green("Done!"),
        pc::dim("  Review skills before use; they run with full agent permissions.")
    ));
    prompt_for_find_skills(options, &target_agents);
    true
}

// ─── JSON output state ───

struct JsonState {
    results: Vec<Value>,
    emitted: bool,
}

static JSON_STATE: Mutex<Option<JsonState>> = Mutex::new(None);

fn json_push(v: Value) {
    if let Some(s) = JSON_STATE.lock().unwrap().as_mut() {
        s.results.push(v);
    }
}

fn json_is_empty() -> bool {
    JSON_STATE
        .lock()
        .unwrap()
        .as_ref()
        .map(|s| s.results.is_empty())
        .unwrap_or(true)
}

fn emit_json() {
    let mut guard = JSON_STATE.lock().unwrap();
    let Some(state) = guard.as_mut() else { return };
    if state.emitted {
        return;
    }
    state.emitted = true;
    sys::set_stdout_redirect(false);
    sys::write_real_stdout(&format!(
        "{}\n",
        serde_json::to_string_pretty(&Value::Array(state.results.clone())).unwrap()
    ));
}

enum AddError {
    Clone(GitCloneError),
    Other(String),
}

impl From<String> for AddError {
    fn from(s: String) -> Self {
        AddError::Other(s)
    }
}

impl From<GitCloneError> for AddError {
    fn from(e: GitCloneError) -> Self {
        AddError::Clone(e)
    }
}

struct AddCtx {
    json: bool,
    temp_dir: Option<String>,
    install_tip_shown: bool,
}

impl AddCtx {
    fn cleanup(&mut self) {
        if let Some(t) = self.temp_dir.take() {
            let _ = cleanup_temp_dir(&t);
        }
    }

    /// Every exit path emits exactly one JSON array in json mode.
    fn exit(&mut self, code: i32, message: Option<&str>) -> ! {
        if self.json {
            if let Some(m) = message {
                errln!("{}", m);
            }
            if code != 0 && json_is_empty() {
                json_push(
                    json!({"status": "failed", "error": message.unwrap_or("Installation failed")}),
                );
            }
            emit_json();
        }
        sys::exit(code);
    }

    fn show_install_tip(&mut self) {
        if self.install_tip_shown {
            return;
        }
        log::message(&pc::dim(
            "Tip: use the --yes (-y) and --global (-g) flags to install without prompts.",
        ));
        self.install_tip_shown = true;
    }
}

fn kebab_to_title(s: &str) -> String {
    s.split('-')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

type Grouped<'a, T> = (Vec<(String, Vec<&'a T>)>, Vec<&'a T>);

fn group_by_plugin<T>(items: &[T], plugin: impl Fn(&T) -> Option<&str>) -> Grouped<'_, T> {
    let mut groups: Vec<(String, Vec<&T>)> = Vec::new();
    let mut ungrouped = Vec::new();
    for it in items {
        match plugin(it).filter(|p| !p.is_empty()) {
            Some(g) => match groups.iter_mut().find(|(k, _)| k == g) {
                Some((_, v)) => v.push(it),
                None => groups.push((g.to_string(), vec![it])),
            },
            None => ungrouped.push(it),
        }
    }
    groups.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
    (groups, ungrouped)
}

pub fn run_add(args: &[String], mut options: AddOptions) {
    crate::installer::reset_populated();
    let source = args.first().cloned();
    let json_mode = options.json;
    let mut ctx = AddCtx {
        json: json_mode,
        temp_dir: None,
        install_tip_shown: false,
    };

    if json_mode {
        *JSON_STATE.lock().unwrap() = Some(JsonState {
            results: Vec::new(),
            emitted: false,
        });
        sys::set_stdout_redirect(true);
        sys::on_exit(Box::new(emit_json));
    }

    let Some(source) = source else {
        outln!();
        outln!(
            "{} {}",
            pc::bg_red(pc::white(pc::bold(" ERROR "))),
            pc::red("Missing required argument: source")
        );
        outln!();
        outln!("{}", pc::dim("  Usage:"));
        outln!(
            "    {} {} {}",
            pc::cyan("skills add"),
            pc::yellow("<source>"),
            pc::dim("[options]")
        );
        outln!();
        outln!("{}", pc::dim("  Example:"));
        outln!(
            "    {} {}",
            pc::cyan("skills add"),
            pc::yellow("vercel-labs/agent-skills")
        );
        outln!();
        ctx.exit(1, Some("Missing required argument: source"));
    };

    let mut explicitly_selected: Vec<String> = match &options.agent {
        Some(a) if a.iter().any(|x| x == "*") => Vec::new(),
        Some(a) => a.clone(),
        None => Vec::new(),
    };

    if options.all {
        options.skill = Some(vec!["*".into()]);
        options.agent = Some(vec!["*".into()]);
        options.yes = true;
    }

    let agent_result = detect_agent();
    if agent_result.is_agent() {
        options.yes = true;
        if options.agent.as_ref().map(|a| a.is_empty()).unwrap_or(true) {
            if let Some(mapped) = get_agent_type(agent_result.name()) {
                options.agent = Some(
                    ensure_universal_agents(&[mapped])
                        .into_iter()
                        .map(|s| s.to_string())
                        .collect(),
                );
            }
        }
    }

    if json_mode && !options.yes {
        ctx.exit(
            1,
            Some("The --json flag requires --yes (or --all) to run non-interactively."),
        );
    }
    if json_mode && options.list {
        ctx.exit(1, Some("The --json flag cannot be combined with --list."));
    }

    outln!();
    if !agent_result.is_agent() {
        ui::intro(&pc::bg_cyan(pc::black(" skills ")));
    }
    if agent_result.is_agent() {
        log::info(&format!(
            "{} Agent detected — installing non-interactively",
            pc::bg_cyan(pc::black(pc::bold(format!(" {} ", agent_result.name()))))
        ));
    } else if !sys::stdin_is_tty() {
        ctx.show_install_tip();
    }

    let outcome = run_add_inner(
        &source,
        &mut options,
        &mut ctx,
        &mut explicitly_selected,
        agent_result.is_agent(),
    );
    if let Err(err) = outcome {
        match &err {
            AddError::Clone(e) => {
                log::error(&pc::red("Failed to clone repository"));
                for line in e.message.split('\n') {
                    log::message(&pc::dim(line));
                }
            }
            AddError::Other(m) => log::error(m),
        }
        ctx.show_install_tip();
        ui::outro(&pc::red("Installation failed"));
        let msg = match &err {
            AddError::Clone(e) => format!("Failed to clone repository\n{}", e.message),
            AddError::Other(m) => m.clone(),
        };
        ctx.cleanup();
        ctx.exit(1, Some(&msg));
    }
    if json_mode {
        sys::clear_exit_hooks();
        sys::set_stdout_redirect(false);
    }
    ctx.cleanup();
}

fn run_add_inner(
    source: &str,
    options: &mut AddOptions,
    ctx: &mut AddCtx,
    explicitly_selected: &mut Vec<String>,
    in_agent: bool,
) -> Result<(), AddError> {
    let json_mode = ctx.json;
    let mut effective_source = source.to_string();
    let mut notion_label: Option<String> = None;
    let notion_page = crate::notion::parse_notion_skill_url(source);
    if crate::notion::is_notion_source(source) || notion_page.is_some() {
        let prepared = match &notion_page {
            Some(id) => Some(crate::notion::prepare_notion_skill_source(id)?),
            None => crate::notion::prepare_notion_pack_source(
                options.yes,
                options.list,
                options.skill.as_deref(),
            )?,
        };
        let Some(prepared) = prepared else {
            return Ok(());
        };
        effective_source = prepared.root_dir.clone();
        ctx.temp_dir = Some(prepared.temp_dir.clone());
        notion_label = Some(match prepared.pack_count {
            Some(n) => format!(
                "{} selected Notion pack{}",
                n,
                if n == 1 { "" } else { "s" }
            ),
            None => "Notion page".into(),
        });
        options.skill = Some(vec!["*".into()]);
    }

    let mut spinner = if json_mode {
        ui::Spinner::inert()
    } else {
        ui::Spinner::new()
    };
    spinner.start("Parsing source…");
    let parsed: ParsedSource = parse_source(&effective_source)?;
    let mut direct_download = parsed.kind == "download" || notion_label.is_some();
    spinner.stop(&match &notion_label {
        Some(l) => format!("Source: {}", l),
        None => format!(
            "Source: {}{}{}{}",
            if parsed.kind == "local" {
                parsed.local_path.clone().unwrap_or_default()
            } else {
                parsed.url.clone()
            },
            parsed
                .r#ref
                .as_ref()
                .map(|r| format!(" @ {}", pc::yellow(r)))
                .unwrap_or_default(),
            parsed
                .subpath
                .as_ref()
                .map(|s| format!(" ({})", s))
                .unwrap_or_default(),
            parsed
                .skill_filter
                .as_ref()
                .map(|s| format!(" {}{}", pc::dim("@"), pc::cyan(s)))
                .unwrap_or_default()
        ),
    });

    let owner_repo_raw = if parsed.kind == "well-known" || parsed.kind == "download" {
        None
    } else {
        get_owner_repo(&parsed)
    };
    let privacy: Shared<Option<bool>> = match (
        parsed.kind.as_str(),
        owner_repo_raw.as_deref().and_then(parse_owner_repo),
    ) {
        ("github", Some((o, r))) => Shared::spawn(move || is_repo_private(&o, &r)),
        _ => Shared::ready(None),
    };

    if parsed.kind == "well-known" {
        if json_mode {
            ctx.exit(
                1,
                Some("--json is not yet supported for well-known skill sources."),
            );
        }
        if handle_well_known_skills(&parsed.url, options, &mut spinner) {
            return Ok(());
        }
        direct_download = true;
    }

    if let Some(filter) = &parsed.skill_filter {
        let list = options.skill.get_or_insert_with(Vec::new);
        if !list.contains(filter) {
            list.push(filter.clone());
        }
    }

    let include_internal = options
        .skill
        .as_ref()
        .map(|s| !s.is_empty() && !s.iter().any(|x| x == "*"))
        .unwrap_or(false);
    let discover_opts = DiscoverOptions {
        include_internal,
        full_depth: options.full_depth,
        include_duplicate_names: false,
    };

    let mut blob_result: Option<BlobInstallResult> = None;
    let skills: Vec<Skill>;

    if parsed.kind == "local" {
        let local = parsed.local_path.clone().unwrap_or_default();
        spinner.start("Validating local path…");
        if !std::path::Path::new(&local).exists() {
            spinner.stop(&pc::red("Path not found"));
            ui::outro(&pc::red(format!("Local path does not exist: {}", local)));
            ctx.exit(1, Some(&format!("Local path does not exist: {}", local)));
        }
        spinner.stop("Local path validated");
        spinner.start("Discovering skills…");
        skills = discover_skills(&local, parsed.subpath.as_deref(), discover_opts)?;
    } else if parsed.kind == "well-known" || parsed.kind == "download" {
        spinner.start("Downloading source...");
        let downloaded = download_source(&parsed.url, DownloadOptions::default())?;
        ctx.temp_dir = Some(downloaded.temp_dir.clone());
        spinner.stop(&format!(
            "Downloaded {}",
            if downloaded.kind == DownloadKind::SkillMd {
                "SKILL.md file"
            } else {
                "archive"
            }
        ));
        spinner.start("Discovering skills...");
        skills = discover_skills(
            &downloaded.root_dir,
            parsed.subpath.as_deref(),
            discover_opts,
        )?;
    } else if parsed.kind == "github" && !options.full_depth {
        let mut attempted = false;
        let owner_repo = get_owner_repo(&parsed);
        if let Some(or) = &owner_repo {
            let owner = or.split('/').next().unwrap_or("").to_lowercase();
            if !owner.is_empty()
                && (is_blob_allowed_repo(&or.to_lowercase())
                    || BLOB_ALLOWED_OWNERS.contains(&owner.as_str()))
            {
                attempted = true;
                spinner.start("Fetching skills…");
                blob_result = try_blob_install(
                    or,
                    &BlobOptions {
                        subpath: parsed.subpath.as_deref(),
                        skill_filter: parsed.skill_filter.as_deref(),
                        r#ref: parsed.r#ref.as_deref(),
                        use_token: true,
                        include_internal,
                    },
                );
            }
        }
        if let Some(b) = &blob_result {
            skills = b.skills.clone();
            spinner.stop(&format!(
                "Found {} skill{}",
                pc::green(skills.len().to_string()),
                if skills.len() > 1 { "s" } else { "" }
            ));
        } else {
            if attempted {
                spinner.message("Cloning repository…");
            } else {
                spinner.start("Cloning repository…");
            }
            let t = clone_repo(&parsed.url, parsed.r#ref.as_deref())?;
            ctx.temp_dir = Some(t.clone());
            spinner.stop("Repository cloned");
            spinner.start("Discovering skills…");
            skills = discover_skills(&t, parsed.subpath.as_deref(), discover_opts)?;
        }
    } else {
        spinner.start("Cloning repository…");
        let t = clone_repo(&parsed.url, parsed.r#ref.as_deref())?;
        ctx.temp_dir = Some(t.clone());
        spinner.stop("Repository cloned");
        spinner.start("Discovering skills…");
        skills = discover_skills(&t, parsed.subpath.as_deref(), discover_opts)?;
    }

    if skills.is_empty() {
        spinner.stop(&pc::red("No skills found"));
        ui::outro(&pc::red(
            "No valid skills found. Skills require a SKILL.md with name and description.",
        ));
        ctx.cleanup();
        ctx.exit(
            1,
            Some("No valid skills found. Skills require a SKILL.md with name and description."),
        );
    }
    if blob_result.is_none() {
        spinner.stop(&format!(
            "Found {} skill{}",
            pc::green(skills.len().to_string()),
            if skills.len() > 1 { "s" } else { "" }
        ));
    }

    if options.list {
        outln!();
        log::step(&pc::bold("Available Skills"));
        let (groups, ungrouped) = group_by_plugin(&skills, |s| s.plugin_name.as_deref());
        for (g, list) in &groups {
            outln!("{}", pc::bold(kebab_to_title(g)));
            for s in list {
                log::message(&format!("  {}", pc::cyan(get_skill_display_name(s))));
                log::message(&format!("    {}", pc::dim(&s.description)));
            }
            outln!();
        }
        if !ungrouped.is_empty() {
            if !groups.is_empty() {
                outln!("{}", pc::bold("General"));
            }
            for s in &ungrouped {
                log::message(&format!("  {}", pc::cyan(get_skill_display_name(s))));
                log::message(&format!("    {}", pc::dim(&s.description)));
            }
        }
        outln!();
        ui::outro("Use --skill <name> to install specific skills");
        ctx.cleanup();
        ctx.exit(0, None);
    }

    let log_chosen = |chosen: &[Skill]| {
        log_auto_selected_skills(
            &chosen
                .iter()
                .map(|s| (get_skill_display_name(s), s.description.clone()))
                .collect::<Vec<_>>(),
        );
    };

    let selected: Vec<Skill> = if options
        .skill
        .as_ref()
        .map(|s| s.iter().any(|x| x == "*"))
        .unwrap_or(false)
    {
        log_chosen(&skills);
        skills.clone()
    } else if let Some(names) = options.skill.clone().filter(|s| !s.is_empty()) {
        let sel = filter_skills(&skills, &names);
        if json_mode {
            for requested in &names {
                if filter_skills(&skills, std::slice::from_ref(requested)).is_empty() {
                    json_push(
                        json!({"name": requested, "status": "skipped", "reason": "No matching skill found in source"}),
                    );
                }
            }
        }
        if sel.is_empty() {
            log::error(&format!(
                "No matching skills found for: {}",
                names.join(", ")
            ));
            log::info("Available skills:");
            for s in &skills {
                log::message(&format!("  - {}", get_skill_display_name(s)));
            }
            ctx.cleanup();
            ctx.exit(
                1,
                Some(&format!(
                    "No matching skills found for: {}",
                    names.join(", ")
                )),
            );
        }
        log::info(&format!(
            "Selected {} skill{}: {}",
            sel.len(),
            if sel.len() != 1 { "s" } else { "" },
            sel.iter()
                .map(|s| pc::cyan(get_skill_display_name(s)))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        sel
    } else if skills.len() == 1 || options.yes {
        log_chosen(&skills);
        skills.clone()
    } else {
        let mut sorted: Vec<&Skill> = skills.iter().collect();
        sorted.sort_by(|a, b| match (&a.plugin_name, &b.plugin_name) {
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x != y => crate::collate::locale_compare(x, y),
            _ => crate::collate::locale_compare(
                &get_skill_display_name(a),
                &get_skill_display_name(b),
            ),
        });
        let has_groups = sorted.iter().any(|s| s.plugin_name.is_some());
        let items: Vec<SearchItem<usize>> = sorted
            .iter()
            .map(|s| {
                let idx = skills.iter().position(|x| std::ptr::eq(x, *s)).unwrap();
                let group = if has_groups {
                    Some(
                        s.plugin_name
                            .as_deref()
                            .map(kebab_to_title)
                            .unwrap_or_else(|| "Other".into()),
                    )
                } else {
                    None
                };
                SearchItem::new(idx, "[object Object]", &get_skill_display_name(s))
                    .group(group)
                    .detail(Some(s.description.clone()))
            })
            .collect();
        let message = if has_groups {
            format!("Select skills to install {}", pc::dim("(space to toggle)"))
        } else {
            "Select skills to install".to_string()
        };
        let mut o = Options::new(&message, items);
        o.required = true;
        o.max_visible = 20;
        o.searchable = !has_groups;
        o.show_detail = true;
        o.show_selected_summary = false;
        o.select_groups = has_groups;
        o.select_all = true;
        match search_multiselect(o) {
            Some(idx) => idx.into_iter().map(|i| skills[i].clone()).collect(),
            None => {
                ctx.cleanup();
                exit_installation_cancelled();
            }
        }
    };

    // Security audit only after GitHub positively confirmed the repo is public.
    let owner_repo_for_audit = get_owner_repo(&parsed);
    let audit: Shared<Option<AuditResponse>> = match &owner_repo_for_audit {
        Some(or) => {
            let or = or.clone();
            let p = privacy.clone();
            let names: Vec<String> = selected.iter().map(get_skill_display_name).collect();
            Shared::spawn(move || {
                if p.wait() == Some(false) {
                    fetch_audit_data(&or, &names, 3000)
                } else {
                    None
                }
            })
        }
        None => Shared::ready(None),
    };

    let mut target_agents: Vec<AgentType> = if options
        .agent
        .as_ref()
        .map(|a| a.iter().any(|x| x == "*"))
        .unwrap_or(false)
    {
        let all = agents::all_agent_names();
        log::info(&format!("Installing to all {} agents", all.len()));
        all
    } else if let Some(list) = options.agent.clone().filter(|l| !l.is_empty()) {
        match validate_agents(&list) {
            Ok(v) => v,
            Err(invalid) => {
                log::error(&format!("Invalid agents: {}", invalid.join(", ")));
                log::info(&format!(
                    "Valid agents: {}",
                    agents::all_agent_names().join(", ")
                ));
                ctx.cleanup();
                ctx.exit(1, Some(&format!("Invalid agents: {}", invalid.join(", "))));
            }
        }
    } else {
        spinner.start("Loading agents…");
        let installed = detect_installed_agents();
        spinner.stop(&format!("{} agents", agents::agents().len()));
        if installed.contains(&"eve") && (options.yes || !in_agent) {
            let refs: Vec<&Skill> = selected.iter().collect();
            let use_eve = if options.yes {
                Some(true)
            } else {
                ui::confirm(&format_eve_install_prompt_message(&refs), true)
            };
            match use_eve {
                None => {
                    ctx.cleanup();
                    exit_installation_cancelled();
                }
                Some(true) => {
                    if !options.yes {
                        explicitly_selected.push("eve".into());
                    }
                    log::info(&format!("Installing to: {}", pc::cyan(EVE_AGENT_LABEL)));
                    vec!["eve"]
                }
                Some(false) => match select_agents_interactive(options.global.unwrap_or(false)) {
                    Some(v) => {
                        explicitly_selected.extend(v.iter().map(|s| s.to_string()));
                        v
                    }
                    None => {
                        ctx.cleanup();
                        exit_installation_cancelled();
                    }
                },
            }
        } else if installed.is_empty() {
            if options.yes {
                log::info("Installing to all agents");
                agents::all_agent_names()
            } else {
                log::info("Select agents to install skills to");
                let choices: Vec<AgentType> = agents::all_agent_names()
                    .into_iter()
                    .filter(|a| *a != "eve")
                    .collect();
                match prompt_for_agents("Which agents do you want to install to?", choices) {
                    Some(v) => {
                        explicitly_selected.extend(v.iter().map(|s| s.to_string()));
                        v
                    }
                    None => {
                        ctx.cleanup();
                        exit_installation_cancelled();
                    }
                }
            }
        } else if installed.len() == 1 || options.yes {
            if installed.len() == 1 {
                log::info(&format!(
                    "Installing to: {}",
                    pc::cyan(agent(installed[0]).display_name)
                ));
            } else {
                log::info(&format!(
                    "Installing to: {}",
                    installed
                        .iter()
                        .map(|a| pc::cyan(agent(a).display_name))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            ensure_universal_agents(&installed)
        } else {
            match select_agents_interactive(options.global.unwrap_or(false)) {
                Some(v) => {
                    explicitly_selected.extend(v.iter().map(|s| s.to_string()));
                    v
                }
                None => {
                    ctx.cleanup();
                    exit_installation_cancelled();
                }
            }
        }
    };

    if options
        .subagent
        .as_ref()
        .map(|s| !s.is_empty())
        .unwrap_or(false)
    {
        explicitly_selected.push("eve".into());
        if !target_agents.contains(&"eve") {
            target_agents.push("eve");
        }
    }

    let mut eve_targets: Vec<Option<String>> = vec![None];
    if target_agents.contains(&"eve") {
        let available = get_eve_subagents(&sys::cwd());
        if let Some(subs) = options.subagent.as_ref().filter(|s| !s.is_empty()) {
            eve_targets = subs
                .iter()
                .map(|s| {
                    if s == "root" || s == "." {
                        None
                    } else {
                        Some(s.clone())
                    }
                })
                .collect();
        } else if !available.is_empty() && !options.yes {
            let mut choices = vec![SelectOption::new(
                String::new(),
                "Root agent",
                Some("agent/skills"),
            )];
            for name in &available {
                choices.push(SelectOption::new(
                    name.clone(),
                    name,
                    Some(&format!("agent/subagents/{}/skills", name)),
                ));
            }
            match ui::multiselect(
                "Where should Eve skills be installed?",
                &choices,
                &[String::new()],
                true,
            ) {
                Some(sel) => {
                    eve_targets = sel
                        .into_iter()
                        .map(|s| if s.is_empty() { None } else { Some(s) })
                        .collect()
                }
                None => {
                    ctx.cleanup();
                    exit_installation_cancelled();
                }
            }
        }
    }

    let targets = build_install_targets(&target_agents, &eve_targets);

    let mut install_globally = options.global.unwrap_or(false);
    let supports_global = target_agents
        .iter()
        .any(|a| agent(a).global_skills_dir.is_some());
    if options.global.is_none() && !options.yes && supports_global {
        let opts = vec![
            SelectOption::new(
                false,
                "Project",
                Some("Install in current directory (committed with your project)"),
            ),
            SelectOption::new(
                true,
                "Global",
                Some("Install in home directory (available across all projects)"),
            ),
        ];
        match ui::select("Installation scope", &opts, 0) {
            Some(v) => install_globally = v,
            None => {
                ctx.cleanup();
                exit_installation_cancelled();
            }
        }
    }

    let mut mode = if options.copy {
        InstallMode::Copy
    } else {
        InstallMode::Symlink
    };
    let all_eve = targets.iter().all(|t| t.agent == "eve");
    let unique_dirs: std::collections::HashSet<String> = targets
        .iter()
        .map(|t| match &t.subagent {
            Some(s) => format!("eve:subagent:{}", s),
            None => agent(t.agent).skills_dir.to_string(),
        })
        .collect();
    if !options.copy && !options.yes && unique_dirs.len() > 1 && !all_eve {
        match select_mode() {
            Some(m) => mode = m,
            None => {
                ctx.cleanup();
                exit_installation_cancelled();
            }
        }
    } else if unique_dirs.len() <= 1 || all_eve {
        mode = InstallMode::Copy;
    }

    let cwd = sys::cwd();
    let mut summary: Vec<String> = Vec::new();
    let (groups, ungrouped) = group_by_plugin(&selected, |s| s.plugin_name.as_deref());
    let print_summary = |list: &[&Skill], summary: &mut Vec<String>| {
        for s in list {
            if !summary.is_empty() {
                summary.push(String::new());
            }
            let canonical = if targets.len() == 1 {
                get_canonical_path(
                    &s.name,
                    install_globally,
                    None,
                    Some(targets[0].agent),
                    targets[0].subagent.as_deref(),
                )
            } else {
                get_canonical_path(&s.name, install_globally, None, None, None)
            }
            .unwrap_or_default();
            summary.push(pc::cyan(shorten_path(&canonical, &cwd)));
            summary.extend(build_target_summary_lines(&targets, mode));
            let overwrites: Vec<String> = targets
                .iter()
                .filter(|t| {
                    is_skill_installed(
                        &s.name,
                        t.agent,
                        install_globally,
                        None,
                        t.subagent.as_deref(),
                    )
                })
                .map(target_display_name)
                .collect();
            if !overwrites.is_empty() {
                summary.push(format!(
                    "  {} {}",
                    pc::yellow("overwrites:"),
                    format_list(&overwrites, 5)
                ));
            }
        }
    };
    for (g, list) in &groups {
        summary.push(String::new());
        summary.push(pc::bold(kebab_to_title(g)));
        print_summary(list, &mut summary);
    }
    if !ungrouped.is_empty() {
        if !groups.is_empty() {
            summary.push(String::new());
            summary.push(pc::bold("General"));
        }
        print_summary(&ungrouped, &mut summary);
    }
    outln!();
    ui::note(&summary.join("\n"), "Installation Summary");

    let audit_data = audit.wait();
    if let (Some(a), Some(or)) = (&audit_data, &owner_repo_for_audit) {
        let names: Vec<String> = selected.iter().map(get_skill_display_name).collect();
        let lines = build_security_lines(Some(a), &names, or);
        if !lines.is_empty() {
            ui::note(&lines.join("\n"), "Security Risk Assessments");
        }
    }

    if !options.yes {
        match ui::confirm("Proceed with installation?", true) {
            Some(true) => {}
            _ => {
                ctx.cleanup();
                exit_installation_cancelled();
            }
        }
    }

    spinner.start("Installing skills…");
    let mut results: Vec<AddResult> = Vec::new();
    for s in &selected {
        for t in &targets {
            let opts = InstallOptions {
                global: install_globally,
                cwd: None,
                mode: Some(mode),
                eve_subagent: t.subagent.clone(),
                create_missing_agent_root: explicitly_selected.iter().any(|a| a == t.agent),
            };
            let r = match (&blob_result, &s.blob) {
                (Some(_), Some(b)) => {
                    install_blob_skill_for_agent(&s.name, &b.files, t.agent, &opts)
                }
                _ => install_skill_for_agent(s, t.agent, &opts),
            };
            results.push(AddResult {
                skill: get_skill_display_name(s),
                agent: target_display_name(t),
                plugin_name: s.plugin_name.clone(),
                r,
            });
        }
    }
    spinner.stop("Installation complete");
    outln!();

    let successful: Vec<&AddResult> = results.iter().filter(|r| r.r.success).collect();
    let failed: Vec<&AddResult> = results.iter().filter(|r| !r.r.success).collect();
    let ok_names: Vec<String> = successful.iter().map(|r| r.skill.clone()).collect();

    let temp_dir = ctx.temp_dir.clone();
    let mut skill_files = Map::new();
    for s in &selected {
        if let (Some(_), Some(b)) = (&blob_result, &s.blob) {
            skill_files.insert(s.name.clone(), Value::String(b.repo_path.clone()));
        } else if let Some(t) = temp_dir.as_ref().filter(|t| s.path == **t) {
            let _ = t;
            skill_files.insert(s.name.clone(), Value::String("SKILL.md".into()));
        } else if let Some(t) = temp_dir
            .as_ref()
            .filter(|t| s.path.starts_with(&format!("{}{}", t, paths::SEP)))
        {
            let rel = s.path[t.len() + 1..]
                .split(paths::SEP)
                .collect::<Vec<_>>()
                .join("/");
            skill_files.insert(s.name.clone(), Value::String(format!("{}/SKILL.md", rel)));
        }
    }

    let normalized_source = if direct_download {
        None
    } else {
        get_owner_repo(&parsed)
    };
    let lock_source = if direct_download {
        None
    } else {
        get_lock_source(&parsed.url, normalized_source.as_deref())
    };
    let project_lock_source_url = if direct_download {
        None
    } else {
        get_project_lock_source_url(&parsed.kind, &parsed.url)
    };

    if let Some(ns) = &normalized_source {
        let event = || {
            track(&[
                ("event", Some("install".into())),
                ("source", Some(ns.clone())),
                (
                    "skills",
                    Some(
                        selected
                            .iter()
                            .map(|s| s.name.clone())
                            .collect::<Vec<_>>()
                            .join(","),
                    ),
                ),
                ("agents", Some(target_agents.join(","))),
                (
                    "global",
                    if install_globally {
                        Some("1".into())
                    } else {
                        None
                    },
                ),
                (
                    "skillFiles",
                    Some(serde_json::to_string(&skill_files).unwrap()),
                ),
                ("metadata", options.metadata.clone()),
            ]);
        };
        if parse_owner_repo(ns).is_some() {
            if privacy.wait() == Some(false) {
                event();
            }
        } else {
            event();
        }
    }

    let mut hashes: Vec<(String, String)> = Vec::new();
    if !successful.is_empty() && (json_mode || !install_globally) {
        for s in &selected {
            let name = get_skill_display_name(s);
            if !ok_names.contains(&name) {
                continue;
            }
            let h = match (&blob_result, &s.blob) {
                (Some(_), Some(b)) => Some(b.snapshot_hash.clone()),
                _ => compute_skill_folder_hash(&s.path).ok(),
            };
            if let Some(h) = h {
                hashes.push((name, h));
            }
        }
    }
    let hash_of = |name: &str| {
        hashes
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, h)| h.clone())
    };

    if !successful.is_empty() && install_globally {
        if let Some(ns) = &normalized_source {
            let cached_tree = if parsed.kind == "github" && blob_result.is_none() {
                fetch_repo_tree(ns, parsed.r#ref.as_deref(), true)
            } else {
                None
            };
            for s in &selected {
                let name = get_skill_display_name(s);
                if !ok_names.contains(&name) {
                    continue;
                }
                let skill_path = skill_files
                    .get(&s.name)
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let mut folder_hash = String::new();
                if let (Some(b), Some(sp)) = (&blob_result, &skill_path) {
                    if let Some(h) = get_skill_folder_hash_from_tree(&b.tree, sp) {
                        folder_hash = h;
                    }
                } else if let (true, Some(sp), Some(tree)) =
                    (parsed.kind == "github", &skill_path, &cached_tree)
                {
                    if let Some(h) = get_skill_folder_hash_from_tree(tree, sp) {
                        folder_hash = h;
                    }
                } else if let (Some(sp), Some(t)) = (&skill_path, &temp_dir) {
                    let dir = join(&[t.as_str(), paths::dirname(sp).as_str()]);
                    if let Ok(h) = compute_skill_folder_hash(&dir) {
                        folder_hash = h;
                    }
                }
                let mut e = Map::new();
                e.insert(
                    "source".into(),
                    Value::String(lock_source.clone().unwrap_or_else(|| ns.clone())),
                );
                e.insert("sourceType".into(), Value::String(parsed.kind.clone()));
                e.insert("sourceUrl".into(), Value::String(parsed.url.clone()));
                if let Some(r) = &parsed.r#ref {
                    e.insert("ref".into(), Value::String(r.clone()));
                }
                if let Some(sp) = &skill_path {
                    e.insert("skillPath".into(), Value::String(sp.clone()));
                }
                e.insert("skillFolderHash".into(), Value::String(folder_hash));
                if let Some(p) = &s.plugin_name {
                    e.insert("pluginName".into(), Value::String(p.clone()));
                }
                let _ = add_skill_to_lock(&s.name, e);
            }
        }
    }

    if !successful.is_empty() && !install_globally && !direct_download {
        let eve_subagents: Option<Vec<String>> = if target_agents.contains(&"eve") {
            Some(
                eve_targets
                    .iter()
                    .map(|s| s.clone().unwrap_or_default())
                    .collect(),
            )
        } else {
            None
        };
        let record = eve_subagents
            .as_ref()
            .map(|v| v.len() > 1 || v.iter().any(|s| !s.is_empty()))
            .unwrap_or(false);
        for s in &selected {
            let name = get_skill_display_name(s);
            if !ok_names.contains(&name) {
                continue;
            }
            let Some(h) = hash_of(&name) else { continue };
            let skill_path = skill_files
                .get(&s.name)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            let mut e = Map::new();
            e.insert(
                "source".into(),
                Value::String(
                    lock_source
                        .clone()
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| parsed.url.clone()),
                ),
            );
            if let Some(u) = &project_lock_source_url {
                e.insert("sourceUrl".into(), Value::String(u.clone()));
            }
            if let Some(r) = &parsed.r#ref {
                e.insert("ref".into(), Value::String(r.clone()));
            }
            e.insert("sourceType".into(), Value::String(parsed.kind.clone()));
            if let Some(sp) = skill_path {
                e.insert("skillPath".into(), Value::String(sp));
            }
            e.insert("computedHash".into(), Value::String(h));
            if record {
                e.insert("subagents".into(), json!(eve_subagents.clone().unwrap()));
            }
            let _ = add_skill_to_local_lock(&s.name, e, Some(&cwd));
        }
    }

    if json_mode {
        let json_source = normalized_source.clone().unwrap_or_else(|| {
            if parsed.kind == "local" {
                parsed.local_path.clone().unwrap_or_default()
            } else {
                parsed.url.clone()
            }
        });
        for s in &selected {
            let name = get_skill_display_name(s);
            let rs: Vec<&AddResult> = results.iter().filter(|r| r.skill == name).collect();
            let fails: Vec<&&AddResult> = rs.iter().filter(|r| !r.r.success).collect();
            if !fails.is_empty() {
                json_push(
                    json!({"name": name, "status": "failed", "error": fails[0].r.error.clone().unwrap_or_else(|| "Installation failed".into())}),
                );
                continue;
            }
            let mut o = Map::new();
            o.insert("name".into(), Value::String(name.clone()));
            o.insert("status".into(), Value::String("installed".into()));
            o.insert("source".into(), Value::String(json_source.clone()));
            o.insert(
                "ref".into(),
                parsed
                    .r#ref
                    .clone()
                    .map(Value::String)
                    .unwrap_or(Value::Null),
            );
            o.insert(
                "hash".into(),
                hash_of(&name).map(Value::String).unwrap_or(Value::Null),
            );
            if let Some(first) = rs.first() {
                let p = first
                    .r
                    .canonical_path
                    .clone()
                    .unwrap_or_else(|| first.r.path.clone());
                o.insert("path".into(), Value::String(p));
            }
            o.insert(
                "scope".into(),
                Value::String(
                    if install_globally {
                        "global"
                    } else {
                        "project"
                    }
                    .into(),
                ),
            );
            o.insert(
                "agents".into(),
                json!(rs
                    .iter()
                    .filter(|r| !r.r.skipped)
                    .map(|r| r.agent.clone())
                    .collect::<Vec<_>>()),
            );
            o.insert(
                "mode".into(),
                Value::String(rs.first().map(|r| r.r.mode).unwrap_or(mode).as_str().into()),
            );
            o.insert(
                "security".into(),
                build_json_security(audit_data.as_ref(), &name, owner_repo_for_audit.as_deref()),
            );
            json_push(Value::Object(o));
        }
        emit_json();
        let any_skipped = JSON_STATE
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| {
                s.results
                    .iter()
                    .any(|r| r.get("status") == Some(&json!("skipped")))
            })
            .unwrap_or(false);
        if !failed.is_empty() || any_skipped {
            sys::set_exit_code(1);
        }
        return Ok(());
    }

    if !successful.is_empty() {
        let mut by_skill: Vec<(String, Vec<&AddResult>)> = Vec::new();
        for r in &successful {
            match by_skill.iter_mut().find(|(k, _)| *k == r.skill) {
                Some((_, v)) => v.push(r),
                None => by_skill.push((r.skill.clone(), vec![r])),
            }
        }
        let firsts: Vec<&AddResult> = by_skill.iter().map(|(_, v)| v[0]).collect();
        let (rgroups, rungrouped) = group_by_plugin(&firsts, |r| r.plugin_name.as_deref());
        let symlink_failures: Vec<&&AddResult> = successful
            .iter()
            .filter(|r| r.r.mode == InstallMode::Symlink && r.r.symlink_failed)
            .collect();
        let mut lines: Vec<String> = Vec::new();
        let print_results = |entries: &[&&AddResult], lines: &mut Vec<String>| {
            for entry in entries {
                let rs = &by_skill.iter().find(|(k, _)| *k == entry.skill).unwrap().1;
                let first = rs[0];
                if first.r.mode == InstallMode::Copy {
                    lines.push(format!(
                        "{} {} {}",
                        pc::green("✓"),
                        entry.skill,
                        pc::dim("(copied)")
                    ));
                    let mut seen: Vec<String> = Vec::new();
                    for r in rs {
                        let sp = shorten_path(&r.r.path, &cwd);
                        if !seen.contains(&sp) {
                            lines.push(format!("  {} {}", pc::dim("→"), sp));
                            seen.push(sp);
                        }
                    }
                } else {
                    match &first.r.canonical_path {
                        Some(c) if !c.is_empty() => {
                            lines.push(format!("{} {}", pc::green("✓"), shorten_path(c, &cwd)))
                        }
                        _ => lines.push(format!("{} {}", pc::green("✓"), entry.skill)),
                    }
                    lines.extend(build_result_lines(rs, &target_agents));
                }
            }
        };
        for (g, entries) in &rgroups {
            lines.push(String::new());
            lines.push(pc::bold(kebab_to_title(g)));
            print_results(entries, &mut lines);
        }
        if !rungrouped.is_empty() {
            if !rgroups.is_empty() {
                lines.push(String::new());
                lines.push(pc::bold("General"));
            }
            print_results(&rungrouped, &mut lines);
        }
        let n = by_skill.len();
        ui::note(
            &lines.join("\n"),
            &pc::green(format!(
                "Installed {} skill{}",
                n,
                if n != 1 { "s" } else { "" }
            )),
        );
        if !symlink_failures.is_empty() {
            let list: Vec<String> = symlink_failures.iter().map(|r| r.agent.clone()).collect();
            log::warn(&pc::yellow(format!(
                "Symlinks failed for: {}",
                format_list(&list, 5)
            )));
            log::message(&pc::dim("  Files were copied instead. On Windows, enable Developer Mode for symlink support."));
        }
    }

    if !failed.is_empty() {
        outln!();
        log::error(&pc::red(format!("Failed to install {}", failed.len())));
        for r in &failed {
            log::message(&format!(
                "  {} {} → {}: {}",
                pc::red("✗"),
                r.skill,
                r.agent,
                pc::dim(r.r.error.clone().unwrap_or_default())
            ));
        }
    }

    outln!();
    ui::outro(&format!(
        "{}{}",
        pc::green("Done!"),
        pc::dim("  Review skills before use; they run with full agent permissions.")
    ));
    ctx.cleanup();
    prompt_for_find_skills(options, &target_agents);
    Ok(())
}

/// One-time prompt to install the find-skills skill after an install.
fn prompt_for_find_skills(options: &AddOptions, target_agents: &[AgentType]) {
    if !sys::stdin_is_tty() || options.yes {
        return;
    }
    if is_prompt_dismissed("findSkillsPrompt") {
        return;
    }
    if is_skill_installed("find-skills", "claude-code", true, None, None) {
        let _ = dismiss_prompt("findSkillsPrompt");
        return;
    }
    outln!();
    log::message(&pc::dim(
        "One-time prompt - you won't be asked again if you dismiss.",
    ));
    let answer = ui::confirm(
        &format!(
            "Install the {} skill? It helps your agent discover and suggest skills.",
            pc::cyan("find-skills")
        ),
        true,
    );
    match answer {
        None => {
            let _ = dismiss_prompt("findSkillsPrompt");
        }
        Some(true) => {
            let _ = dismiss_prompt("findSkillsPrompt");
            let agents: Vec<String> = target_agents
                .iter()
                .filter(|a| **a != "replit")
                .map(|a| a.to_string())
                .collect();
            if agents.is_empty() {
                return;
            }
            outln!();
            log::step("Installing find-skills skill…");
            run_add(
                &["vercel-labs/skills".to_string()],
                AddOptions {
                    skill: Some(vec!["find-skills".into()]),
                    global: Some(true),
                    yes: true,
                    agent: Some(agents),
                    ..Default::default()
                },
            );
        }
        Some(false) => {
            let _ = dismiss_prompt("findSkillsPrompt");
            log::message(&pc::dim(
                "You can install it later with: skills add vercel-labs/skills@find-skills",
            ));
        }
    }
}

/// Parse `add` arguments. Returns (sources, options, errors).
pub fn parse_add_options(args: &[String]) -> (Vec<String>, AddOptions, Vec<String>) {
    let mut o = AddOptions::default();
    let mut source = Vec::new();
    let mut errors = Vec::new();
    let collect = |i: &mut usize, list: &mut Vec<String>| {
        while *i + 1 < args.len() && !args[*i + 1].is_empty() && !args[*i + 1].starts_with('-') {
            *i += 1;
            list.push(args[*i].clone());
        }
    };
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-g" | "--global" => o.global = Some(true),
            "-y" | "--yes" => o.yes = true,
            "-l" | "--list" => o.list = true,
            "--all" => o.all = true,
            "-a" | "--agent" => collect(&mut i, o.agent.get_or_insert_with(Vec::new)),
            "-s" | "--skill" => collect(&mut i, o.skill.get_or_insert_with(Vec::new)),
            "--metadata" => {
                i += 1;
                match args.get(i) {
                    None => errors.push("--metadata requires a JSON value".to_string()),
                    Some(m) => {
                        if serde_json::from_str::<Value>(m).is_ok() {
                            o.metadata = Some(m.clone());
                        } else {
                            errors.push("--metadata must be valid JSON".to_string());
                        }
                    }
                }
            }
            "--full-depth" => o.full_depth = true,
            "--json" => o.json = true,
            "--copy" => o.copy = true,
            "--subagent" => collect(&mut i, o.subagent.get_or_insert_with(Vec::new)),
            _ => {
                if !a.is_empty() && !a.starts_with('-') {
                    source.push(a.to_string());
                }
            }
        }
        i += 1;
    }
    (source, o, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parses_add_options() {
        let (src, o, errs) = parse_add_options(&s(&[
            "owner/repo",
            "-a",
            "claude-code",
            "cursor",
            "-s",
            "x",
            "-g",
            "-y",
            "--copy",
            "--json",
            "--metadata",
            "{\"a\":1}",
        ]));
        assert_eq!(src, s(&["owner/repo"]));
        assert_eq!(o.agent.unwrap(), s(&["claude-code", "cursor"]));
        assert_eq!(o.skill.unwrap(), s(&["x"]));
        assert_eq!(o.global, Some(true));
        assert!(o.yes && o.copy && o.json);
        assert_eq!(o.metadata.as_deref(), Some("{\"a\":1}"));
        assert!(errs.is_empty());
        let (_, _, errs) = parse_add_options(&s(&["x", "--metadata", "{bad"]));
        assert_eq!(errs, s(&["--metadata must be valid JSON"]));
        let (_, _, errs) = parse_add_options(&s(&["x", "--metadata"]));
        assert_eq!(errs, s(&["--metadata requires a JSON value"]));
    }

    #[test]
    fn lock_sources() {
        assert_eq!(
            get_lock_source("git@github.com:o/r.git", Some("o/r")).as_deref(),
            Some("git@github.com:o/r.git")
        );
        assert_eq!(
            get_lock_source("https://gitlab.com/o/r.git", Some("o/r")).as_deref(),
            Some("https://gitlab.com/o/r.git")
        );
        assert_eq!(
            get_lock_source("https://github.com/o/r.git", Some("o/r")).as_deref(),
            Some("o/r")
        );
        assert_eq!(
            get_project_lock_source_url("gitlab", "u").as_deref(),
            Some("u")
        );
        assert_eq!(get_project_lock_source_url("github", "u"), None);
    }

    #[test]
    fn eve_prompt_message() {
        let a = Skill {
            name: "a".into(),
            ..Default::default()
        };
        let msg = format_eve_install_prompt_message(&[&a]);
        assert!(strip_terminal_escapes(&msg).contains("Install a for your eve agent to use?"));
    }
}
