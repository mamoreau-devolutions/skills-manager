//! `skills remove` (port of remove.ts).

use crate::agents::{self, agent, detect_installed_agents, get_eve_subagents, AgentType};
use crate::color::pc;
use crate::detect_agent::detect_agent;
use crate::git::remove_all;
use crate::installer::{
    get_canonical_path, get_canonical_skills_dir, get_eve_subagent_skills_dir, get_install_path,
    sanitize_name,
};
use crate::local_lock::{read_local_lock, remove_skill_from_local_lock};
use crate::paths::join;
use crate::skill_lock::{get_skill_from_lock, read_skill_lock, remove_skill_from_lock};
use crate::skills::has_skill_md;
use crate::telemetry::track;
use crate::ui::{self, log, SelectOption};
use crate::{outln, sys};

#[derive(Default, Clone)]
pub struct RemoveOptions {
    pub global: bool,
    pub agent: Option<Vec<String>>,
    pub yes: bool,
    pub all: bool,
}

/// Resolve requested names to canonical removal targets, preferring lock keys.
pub fn resolve_skills_to_remove(
    requested: &[String],
    folder_names: &[String],
    lock_keys: &[String],
) -> Vec<String> {
    let mut identity: Vec<(String, String)> = Vec::new();
    let mut set = |k: String, v: String| {
        if let Some(e) = identity.iter_mut().find(|(ek, _)| *ek == k) {
            e.1 = v;
        } else {
            identity.push((k, v));
        }
    };
    for f in folder_names {
        set(sanitize_name(f), f.clone());
    }
    for k in lock_keys {
        set(sanitize_name(k), k.clone());
    }
    let mut matched: Vec<String> = Vec::new();
    for name in requested {
        let s = sanitize_name(name);
        if let Some((_, hit)) = identity.iter().find(|(k, _)| *k == s) {
            if !matched.contains(hit) {
                matched.push(hit.clone());
            }
        }
    }
    matched
}

pub fn remove_command(mut skill_names: Vec<String>, mut options: RemoveOptions) {
    crate::installer::reset_populated();
    let agent_result = detect_agent();
    if agent_result.is_agent() {
        options.yes = true;
        log::info(&format!(
            "{} Agent detected — removing non-interactively",
            pc::bg_cyan(pc::black(pc::bold(format!(" {} ", agent_result.name()))))
        ));
    }

    if skill_names.iter().any(|n| n == "*") {
        options.all = true;
        skill_names.retain(|n| n != "*");
    }
    let named: Vec<&String> = skill_names.iter().filter(|n| *n != "*").collect();
    if options.all && !named.is_empty() {
        log::error("Cannot combine --all with specific skill names.");
        log::info("Use `skills remove --all` to remove every skill, or omit --all to remove only the named skills.");
        log::info(&format!("Example: skills remove {} -y", named[0]));
        sys::exit(1);
    }

    let is_global = options.global;
    let cwd = sys::cwd();
    let mut spinner = ui::Spinner::new();
    spinner.start("Scanning for installed skills…");

    let mut found: Vec<String> = Vec::new();
    let mut scan_dir = |dir: &str| match std::fs::read_dir(dir) {
        Ok(entries) => {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) || name.starts_with('.') {
                    continue;
                }
                if !has_skill_md(&join(&[dir, name.as_str()])) {
                    continue;
                }
                if !found.contains(&name) {
                    found.push(name);
                }
            }
        }
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            log::warn(&format!("Could not scan directory {}: {}", dir, e));
        }
        Err(_) => {}
    };

    if is_global {
        scan_dir(&get_canonical_skills_dir(true, Some(&cwd)));
        for a in agents::agents() {
            if let Some(g) = &a.global_skills_dir {
                scan_dir(g);
            }
        }
    } else {
        scan_dir(&get_canonical_skills_dir(false, Some(&cwd)));
        for a in agents::agents() {
            scan_dir(&join(&[cwd.as_str(), a.skills_dir]));
        }
        for sub in get_eve_subagents(&cwd) {
            scan_dir(&get_eve_subagent_skills_dir(&sub, Some(&cwd)));
        }
    }

    let mut installed = found;
    installed.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    spinner.stop(&format!(
        "Found {} unique installed skill(s)",
        installed.len()
    ));

    let lock_keys: Vec<String> = if is_global {
        read_skill_lock().skills().keys().cloned().collect()
    } else {
        read_local_lock(Some(&cwd)).skills.keys().cloned().collect()
    };

    let requested: Vec<String> = if options.all {
        installed.iter().chain(lock_keys.iter()).cloned().collect()
    } else {
        skill_names.clone()
    };
    let resolved = if options.all || !skill_names.is_empty() {
        resolve_skills_to_remove(&requested, &installed, &lock_keys)
    } else {
        Vec::new()
    };

    if installed.is_empty() && resolved.is_empty() {
        ui::outro(&pc::yellow("No skills found to remove."));
        return;
    }

    if let Some(list) = options.agent.as_ref().filter(|l| !l.is_empty()) {
        let invalid: Vec<&String> = list.iter().filter(|a| agents::get(a).is_none()).collect();
        if !invalid.is_empty() {
            log::error(&format!(
                "Invalid agents: {}",
                invalid
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            log::info(&format!(
                "Valid agents: {}",
                agents::all_agent_names().join(", ")
            ));
            sys::exit(1);
        }
    }

    let selected: Vec<String> = if options.all {
        resolved
    } else if !skill_names.is_empty() {
        if resolved.is_empty() {
            log::error(&format!(
                "No matching skills found for: {}",
                skill_names.join(", ")
            ));
            return;
        }
        resolved
    } else {
        let choices: Vec<SelectOption<String>> = installed
            .iter()
            .map(|s| SelectOption::new(s.clone(), s, None))
            .collect();
        match ui::multiselect(
            &format!("Select skills to remove {}", pc::dim("(space to toggle)")),
            &choices,
            &[],
            true,
        ) {
            None => {
                ui::cancel("Removal cancelled");
                sys::exit(0);
            }
            Some(chosen) => resolve_skills_to_remove(&chosen, &installed, &lock_keys),
        }
    };

    let target_agents: Vec<AgentType> = match options.agent.as_ref().filter(|l| !l.is_empty()) {
        Some(list) => list
            .iter()
            .map(|a| agents::to_agent_type(a).unwrap())
            .collect(),
        None => {
            let all = agents::all_agent_names();
            spinner.stop(&format!("Targeting {} potential agent(s)", all.len()));
            all
        }
    };

    if !options.yes {
        outln!();
        log::info("Skills to remove:");
        for s in &selected {
            log::message(&format!("  {} {}", pc::red("•"), s));
        }
        outln!();
        match ui::confirm(
            &format!(
                "Are you sure you want to uninstall {} skill(s)?",
                selected.len()
            ),
            true,
        ) {
            Some(true) => {}
            _ => {
                ui::cancel("Removal cancelled");
                sys::exit(0);
            }
        }
    }

    spinner.start("Removing skills…");

    struct R {
        skill: String,
        success: bool,
        source: String,
        source_type: String,
        error: Option<String>,
    }
    let mut results: Vec<R> = Vec::new();

    for skill_name in &selected {
        let run = || -> Result<(String, String), String> {
            let canonical = get_canonical_path(skill_name, is_global, Some(&cwd), None, None)?;
            for at in &target_agents {
                let a = agent(at);
                let skill_path = get_install_path(skill_name, at, is_global, Some(&cwd), None)?;
                let sanitized = sanitize_name(skill_name);
                let mut cleanup: Vec<String> = vec![skill_path];
                let mut add = |p: String| {
                    if !cleanup.contains(&p) {
                        cleanup.push(p);
                    }
                };
                if let (true, Some(g)) = (is_global, &a.global_skills_dir) {
                    add(join(&[g.as_str(), sanitized.as_str()]));
                } else {
                    add(join(&[cwd.as_str(), a.skills_dir, sanitized.as_str()]));
                    if *at == "eve" {
                        for sub in get_eve_subagents(&cwd) {
                            add(join(&[
                                get_eve_subagent_skills_dir(&sub, Some(&cwd)).as_str(),
                                sanitized.as_str(),
                            ]));
                        }
                    }
                }
                for p in cleanup {
                    if p == canonical {
                        continue;
                    }
                    if std::fs::symlink_metadata(&p).is_ok() {
                        if let Err(e) = remove_all(&p) {
                            log::warn(&format!(
                                "Could not remove skill from {}: {}",
                                a.display_name, e
                            ));
                        }
                    }
                }
            }

            let installed_agents = detect_installed_agents();
            let remaining: Vec<AgentType> = installed_agents
                .into_iter()
                .filter(|a| !target_agents.contains(a))
                .collect();
            let mut still_used = false;
            for at in remaining {
                let p = get_install_path(skill_name, at, is_global, Some(&cwd), None)?;
                if std::fs::symlink_metadata(&p).is_ok() {
                    still_used = true;
                    break;
                }
            }
            if !still_used {
                remove_all(&canonical).map_err(|e| e.to_string())?;
            }

            let entry = if is_global {
                get_skill_from_lock(skill_name)
            } else {
                read_local_lock(Some(&cwd)).skills.get(skill_name).cloned()
            };
            let field = |k: &str| {
                entry
                    .as_ref()
                    .and_then(|e| e.get(k))
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "local".into())
            };
            let (source, source_type) = (field("source"), field("sourceType"));
            if !still_used {
                if is_global {
                    remove_skill_from_lock(skill_name).map_err(|e| e.to_string())?;
                } else {
                    remove_skill_from_local_lock(skill_name, Some(&cwd))
                        .map_err(|e| e.to_string())?;
                }
            }
            Ok((source, source_type))
        };
        match run() {
            Ok((source, source_type)) => results.push(R {
                skill: skill_name.clone(),
                success: true,
                source,
                source_type,
                error: None,
            }),
            Err(e) => results.push(R {
                skill: skill_name.clone(),
                success: false,
                source: String::new(),
                source_type: String::new(),
                error: Some(e),
            }),
        }
    }

    spinner.stop("Removal process complete");

    let successful: Vec<&R> = results.iter().filter(|r| r.success).collect();
    let failed: Vec<&R> = results.iter().filter(|r| !r.success).collect();

    if !successful.is_empty() {
        let mut by_source: Vec<(String, Vec<String>, String)> = Vec::new();
        for r in &successful {
            let src = if r.source.is_empty() {
                "local".to_string()
            } else {
                r.source.clone()
            };
            match by_source.iter_mut().find(|(s, _, _)| *s == src) {
                Some(e) => {
                    e.1.push(r.skill.clone());
                    e.2 = r.source_type.clone();
                }
                None => by_source.push((src, vec![r.skill.clone()], r.source_type.clone())),
            }
        }
        for (source, skills, source_type) in by_source {
            track(&[
                ("event", Some("remove".into())),
                ("source", Some(source)),
                ("skills", Some(skills.join(","))),
                ("agents", Some(target_agents.join(","))),
                ("global", if is_global { Some("1".into()) } else { None }),
                ("sourceType", Some(source_type)),
            ]);
        }
        log::success(&pc::green(format!(
            "Successfully removed {} skill(s)",
            successful.len()
        )));
    }
    if !failed.is_empty() {
        log::error(&pc::red(format!(
            "Failed to remove {} skill(s)",
            failed.len()
        )));
        for r in &failed {
            log::message(&format!(
                "  {} {}: {}",
                pc::red("✗"),
                r.skill,
                r.error.clone().unwrap_or_default()
            ));
        }
    }
    outln!();
    ui::outro(&pc::green("Done!"));
}

/// Separate skill names from option flags.
pub fn parse_remove_options(args: &[String]) -> (Vec<String>, RemoveOptions) {
    let mut o = RemoveOptions::default();
    let mut skills = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-g" | "--global" => o.global = true,
            "-y" | "--yes" => o.yes = true,
            "--all" => {
                o.all = true;
                o.yes = true;
            }
            "-s" | "--skill" => {
                while i + 1 < args.len() && !args[i + 1].is_empty() && !args[i + 1].starts_with('-')
                {
                    i += 1;
                    skills.push(args[i].clone());
                }
            }
            "-a" | "--agent" => {
                let list = o.agent.get_or_insert_with(Vec::new);
                while i + 1 < args.len() && !args[i + 1].is_empty() && !args[i + 1].starts_with('-')
                {
                    i += 1;
                    list.push(args[i].clone());
                }
            }
            _ => {
                if !a.is_empty() && !a.starts_with('-') {
                    skills.push(a.to_string());
                }
            }
        }
        i += 1;
    }
    (skills, o)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn resolves_lock_keys_first() {
        assert_eq!(
            resolve_skills_to_remove(&s(&["ce:review"]), &s(&["ce-review"]), &s(&["ce:review"])),
            s(&["ce:review"])
        );
        assert_eq!(
            resolve_skills_to_remove(&s(&["CE-Review"]), &s(&["ce-review"]), &[]),
            s(&["ce-review"])
        );
        assert!(resolve_skills_to_remove(&s(&["missing"]), &s(&["a"]), &[]).is_empty());
    }

    #[test]
    fn parses_options() {
        let (skills, o) = parse_remove_options(&s(&[
            "a", "-s", "b", "c", "--agent", "cursor", "-g", "--all", "--bogus",
        ]));
        assert_eq!(skills, s(&["a", "b", "c"]));
        assert!(o.global && o.all && o.yes);
        assert_eq!(o.agent.unwrap(), s(&["cursor"]));
    }
}
