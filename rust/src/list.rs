//! `skills list` (port of list.ts).

use crate::agents::{self, agent, AgentType};
use crate::color::ansi::{BOLD, CYAN, DIM, RESET, YELLOW};
use crate::gh_installed::{read_gh_origin, short_ref, GhOrigin};
use crate::installer::{list_installed_skills, sanitize_name, InstalledSkill};
use crate::local_lock::read_local_lock;
use crate::outln;
use crate::pinning::pinned_ref;
use crate::sanitize::{js_len, js_pad_end, sanitize_metadata};
use crate::skill_lock::get_all_locked_skills;
use crate::sys;
use serde_json::{json, Map, Value};

#[derive(Default)]
pub struct ListOptions {
    pub global: bool,
    pub agent: Option<Vec<String>>,
    pub json: bool,
}

/// Shorten a path for display (note: plain prefix checks, as in list.ts).
fn shorten_path(full: &str, cwd: &str) -> String {
    let home = sys::homedir();
    if full.starts_with(&home) {
        return full.replacen(&home, "~", 1);
    }
    if let Some(rest) = full.strip_prefix(cwd) {
        return format!(".{}", rest);
    }
    full.to_string()
}

pub fn format_list(items: &[String], max_show: usize) -> String {
    if items.len() <= max_show {
        return items.join(", ");
    }
    format!(
        "{} +{} more",
        items[..max_show].join(", "),
        items.len() - max_show
    )
}

pub fn parse_list_options(args: &[String]) -> ListOptions {
    let mut o = ListOptions::default();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "-g" || a == "--global" {
            o.global = true;
        } else if a == "--json" {
            o.json = true;
        } else if a == "-a" || a == "--agent" {
            let list = o.agent.get_or_insert_with(Vec::new);
            while i + 1 < args.len() && !args[i + 1].starts_with('-') {
                i += 1;
                list.push(args[i].clone());
            }
        }
        i += 1;
    }
    o
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

/// The `Source:` value: the lock's source (plus ` (pinned <ref>)` for a
/// pinned entry), a `gh skill` origin for untracked skills, else `local`.
fn source_label(entry: Option<&Value>, source: Option<&str>, skill_dir: &str) -> String {
    let base = match (entry, source) {
        (_, Some(s)) => sanitize_metadata(s),
        (None, None) => match read_gh_origin(skill_dir) {
            Some(origin) => return sanitize_metadata(&origin.list_label()),
            None => "local".into(),
        },
        (Some(_), None) => "local".into(),
    };
    match entry.and_then(pinned_ref) {
        Some(r) => format!("{} (pinned {})", base, sanitize_metadata(&r)),
        None => base,
    }
}

/// `list --json` extension keys: `ref`/`pinned` for pinned lock entries, the
/// origin plus `managedBy: "gh"` for skills installed by `gh skill`.
fn add_extension_json(o: &mut Map<String, Value>, entry: Option<&Value>, skill_dir: &str) {
    if let Some(e) = entry {
        if let Some(r) = pinned_ref(e) {
            o.insert("ref".into(), Value::String(r));
            o.insert("pinned".into(), Value::Bool(true));
        }
        return;
    }
    let Some(origin) = read_gh_origin(skill_dir) else {
        return;
    };
    let source = origin.source();
    match origin {
        GhOrigin::GitHub {
            url,
            git_ref,
            pinned,
            ..
        } => {
            o.insert("source".into(), Value::String(source));
            o.insert("sourceUrl".into(), Value::String(url));
            o.insert("sourceType".into(), Value::String("github".into()));
            o.insert(
                "ref".into(),
                git_ref
                    .as_deref()
                    .map(|r| Value::String(short_ref(r)))
                    .unwrap_or(Value::Null),
            );
            o.insert("pinned".into(), Value::Bool(pinned.is_some()));
            o.insert("managedBy".into(), Value::String("gh".into()));
        }
        GhOrigin::Local { path } => {
            o.insert("source".into(), Value::String(path));
            o.insert("sourceUrl".into(), Value::Null);
            o.insert("sourceType".into(), Value::String("local".into()));
            o.insert("managedBy".into(), Value::String("gh".into()));
        }
    }
}

pub fn run_list(args: &[String]) {
    let options = parse_list_options(args);
    let scope = options.global;

    let mut agent_filter: Option<Vec<AgentType>> = None;
    if let Some(list) = options.agent.as_ref().filter(|l| !l.is_empty()) {
        let invalid: Vec<&String> = list.iter().filter(|a| agents::get(a).is_none()).collect();
        if !invalid.is_empty() {
            outln!(
                "{}Invalid agents: {}{}",
                YELLOW,
                invalid
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                RESET
            );
            outln!(
                "{}Valid agents: {}{}",
                DIM,
                agents::all_agent_names().join(", "),
                RESET
            );
            sys::exit(1);
        }
        agent_filter = Some(
            list.iter()
                .map(|a| agents::to_agent_type(a).unwrap())
                .collect(),
        );
    }

    let installed = list_installed_skills(Some(scope), None, agent_filter.as_deref());
    let cwd = sys::cwd();
    let locked: Map<String, Value> = if scope {
        get_all_locked_skills()
    } else {
        read_local_lock(Some(&cwd)).skills
    };
    let by_sanitized: Vec<(String, &Value)> =
        locked.iter().map(|(k, v)| (sanitize_name(k), v)).collect();
    let get_lock_entry = |name: &str| -> Option<&Value> {
        locked.get(name).or_else(|| {
            let s = sanitize_name(name);
            // Map semantics: the last entry with a given sanitized key wins
            by_sanitized
                .iter()
                .rev()
                .find(|(k, _)| *k == s)
                .map(|(_, v)| *v)
        })
    };
    let lock_str = |e: Option<&Value>, key: &str| -> Value {
        e.and_then(|x| x.get(key))
            .filter(|v| !v.is_null())
            .cloned()
            .unwrap_or(Value::Null)
    };

    if options.json {
        let out: Vec<Value> = installed
            .iter()
            .map(|s| {
                let e = get_lock_entry(&s.name);
                let mut v = json!({
                    "name": s.name,
                    "path": s.canonical_path,
                    "scope": s.scope,
                    "agents": s.agents.iter().map(|a| agent(a).display_name).collect::<Vec<_>>(),
                    "source": lock_str(e, "source"),
                    "sourceUrl": lock_str(e, "sourceUrl"),
                    "sourceType": lock_str(e, "sourceType"),
                });
                if let Value::Object(o) = &mut v {
                    add_extension_json(o, e, &s.canonical_path);
                }
                v
            })
            .collect();
        outln!(
            "{}",
            serde_json::to_string_pretty(&Value::Array(out)).unwrap()
        );
        return;
    }

    let scope_label = if scope { "Global" } else { "Project" };
    if installed.is_empty() {
        outln!(
            "{}No {} skills found.{}",
            DIM,
            scope_label.to_lowercase(),
            RESET
        );
        if scope {
            outln!("{}Try listing project skills without -g{}", DIM, RESET);
        } else {
            outln!("{}Try listing global skills with -g{}", DIM, RESET);
        }
        return;
    }

    let print_skill = |skill: &InstalledSkill, indent: bool, max_name: usize, max_path: usize| {
        let prefix = if indent { "  " } else { "" };
        let short = shorten_path(&skill.canonical_path, &cwd);
        let names: Vec<String> = skill
            .agents
            .iter()
            .map(|a| agent(a).display_name.to_string())
            .collect();
        let agent_info = if !skill.agents.is_empty() {
            format_list(&names, 5)
        } else {
            format!("{}not linked{}", YELLOW, RESET)
        };
        let padded_name = js_pad_end(&sanitize_metadata(&skill.name), max_name);
        let padded_path = js_pad_end(&short, max_path);
        let entry = get_lock_entry(&skill.name);
        let source = entry
            .and_then(|e| e.get("source"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let source_label = source_label(entry, source, &skill.canonical_path);
        outln!(
            "{}{}{}{} {}{}{}",
            prefix,
            CYAN,
            padded_name,
            RESET,
            DIM,
            padded_path,
            RESET
        );
        outln!(
            "{}  {}Agents:{} {}  {}Source:{} {}",
            prefix,
            DIM,
            RESET,
            agent_info,
            DIM,
            RESET,
            source_label
        );
    };
    let widths = |list: &[&InstalledSkill]| -> (usize, usize) {
        let mut n = 0;
        let mut p = 0;
        for s in list {
            n = n.max(js_len(&sanitize_metadata(&s.name)));
            p = p.max(js_len(&shorten_path(&s.canonical_path, &cwd)));
        }
        (n, p)
    };

    outln!("{}{} Skills{}", BOLD, scope_label, RESET);
    outln!();

    let mut groups: Vec<(String, Vec<&InstalledSkill>)> = Vec::new();
    let mut ungrouped: Vec<&InstalledSkill> = Vec::new();
    for s in &installed {
        match get_lock_entry(&s.name)
            .and_then(|e| e.get("pluginName"))
            .and_then(|v| v.as_str())
            .filter(|p| !p.is_empty())
        {
            Some(g) => match groups.iter_mut().find(|(k, _)| k == g) {
                Some((_, v)) => v.push(s),
                None => groups.push((g.to_string(), vec![s])),
            },
            None => ungrouped.push(s),
        }
    }

    if !groups.is_empty() {
        groups.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
        for (group, skills) in &groups {
            outln!("{}{}{}", BOLD, kebab_to_title(group), RESET);
            let (n, p) = widths(skills);
            for s in skills {
                print_skill(s, true, n, p);
            }
            outln!();
        }
        if !ungrouped.is_empty() {
            outln!("{}General{}", BOLD, RESET);
            let (n, p) = widths(&ungrouped);
            for s in &ungrouped {
                print_skill(s, true, n, p);
            }
            outln!();
        }
    } else {
        let all: Vec<&InstalledSkill> = installed.iter().collect();
        let (n, p) = widths(&all);
        for s in &all {
            print_skill(s, false, n, p);
        }
        outln!();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_list_options() {
        let o = parse_list_options(&[
            "-g".into(),
            "-a".into(),
            "cursor".into(),
            "codex".into(),
            "--json".into(),
        ]);
        assert!(o.global && o.json);
        assert_eq!(o.agent.unwrap(), vec!["cursor", "codex"]);
    }

    #[test]
    fn titles_and_lists() {
        assert_eq!(kebab_to_title("document-skills"), "Document Skills");
        let items: Vec<String> = (1..=7).map(|i| i.to_string()).collect();
        assert_eq!(format_list(&items, 5), "1, 2, 3, 4, 5 +2 more");
    }
}
