//! `skills find` — search skills.sh (port of find.ts).

use crate::add::{parse_add_options, run_add};
use crate::color::ansi::{BOLD, CYAN, DIM, RESET, TEXT};
use crate::detect_agent::is_running_in_agent;
use crate::sanitize::sanitize_metadata;
use crate::source_parser::is_repo_private;
use crate::telemetry::track;
use crate::ui::{self, Key};
use crate::urlutil::search_params;
use crate::{errln, outln, sys};
use regex::Regex;
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct SearchSkill {
    pub name: String,
    pub slug: String,
    pub source: String,
    pub installs: f64,
}

/// `(n).toFixed(1).replace(/\.0$/, '')`
fn fixed1(n: f64) -> String {
    let s = format!("{:.1}", n);
    s.strip_suffix(".0").map(|x| x.to_string()).unwrap_or(s)
}

pub fn format_installs(count: f64) -> String {
    if count.is_nan() || count <= 0.0 {
        return String::new();
    }
    if count >= 1_000_000.0 {
        return format!("{}M installs", fixed1(count / 1_000_000.0));
    }
    if count >= 1_000.0 {
        return format!("{}K installs", fixed1(count / 1_000.0));
    }
    format!("{} install{}", count, if count == 1.0 { "" } else { "s" })
}

pub fn parse_find_options(args: &[String]) -> (String, Option<String>, Vec<String>) {
    let owner_re = Regex::new(r"(?i)^[a-z0-9](?:[a-z0-9-]{0,38})$").unwrap();
    let mut query = Vec::new();
    let mut owner: Option<String> = None;
    let mut errors = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a.is_empty() {
            i += 1;
            continue;
        }
        let value: String;
        if a == "--owner" {
            match args.get(i + 1) {
                Some(v) if !v.is_empty() && !v.starts_with('-') => {
                    value = v.clone();
                    i += 1;
                }
                _ => {
                    errors.push("--owner requires a GitHub owner".to_string());
                    i += 1;
                    continue;
                }
            }
        } else if let Some(v) = a.strip_prefix("--owner=") {
            if v.is_empty() {
                errors.push("--owner requires a GitHub owner".to_string());
                i += 1;
                continue;
            }
            value = v.to_string();
        } else {
            query.push(a.clone());
            i += 1;
            continue;
        }
        let o = value.trim().to_lowercase();
        if !owner_re.is_match(&o) {
            errors.push("--owner must be a valid GitHub owner".to_string());
        } else {
            owner = Some(o);
        }
        i += 1;
    }
    (query.join(" "), owner, errors)
}

fn api_base() -> String {
    sys::env("SKILLS_API_URL").unwrap_or_else(|| "https://skills.sh".into())
}

pub fn search_skills_api(query: &str, owner: Option<&str>) -> Vec<SearchSkill> {
    let mut pairs = vec![("q", query), ("limit", "20")];
    if let Some(o) = owner {
        pairs.push(("owner", o));
    }
    let url = format!("{}/api/search?{}", api_base(), search_params(&pairs));
    let Ok(resp) = crate::http::get(&url)
        .timeout(Duration::from_secs(60))
        .send()
    else {
        return Vec::new();
    };
    if !resp.ok() {
        return Vec::new();
    }
    let Some(data) = resp.json() else {
        return Vec::new();
    };
    let Some(list) = data.get("skills").and_then(|s| s.as_array()) else {
        return Vec::new();
    };
    let mut out: Vec<SearchSkill> = list
        .iter()
        .map(|s| SearchSkill {
            name: sanitize_metadata(s.get("name").and_then(|v| v.as_str()).unwrap_or("")),
            slug: sanitize_metadata(s.get("id").and_then(|v| v.as_str()).unwrap_or("")),
            source: sanitize_metadata(s.get("source").and_then(|v| v.as_str()).unwrap_or("")),
            installs: s.get("installs").and_then(|v| v.as_f64()).unwrap_or(0.0),
        })
        .collect();
    out.sort_by(|a, b| {
        b.installs
            .partial_cmp(&a.installs)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

/// fzf-style interactive search. Returns the chosen skill or `None`.
fn run_search_prompt(owner: Option<String>) -> Option<SearchSkill> {
    let raw = ui::RawMode::enable()?;
    let mut results: Vec<SearchSkill> = Vec::new();
    let mut selected = 0usize;
    let mut query = String::new();
    let mut loading = false;
    let mut last_lines = 0usize;
    let mut debounce: Option<(Instant, String)> = None;
    let mut generation: u64 = 0;
    let (tx, rx) = mpsc::channel::<(u64, Vec<SearchSkill>)>();

    sys::write_out("\x1b[?25l");
    let render = |query: &str,
                  results: &[SearchSkill],
                  selected: usize,
                  loading: bool,
                  last_lines: &mut usize| {
        let mut out = String::new();
        if *last_lines > 0 {
            out.push_str(&format!("\x1b[{}A\x1b[1G", last_lines));
        }
        out.push_str("\x1b[J");
        let mut lines: Vec<String> = Vec::new();
        lines.push(format!(
            "{}Search skills:{} {}{}_{}",
            TEXT, RESET, query, BOLD, RESET
        ));
        lines.push(String::new());
        if query.chars().count() < 2 {
            lines.push(format!(
                "{}Start typing to search (min 2 chars){}",
                DIM, RESET
            ));
        } else if results.is_empty() && loading {
            lines.push(format!("{}Searching…{}", DIM, RESET));
        } else if results.is_empty() {
            lines.push(format!("{}No skills found{}", DIM, RESET));
        } else {
            for (i, s) in results.iter().take(8).enumerate() {
                let sel = i == selected;
                let arrow = if sel {
                    format!("{}>{}", BOLD, RESET)
                } else {
                    " ".to_string()
                };
                let name = if sel {
                    format!("{}{}{}", BOLD, s.name, RESET)
                } else {
                    format!("{}{}{}", TEXT, s.name, RESET)
                };
                let source = if s.source.is_empty() {
                    String::new()
                } else {
                    format!(" {}{}{}", DIM, s.source, RESET)
                };
                let installs = format_installs(s.installs);
                let badge = if installs.is_empty() {
                    String::new()
                } else {
                    format!(" {}{}{}", CYAN, installs, RESET)
                };
                let loading_ind = if loading && i == 0 {
                    format!(" {}…{}", DIM, RESET)
                } else {
                    String::new()
                };
                lines.push(format!(
                    "  {} {}{}{}{}",
                    arrow, name, source, badge, loading_ind
                ));
            }
        }
        lines.push(String::new());
        lines.push(format!(
            "{}up/down navigate | enter select | esc cancel{}",
            DIM, RESET
        ));
        for l in &lines {
            out.push_str(l);
            out.push_str("\r\n");
        }
        sys::write_out(&out);
        *last_lines = lines.len();
    };

    render(&query, &results, selected, loading, &mut last_lines);
    let chosen = loop {
        // Deliver finished searches
        while let Ok((gen, res)) = rx.try_recv() {
            if gen == generation {
                results = res;
                selected = 0;
                loading = false;
                render(&query, &results, selected, loading, &mut last_lines);
            }
        }
        // Fire debounced search
        if let Some((at, q)) = &debounce {
            if Instant::now() >= *at {
                let q = q.clone();
                let tx = tx.clone();
                let gen = generation;
                let owner = owner.clone();
                std::thread::spawn(move || {
                    let r = search_skills_api(&q, owner.as_deref());
                    let _ = tx.send((gen, r));
                });
                debounce = None;
            }
        }
        let Some(key) = ui::poll_key(Duration::from_millis(30)) else {
            continue;
        };
        let mut trigger = false;
        match key {
            Key::Escape | Key::CtrlC => break None,
            Key::Enter => break results.get(selected).cloned(),
            Key::Up => {
                selected = selected.saturating_sub(1);
                render(&query, &results, selected, loading, &mut last_lines);
            }
            Key::Down => {
                selected = (selected + 1).min(results.len().saturating_sub(1));
                render(&query, &results, selected, loading, &mut last_lines);
            }
            Key::Backspace => {
                if !query.is_empty() {
                    query.pop();
                    trigger = true;
                }
            }
            Key::Char(c) if (' '..='~').contains(&c) => {
                query.push(c);
                trigger = true;
            }
            Key::Space => {
                query.push(' ');
                trigger = true;
            }
            _ => {}
        }
        if trigger {
            generation += 1;
            debounce = None;
            loading = false;
            let len = query.chars().count();
            if len < 2 {
                results.clear();
                selected = 0;
            } else {
                loading = true;
                let ms = 150u64.max(350u64.saturating_sub(len as u64 * 50));
                debounce = Some((Instant::now() + Duration::from_millis(ms), query.clone()));
            }
            render(&query, &results, selected, loading, &mut last_lines);
        }
    };
    drop(raw);
    sys::write_out("\x1b[?25h");
    chosen
}

fn owner_repo_from_string(pkg: &str) -> Option<(String, String)> {
    let at = pkg.rfind('@');
    let repo_path = match at {
        Some(i) if i > 0 => &pkg[..i],
        _ => pkg,
    };
    crate::source_parser::parse_owner_repo(repo_path)
}

pub fn run_find(args: &[String]) {
    let (query, owner, errors) = parse_find_options(args);
    let non_interactive = !sys::stdin_is_tty();
    if !errors.is_empty() {
        for e in &errors {
            errln!("{}", e);
        }
        errln!("Usage: skills find <query> [--owner <owner>]");
        return;
    }

    if !query.is_empty() {
        let results = search_skills_api(&query, owner.as_deref());
        track(&[
            ("event", Some("find".into())),
            ("query", Some(query.clone())),
            ("resultCount", Some(results.len().to_string())),
        ]);
        if results.is_empty() {
            let suffix = owner
                .as_ref()
                .map(|o| format!(" from owner \"{}\"", o))
                .unwrap_or_default();
            outln!(
                "{}No skills found for \"{}\"{}{}",
                DIM,
                query,
                suffix,
                RESET
            );
            return;
        }
        outln!("{}Install with{} skills add <owner/repo@skill>", DIM, RESET);
        outln!();
        for s in &results {
            let pkg = if s.source.is_empty() {
                &s.slug
            } else {
                &s.source
            };
            let installs = format_installs(s.installs);
            outln!(
                "{}{}@{}{}{}",
                TEXT,
                pkg,
                s.name,
                RESET,
                if installs.is_empty() {
                    String::new()
                } else {
                    format!(" {}{}{}", CYAN, installs, RESET)
                }
            );
            outln!("{}└ https://skills.sh/{}{}", DIM, s.slug, RESET);
            outln!();
        }
        return;
    }

    if non_interactive || is_running_in_agent() {
        outln!(
            "{}Tip: if running in a coding agent, follow these steps:{}",
            DIM,
            RESET
        );
        outln!("{}  1) skills find [query] [--owner <owner>]{}", DIM, RESET);
        outln!("{}  2) skills add <owner/repo@skill>{}", DIM, RESET);
        outln!();
        outln!(
            "{}Usage: skills find <query> [--owner <owner>]{}",
            DIM,
            RESET
        );
        return;
    }

    let selected = run_search_prompt(owner);
    track(&[
        ("event", Some("find".into())),
        ("query", Some(String::new())),
        (
            "resultCount",
            Some(if selected.is_some() { "1" } else { "0" }.into()),
        ),
        ("interactive", Some("1".into())),
    ]);
    let Some(selected) = selected else {
        outln!("{}Search cancelled{}", DIM, RESET);
        outln!();
        return;
    };
    let pkg = if selected.source.is_empty() {
        selected.slug.clone()
    } else {
        selected.source.clone()
    };
    outln!();
    outln!(
        "{}Installing {}{}{} from {}{}{}…",
        TEXT,
        BOLD,
        selected.name,
        RESET,
        DIM,
        pkg,
        RESET
    );
    outln!();
    let (source, opts, _) =
        parse_add_options(&[pkg.clone(), "--skill".into(), selected.name.clone()]);
    run_add(&source, opts);
    outln!();
    let public = owner_repo_from_string(&pkg)
        .map(|(o, r)| is_repo_private(&o, &r) == Some(false))
        .unwrap_or(false);
    if public {
        outln!(
            "{}View the skill at{} {}https://skills.sh/{}{}",
            DIM,
            RESET,
            TEXT,
            selected.slug,
            RESET
        );
    } else {
        outln!(
            "{}Discover more skills at{} {}https://skills.sh{}",
            DIM,
            RESET,
            TEXT,
            RESET
        );
    }
    outln!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_formatting() {
        assert_eq!(format_installs(0.0), "");
        assert_eq!(format_installs(1.0), "1 install");
        assert_eq!(format_installs(999.0), "999 installs");
        assert_eq!(format_installs(1000.0), "1K installs");
        assert_eq!(format_installs(1540.0), "1.5K installs");
        assert_eq!(format_installs(2_000_000.0), "2M installs");
    }

    #[test]
    fn find_options() {
        let (q, o, e) = parse_find_options(&[
            "react".into(),
            "hooks".into(),
            "--owner".into(),
            "Vercel".into(),
        ]);
        assert_eq!(q, "react hooks");
        assert_eq!(o.as_deref(), Some("vercel"));
        assert!(e.is_empty());
        let (_, _, e) = parse_find_options(&["--owner=bad_owner!".into()]);
        assert_eq!(e, vec!["--owner must be a valid GitHub owner"]);
        let (_, _, e) = parse_find_options(&["--owner".into()]);
        assert_eq!(e, vec!["--owner requires a GitHub owner"]);
    }
}
