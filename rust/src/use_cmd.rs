//! `skills use` — generate a prompt for one skill, or launch an agent with it
//! (port of use.ts).

use crate::agents::{self, agent};
use crate::blob::try_blob_install;
use crate::blob::BlobOptions;
use crate::download_source::{download_source, DownloadOptions};
use crate::git::{cleanup_temp_dir, clone_repo};
use crate::installer::sanitize_name;
use crate::paths::{self, join};
use crate::proc::{self, ProcError};
use crate::skills::{discover_skills, filter_skills, get_skill_display_name, DiscoverOptions};
use crate::source_parser::{get_owner_repo, parse_source};
use crate::types::{Skill, SnapshotFile};
use crate::wellknown::{self, WellKnownSkill};
use crate::{errln, outln, sys};

const BLOB_ALLOWED_OWNERS: &[&str] = &["vercel", "vercel-labs", "heygen-com", "remotion-dev"];
const SUPPORTED_USE_AGENTS: &[(&str, &str)] = &[
    ("claude-code", "claude"),
    ("codex", "codex"),
    ("sarvam-code", "sarvam-code"),
];

#[derive(Default, Clone, Debug)]
pub struct UseOptions {
    pub skill: Option<String>,
    pub agent: Option<Vec<String>>,
    pub full_depth: bool,
    pub help: bool,
}

enum UseSkill {
    Files {
        name: String,
        directory_name: String,
        raw_content: String,
        files: Vec<SnapshotFile>,
    },
    Disk {
        name: String,
        directory_name: String,
        raw_content: Option<String>,
        path: String,
    },
}

pub struct Materialized {
    pub temp_root: String,
    pub skill_dir: String,
    pub skill_md: String,
    pub has_supporting_files: bool,
}

pub fn parse_use_options(args: &[String]) -> (Vec<String>, UseOptions, Vec<String>) {
    let mut source = Vec::new();
    let mut o = UseOptions::default();
    let mut errors = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a.is_empty() {
            i += 1;
            continue;
        }
        match a {
            "--help" | "-h" => o.help = true,
            "--full-depth" => o.full_depth = true,
            "--skill" | "-s" => {
                let v = args.get(i + 1);
                match v {
                    Some(v) if !v.is_empty() && !v.starts_with('-') => {
                        if o.skill.is_some() {
                            errors.push("Only one --skill value can be provided".to_string());
                        } else {
                            o.skill = Some(v.clone());
                        }
                        i += 1;
                    }
                    _ => errors.push(format!("{} requires a skill name", a)),
                }
            }
            "--agent" | "-a" => {
                let list = o.agent.get_or_insert_with(Vec::new);
                let start = list.len();
                while i + 1 < args.len() && !args[i + 1].is_empty() && !args[i + 1].starts_with('-')
                {
                    i += 1;
                    list.push(args[i].clone());
                }
                if list.len() == start {
                    errors.push(format!("{} requires an agent name", a));
                }
            }
            _ => {
                if a.starts_with('-') {
                    errors.push(format!("Unknown option: {}", a));
                } else {
                    source.push(a.to_string());
                }
            }
        }
        i += 1;
    }
    errors.extend(validate_use_agent_option(o.agent.as_deref()));
    (source, o, errors)
}

fn validate_use_agent_option(values: Option<&[String]>) -> Vec<String> {
    let Some(values) = values.filter(|v| !v.is_empty()) else {
        return Vec::new();
    };
    let mut errors = Vec::new();
    let invalid: Vec<&String> = values
        .iter()
        .filter(|a| *a != "*" && agents::get(a).is_none())
        .collect();
    if values.iter().any(|a| a == "*") {
        errors.push(
            "skills use --agent does not support '*'; specify exactly one agent.".to_string(),
        );
    }
    if values.len() > 1 {
        errors.push("skills use --agent accepts exactly one agent.".to_string());
    }
    if !invalid.is_empty() {
        errors.push(format!(
            "Invalid agents: {}\nValid agents: {}",
            invalid
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            agents::all_agent_names().join(", ")
        ));
    }
    errors
}

pub fn build_use_prompt(
    skill_md: &str,
    support_dir: Option<&str>,
    has_supporting_files: bool,
) -> String {
    let mut sections = vec![
        "You are being given a Skill to execute for the user's next request.".to_string(),
        "Use the following SKILL.md as your instructions:".to_string(),
        format!("<SKILL.md>\n{}\n</SKILL.md>", skill_md),
    ];
    if let (true, Some(dir)) = (has_supporting_files, support_dir) {
        sections.push(format!("Supporting files for this skill were downloaded to:\n{}\n\nWhen the SKILL.md references relative paths, read them from that directory.", dir));
    }
    sections.join("\n\n") + "\n"
}

fn get_use_help() -> String {
    let supported: Vec<&str> = SUPPORTED_USE_AGENTS.iter().map(|(a, _)| *a).collect();
    format!(
        "Usage: skills use <source>[@<skill>] [options]\n\nGenerate a prompt for using one skill without installing it.\n\nOptions:\n  -s, --skill <skill>   Select the skill to use\n  -a, --agent <agent>   Start one supported agent interactively ({})\n  --full-depth          Search nested directories like skills add --full-depth\n  -h, --help            Show this help message\n\nExamples:\n  skills use vercel-labs/agent-skills@web-design-guidelines | claude\n  skills use vercel-labs/agent-skills --skill web-design-guidelines --agent claude-code\n  skills use vercel-labs/agent-skills@web-design-guidelines --agent codex",
        supported.join(", ")
    )
}

fn unsupported_agent_error(a: &str) -> String {
    let supported: Vec<&str> = SUPPORTED_USE_AGENTS.iter().map(|(a, _)| *a).collect();
    format!(
        "Running {} is not supported yet.\nSupported agents for skills use --agent: {}",
        agent(a).display_name,
        supported.join(", ")
    )
}

fn multiple_error(source: &str, names: &[String]) -> String {
    let first = names.first().cloned().unwrap_or_else(|| "<skill>".into());
    let mut lines =
        vec!["This source contains multiple skills. Specify exactly one skill:".to_string()];
    lines.extend(names.iter().map(|n| format!("  - {}", n)));
    lines.push(String::new());
    lines.push(format!(
        "Examples:\n  skills use {}@{}\n  skills use {} --skill {}",
        source, first, source, first
    ));
    lines.join("\n")
}

fn no_match_error(selector: &str, names: &[String]) -> String {
    let mut lines = vec![
        format!("No matching skill found for: {}", selector),
        "Available skills:".to_string(),
    ];
    lines.extend(names.iter().map(|n| format!("  - {}", n)));
    lines.join("\n")
}

fn resolve_selector(
    source_sel: Option<&str>,
    opt_sel: Option<&str>,
) -> Result<Option<String>, String> {
    if let (Some(s), Some(o)) = (source_sel, opt_sel) {
        if s.to_lowercase() != o.to_lowercase() {
            return Err(format!("Conflicting skill selectors: source selects \"{}\" but --skill selects \"{}\". Provide one selector.", s, o));
        }
        return Ok(Some(o.to_string()));
    }
    Ok(opt_sel.or(source_sel).map(|s| s.to_string()))
}

fn select_skill(skills: &[Skill], selector: Option<&str>, source: &str) -> Result<Skill, String> {
    if skills.is_empty() {
        return Err(
            "No valid skills found. Skills require a SKILL.md with name and description.".into(),
        );
    }
    let Some(sel) = selector else {
        if skills.len() == 1 {
            return Ok(skills[0].clone());
        }
        return Err(multiple_error(
            source,
            &skills
                .iter()
                .map(get_skill_display_name)
                .collect::<Vec<_>>(),
        ));
    };
    let matched = filter_skills(skills, &[sel.to_string()]);
    if matched.is_empty() {
        return Err(no_match_error(
            sel,
            &skills
                .iter()
                .map(get_skill_display_name)
                .collect::<Vec<_>>(),
        ));
    }
    if matched.len() > 1 {
        return Err(format!(
            "Skill selector \"{}\" matched multiple skills.",
            sel
        ));
    }
    Ok(matched[0].clone())
}

fn select_well_known(
    skills: &[WellKnownSkill],
    selector: Option<&str>,
    source: &str,
) -> Result<UseSkill, String> {
    if skills.is_empty() {
        return Err("No skills found at this URL. Make sure the server has a /.well-known/agent-skills/index.json or /.well-known/skills/index.json file.".into());
    }
    let chosen: Vec<&WellKnownSkill> = match selector {
        None => {
            if skills.len() != 1 {
                return Err(multiple_error(
                    source,
                    &skills
                        .iter()
                        .map(|s| s.install_name.clone())
                        .collect::<Vec<_>>(),
                ));
            }
            skills.iter().collect()
        }
        Some(sel) => {
            let l = sel.to_lowercase();
            let m: Vec<&WellKnownSkill> = skills
                .iter()
                .filter(|s| s.install_name.to_lowercase() == l || s.name.to_lowercase() == l)
                .collect();
            if m.is_empty() {
                return Err(no_match_error(
                    sel,
                    &skills
                        .iter()
                        .map(|s| s.install_name.clone())
                        .collect::<Vec<_>>(),
                ));
            }
            if m.len() > 1 {
                return Err(format!(
                    "Skill selector \"{}\" matched multiple skills.",
                    sel
                ));
            }
            m
        }
    };
    let s = chosen[0];
    Ok(UseSkill::Files {
        name: s.name.clone(),
        directory_name: s.install_name.clone(),
        raw_content: s.content.clone(),
        files: s.files.clone(),
    })
}

fn write_safe_file(dir: &str, rel: &str, contents: &[u8]) -> std::io::Result<()> {
    let full = join(&[dir, rel]);
    if !paths::is_path_safe(dir, &full) {
        return Ok(());
    }
    std::fs::create_dir_all(paths::dirname(&full))?;
    std::fs::write(full, contents)
}

fn copy_skill_directory(src: &str, dest: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let name = e.file_name().to_string_lossy().to_string();
        let ft = e.file_type()?;
        if name == "metadata.json"
            || (ft.is_dir() && matches!(name.as_str(), ".git" | "__pycache__" | "__pypackages__"))
        {
            continue;
        }
        let s = join(&[src, name.as_str()]);
        let d = join(&[dest, name.as_str()]);
        if !paths::is_path_safe(dest, &d) {
            continue;
        }
        if ft.is_dir() {
            copy_skill_directory(&s, &d)?;
            continue;
        }
        let r = match std::fs::metadata(&s) {
            Ok(m) if m.is_dir() => copy_skill_directory(&s, &d),
            Ok(_) => std::fs::copy(&s, &d).map(|_| ()),
            Err(e) => Err(e),
        };
        match r {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound && ft.is_symlink() => {
                errln!("Skipping broken symlink: {}", s)
            }
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

fn contains_supporting_files(root: &str, current: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(current) else {
        return false;
    };
    for e in entries.flatten() {
        let p = join(&[current, e.file_name().to_string_lossy().as_ref()]);
        let rel = paths::relative(root, &p)
            .split(paths::SEP)
            .collect::<Vec<_>>()
            .join("/");
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            if contains_supporting_files(root, &p) {
                return true;
            }
        } else if rel.to_lowercase() != "skill.md" {
            return true;
        }
    }
    false
}

fn materialize(skill: &UseSkill) -> Result<Materialized, String> {
    let temp_root = sys::mkdtemp("skills-use-").map_err(|e| e.to_string())?;
    let (name, dir_name) = match skill {
        UseSkill::Files {
            name,
            directory_name,
            ..
        }
        | UseSkill::Disk {
            name,
            directory_name,
            ..
        } => (name, directory_name),
    };
    let skill_dir = join(&[
        temp_root.as_str(),
        sanitize_name(if dir_name.is_empty() { name } else { dir_name }).as_str(),
    ]);
    if !paths::is_path_safe(&temp_root, &skill_dir) {
        return Err("Invalid skill name: potential path traversal detected".into());
    }
    std::fs::create_dir_all(&skill_dir).map_err(|e| e.to_string())?;
    let raw = match skill {
        UseSkill::Files {
            files, raw_content, ..
        } => {
            for f in files {
                write_safe_file(&skill_dir, &f.path, &f.contents).map_err(|e| e.to_string())?;
            }
            Some(raw_content.clone())
        }
        UseSkill::Disk {
            path, raw_content, ..
        } => {
            copy_skill_directory(path, &skill_dir).map_err(|e| e.to_string())?;
            raw_content.clone()
        }
    };
    let skill_md = match raw {
        Some(r) => r,
        None => crate::skills::read_utf8_lossy(&join(&[skill_dir.as_str(), "SKILL.md"]))
            .map_err(|e| e.to_string())?,
    };
    let has = contains_supporting_files(&skill_dir, &skill_dir);
    Ok(Materialized {
        temp_root,
        skill_dir,
        skill_md,
        has_supporting_files: has,
    })
}

fn fail(message: &str) -> ! {
    errln!("{}", message);
    sys::exit(1);
}

fn launch_agent(use_agent: &str, prompt: &str) -> Result<i32, String> {
    let (_, command) = SUPPORTED_USE_AGENTS
        .iter()
        .find(|(a, _)| *a == use_agent)
        .ok_or_else(|| unsupported_agent_error(use_agent))?;
    match proc::command(command, &[prompt]).status_inherit() {
        Ok(code) => Ok(code.unwrap_or(1)),
        Err(ProcError::NotFound) => Err(format!(
            "Could not launch {}: command not found: {}",
            agent(use_agent).display_name,
            command
        )),
        Err(e) => Err(e.to_string()),
    }
}

pub fn run_use(source_args: &[String], options: &UseOptions, parse_errors: &[String]) {
    if options.help {
        outln!("{}", get_use_help());
        return;
    }
    if !parse_errors.is_empty() {
        fail(&parse_errors.join("\n"));
    }
    if source_args.is_empty() {
        fail(&format!(
            "Missing required argument: source\n\n{}",
            get_use_help()
        ));
    }
    if source_args.len() > 1 {
        fail(&format!(
            "Expected one source, received {}: {}",
            source_args.len(),
            source_args.join(", ")
        ));
    }
    let use_agent = options.agent.as_ref().and_then(|a| a.first()).cloned();
    if let Some(a) = &use_agent {
        if !SUPPORTED_USE_AGENTS.iter().any(|(x, _)| x == a) {
            fail(&unsupported_agent_error(a));
        }
    }

    let source = &source_args[0];
    let mut clone_temp: Option<String> = None;
    let result = (|| -> Result<Materialized, String> {
        let parsed = parse_source(source)?;
        let selector = resolve_selector(parsed.skill_filter.as_deref(), options.skill.as_deref())?;
        let include_internal = selector.is_some();
        let opts = DiscoverOptions {
            include_internal,
            full_depth: options.full_depth,
            include_duplicate_names: false,
        };

        let selected: UseSkill = if parsed.kind == "well-known" {
            let skills = match wellknown::fetch_all_skills(&parsed.url, include_internal) {
                Ok(s) => s,
                Err(e) => fail(&e.to_string()),
            };
            if !skills.is_empty() {
                select_well_known(&skills, selector.as_deref(), source)?
            } else {
                let d = download_source(&parsed.url, DownloadOptions::default())?;
                clone_temp = Some(d.temp_dir.clone());
                let skills = discover_skills(&d.root_dir, None, opts)?;
                let s = select_skill(&skills, selector.as_deref(), source)?;
                UseSkill::Disk {
                    name: s.name.clone(),
                    directory_name: s.name.clone(),
                    raw_content: s.raw_content.clone(),
                    path: s.path.clone(),
                }
            }
        } else {
            let mut blob_used = false;
            let skills: Vec<Skill> = if parsed.kind == "download" {
                let d = download_source(&parsed.url, DownloadOptions::default())?;
                clone_temp = Some(d.temp_dir.clone());
                discover_skills(&d.root_dir, None, opts)?
            } else if parsed.kind == "local" {
                let local = parsed.local_path.clone().unwrap_or_default();
                if !std::path::Path::new(&local).exists() {
                    fail(&format!("Local path does not exist: {}", local));
                }
                discover_skills(&local, parsed.subpath.as_deref(), opts)?
            } else if parsed.kind == "github" && !options.full_depth {
                let mut blob = None;
                if let Some(or) = get_owner_repo(&parsed) {
                    let owner = or.split('/').next().unwrap_or("").to_lowercase();
                    if BLOB_ALLOWED_OWNERS.contains(&owner.as_str()) {
                        blob = try_blob_install(
                            &or,
                            &BlobOptions {
                                subpath: parsed.subpath.as_deref(),
                                skill_filter: selector.as_deref(),
                                r#ref: parsed.r#ref.as_deref(),
                                use_token: true,
                                include_internal,
                            },
                        );
                    }
                }
                match blob {
                    Some(b) => {
                        blob_used = true;
                        b.skills
                    }
                    None => {
                        let t = clone_repo(&parsed.url, parsed.r#ref.as_deref())
                            .map_err(|e| e.message)?;
                        clone_temp = Some(t.clone());
                        discover_skills(&t, parsed.subpath.as_deref(), opts)?
                    }
                }
            } else {
                let t = clone_repo(&parsed.url, parsed.r#ref.as_deref()).map_err(|e| e.message)?;
                clone_temp = Some(t.clone());
                discover_skills(&t, parsed.subpath.as_deref(), opts)?
            };
            let s = select_skill(&skills, selector.as_deref(), source)?;
            match (&s.blob, blob_used) {
                (Some(b), true) => {
                    let raw = s.raw_content.clone().unwrap_or_else(|| {
                        b.files
                            .iter()
                            .find(|f| f.path.to_lowercase() == "skill.md")
                            .map(|f| String::from_utf8_lossy(&f.contents).into_owned())
                            .unwrap_or_default()
                    });
                    UseSkill::Files {
                        name: s.name.clone(),
                        directory_name: s.name.clone(),
                        raw_content: raw,
                        files: b.files.clone(),
                    }
                }
                _ => UseSkill::Disk {
                    name: s.name.clone(),
                    directory_name: s.name.clone(),
                    raw_content: s.raw_content.clone(),
                    path: s.path.clone(),
                },
            }
        };
        materialize(&selected)
    })();

    if let Some(t) = clone_temp.take() {
        let _ = cleanup_temp_dir(&t);
    }
    let m = match result {
        Ok(m) => m,
        Err(e) => fail(&e),
    };
    let prompt = build_use_prompt(&m.skill_md, Some(&m.skill_dir), m.has_supporting_files);
    if let Some(a) = use_agent {
        match launch_agent(&a, &prompt) {
            Ok(0) => return,
            Ok(code) => sys::exit(code),
            Err(e) => fail(&e),
        }
    }
    sys::write_out(&prompt);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parses_use_options() {
        let (src, o, errs) = parse_use_options(&s(&["o/r", "--skill", "x", "--agent", "codex"]));
        assert_eq!(src, s(&["o/r"]));
        assert_eq!(o.skill.as_deref(), Some("x"));
        assert!(errs.is_empty());
        let (_, _, errs) = parse_use_options(&s(&["o/r", "--agent", "*"]));
        assert_eq!(
            errs,
            s(&["skills use --agent does not support '*'; specify exactly one agent."])
        );
        let (_, _, errs) = parse_use_options(&s(&["o/r", "--skill"]));
        assert_eq!(errs, s(&["--skill requires a skill name"]));
        let (_, _, errs) = parse_use_options(&s(&["--bogus"]));
        assert_eq!(errs, s(&["Unknown option: --bogus"]));
    }

    #[test]
    fn builds_prompt() {
        assert_eq!(
            build_use_prompt("BODY", Some("/tmp/x"), false),
            "You are being given a Skill to execute for the user's next request.\n\nUse the following SKILL.md as your instructions:\n\n<SKILL.md>\nBODY\n</SKILL.md>\n"
        );
        assert!(build_use_prompt("BODY", Some("/tmp/x"), true)
            .contains("Supporting files for this skill were downloaded to:\n/tmp/x"));
    }

    #[test]
    fn selectors_conflict() {
        assert!(resolve_selector(Some("a"), Some("b")).is_err());
        assert_eq!(
            resolve_selector(Some("a"), Some("A")).unwrap().as_deref(),
            Some("A")
        );
    }
}
