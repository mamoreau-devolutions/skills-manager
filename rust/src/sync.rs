//! `skills experimental_sync` — crawl node_modules for skills (port of sync.ts).

use crate::agents::{
    self, agent, detect_installed_agents, get_non_universal_agents, get_universal_agents,
    get_visible_universal_agents, AgentType,
};
use crate::color::pc;
use crate::detect_agent::{detect_agent, get_agent_type};
use crate::installer::{get_canonical_path, install_skill_for_agent, InstallMode, InstallOptions};
use crate::local_lock::{
    add_skill_to_local_lock, compute_skill_folder_hash, read_local_lock, Entry,
};
use crate::paths::{self, join};
use crate::search_multiselect::{search_multiselect, LockedSection, Options, SearchItem};
use crate::skills::parse_skill_md;
use crate::telemetry::track;
use crate::types::Skill;
use crate::ui::{self, log};
use crate::{outln, sys};
use serde_json::Value;

#[derive(Default, Clone)]
pub struct SyncOptions {
    pub agent: Option<Vec<String>>,
    pub yes: bool,
    pub force: bool,
}

pub fn shorten_path(full: &str, cwd: &str) -> String {
    let home = sys::homedir();
    if full == home || full.starts_with(&format!("{}{}", home, paths::SEP)) {
        return format!("~{}", &full[home.len()..]);
    }
    if full == cwd || full.starts_with(&format!("{}{}", cwd, paths::SEP)) {
        return format!(".{}", &full[cwd.len()..]);
    }
    full.to_string()
}

fn is_dir(p: &str) -> bool {
    std::fs::metadata(p).map(|m| m.is_dir()).unwrap_or(false)
}

fn list_names(dir: &str) -> Option<Vec<String>> {
    Some(
        std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect(),
    )
}

/// Skills in node_modules packages (top-level and scoped), tagged with the
/// package they came from.
fn discover_node_module_skills(cwd: &str) -> Vec<(Skill, String)> {
    let nm = join(&[cwd, "node_modules"]);
    let mut out = Vec::new();
    let Some(top) = list_names(&nm) else {
        return out;
    };

    let process = |pkg_dir: &str, pkg: &str, out: &mut Vec<(Skill, String)>| {
        let root_md = join(&[pkg_dir, "SKILL.md"]);
        if std::path::Path::new(&root_md).exists() {
            if let Some(s) = parse_skill_md(&root_md, false) {
                out.push((s, pkg.to_string()));
                return;
            }
        }
        for search in [
            pkg_dir.to_string(),
            join(&[pkg_dir, "skills"]),
            join(&[pkg_dir, ".agents", "skills"]),
        ] {
            let Some(names) = list_names(&search) else {
                continue;
            };
            for name in names {
                let skill_dir = join(&[search.as_str(), name.as_str()]);
                if !is_dir(&skill_dir) {
                    continue;
                }
                let md = join(&[skill_dir.as_str(), "SKILL.md"]);
                if !std::path::Path::new(&md).exists() {
                    continue;
                }
                if let Some(s) = parse_skill_md(&md, false) {
                    out.push((s, pkg.to_string()));
                }
            }
        }
    };

    for name in top {
        if name.starts_with('.') {
            continue;
        }
        let full = join(&[nm.as_str(), name.as_str()]);
        if !is_dir(&full) {
            continue;
        }
        if name.starts_with('@') {
            let Some(scoped) = list_names(&full) else {
                continue;
            };
            for sn in scoped {
                let sp = join(&[full.as_str(), sn.as_str()]);
                if is_dir(&sp) {
                    process(&sp, &format!("{}/{}", name, sn), &mut out);
                }
            }
        } else {
            process(&full, &name, &mut out);
        }
    }
    out
}

pub fn run_sync(_args: &[String], mut options: SyncOptions) {
    crate::installer::reset_populated();
    let cwd = sys::cwd();
    let agent_result = detect_agent();
    if agent_result.is_agent() {
        options.yes = true;
        if options.agent.as_ref().map(|a| a.is_empty()).unwrap_or(true) {
            if let Some(mapped) = get_agent_type(agent_result.name()) {
                let mut list: Vec<String> = vec![mapped.to_string()];
                for ua in get_universal_agents() {
                    if !list.iter().any(|x| x == ua) {
                        list.push(ua.to_string());
                    }
                }
                options.agent = Some(list);
            }
        }
    }

    outln!();
    if !agent_result.is_agent() {
        ui::intro(&pc::bg_cyan(pc::black(" skills experimental_sync ")));
    } else {
        log::info(&format!(
            "{} Agent detected — installing non-interactively",
            pc::bg_cyan(pc::black(pc::bold(format!(" {} ", agent_result.name()))))
        ));
    }

    let mut spinner = ui::Spinner::new();
    spinner.start("Scanning node_modules for skills…");
    let discovered = discover_node_module_skills(&cwd);
    if discovered.is_empty() {
        spinner.stop(&pc::yellow("No skills found"));
        ui::outro(&pc::dim("No SKILL.md files found in node_modules."));
        return;
    }
    spinner.stop(&format!(
        "Found {} skill{} in node_modules",
        pc::green(discovered.len().to_string()),
        if discovered.len() > 1 { "s" } else { "" }
    ));

    for (s, pkg) in &discovered {
        log::info(&format!(
            "{} {}",
            pc::cyan(&s.name),
            pc::dim(format!("from {}", pkg))
        ));
        if !s.description.is_empty() {
            log::message(&pc::dim(format!("  {}", s.description)));
        }
    }

    let local_lock = read_local_lock(Some(&cwd));
    let mut to_install: Vec<&(Skill, String)> = Vec::new();
    let mut up_to_date: Vec<String> = Vec::new();
    if options.force {
        to_install.extend(discovered.iter());
        log::info(&pc::dim("Force mode: reinstalling all skills"));
    } else {
        for item in &discovered {
            if let Some(existing) = local_lock.skills.get(&item.0.name) {
                if let Ok(h) = compute_skill_folder_hash(&item.0.path) {
                    if Some(h.as_str()) == existing.get("computedHash").and_then(|v| v.as_str()) {
                        up_to_date.push(item.0.name.clone());
                        continue;
                    }
                }
            }
            to_install.push(item);
        }
        if !up_to_date.is_empty() {
            log::info(&pc::dim(format!(
                "{} skill{} already up to date",
                up_to_date.len(),
                if up_to_date.len() != 1 { "s" } else { "" }
            )));
        }
        if to_install.is_empty() {
            outln!();
            ui::outro(&pc::green("All skills are up to date."));
            return;
        }
    }

    log::info(&format!(
        "{} skill{} to install/update",
        to_install.len(),
        if to_install.len() != 1 { "s" } else { "" }
    ));

    let universal = get_universal_agents();
    let visible_universal = get_visible_universal_agents();
    let locked_section = || LockedSection {
        title: "Universal (.agents/skills)".into(),
        items: visible_universal
            .iter()
            .map(|a| SearchItem::new(*a, a, agent(a).display_name))
            .collect(),
        hidden_count: universal.len() - visible_universal.len(),
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
        list.iter()
            .map(|a| agents::to_agent_type(a).unwrap())
            .collect()
    } else {
        spinner.start("Loading agents…");
        let installed = detect_installed_agents();
        spinner.stop(&format!("{} agents", agents::agents().len()));
        if installed.is_empty() {
            if options.yes {
                log::info("Installing to universal agents");
                universal.clone()
            } else {
                let items: Vec<SearchItem<AgentType>> = get_non_universal_agents()
                    .into_iter()
                    .map(|a| {
                        SearchItem::new(a, a, agent(a).display_name)
                            .hint(Some(agent(a).skills_dir.to_string()))
                    })
                    .collect();
                let mut o = Options::new("Which agents do you want to install to?", items);
                o.locked_section = Some(locked_section());
                match search_multiselect(o) {
                    Some(v) => v,
                    None => {
                        ui::cancel("Sync cancelled");
                        sys::exit(0);
                    }
                }
            }
        } else if installed.len() == 1 || options.yes {
            let mut t = installed.clone();
            for ua in &universal {
                if !t.contains(ua) {
                    t.push(ua);
                }
            }
            t
        } else {
            let others: Vec<AgentType> = get_non_universal_agents()
                .into_iter()
                .filter(|a| installed.contains(a))
                .collect();
            let items: Vec<SearchItem<AgentType>> = others
                .iter()
                .map(|a| {
                    SearchItem::new(*a, a, agent(a).display_name)
                        .hint(Some(agent(a).skills_dir.to_string()))
                })
                .collect();
            let initial: Vec<usize> = installed
                .iter()
                .filter(|a| !universal.contains(a))
                .filter_map(|a| others.iter().position(|o| o == a))
                .collect();
            let mut o = Options::new("Which agents do you want to install to?", items);
            o.initial_selected = initial;
            o.locked_section = Some(locked_section());
            match search_multiselect(o) {
                Some(v) => v,
                None => {
                    ui::cancel("Sync cancelled");
                    sys::exit(0);
                }
            }
        }
    };

    let mut summary: Vec<String> = Vec::new();
    for (s, pkg) in &to_install {
        let canonical = get_canonical_path(&s.name, false, None, None, None).unwrap_or_default();
        summary.push(format!(
            "{} {}",
            pc::cyan(&s.name),
            pc::dim(format!("← {}", pkg))
        ));
        summary.push(format!("  {}", pc::dim(shorten_path(&canonical, &cwd))));
    }
    outln!();
    ui::note(&summary.join("\n"), "Sync Summary");

    if !options.yes {
        match ui::confirm("Proceed with sync?", true) {
            Some(true) => {}
            _ => {
                ui::cancel("Sync cancelled");
                sys::exit(0);
            }
        }
    }

    spinner.start("Syncing skills…");
    struct R {
        skill: String,
        agent: String,
        success: bool,
        canonical: Option<String>,
        error: Option<String>,
    }
    let mut results: Vec<R> = Vec::new();
    for (s, _) in &to_install {
        for at in &target_agents {
            let r = install_skill_for_agent(
                s,
                at,
                &InstallOptions {
                    global: false,
                    cwd: Some(cwd.clone()),
                    mode: Some(InstallMode::Symlink),
                    ..Default::default()
                },
            );
            results.push(R {
                skill: s.name.clone(),
                agent: agent(at).display_name.to_string(),
                success: r.success,
                canonical: r.canonical_path,
                error: r.error,
            });
        }
    }
    spinner.stop("Sync complete");

    let successful_names: Vec<String> = {
        let mut v: Vec<String> = Vec::new();
        for r in results.iter().filter(|r| r.success) {
            if !v.contains(&r.skill) {
                v.push(r.skill.clone());
            }
        }
        v
    };

    for (s, pkg) in &to_install {
        if successful_names.contains(&s.name) {
            if let Ok(h) = compute_skill_folder_hash(&s.path) {
                let mut e = Entry::new();
                e.insert("source".into(), Value::String(pkg.clone()));
                e.insert("sourceType".into(), Value::String("node_modules".into()));
                e.insert("computedHash".into(), Value::String(h));
                let _ = add_skill_to_local_lock(&s.name, e, Some(&cwd));
            }
        }
    }

    outln!();
    if !successful_names.is_empty() {
        let mut lines = Vec::new();
        for name in &successful_names {
            let first = results
                .iter()
                .find(|r| r.success && &r.skill == name)
                .unwrap();
            let pkg = to_install
                .iter()
                .find(|(s, _)| &s.name == name)
                .map(|(_, p)| p.clone())
                .unwrap_or_default();
            lines.push(format!(
                "{} {} {}",
                pc::green("✓"),
                name,
                pc::dim(format!("← {}", pkg))
            ));
            if let Some(c) = &first.canonical {
                lines.push(format!("  {}", pc::dim(shorten_path(c, &cwd))));
            }
        }
        let n = successful_names.len();
        ui::note(
            &lines.join("\n"),
            &pc::green(format!(
                "Synced {} skill{}",
                n,
                if n != 1 { "s" } else { "" }
            )),
        );
    }
    let failed: Vec<&R> = results.iter().filter(|r| !r.success).collect();
    if !failed.is_empty() {
        outln!();
        log::error(&pc::red(format!("Failed to install {}", failed.len())));
        for r in failed {
            log::message(&format!(
                "  {} {} → {}: {}",
                pc::red("✗"),
                r.skill,
                r.agent,
                pc::dim(r.error.clone().unwrap_or_else(|| "undefined".into()))
            ));
        }
    }

    track(&[
        ("event", Some("experimental_sync".into())),
        ("skillCount", Some(to_install.len().to_string())),
        ("successCount", Some(successful_names.len().to_string())),
        ("agents", Some(target_agents.join(","))),
    ]);

    outln!();
    ui::outro(&format!(
        "{}{}",
        pc::green("Done!"),
        pc::dim("  Review skills before use; they run with full agent permissions.")
    ));
}

pub fn parse_sync_options(args: &[String]) -> SyncOptions {
    let mut o = SyncOptions::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-y" | "--yes" => o.yes = true,
            "-f" | "--force" => o.force = true,
            "-a" | "--agent" => {
                let list = o.agent.get_or_insert_with(Vec::new);
                while i + 1 < args.len() && !args[i + 1].is_empty() && !args[i + 1].starts_with('-')
                {
                    i += 1;
                    list.push(args[i].clone());
                }
            }
            _ => {}
        }
        i += 1;
    }
    o
}
