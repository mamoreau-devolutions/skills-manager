//! `skills experimental_install` — restore project skills from
//! `skills-lock.json` into `.agents/skills` (port of install.ts).

use crate::add::{run_add, AddOptions};
use crate::agents::get_universal_agents;
use crate::color::pc;
use crate::local_lock::read_local_lock;
use crate::sync::{parse_sync_options, run_sync};
use crate::ui::log;
use crate::update_source::{build_local_update_source, UpdateSourceEntry};

pub fn run_install_from_lock(args: &[String]) {
    let lock = read_local_lock(None);
    if lock.skills.is_empty() {
        log::warn("No project skills found in skills-lock.json");
        log::info(&format!(
            "Add project-level skills with {} (without {})",
            pc::cyan("skills add <package>"),
            pc::cyan("-g")
        ));
        return;
    }
    let universal: Vec<String> = get_universal_agents()
        .into_iter()
        .map(|s| s.to_string())
        .collect();
    let mut node_module_skills: Vec<String> = Vec::new();
    let mut by_source: Vec<(String, Vec<String>)> = Vec::new();

    for (name, entry) in &lock.skills {
        if entry.get("sourceType").and_then(|v| v.as_str()) == Some("node_modules") {
            node_module_skills.push(name.clone());
            continue;
        }
        let Some(source) = build_local_update_source(&UpdateSourceEntry::from_json(entry)) else {
            log::error(&format!("Cannot restore {}: skills-lock.json is missing sourceUrl for this generic Git source", pc::cyan(name)));
            continue;
        };
        match by_source.iter_mut().find(|(s, _)| *s == source) {
            Some((_, v)) => v.push(name.clone()),
            None => by_source.push((source, vec![name.clone()])),
        }
    }

    let remote = lock.skills.len() - node_module_skills.len();
    if remote > 0 {
        log::info(&format!(
            "Restoring {} skill{} from skills-lock.json into {}",
            pc::cyan(remote.to_string()),
            if remote != 1 { "s" } else { "" },
            pc::dim(".agents/skills/")
        ));
    }

    for (source, skills) in by_source {
        run_add(
            &[source],
            AddOptions {
                skill: Some(skills),
                agent: Some(universal.clone()),
                yes: true,
                ..Default::default()
            },
        );
    }

    if !node_module_skills.is_empty() {
        let n = node_module_skills.len();
        log::info(&format!(
            "{} skill{} from node_modules",
            pc::cyan(n.to_string()),
            if n != 1 { "s" } else { "" }
        ));
        let mut o = parse_sync_options(args);
        o.yes = true;
        o.agent = Some(universal);
        run_sync(args, o);
    }
}
