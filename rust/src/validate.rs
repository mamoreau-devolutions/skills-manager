//! `skills validate` — check skills against the Agent Skills specification
//! (https://agentskills.io/specification) before publishing them
//! (extension; not in the reference CLI). Runs offline.

use crate::collate::ordinal_cmp;
use crate::color::ansi::{DIM, RESET, TEXT, YELLOW};
use crate::color::pc;
use crate::frontmatter::parse_yaml;
use crate::paths::{self, join};
use crate::sanitize::sanitize_metadata;
use crate::skills::has_skill_md;
use crate::urlutil::decode_uri_component;
use crate::{errln, outln, sys};
use regex::Regex;
use serde_json::{Map, Value};
use std::sync::OnceLock;

/// Install tracking keys written under `metadata` by `gh skill install` (and
/// similar tools), in the order they are reported.
pub const INSTALL_METADATA_KEYS: &[&str] = &[
    "github-owner",
    "github-repo",
    "github-ref",
    "github-sha",
    "github-tree-sha",
    "github-path",
    "github-pinned",
    "local-path",
];

const KNOWN_FIELDS: &[&str] = &[
    "name",
    "description",
    "license",
    "compatibility",
    "metadata",
    "allowed-tools",
];

const MAX_NAME_LEN: usize = 64;
const MAX_DESCRIPTION_LEN: usize = 1024;
const MAX_COMPATIBILITY_LEN: usize = 500;
const MAX_LINES: usize = 500;
const MAX_DISCOVERY_DEPTH: usize = 8;

#[derive(Default, Clone, Debug)]
pub struct ValidateOptions {
    pub fix: bool,
    pub strict: bool,
    pub json: bool,
    pub help: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Error => "error",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: &'static str,
    pub message: String,
}

fn diag(severity: Severity, code: &'static str, message: String) -> Diagnostic {
    Diagnostic {
        severity,
        code,
        message,
    }
}

#[derive(Clone, Debug)]
pub struct SkillReport {
    /// The frontmatter `name` when it is a string.
    pub name: Option<String>,
    /// SKILL.md path relative to the validated root, `/`-separated.
    pub rel: String,
    pub diagnostics: Vec<Diagnostic>,
}

pub struct PathReport {
    pub display_root: String,
    pub root: String,
    pub skills: Vec<SkillReport>,
    pub repository: Vec<Diagnostic>,
    pub fixed: Vec<String>,
}

impl PathReport {
    fn all_diagnostics(&self) -> impl Iterator<Item = &Diagnostic> {
        self.skills
            .iter()
            .flat_map(|s| s.diagnostics.iter())
            .chain(self.repository.iter())
    }

    pub fn errors(&self) -> usize {
        self.all_diagnostics()
            .filter(|d| d.severity == Severity::Error)
            .count()
    }

    pub fn warnings(&self) -> usize {
        self.all_diagnostics()
            .filter(|d| d.severity == Severity::Warning)
            .count()
    }

    pub fn failed(&self, strict: bool) -> bool {
        self.skills.is_empty() || self.errors() > 0 || (strict && self.warnings() > 0)
    }
}

pub fn get_validate_help() -> &'static str {
    "Usage: skills validate [path...] [options]\n\nCheck skills against the Agent Skills specification (https://agentskills.io/specification).\n\nOptions:\n  --fix                 Remove install tracking metadata from SKILL.md files\n  --strict              Exit with status 1 on warnings as well as errors\n  --json                Output as JSON\n  -h, --help            Show this help message\n\nExamples:\n  skills validate\n  skills validate skills/my-skill --strict"
}

pub fn print_validate_help() {
    outln!("{}", get_validate_help());
}

/// Parse `validate` arguments. Returns (paths, options, errors).
pub fn parse_validate_options(args: &[String]) -> (Vec<String>, ValidateOptions, Vec<String>) {
    let mut paths = Vec::new();
    let mut o = ValidateOptions::default();
    let mut errors = Vec::new();
    for a in args {
        match a.as_str() {
            "" => {}
            "--fix" => o.fix = true,
            "--strict" => o.strict = true,
            "--json" => o.json = true,
            "--help" | "-h" => o.help = true,
            _ if a.starts_with('-') => errors.push(format!("Unknown option: {}", a)),
            _ => paths.push(a.clone()),
        }
    }
    (paths, o, errors)
}

/// `^[a-z0-9]+(-[a-z0-9]+)*$`, at most 64 characters.
pub fn is_valid_skill_name(name: &str) -> bool {
    if name.is_empty() || name.chars().count() > MAX_NAME_LEN {
        return false;
    }
    if name.starts_with('-') || name.ends_with('-') || name.contains("--") {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

/// Content of a line without its `\n` / `\r\n` terminator.
fn line_content(line: &str) -> &str {
    let l = line.strip_suffix('\n').unwrap_or(line);
    l.strip_suffix('\r').unwrap_or(l)
}

/// Split SKILL.md text (BOM already stripped) into (frontmatter YAML, body).
/// The file must start with a `---` line; the first later line that is `---`
/// (trailing whitespace allowed) closes the block.
pub fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    let mut offset = 0;
    let mut yaml_start = None;
    for (i, line) in text.split_inclusive('\n').enumerate() {
        let content = line_content(line);
        if i == 0 {
            if content != "---" || !line.ends_with('\n') {
                return None;
            }
            yaml_start = Some(line.len());
        } else if content.trim_end() == "---" {
            let start = yaml_start?;
            return Some((&text[start..offset], &text[offset + line.len()..]));
        }
        offset += line.len();
    }
    None
}

fn line_count(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let n = text.matches('\n').count();
    if text.ends_with('\n') {
        n
    } else {
        n + 1
    }
}

fn link_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"!?\[[^\]]*\]\(([^)\s]+)(?:\s+"[^"]*")?\)"#).unwrap())
}

fn inline_code_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"`[^`]*`").unwrap())
}

fn fence_marker(line: &str) -> Option<&'static str> {
    let t = line.trim_start();
    if t.starts_with("```") {
        Some("```")
    } else if t.starts_with("~~~") {
        Some("~~~")
    } else {
        None
    }
}

/// Distinct inline link/image targets in a Markdown body, in order of first
/// appearance, ignoring fenced code blocks and inline code spans.
pub fn extract_link_targets(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut fence: Option<&str> = None;
    for raw in body.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(marker) = fence {
            if line.trim_start().starts_with(marker) {
                fence = None;
            }
            continue;
        }
        if let Some(marker) = fence_marker(line) {
            fence = Some(marker);
            continue;
        }
        let stripped = inline_code_regex().replace_all(line, "");
        for c in link_regex().captures_iter(&stripped) {
            let target = c[1].to_string();
            if !out.contains(&target) {
                out.push(target);
            }
        }
    }
    out
}

/// The local path a link target refers to, or `None` for anchors, absolute
/// paths, URLs and other targets that are not checked.
pub fn local_link_path(target: &str) -> Option<String> {
    let lower = target.to_lowercase();
    if target.starts_with('#')
        || target.starts_with('/')
        || target.starts_with('<')
        || target.contains("://")
        || lower.starts_with("mailto:")
        || lower.starts_with("tel:")
        || lower.starts_with("data:")
    {
        return None;
    }
    let cut = target.find(['#', '?']).unwrap_or(target.len());
    let path = &target[..cut];
    let decoded = decode_uri_component(path).unwrap_or_else(|_| path.to_string());
    if decoded.is_empty() {
        None
    } else {
        Some(decoded)
    }
}

fn check_links(body: &str, skill_dir: &str) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for target in extract_link_targets(body) {
        let Some(path) = local_link_path(&target) else {
            continue;
        };
        let resolved = paths::resolve(&[skill_dir, path.as_str()]);
        if !paths::is_path_safe(skill_dir, &resolved) {
            out.push(diag(
                Severity::Warning,
                "link-outside-skill",
                format!(
                    "link to \"{}\" points outside the skill directory",
                    sanitize_metadata(&target)
                ),
            ));
        } else if !std::path::Path::new(&resolved).exists() {
            out.push(diag(
                Severity::Warning,
                "broken-link",
                format!(
                    "link to \"{}\" points to a file that does not exist",
                    sanitize_metadata(&target)
                ),
            ));
        }
    }
    out
}

/// Install tracking keys present under a `metadata` mapping, in report order.
pub fn install_keys_present(fm: &Map<String, Value>) -> Vec<&'static str> {
    match fm.get("metadata") {
        Some(Value::Object(m)) => INSTALL_METADATA_KEYS
            .iter()
            .copied()
            .filter(|k| m.contains_key(*k))
            .collect(),
        _ => Vec::new(),
    }
}

/// Validate one SKILL.md. `dir_name` is the name of its directory, `at_root`
/// whether it sits directly in the validated root, `skill_dir` its directory
/// on disk (for link checks). Returns (name, diagnostics).
pub fn check_skill(
    raw: &str,
    dir_name: &str,
    at_root: bool,
    skill_dir: &str,
) -> (Option<String>, Vec<Diagnostic>) {
    use Severity::*;
    let text = strip_bom(raw);
    let mut d = Vec::new();
    let Some((yaml, body)) = split_frontmatter(text) else {
        d.push(diag(
            Error,
            "frontmatter-missing",
            "SKILL.md has no YAML frontmatter".into(),
        ));
        return (None, d);
    };
    let fm = match parse_yaml(yaml) {
        Ok(Value::Object(m)) => m,
        _ => {
            d.push(diag(
                Error,
                "frontmatter-invalid",
                "frontmatter is not valid YAML".into(),
            ));
            return (None, d);
        }
    };

    let mut name = None;
    match fm.get("name") {
        None | Some(Value::Null) => d.push(diag(
            Error,
            "name-missing",
            "required field \"name\" is missing".into(),
        )),
        Some(Value::String(n)) => {
            let safe = sanitize_metadata(n);
            if !is_valid_skill_name(n) {
                d.push(diag(Error, "name-format", format!("name \"{}\" must be 1-64 lowercase letters, digits and hyphens, with no leading, trailing or consecutive hyphens", safe)));
            }
            if n != dir_name {
                d.push(diag(
                    if at_root { Warning } else { Error },
                    "name-mismatch",
                    format!(
                        "name \"{}\" does not match its directory name \"{}\"",
                        safe,
                        sanitize_metadata(dir_name)
                    ),
                ));
            }
            name = Some(n.clone());
        }
        Some(_) => d.push(diag(Error, "name-type", "\"name\" must be a string".into())),
    }

    match fm.get("description") {
        None | Some(Value::Null) => d.push(diag(
            Error,
            "description-missing",
            "required field \"description\" is missing".into(),
        )),
        Some(Value::String(s)) => {
            let len = s.chars().count();
            if s.trim().is_empty() {
                d.push(diag(
                    Error,
                    "description-empty",
                    "\"description\" must not be empty".into(),
                ));
            } else if len > MAX_DESCRIPTION_LEN {
                d.push(diag(
                    Error,
                    "description-length",
                    format!(
                        "description is {} characters; the maximum is {}",
                        len, MAX_DESCRIPTION_LEN
                    ),
                ));
            }
        }
        Some(_) => d.push(diag(
            Error,
            "description-type",
            "\"description\" must be a string".into(),
        )),
    }

    match fm.get("allowed-tools") {
        None | Some(Value::Null) | Some(Value::String(_)) => {}
        Some(Value::Array(_)) => d.push(diag(
            Error,
            "allowed-tools-type",
            "\"allowed-tools\" must be a space-separated string, not a list".into(),
        )),
        Some(_) => d.push(diag(
            Error,
            "allowed-tools-type",
            "\"allowed-tools\" must be a string".into(),
        )),
    }

    match fm.get("compatibility") {
        None | Some(Value::Null) => {}
        Some(Value::String(s)) => {
            let len = s.chars().count();
            if len > MAX_COMPATIBILITY_LEN {
                d.push(diag(
                    Warning,
                    "compatibility-length",
                    format!(
                        "compatibility is {} characters; the maximum is {}",
                        len, MAX_COMPATIBILITY_LEN
                    ),
                ));
            }
        }
        Some(_) => d.push(diag(
            Warning,
            "compatibility-length",
            "\"compatibility\" must be a string".into(),
        )),
    }

    let metadata = fm.get("metadata");
    let metadata_ok = match metadata {
        None | Some(Value::Null) => true,
        Some(Value::Object(m)) => m.values().all(|v| v.is_string()),
        Some(_) => false,
    };
    if !metadata_ok {
        d.push(diag(
            Warning,
            "metadata-type",
            "\"metadata\" should map string keys to string values".into(),
        ));
    }

    let install = install_keys_present(&fm);
    if !install.is_empty() {
        d.push(diag(
            Warning,
            "install-metadata",
            format!(
                "metadata contains install tracking keys ({}); run \"skills validate --fix\" to remove them",
                install.join(", ")
            ),
        ));
    }

    if body.trim().is_empty() {
        d.push(diag(
            Warning,
            "body-empty",
            "SKILL.md has no instructions after the frontmatter".into(),
        ));
    }

    d.extend(check_links(body, skill_dir));

    for k in fm.keys() {
        if !KNOWN_FIELDS.contains(&k.as_str()) {
            d.push(diag(
                Info,
                "unknown-field",
                format!("unknown frontmatter field \"{}\"", sanitize_metadata(k)),
            ));
        }
    }

    let lines = line_count(raw);
    if lines > MAX_LINES {
        d.push(diag(
            Info,
            "body-long",
            format!(
                "SKILL.md is {} lines; the recommended maximum is {}",
                lines, MAX_LINES
            ),
        ));
    }

    (name, d)
}

fn leading_spaces(s: &str) -> usize {
    s.chars().take_while(|c| *c == ' ' || *c == '\t').count()
}

fn is_blank(s: &str) -> bool {
    s.trim().is_empty()
}

fn yaml_key_of(content: &str) -> String {
    let t = content.trim();
    let key = match t.find(':') {
        Some(i) => t[..i].trim(),
        None => t,
    };
    let unquoted = if key.len() >= 2
        && ((key.starts_with('"') && key.ends_with('"'))
            || (key.starts_with('\'') && key.ends_with('\'')))
    {
        &key[1..key.len() - 1]
    } else {
        key
    };
    unquoted.to_string()
}

/// Remove install tracking keys from a block-style `metadata:` mapping in the
/// frontmatter, touching only those lines (line endings and everything else
/// preserved). Returns `None` when nothing changes.
pub fn strip_install_metadata(raw: &str) -> Option<String> {
    let (bom, text) = match raw.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", raw),
    };
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    if lines.is_empty() || line_content(lines[0]) != "---" {
        return None;
    }
    let close = (1..lines.len()).find(|&i| line_content(lines[i]).trim_end() == "---")?;
    let meta = (1..close).find(|&i| line_content(lines[i]).trim_end() == "metadata:")?;
    let mut block_end = meta + 1;
    while block_end < close {
        let c = line_content(lines[block_end]);
        if !is_blank(c) && leading_spaces(c) == 0 {
            break;
        }
        block_end += 1;
    }
    let block = meta + 1..block_end;
    let first = block.clone().find(|&i| !is_blank(line_content(lines[i])))?;
    let child_indent = leading_spaces(line_content(lines[first]));

    // Child entries: (start, end) line ranges; trailing blank lines are not
    // part of an entry.
    let mut entries: Vec<(usize, usize)> = Vec::new();
    let mut i = block.start;
    while i < block.end {
        let c = line_content(lines[i]);
        if is_blank(c) || leading_spaces(c) != child_indent {
            i += 1;
            continue;
        }
        let start = i;
        let mut end = i + 1;
        let mut j = i + 1;
        while j < block.end {
            let cj = line_content(lines[j]);
            if is_blank(cj) {
                j += 1;
                continue;
            }
            if leading_spaces(cj) > child_indent {
                j += 1;
                end = j;
                continue;
            }
            break;
        }
        entries.push((start, end));
        i = end;
    }

    let mut remove = vec![false; lines.len()];
    let mut removed_any = false;
    for (s, e) in &entries {
        if INSTALL_METADATA_KEYS.contains(&yaml_key_of(line_content(lines[*s])).as_str()) {
            for r in remove.iter_mut().take(*e).skip(*s) {
                *r = true;
            }
            removed_any = true;
        }
    }
    if !removed_any {
        return None;
    }
    let remaining = block
        .clone()
        .any(|i| !remove[i] && !is_blank(line_content(lines[i])));
    if !remaining {
        remove[meta] = true;
        for r in remove.iter_mut().take(block.end).skip(block.start) {
            *r = true;
        }
    }
    let mut out = String::from(bom);
    for (idx, line) in lines.iter().enumerate() {
        if !remove[idx] {
            out.push_str(line);
        }
    }
    Some(out)
}

/// SKILL.md files under `root` (relative, `/`-separated, ordinal order).
/// Hidden directories, `.git` and `node_modules` are skipped; a directory
/// with a SKILL.md is recorded and not descended into.
pub fn discover_skill_files(root: &str) -> Vec<String> {
    fn walk(dir: &str, rel: &str, depth: usize, out: &mut Vec<String>) {
        if has_skill_md(dir) {
            out.push(if rel.is_empty() {
                "SKILL.md".to_string()
            } else {
                format!("{}/SKILL.md", rel)
            });
            return;
        }
        if depth >= MAX_DISCOVERY_DEPTH {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == "node_modules" {
                continue;
            }
            let child_rel = if rel.is_empty() {
                name.clone()
            } else {
                format!("{}/{}", rel, name)
            };
            walk(&join(&[dir, name.as_str()]), &child_rel, depth + 1, out);
        }
    }
    let mut out = Vec::new();
    walk(root, "", 0, &mut out);
    out.sort_by(|a, b| ordinal_cmp(a, b));
    out
}

/// Project agent install directories (`.agents/skills`, `.claude/skills`, …)
/// checked for being gitignored. Only hidden directories count: a plain
/// `skills/` directory is the usual publishing layout.
fn install_dir_candidates() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for a in crate::agents::agents() {
        if a.skills_dir.starts_with('.') && !out.contains(&a.skills_dir) {
            out.push(a.skills_dir);
        }
    }
    out
}

fn contains_installed_skill(dir: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        let p = join(&[dir, e.file_name().to_string_lossy().as_ref()]);
        std::fs::metadata(&p).map(|m| m.is_dir()).unwrap_or(false) && has_skill_md(&p)
    })
}

fn git(root: &str, args: &[&str]) -> Option<crate::proc::Output> {
    let mut full = vec!["-C", root];
    full.extend_from_slice(args);
    crate::proc::command("git", &full)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .timeout(std::time::Duration::from_secs(30))
        .output()
        .ok()
}

fn repository_checks(root: &str) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let mut in_work_tree: Option<bool> = None;
    for dir in install_dir_candidates() {
        let full = join(&[root, dir]);
        if !contains_installed_skill(&full) {
            continue;
        }
        let inside = *in_work_tree.get_or_insert_with(|| {
            git(root, &["rev-parse", "--is-inside-work-tree"])
                .map(|o| o.success() && o.stdout_str().trim() == "true")
                .unwrap_or(false)
        });
        if !inside {
            break;
        }
        let ignored = git(root, &["check-ignore", "-q", dir])
            .map(|o| o.success())
            .unwrap_or(true);
        if !ignored {
            out.push(diag(
                Severity::Warning,
                "installed-skills-not-ignored",
                format!("{}/ contains installed skills but is not gitignored; add it to .gitignore so other authors' skills are not published", dir),
            ));
        }
    }
    out
}

/// Validate one path argument (after `--fix`, when requested).
pub fn validate_path(display: &str, fix: bool) -> Result<PathReport, String> {
    let abs = paths::resolve1(display);
    let meta = std::fs::metadata(&abs).map_err(|_| format!("Path does not exist: {}", display))?;
    let (root, files, is_dir) = if meta.is_dir() {
        (abs.clone(), discover_skill_files(&abs), true)
    } else if paths::basename(&abs).eq_ignore_ascii_case("SKILL.md") {
        (paths::dirname(&abs), vec![paths::basename(&abs)], false)
    } else {
        return Err(format!(
            "Not a skill directory or SKILL.md file: {}",
            display
        ));
    };

    let mut fixed = Vec::new();
    if fix {
        for rel in &files {
            let full = join(&[root.as_str(), rel.as_str()]);
            let Ok(raw) = crate::skills::read_utf8_lossy(&full) else {
                continue;
            };
            let has_keys = split_frontmatter(strip_bom(&raw))
                .and_then(|(yaml, _)| match parse_yaml(yaml) {
                    Ok(Value::Object(m)) => Some(!install_keys_present(&m).is_empty()),
                    _ => None,
                })
                .unwrap_or(false);
            if !has_keys {
                continue;
            }
            if let Some(new_text) = strip_install_metadata(&raw) {
                if new_text != raw && std::fs::write(&full, new_text).is_ok() {
                    fixed.push(rel.clone());
                }
            }
        }
    }

    let root_name = paths::basename(&root);
    let mut skills: Vec<SkillReport> = Vec::new();
    for rel in &files {
        let full = join(&[root.as_str(), rel.as_str()]);
        let skill_dir = paths::dirname(&full);
        let at_root = !rel.contains('/');
        let dir_name = if at_root {
            root_name.clone()
        } else {
            rel.rsplit('/').nth(1).unwrap_or("").to_string()
        };
        let raw = crate::skills::read_utf8_lossy(&full).unwrap_or_default();
        let (name, diagnostics) = check_skill(&raw, &dir_name, at_root, &skill_dir);
        skills.push(SkillReport {
            name,
            rel: rel.clone(),
            diagnostics,
        });
    }
    add_duplicate_name_diagnostics(&mut skills);

    let repository = if is_dir && !skills.is_empty() {
        repository_checks(&root)
    } else {
        Vec::new()
    };
    Ok(PathReport {
        display_root: display.to_string(),
        root,
        skills,
        repository,
        fixed,
    })
}

pub fn add_duplicate_name_diagnostics(skills: &mut [SkillReport]) {
    let names: Vec<(Option<String>, String)> = skills
        .iter()
        .map(|s| (s.name.clone(), s.rel.clone()))
        .collect();
    for s in skills.iter_mut() {
        let Some(n) = &s.name else { continue };
        let others: Vec<&str> = names
            .iter()
            .filter(|(on, rel)| on.as_deref() == Some(n.as_str()) && *rel != s.rel)
            .map(|(_, rel)| rel.as_str())
            .collect();
        if !others.is_empty() {
            s.diagnostics.push(diag(
                Severity::Error,
                "duplicate-name",
                format!(
                    "name \"{}\" is also used by {}",
                    sanitize_metadata(n),
                    others
                        .iter()
                        .map(|r| sanitize_metadata(r))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
    }
}

fn severity_label(s: Severity) -> String {
    match s {
        Severity::Error => pc::red("error"),
        Severity::Warning => format!("{}warning{}", YELLOW, RESET),
        Severity::Info => format!("{}info{}", DIM, RESET),
    }
}

fn status_symbol(worst: Option<Severity>) -> String {
    match worst {
        Some(Severity::Error) => pc::red("✗"),
        Some(Severity::Warning) => format!("{}⚠{}", YELLOW, RESET),
        _ => format!("{}✓{}", TEXT, RESET),
    }
}

fn diagnostic_line(d: &Diagnostic) -> String {
    format!(
        "    {} {}: {}",
        severity_label(d.severity),
        d.code,
        d.message
    )
}

pub fn render_text(r: &PathReport) -> String {
    let mut lines: Vec<String> = Vec::new();
    if !r.fixed.is_empty() {
        for f in &r.fixed {
            lines.push(format!(
                "Removed install metadata from {}",
                sanitize_metadata(f)
            ));
        }
        lines.push(String::new());
    }
    lines.push(format!("Validating skills in {}", r.display_root));
    lines.push(String::new());
    if r.skills.is_empty() {
        lines.push("No skills found.".into());
        return lines.join("\n") + "\n";
    }
    for s in &r.skills {
        let worst = s.diagnostics.iter().map(|d| d.severity).max();
        let name = s
            .name
            .as_deref()
            .map(sanitize_metadata)
            .unwrap_or_else(|| "<no name>".into());
        lines.push(format!(
            "{} {} {}{}{}",
            status_symbol(worst),
            name,
            DIM,
            sanitize_metadata(&s.rel),
            RESET
        ));
        lines.extend(s.diagnostics.iter().map(diagnostic_line));
    }
    lines.push(String::new());
    if !r.repository.is_empty() {
        lines.push("Repository".into());
        lines.extend(r.repository.iter().map(diagnostic_line));
        lines.push(String::new());
    }
    lines.push(format!(
        "Checked {} skill(s): {} error(s), {} warning(s)",
        r.skills.len(),
        r.errors(),
        r.warnings()
    ));
    lines.join("\n") + "\n"
}

fn diagnostic_json(d: &Diagnostic) -> Value {
    let mut o = Map::new();
    o.insert("severity".into(), Value::String(d.severity.as_str().into()));
    o.insert("code".into(), Value::String(d.code.into()));
    o.insert("message".into(), Value::String(d.message.clone()));
    Value::Object(o)
}

pub fn render_json_value(r: &PathReport) -> Value {
    let mut o = Map::new();
    o.insert("root".into(), Value::String(r.root.clone()));
    o.insert(
        "skills".into(),
        Value::Array(
            r.skills
                .iter()
                .map(|s| {
                    let mut e = Map::new();
                    e.insert(
                        "name".into(),
                        s.name.clone().map(Value::String).unwrap_or(Value::Null),
                    );
                    e.insert("path".into(), Value::String(s.rel.clone()));
                    e.insert(
                        "diagnostics".into(),
                        Value::Array(s.diagnostics.iter().map(diagnostic_json).collect()),
                    );
                    Value::Object(e)
                })
                .collect(),
        ),
    );
    o.insert(
        "repository".into(),
        Value::Array(r.repository.iter().map(diagnostic_json).collect()),
    );
    o.insert(
        "fixed".into(),
        Value::Array(r.fixed.iter().cloned().map(Value::String).collect()),
    );
    o.insert("errors".into(), Value::from(r.errors()));
    o.insert("warnings".into(), Value::from(r.warnings()));
    Value::Object(o)
}

pub fn run_validate(args: &[String]) {
    let (mut targets, o, errors) = parse_validate_options(args);
    if o.help {
        print_validate_help();
        return;
    }
    if !errors.is_empty() {
        errln!("{}", errors.join("\n"));
        sys::exit(1);
    }
    if targets.is_empty() {
        targets.push(".".into());
    }
    for t in &targets {
        if std::fs::metadata(paths::resolve1(t)).is_err() {
            errln!("Path does not exist: {}", t);
            sys::exit(1);
        }
    }
    let mut reports = Vec::new();
    for t in &targets {
        match validate_path(t, o.fix) {
            Ok(r) => reports.push(r),
            Err(e) => {
                errln!("{}", e);
                sys::exit(1);
            }
        }
    }
    if o.json {
        let value = if reports.len() == 1 {
            render_json_value(&reports[0])
        } else {
            Value::Array(reports.iter().map(render_json_value).collect())
        };
        outln!("{}", serde_json::to_string_pretty(&value).unwrap());
    } else {
        let blocks: Vec<String> = reports.iter().map(render_text).collect();
        sys::write_out(&blocks.join("\n"));
    }
    if reports.iter().any(|r| r.failed(o.strict)) {
        sys::set_exit_code(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(d: &[Diagnostic]) -> Vec<&'static str> {
        d.iter().map(|x| x.code).collect()
    }

    #[test]
    fn parses_validate_options() {
        let (p, o, e) = parse_validate_options(&["a".into(), "--fix".into(), "--json".into()]);
        assert_eq!(p, vec!["a"]);
        assert!(o.fix && o.json && !o.strict && e.is_empty());
        let (_, _, e) = parse_validate_options(&["--nope".into()]);
        assert_eq!(e, vec!["Unknown option: --nope"]);
    }

    #[test]
    fn name_rule() {
        assert!(is_valid_skill_name("a"));
        assert!(is_valid_skill_name("pdf-tools2"));
        assert!(!is_valid_skill_name(""));
        assert!(!is_valid_skill_name("-a"));
        assert!(!is_valid_skill_name("a-"));
        assert!(!is_valid_skill_name("a--b"));
        assert!(!is_valid_skill_name("Abc"));
        assert!(!is_valid_skill_name("a_b"));
        assert!(is_valid_skill_name(&"a".repeat(64)));
        assert!(!is_valid_skill_name(&"a".repeat(65)));
    }

    #[test]
    fn splits_frontmatter() {
        assert_eq!(
            split_frontmatter("---\nname: x\n---\nbody"),
            Some(("name: x\n", "body"))
        );
        assert_eq!(
            split_frontmatter("---\r\nname: x\r\n---  \r\nbody\r\n"),
            Some(("name: x\r\n", "body\r\n"))
        );
        assert_eq!(split_frontmatter("---\n---\n"), Some(("", "")));
        assert_eq!(split_frontmatter("# no fm\n"), None);
        assert_eq!(split_frontmatter("---\nname: x\n"), None);
        assert_eq!(split_frontmatter("--- \nname: x\n---\n"), None);
    }

    #[test]
    fn checks_fields() {
        let (n, d) = check_skill(
            "---\nname: Bad_Name\ndescription: ''\nallowed-tools: [a]\nextra: 1\nmetadata:\n  github-repo: x\n  n: 1\n---\n",
            "bad",
            false,
            ".",
        );
        assert_eq!(n.as_deref(), Some("Bad_Name"));
        assert_eq!(
            codes(&d),
            vec![
                "name-format",
                "name-mismatch",
                "description-empty",
                "allowed-tools-type",
                "metadata-type",
                "install-metadata",
                "body-empty",
                "unknown-field"
            ]
        );
        let (_, d) = check_skill("---\nname: x\ndescription: d\n---\nBody\n", "y", true, ".");
        assert_eq!(codes(&d), vec!["name-mismatch"]);
        assert_eq!(d[0].severity, Severity::Warning);
        let (_, d) = check_skill("\u{feff}---\n: [\n---\nx", "x", false, ".");
        assert_eq!(codes(&d), vec!["frontmatter-invalid"]);
        let (_, d) = check_skill("no frontmatter", "x", false, ".");
        assert_eq!(codes(&d), vec!["frontmatter-missing"]);
    }

    #[test]
    fn extracts_links() {
        let body = "See [a](ref/a.md) and ![img](img.png \"t\").\n```\n[x](in-fence.md)\n```\nUse `[y](code.md)` or [a](ref/a.md) [w](https://x.y) [h](#top)\n";
        assert_eq!(
            extract_link_targets(body),
            vec!["ref/a.md", "img.png", "https://x.y", "#top"]
        );
        assert_eq!(local_link_path("https://x.y"), None);
        assert_eq!(local_link_path("#top"), None);
        assert_eq!(local_link_path("MAILTO:a@b"), None);
        assert_eq!(local_link_path("a%20b.md#s"), Some("a b.md".into()));
        assert_eq!(local_link_path("a%zz.md?x"), Some("a%zz.md".into()));
        assert_eq!(local_link_path("?q"), None);
    }

    #[test]
    fn strips_install_metadata() {
        let src = "---\nname: x\nmetadata:\n  github-repo: https://github.com/o/r\n  author: me\n  github-path: >-\n    skills/x\n  local-path: /tmp\n---\nbody\n";
        assert_eq!(
            strip_install_metadata(src).unwrap(),
            "---\nname: x\nmetadata:\n  author: me\n---\nbody\n"
        );
        let crlf = "---\r\nname: x\r\nmetadata:\r\n  github-repo: r\r\n  github-ref: refs/heads/main\r\n\r\ndescription: d\r\n---\r\n";
        assert_eq!(
            strip_install_metadata(crlf).unwrap(),
            "---\r\nname: x\r\ndescription: d\r\n---\r\n"
        );
        assert_eq!(
            strip_install_metadata("---\nmetadata: {github-repo: x}\n---\n"),
            None
        );
        assert_eq!(
            strip_install_metadata("---\nmetadata:\n  author: me\n---\n"),
            None
        );
        let bom = "\u{feff}---\nmetadata:\n    \"github-sha\": abc\n    k: v\n---\n";
        assert_eq!(
            strip_install_metadata(bom).unwrap(),
            "\u{feff}---\nmetadata:\n    k: v\n---\n"
        );
    }

    #[test]
    fn duplicate_names() {
        let mut skills = vec![
            SkillReport {
                name: Some("a".into()),
                rel: "x/SKILL.md".into(),
                diagnostics: vec![],
            },
            SkillReport {
                name: Some("a".into()),
                rel: "y/SKILL.md".into(),
                diagnostics: vec![],
            },
            SkillReport {
                name: None,
                rel: "z/SKILL.md".into(),
                diagnostics: vec![],
            },
        ];
        add_duplicate_name_diagnostics(&mut skills);
        assert_eq!(
            skills[0].diagnostics[0].message,
            "name \"a\" is also used by y/SKILL.md"
        );
        assert_eq!(skills[1].diagnostics.len(), 1);
        assert!(skills[2].diagnostics.is_empty());
    }
}
