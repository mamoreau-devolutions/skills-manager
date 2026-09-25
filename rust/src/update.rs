//! `skills update` / `check` / `upgrade` (port of update.ts).
//!
//! Changed skills are reinstalled by invoking this executable's own `add`
//! command (the TS CLI runs `node <repo>/bin/cli.mjs add ...`), never through
//! a shell.

use crate::agents::{agents, is_universal_agent};
use crate::blob::{fetch_repo_tree, get_skill_folder_hash_from_tree};
use crate::color::ansi::{BOLD, DIM, RESET, TEXT};
use crate::git::{cleanup_temp_dir, clone_repo, get_git_tree_hash};
use crate::local_lock::{compute_skill_folder_hash, read_local_lock};
use crate::paths::{self, join};
use crate::remove::{remove_command, RemoveOptions};
use crate::sanitize::sanitize_metadata;
use crate::skill_lock::read_skill_lock;
use crate::skills::{discover_skills, DiscoverOptions};
use crate::telemetry::track;
use crate::ui::{self, SelectOption};
use crate::update_source::{
    build_local_clone_source, build_local_update_source, build_update_install_source,
    format_source_input, should_use_full_depth_for_update, UpdateSourceEntry,
};
use crate::wellknown::{self, compute_well_known_skill_digest, NormalizedEntry};
use crate::{out, outln, sys};
use serde_json::{Map, Value};
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum UpdateScope {
    Project,
    Global,
    Both,
}

impl UpdateScope {
    fn as_str(&self) -> &'static str {
        match self {
            UpdateScope::Project => "project",
            UpdateScope::Global => "global",
            UpdateScope::Both => "both",
        }
    }
}

#[derive(Default, Clone, Debug)]
pub struct UpdateOptions {
    pub global: bool,
    pub project: bool,
    pub yes: bool,
    pub skills: Option<Vec<String>>,
}

pub fn parse_update_options(args: &[String]) -> UpdateOptions {
    let mut o = UpdateOptions::default();
    let mut positional = Vec::new();
    for a in args {
        match a.as_str() {
            "-g" | "--global" => o.global = true,
            "-p" | "--project" => o.project = true,
            "-y" | "--yes" => o.yes = true,
            _ => {
                if !a.starts_with('-') {
                    positional.push(a.clone());
                }
            }
        }
    }
    if !positional.is_empty() {
        o.skills = Some(positional);
    }
    o
}

/// Whether cwd has project skills (a lock file or a skill in .agents/skills).
pub fn has_project_skills(cwd: Option<&str>) -> bool {
    let dir = cwd.map(|s| s.to_string()).unwrap_or_else(sys::cwd);
    if std::path::Path::new(&join(&[dir.as_str(), "skills-lock.json"])).exists() {
        return true;
    }
    let skills_dir = join(&[dir.as_str(), ".agents", "skills"]);
    if let Ok(entries) = std::fs::read_dir(&skills_dir) {
        for e in entries.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false)
                && std::path::Path::new(&join(&[
                    skills_dir.as_str(),
                    e.file_name().to_string_lossy().as_ref(),
                    "SKILL.md",
                ]))
                .exists()
            {
                return true;
            }
        }
    }
    false
}

pub fn resolve_update_scope(o: &UpdateOptions) -> UpdateScope {
    if o.skills.as_ref().map(|s| !s.is_empty()).unwrap_or(false) {
        if o.global {
            return UpdateScope::Global;
        }
        if o.project {
            return UpdateScope::Project;
        }
        return UpdateScope::Both;
    }
    if o.global && o.project {
        return UpdateScope::Both;
    }
    if o.global {
        return UpdateScope::Global;
    }
    if o.project {
        return UpdateScope::Project;
    }
    if o.yes || !sys::stdin_is_tty() {
        return if has_project_skills(None) {
            UpdateScope::Project
        } else {
            UpdateScope::Global
        };
    }
    let options = vec![
        SelectOption::new(
            UpdateScope::Project,
            "Project",
            Some("Update skills in current directory"),
        ),
        SelectOption::new(
            UpdateScope::Global,
            "Global",
            Some("Update skills in home directory"),
        ),
        SelectOption::new(UpdateScope::Both, "Both", Some("Update all skills")),
    ];
    match ui::select("Update scope", &options, 0) {
        Some(s) => s,
        None => {
            ui::cancel("Cancelled");
            sys::exit(0);
        }
    }
}

pub fn matches_skill_filter(name: &str, filter: &Option<Vec<String>>) -> bool {
    match filter {
        None => true,
        Some(f) if f.is_empty() => true,
        Some(f) => f.iter().any(|x| x.to_lowercase() == name.to_lowercase()),
    }
}

#[derive(Clone, Debug)]
pub struct SkippedSkill {
    pub name: String,
    pub reason: String,
    pub source_url: String,
    pub source_type: String,
    pub r#ref: Option<String>,
}

fn s(e: &Value, k: &str) -> Option<String> {
    e.get(k).and_then(|v| v.as_str()).map(|s| s.to_string())
}

fn nonempty(e: &Value, k: &str) -> Option<String> {
    s(e, k).filter(|x| !x.is_empty())
}

pub fn get_skip_reason(e: &Value) -> &'static str {
    match s(e, "sourceType").as_deref() {
        Some("local") => "Local path",
        Some("git") => "Git URL",
        Some("well-known") => "Well-known skill",
        _ => {
            if nonempty(e, "skillFolderHash").is_none() {
                "Private or deleted repo"
            } else if nonempty(e, "skillPath").is_none() {
                "No skill path recorded"
            } else {
                "No version tracking"
            }
        }
    }
}

pub fn get_install_source(skill: &SkippedSkill) -> String {
    let mut url = skill.source_url.clone();
    if skill.source_type == "well-known" {
        if let Some(i) = url.find("/.well-known/") {
            url.truncate(i);
        }
    }
    format_source_input(&url, skill.r#ref.as_deref())
}

pub fn print_skipped_skills(skipped: &[SkippedSkill]) {
    if skipped.is_empty() {
        return;
    }
    outln!();
    outln!(
        "{}{} skill(s) cannot be checked automatically:{}",
        DIM,
        skipped.len(),
        RESET
    );
    let mut grouped: Vec<(String, Vec<&SkippedSkill>)> = Vec::new();
    for sk in skipped {
        let src = get_install_source(sk);
        match grouped.iter_mut().find(|(k, _)| *k == src) {
            Some((_, v)) => v.push(sk),
            None => grouped.push((src, vec![sk])),
        }
    }
    for (source, skills) in grouped {
        if skills.len() == 1 {
            outln!(
                "  {}•{} {} {}({}){}",
                TEXT,
                RESET,
                sanitize_metadata(&skills[0].name),
                DIM,
                skills[0].reason,
                RESET
            );
        } else {
            let names = skills
                .iter()
                .map(|x| sanitize_metadata(&x.name))
                .collect::<Vec<_>>()
                .join(", ");
            outln!(
                "  {}•{} {} {}({}){}",
                TEXT,
                RESET,
                names,
                DIM,
                skills[0].reason,
                RESET
            );
        }
        outln!(
            "    {}To update: {}skills add {} -g -y{}",
            DIM,
            TEXT,
            source,
            RESET
        );
    }
}

struct ProjectSkill {
    name: String,
    entry: Value,
}

fn get_project_skills_for_update(filter: &Option<Vec<String>>) -> Vec<ProjectSkill> {
    let lock = read_local_lock(None);
    let mut out = Vec::new();
    for (name, entry) in &lock.skills {
        if !matches_skill_filter(name, filter) {
            continue;
        }
        if matches!(
            s(entry, "sourceType").as_deref(),
            Some("node_modules") | Some("local")
        ) {
            continue;
        }
        out.push(ProjectSkill {
            name: name.clone(),
            entry: entry.clone(),
        });
    }
    out
}

fn prompt_deletions(source: &str, deleted: &[String], is_global: bool, o: &UpdateOptions) {
    if deleted.is_empty() {
        return;
    }
    outln!();
    outln!(
        "{}Warning:{} The following skills from {}{}{} appear to have been deleted upstream:",
        DIM,
        RESET,
        DIM,
        source,
        RESET
    );
    for d in deleted {
        outln!("  {}•{} {}", DIM, RESET, d);
    }
    if o.yes || !sys::stdin_is_tty() {
        outln!("{}Skipping deletion in non-interactive mode.{}", DIM, RESET);
        return;
    }
    if ui::confirm(
        "Would you like to remove the local copies of these deleted skills?",
        true,
    ) == Some(true)
    {
        for d in deleted {
            outln!("{}Removing{} {}…", DIM, RESET, d);
            remove_command(
                vec![d.clone()],
                RemoveOptions {
                    yes: true,
                    global: is_global,
                    ..Default::default()
                },
            );
        }
    }
}

struct Resolution {
    deleted: Vec<String>,
    resolved: HashMap<String, String>,
}

fn check_and_prompt_for_deletions(
    source: &str,
    locked_names: &[String],
    lock_skills: &Map<String, Value>,
    is_global: bool,
    o: &UpdateOptions,
    discovered: &[crate::skill_relocation::DiscoveredSkillLocation],
) -> Resolution {
    let r = crate::skill_relocation::resolve_skill_locations(locked_names, lock_skills, discovered);
    if !r.ambiguous_skills.is_empty() {
        outln!();
        outln!(
            "{}Warning:{} Multiple current paths match these skills from {}{}{}; skipping them rather than deleting or migrating the wrong skill:",
            DIM,
            RESET,
            DIM,
            source,
            RESET
        );
        for n in &r.ambiguous_skills {
            outln!("  {}•{} {}", DIM, RESET, sanitize_metadata(n));
        }
    }
    prompt_deletions(source, &r.deleted_skills, is_global, o);
    Resolution {
        deleted: r.deleted_skills,
        resolved: r.resolved_paths,
    }
}

#[derive(Clone)]
struct WellKnownItem {
    name: String,
    digest: String,
    subagents: Option<Vec<String>>,
}

enum WellKnownCheck {
    Error,
    Current {
        new_skills: Vec<String>,
    },
    Changed {
        changed: Vec<String>,
        removed: Vec<String>,
        new_skills: Vec<String>,
    },
}

fn check_well_known(base_url: &str, items: &[WellKnownItem]) -> WellKnownCheck {
    let Some(index) = wellknown::fetch_index(base_url, true) else {
        return WellKnownCheck::Error;
    };
    let by_name: HashMap<&str, &NormalizedEntry> =
        index.entries.iter().map(|e| (e.name(), e)).collect();
    let removed: Vec<String> = items
        .iter()
        .filter(|i| !by_name.contains_key(i.name.as_str()))
        .map(|i| i.name.clone())
        .collect();
    let local: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
    let new_skills: Vec<String> = index
        .entries
        .iter()
        .map(|e| e.name().to_string())
        .filter(|n| !local.contains(&n.as_str()))
        .collect();
    let mut changed = Vec::new();
    let mut needs_content: Vec<&WellKnownItem> = Vec::new();
    for item in items {
        let Some(entry) = by_name.get(item.name.as_str()) else {
            continue;
        };
        match entry {
            NormalizedEntry::V2 { digest, .. } => {
                if item.digest.is_empty() || *digest != item.digest {
                    changed.push(item.name.clone());
                }
            }
            NormalizedEntry::V1 { .. } => needs_content.push(item),
        }
    }
    if !needs_content.is_empty() {
        let tracked: Vec<&str> = needs_content.iter().map(|i| i.name.as_str()).collect();
        let entries: Vec<NormalizedEntry> = index
            .entries
            .iter()
            .filter(|e| tracked.contains(&e.name()))
            .cloned()
            .collect();
        let skills: Vec<_> = crate::blob::parallel_map(&entries, wellknown::fetch_skill_by_entry)
            .into_iter()
            .flatten()
            .collect();
        if skills.is_empty() {
            return WellKnownCheck::Error;
        }
        let digests: HashMap<String, String> = skills
            .iter()
            .map(|s| (s.install_name.clone(), compute_well_known_skill_digest(s)))
            .collect();
        for item in needs_content {
            match digests.get(&item.name) {
                Some(d) if !item.digest.is_empty() && *d == item.digest => {}
                _ => changed.push(item.name.clone()),
            }
        }
    }
    if changed.is_empty() && removed.is_empty() {
        return WellKnownCheck::Current { new_skills };
    }
    WellKnownCheck::Changed {
        changed,
        removed,
        new_skills,
    }
}

fn print_new_skills(base_url: &str, new_skills: &[String], is_global: bool) {
    if new_skills.is_empty() {
        return;
    }
    let names: Vec<String> = new_skills.iter().map(|n| sanitize_metadata(n)).collect();
    outln!(
        "  {}{} new skill(s) available from this source:{} {}",
        DIM,
        new_skills.len(),
        RESET,
        names.join(", ")
    );
    outln!(
        "    {}To install: {}skills add {} --skill {}{}{}",
        DIM,
        TEXT,
        base_url,
        names.join(" "),
        if is_global { " -g" } else { "" },
        RESET
    );
}

fn cli_entry() -> Option<String> {
    std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().to_string())
        .filter(|p| std::path::Path::new(p).exists())
}

/// Reinstall via `<self> add ...` with stdin inherited and output captured.
fn spawn_add(args: &[String], github_pin: bool) -> bool {
    let Some(exe) = cli_entry() else { return false };
    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let mut cmd = crate::proc::command(&exe, &refs);
    if github_pin {
        cmd = cmd.env("GH_HOST", "github.com");
    }
    matches!(cmd.output_inherit_stdin(), Ok(o) if o.success())
}

fn process_well_known_updates(
    groups: &[(String, Vec<WellKnownItem>)],
    is_global: bool,
    o: &UpdateOptions,
) -> (usize, usize, bool) {
    let (mut ok, mut fail, mut changed_any) = (0, 0, false);
    for (base_url, items) in groups {
        out!(
            "\r{}Checking skills from source: {}{}\x1b[K\n",
            DIM,
            base_url,
            RESET
        );
        match check_well_known(base_url, items) {
            WellKnownCheck::Error => {
                outln!(
                    "  {}✗ Failed to check skills from {}{}",
                    DIM,
                    base_url,
                    RESET
                );
            }
            WellKnownCheck::Current { new_skills } => {
                print_new_skills(base_url, &new_skills, is_global)
            }
            WellKnownCheck::Changed {
                changed,
                removed,
                new_skills,
            } => {
                changed_any = true;
                prompt_deletions(base_url, &removed, is_global, o);
                print_new_skills(base_url, &new_skills, is_global);
                if changed.is_empty() {
                    continue;
                }
                if cli_entry().is_none() {
                    fail += changed.len();
                    outln!("  {}✗ CLI entrypoint not found{}", DIM, RESET);
                    continue;
                }
                for name in &changed {
                    let safe = sanitize_metadata(name);
                    outln!("{}Updating {}…{}", TEXT, safe, RESET);
                    let mut args: Vec<String> = vec![
                        "add".into(),
                        base_url.clone(),
                        "--skill".into(),
                        name.clone(),
                    ];
                    if !is_global {
                        if let Some(subs) = items
                            .iter()
                            .find(|i| &i.name == name)
                            .and_then(|i| i.subagents.clone())
                            .filter(|s| !s.is_empty())
                        {
                            args.push("--subagent".into());
                            args.extend(subs.into_iter().map(|s| {
                                if s.is_empty() {
                                    "root".to_string()
                                } else {
                                    s
                                }
                            }));
                        }
                    }
                    if is_global {
                        args.push("-g".into());
                    }
                    args.push("-y".into());
                    if spawn_add(&args, false) {
                        ok += 1;
                        outln!("  {}✓{} Updated {}", TEXT, RESET, safe);
                    } else {
                        fail += 1;
                        outln!("  {}✗ Failed to update {}{}", DIM, safe, RESET);
                    }
                }
            }
        }
    }
    (ok, fail, changed_any)
}

fn discovered_locations(
    temp_dir: &str,
) -> Result<Vec<crate::skill_relocation::DiscoveredSkillLocation>, String> {
    let discovered = discover_skills(
        temp_dir,
        None,
        DiscoverOptions {
            full_depth: true,
            include_duplicate_names: true,
            include_internal: false,
        },
    )?;
    Ok(discovered
        .iter()
        .map(|sk| crate::skill_relocation::DiscoveredSkillLocation {
            name: sk.name.clone(),
            skill_path: join(&[paths::relative(temp_dir, &sk.path).as_str(), "SKILL.md"])
                .split(paths::SEP)
                .collect::<Vec<_>>()
                .join("/"),
        })
        .collect())
}

fn update_global_skills(o: &UpdateOptions) -> (usize, usize, usize) {
    let lock = read_skill_lock();
    let skills = lock.skills().clone();
    let (mut ok, mut fail) = (0usize, 0usize);
    if skills.is_empty() {
        if o.skills.is_none() {
            outln!("{}No global skills tracked in lock file.{}", DIM, RESET);
            outln!(
                "{}Install skills with{} {}skills add <package> -g{}",
                DIM,
                RESET,
                TEXT,
                RESET
            );
        }
        return (0, 0, 0);
    }

    let mut updates: Vec<(String, Value)> = Vec::new();
    let mut skipped: Vec<SkippedSkill> = Vec::new();
    let mut checkable: Vec<(String, Value)> = Vec::new();
    let mut wk_groups: Vec<(String, Vec<WellKnownItem>)> = Vec::new();

    for (name, entry) in &skills {
        if !matches_skill_filter(name, &o.skills) {
            continue;
        }
        if s(entry, "sourceType").as_deref() == Some("well-known") {
            if let (Some(base), Some(digest)) = (
                nonempty(entry, "sourceBaseUrl"),
                nonempty(entry, "wellKnownDigest"),
            ) {
                let item = WellKnownItem {
                    name: name.clone(),
                    digest,
                    subagents: None,
                };
                match wk_groups.iter_mut().find(|(k, _)| *k == base) {
                    Some((_, v)) => v.push(item),
                    None => wk_groups.push((base, vec![item])),
                }
                continue;
            }
        }
        if nonempty(entry, "skillFolderHash").is_none() || nonempty(entry, "skillPath").is_none() {
            skipped.push(SkippedSkill {
                name: name.clone(),
                reason: get_skip_reason(entry).into(),
                source_url: s(entry, "sourceUrl").unwrap_or_default(),
                source_type: s(entry, "sourceType").unwrap_or_default(),
                r#ref: s(entry, "ref"),
            });
            continue;
        }
        checkable.push((name.clone(), entry.clone()));
    }

    let wk_count: usize = wk_groups.iter().map(|(_, v)| v.len()).sum();
    let (wk_ok, wk_fail, wk_changed) = process_well_known_updates(&wk_groups, true, o);
    ok += wk_ok;
    fail += wk_fail;

    let mut by_source: Vec<(String, Vec<(String, Value)>)> = Vec::new();
    for item in &checkable {
        let key = format!(
            "{}\n{}",
            s(&item.1, "source").unwrap_or_default(),
            s(&item.1, "ref").unwrap_or_default()
        );
        match by_source.iter_mut().find(|(k, _)| *k == key) {
            Some((_, v)) => v.push(item.clone()),
            None => by_source.push((key, vec![item.clone()])),
        }
    }

    for (_, items) in &by_source {
        let first = &items[0].1;
        let source = s(first, "source").unwrap_or_default();
        let source_url = nonempty(first, "sourceUrl").unwrap_or_else(|| source.clone());
        let first_ref = s(first, "ref");
        out!(
            "\r{}Checking skills from source: {}{}\x1b[K\n",
            DIM,
            source,
            RESET
        );
        let is_github = s(first, "sourceType").as_deref() == Some("github");
        let locked_for_source: Vec<String> = skills
            .iter()
            .filter(|(_, e)| {
                s(e, "source").as_deref() == Some(source.as_str()) && s(e, "ref") == first_ref
            })
            .map(|(n, _)| n.clone())
            .collect();

        if is_github {
            match fetch_repo_tree(&source, first_ref.as_deref(), true) {
                Some(tree) => {
                    let blob_paths: Vec<&str> = tree
                        .tree
                        .iter()
                        .filter(|e| e.kind == "blob")
                        .map(|e| e.path.as_str())
                        .collect();
                    let missing = locked_for_source.iter().any(|n| {
                        skills
                            .get(n)
                            .and_then(|e| nonempty(e, "skillPath"))
                            .map(|p| !blob_paths.contains(&p.as_str()))
                            .unwrap_or(false)
                    });
                    if !missing {
                        for (name, entry) in items {
                            let sp = s(entry, "skillPath").unwrap_or_default();
                            if let Some(latest) = get_skill_folder_hash_from_tree(&tree, &sp) {
                                if Some(latest.as_str()) != s(entry, "skillFolderHash").as_deref() {
                                    updates.push((name.clone(), entry.clone()));
                                }
                            }
                        }
                        continue;
                    }
                    outln!(
                        "  {}Skill paths changed; resolving via Git clone{}",
                        DIM,
                        RESET
                    );
                }
                None => outln!(
                    "  {}GitHub API unavailable; checking via Git clone{}",
                    DIM,
                    RESET
                ),
            }
        }

        let mut temp: Option<String> = None;
        let result = (|| -> Result<(), String> {
            let t = clone_repo(&source_url, first_ref.as_deref()).map_err(|e| e.message)?;
            temp = Some(t.clone());
            let locations = discovered_locations(&t)?;
            let res = check_and_prompt_for_deletions(
                &source,
                &locked_for_source,
                &skills,
                true,
                o,
                &locations,
            );
            for (name, entry) in items {
                if res.deleted.contains(name) {
                    continue;
                }
                let Some(sp) = res.resolved.get(name) else {
                    continue;
                };
                let hash = s(entry, "skillFolderHash").unwrap_or_default();
                let uses_tree_hash =
                    is_github && hash.len() == 40 && hash.chars().all(|c| c.is_ascii_hexdigit());
                let latest = if uses_tree_hash {
                    get_git_tree_hash(&t, sp)
                } else {
                    compute_skill_folder_hash(&join(&[t.as_str(), paths::dirname(sp).as_str()]))
                        .ok()
                };
                let relocated = Some(sp.as_str()) != s(entry, "skillPath").as_deref();
                if relocated || latest.as_ref().map(|l| *l != hash).unwrap_or(false) {
                    let mut e = entry.clone();
                    e["skillPath"] = Value::String(sp.clone());
                    updates.push((name.clone(), e));
                }
            }
            Ok(())
        })();
        if result.is_err() {
            outln!("  {}✗ Failed to check skills from {}{}", DIM, source, RESET);
        }
        if let Some(t) = temp {
            let _ = cleanup_temp_dir(&t);
        }
    }

    if !checkable.is_empty() {
        out!("\r\x1b[K");
    }
    let checked = checkable.len() + skipped.len() + wk_count;
    if checkable.is_empty() && skipped.is_empty() && wk_count == 0 {
        if o.skills.is_none() {
            outln!("{}No global skills to check.{}", DIM, RESET);
        }
        return (ok, fail, 0);
    }
    if checkable.is_empty() && skipped.is_empty() {
        if !wk_changed {
            outln!("{}✓ All global skills are up to date{}", TEXT, RESET);
        }
        return (ok, fail, checked);
    }
    if checkable.is_empty() && !skipped.is_empty() {
        print_skipped_skills(&skipped);
        return (ok, fail, checked);
    }
    if updates.is_empty() {
        if !wk_changed {
            outln!("{}✓ All global skills are up to date{}", TEXT, RESET);
        }
        return (ok, fail, checked);
    }

    outln!("{}Found {} global update(s){}", TEXT, updates.len(), RESET);
    outln!();
    for (name, entry) in &updates {
        let safe = sanitize_metadata(name);
        outln!("{}Updating {}…{}", TEXT, safe, RESET);
        let use_entry = UpdateSourceEntry::from_json(entry);
        let Some(install_url) = build_update_install_source(&use_entry) else {
            fail += 1;
            outln!("  {}✗ Cannot update {}: lock file is missing sourceUrl for this generic Git source{}", DIM, safe, RESET);
            continue;
        };
        if cli_entry().is_none() {
            fail += 1;
            outln!(
                "  {}✗ Failed to update {}: CLI entrypoint not found{}",
                DIM,
                safe,
                RESET
            );
            continue;
        }
        let mut args: Vec<String> = vec!["add".into(), install_url, "--skill".into(), name.clone()];
        if should_use_full_depth_for_update(&use_entry) {
            args.push("--full-depth".into());
        }
        args.push("-g".into());
        args.push("-y".into());
        if spawn_add(&args, use_entry.source_type.as_deref() == Some("github")) {
            ok += 1;
            outln!("  {}✓{} Updated {}", TEXT, RESET, safe);
        } else {
            fail += 1;
            outln!("  {}✗ Failed to update {}{}", DIM, safe, RESET);
        }
    }
    print_skipped_skills(&skipped);
    (ok, fail, checked)
}

fn print_legacy_project_skills(legacy: &[&ProjectSkill]) {
    if legacy.is_empty() {
        return;
    }
    outln!();
    outln!("{}{} project skill(s) cannot be updated automatically (installed before skillPath tracking):{}", DIM, legacy.len(), RESET);
    for sk in legacy {
        let reinstall = build_local_update_source(&UpdateSourceEntry::from_json(&sk.entry));
        outln!("  {}•{} {}", TEXT, RESET, sanitize_metadata(&sk.name));
        match reinstall {
            Some(r) => outln!("    {}To refresh: {}skills add {} -y{}", DIM, TEXT, r, RESET),
            None => outln!("    {}To refresh: reinstall using the original full Git URL; this lock entry only has an ambiguous shorthand.{}", DIM, RESET),
        }
    }
}

fn update_project_skills(o: &UpdateOptions) -> (usize, usize, usize) {
    let project = get_project_skills_for_update(&o.skills);
    let (mut ok, mut fail) = (0usize, 0usize);
    if project.is_empty() {
        if o.skills.is_none() {
            outln!("{}No project skills to update.{}", DIM, RESET);
            outln!(
                "{}Install project skills with{} {}skills add <package>{}",
                DIM,
                RESET,
                TEXT,
                RESET
            );
        }
        return (0, 0, 0);
    }

    let mut wk_groups: Vec<(String, Vec<WellKnownItem>)> = Vec::new();
    let mut non_wk: Vec<&ProjectSkill> = Vec::new();
    for sk in &project {
        if s(&sk.entry, "sourceType").as_deref() == Some("well-known") {
            if let (Some(url), Some(digest)) = (
                nonempty(&sk.entry, "sourceUrl"),
                nonempty(&sk.entry, "wellKnownDigest"),
            ) {
                let item = WellKnownItem {
                    name: sk.name.clone(),
                    digest,
                    subagents: crate::local_lock::entry_string_array(&sk.entry, "subagents"),
                };
                match wk_groups.iter_mut().find(|(k, _)| *k == url) {
                    Some((_, v)) => v.push(item),
                    None => wk_groups.push((url, vec![item])),
                }
                continue;
            }
        }
        non_wk.push(sk);
    }
    let wk_count: usize = wk_groups.iter().map(|(_, v)| v.len()).sum();
    let updatable: Vec<&ProjectSkill> = non_wk
        .iter()
        .filter(|x| nonempty(&x.entry, "skillPath").is_some())
        .copied()
        .collect();
    let legacy: Vec<&ProjectSkill> = non_wk
        .iter()
        .filter(|x| nonempty(&x.entry, "skillPath").is_none())
        .copied()
        .collect();

    if updatable.is_empty() && wk_count == 0 {
        outln!("{}No project skills can be updated in place.{}", DIM, RESET);
        print_legacy_project_skills(&legacy);
        return (ok, fail, project.len());
    }

    let cwd = sys::cwd();
    let mut targets: Vec<String> = Vec::new();
    let mut has_universal = false;
    for a in agents() {
        if is_universal_agent(a.name) {
            if !has_universal && std::path::Path::new(&join(&[cwd.as_str(), ".agents"])).exists() {
                has_universal = true;
            }
        } else {
            let root = a.skills_dir.split('/').next().unwrap_or("");
            if std::path::Path::new(&join(&[cwd.as_str(), root])).exists() {
                targets.push(a.display_name.to_string());
            }
        }
    }
    let mut parts: Vec<String> = Vec::new();
    if has_universal {
        parts.push("Universal".into());
    }
    parts.extend(targets);
    if !parts.is_empty() {
        outln!("{}Updating for: {}{}", TEXT, parts.join(", "), RESET);
    }
    outln!(
        "{}Refreshing {} skill(s)…{}",
        TEXT,
        updatable.len() + wk_count,
        RESET
    );
    outln!();

    let (wk_ok, wk_fail, _) = process_well_known_updates(&wk_groups, false, o);
    ok += wk_ok;
    fail += wk_fail;

    let mut by_source: Vec<(String, Vec<&ProjectSkill>)> = Vec::new();
    for sk in &updatable {
        let src = nonempty(&sk.entry, "sourceUrl")
            .unwrap_or_else(|| s(&sk.entry, "source").unwrap_or_default());
        let key = format!("{}\n{}", src, s(&sk.entry, "ref").unwrap_or_default());
        match by_source.iter_mut().find(|(k, _)| *k == key) {
            Some((_, v)) => v.push(sk),
            None => by_source.push((key, vec![sk])),
        }
    }

    let local_lock = read_local_lock(None);
    if !updatable.is_empty() && cli_entry().is_none() {
        outln!("{}✗ CLI entrypoint not found{}", DIM, RESET);
        return (ok, fail + updatable.len(), project.len());
    }

    for (_, group) in &by_source {
        let first = &group[0].entry;
        let source =
            nonempty(first, "sourceUrl").unwrap_or_else(|| s(first, "source").unwrap_or_default());
        let clone_source = build_local_clone_source(&UpdateSourceEntry::from_json(first));
        let r = s(first, "ref");
        let locked_for_source: Vec<String> = local_lock
            .skills
            .iter()
            .filter(|(_, e)| {
                nonempty(e, "sourceUrl").unwrap_or_else(|| s(e, "source").unwrap_or_default())
                    == source
                    && s(e, "ref") == r
            })
            .map(|(n, _)| n.clone())
            .collect();

        let Some(clone_source) = clone_source else {
            fail += group.len();
            outln!("{}✗ Cannot update {}: skills-lock.json is missing sourceUrl for this generic Git source{}", DIM, source, RESET);
            continue;
        };

        let mut temp: Option<String> = None;
        let res = (|| -> Result<Resolution, String> {
            let t = clone_repo(&clone_source, r.as_deref()).map_err(|e| e.message)?;
            temp = Some(t.clone());
            let locations = discovered_locations(&t)?;
            Ok(check_and_prompt_for_deletions(
                &source,
                &locked_for_source,
                &local_lock.skills,
                false,
                o,
                &locations,
            ))
        })();
        if let Some(t) = temp {
            let _ = cleanup_temp_dir(&t);
        }
        let res = match res {
            Ok(r) => r,
            Err(_) => {
                outln!(
                    "{}✗ Failed to check for deleted skills from {}{}",
                    DIM,
                    source,
                    RESET
                );
                fail += group.len();
                continue;
            }
        };

        for sk in group.iter().filter(|x| !res.deleted.contains(&x.name)) {
            let safe = sanitize_metadata(&sk.name);
            let Some(resolved) = res.resolved.get(&sk.name) else {
                continue;
            };
            let mut entry = sk.entry.clone();
            entry["skillPath"] = Value::String(resolved.clone());
            outln!("{}Updating {}…{}", TEXT, safe, RESET);
            let use_entry = UpdateSourceEntry::from_json(&entry);
            let Some(install_url) = build_local_update_source(&use_entry) else {
                fail += 1;
                outln!("  {}✗ Cannot update {}: skills-lock.json is missing sourceUrl for this generic Git source{}", DIM, safe, RESET);
                continue;
            };
            let mut args: Vec<String> =
                vec!["add".into(), install_url, "--skill".into(), sk.name.clone()];
            if let Some(subs) = crate::local_lock::entry_string_array(&sk.entry, "subagents")
                .filter(|v| !v.is_empty())
            {
                args.push("--subagent".into());
                args.extend(subs.into_iter().map(|x| {
                    if x.is_empty() {
                        "root".to_string()
                    } else {
                        x
                    }
                }));
            }
            if should_use_full_depth_for_update(&use_entry) {
                args.push("--full-depth".into());
            }
            args.push("-y".into());
            if spawn_add(&args, use_entry.source_type.as_deref() == Some("github")) {
                ok += 1;
                outln!("  {}✓{} Updated {}", TEXT, RESET, safe);
            } else {
                fail += 1;
                outln!("  {}✗ Failed to update {}{}", DIM, safe, RESET);
            }
        }
    }

    print_legacy_project_skills(&legacy);
    (ok, fail, project.len())
}

pub fn run_update(args: &[String]) {
    let o = parse_update_options(args);
    let scope = resolve_update_scope(&o);
    match &o.skills {
        Some(list) => outln!("{}Updating {}…{}", TEXT, list.join(", "), RESET),
        None => outln!("{}Checking for skill updates…{}", TEXT, RESET),
    }
    outln!();

    let (mut total_ok, mut total_fail, mut total_found) = (0, 0, 0);
    if scope == UpdateScope::Global || scope == UpdateScope::Both {
        if scope == UpdateScope::Both && o.skills.is_none() {
            outln!("{}Global Skills{}", BOLD, RESET);
        }
        let (a, b, c) = update_global_skills(&o);
        total_ok += a;
        total_fail += b;
        total_found += c;
        if scope == UpdateScope::Both && o.skills.is_none() {
            outln!();
        }
    }
    if scope == UpdateScope::Project || scope == UpdateScope::Both {
        if scope == UpdateScope::Both && o.skills.is_none() {
            outln!("{}Project Skills{}", BOLD, RESET);
        }
        let (a, b, c) = update_project_skills(&o);
        total_ok += a;
        total_fail += b;
        total_found += c;
    }

    if let Some(list) = &o.skills {
        if total_found == 0 {
            outln!(
                "{}No installed skills found matching: {}{}",
                DIM,
                list.join(", "),
                RESET
            );
        }
    }
    outln!();
    if total_ok > 0 {
        outln!("{}✓ Updated {} skill(s){}", TEXT, total_ok, RESET);
    }
    if total_fail > 0 {
        outln!("{}Failed to update {} skill(s){}", DIM, total_fail, RESET);
        sys::set_exit_code(1);
    }
    track(&[
        ("event", Some("update".into())),
        ("scope", Some(scope.as_str().into())),
        ("skillCount", Some((total_ok + total_fail).to_string())),
        ("successCount", Some(total_ok.to_string())),
        ("failCount", Some(total_fail.to_string())),
    ]);
    outln!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_update_options() {
        let o = parse_update_options(&["-g".into(), "my-skill".into(), "-y".into()]);
        assert!(o.global && o.yes);
        assert_eq!(o.skills.clone().unwrap(), vec!["my-skill"]);
        assert_eq!(resolve_update_scope(&o), UpdateScope::Global);
        let both = parse_update_options(&["x".into()]);
        assert_eq!(resolve_update_scope(&both), UpdateScope::Both);
    }

    #[test]
    fn skip_reasons() {
        assert_eq!(
            get_skip_reason(&json!({"sourceType": "local"})),
            "Local path"
        );
        assert_eq!(
            get_skip_reason(&json!({"sourceType": "github", "skillFolderHash": ""})),
            "Private or deleted repo"
        );
        assert_eq!(
            get_skip_reason(&json!({"sourceType": "github", "skillFolderHash": "x"})),
            "No skill path recorded"
        );
        let sk = SkippedSkill {
            name: "n".into(),
            reason: "r".into(),
            source_url: "https://h.com/.well-known/skills/n/SKILL.md".into(),
            source_type: "well-known".into(),
            r#ref: None,
        };
        assert_eq!(get_install_source(&sk), "https://h.com");
    }
}
