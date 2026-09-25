//! Skill installation (symlink/copy) and installed-skill listing
//! (port of installer.ts).

use crate::agents::{
    self, agent, detect_installed_agents, get_eve_subagents, is_universal_agent, AgentType,
};
use crate::frontmatter::{parse_frontmatter, stringify_yaml};
use crate::git::remove_all;
use crate::paths::{self, join};
use crate::skills::parse_skill_md;
use crate::sys;
use crate::types::{Skill, SnapshotFile, AGENTS_DIR, SKILLS_SUBDIR};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallMode {
    Symlink,
    Copy,
}

impl InstallMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            InstallMode::Symlink => "symlink",
            InstallMode::Copy => "copy",
        }
    }
}

#[derive(Clone, Default)]
pub struct InstallOptions {
    pub global: bool,
    pub cwd: Option<String>,
    pub mode: Option<InstallMode>,
    pub eve_subagent: Option<String>,
    /// Create a missing agent-specific project root because the user selected this agent.
    pub create_missing_agent_root: bool,
}

#[derive(Clone, Debug)]
pub struct InstallResult {
    pub success: bool,
    pub path: String,
    pub canonical_path: Option<String>,
    pub mode: InstallMode,
    pub symlink_failed: bool,
    pub skipped: bool,
    pub skip_reason: Option<&'static str>,
    pub error: Option<String>,
}

impl InstallResult {
    fn ok(path: &str, canonical: Option<&str>, mode: InstallMode) -> Self {
        InstallResult {
            success: true,
            path: path.to_string(),
            canonical_path: canonical.map(|s| s.to_string()),
            mode,
            symlink_failed: false,
            skipped: false,
            skip_reason: None,
            error: None,
        }
    }
    fn fail(path: &str, mode: InstallMode, error: String) -> Self {
        InstallResult {
            success: false,
            path: path.to_string(),
            canonical_path: None,
            mode,
            symlink_failed: false,
            skipped: false,
            skip_reason: None,
            error: Some(error),
        }
    }
}

/// Sanitize a skill name into a safe kebab-case directory name.
pub fn sanitize_name(name: &str) -> String {
    let lower = name.to_lowercase();
    let mut out = String::new();
    let mut in_run = false;
    for c in lower.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_' {
            out.push(c);
            in_run = false;
        } else if !in_run {
            out.push('-');
            in_run = true;
        }
    }
    let trimmed = out.trim_matches(|c| c == '.' || c == '-');
    // substring(0, 255) — the result is ASCII so byte slicing is safe.
    let limited: String = trimmed.chars().take(255).collect();
    if limited.is_empty() {
        "unnamed-skill".to_string()
    } else {
        limited
    }
}

fn is_path_safe(base: &str, target: &str) -> bool {
    paths::is_path_safe(base, target)
}

fn paths_overlap(a: &str, b: &str) -> bool {
    is_path_safe(a, b) || is_path_safe(b, a)
}

fn exists(p: &str) -> bool {
    Path::new(p).exists()
}

fn should_skip_project_agent_symlink(
    agent_type: AgentType,
    is_global: bool,
    cwd: &str,
    create_missing: bool,
) -> bool {
    let a = agent(agent_type);
    if is_global
        || is_universal_agent(agent_type)
        || create_missing
        || a.create_project_skills_dir_by_default
    {
        return false;
    }
    let root = a.skills_dir.split('/').next().unwrap_or("");
    !exists(&join(&[cwd, root]))
}

fn is_dir_entry_or_symlink_to_dir(entry: &std::fs::DirEntry, path: &str) -> bool {
    match entry.file_type() {
        Ok(t) if t.is_dir() => true,
        Ok(t) if t.is_symlink() => std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false),
        _ => false,
    }
}

fn base_dir(global: bool, cwd: Option<&str>) -> String {
    if global {
        sys::homedir()
    } else {
        cwd.filter(|c| !c.is_empty())
            .map(|c| c.to_string())
            .unwrap_or_else(sys::cwd)
    }
}

pub fn get_canonical_skills_dir(global: bool, cwd: Option<&str>) -> String {
    join(&[base_dir(global, cwd).as_str(), AGENTS_DIR, SKILLS_SUBDIR])
}

pub fn get_eve_subagent_skills_dir(subagent: &str, cwd: Option<&str>) -> String {
    let base = base_dir(false, cwd);
    join(&[
        base.as_str(),
        agents::eve_subagents_dir().as_str(),
        sanitize_name(subagent).as_str(),
        "skills",
    ])
}

pub fn get_agent_base_dir(
    agent_type: AgentType,
    global: bool,
    cwd: Option<&str>,
    eve_subagent: Option<&str>,
) -> String {
    if is_universal_agent(agent_type) {
        return get_canonical_skills_dir(global, cwd);
    }
    if agent_type == "eve" {
        if let Some(sub) = eve_subagent.filter(|s| !s.is_empty()) {
            return get_eve_subagent_skills_dir(sub, cwd);
        }
    }
    let a = agent(agent_type);
    let base = base_dir(global, cwd);
    if global {
        return match &a.global_skills_dir {
            Some(g) => g.clone(),
            None => join(&[base.as_str(), a.skills_dir]),
        };
    }
    join(&[base.as_str(), a.skills_dir])
}

fn clean_and_create_directory(path: &str) -> std::io::Result<()> {
    forget_populated(path);
    let _ = remove_all(path);
    std::fs::create_dir_all(path)
}

// ─── Populated directories ───
//
// The TS installer cleans and re-copies the canonical directory once per
// target agent, and every universal agent shares that directory, so a
// 20-agent install copies each skill 20 times. Remember which directories
// this run already filled from which source and skip identical refills: the
// resulting tree is the same, only the redundant I/O is gone.

#[derive(Clone, PartialEq, Debug)]
enum PopulatedFrom {
    /// Resolved source directory; whether Eve frontmatter was rewritten.
    Dir(String, bool),
    /// Identity (address, length) of the in-memory file list; Eve flag.
    Files(usize, usize, bool),
}

static POPULATED: Mutex<Option<HashMap<String, PopulatedFrom>>> = Mutex::new(None);

/// Start a new install run: forget every directory populated so far.
pub fn reset_populated() {
    *POPULATED.lock().unwrap() = None;
}

fn forget_populated(dir: &str) {
    if let Some(m) = POPULATED.lock().unwrap().as_mut() {
        m.remove(&paths::resolve1(dir));
    }
}

fn already_populated(dir: &str, from: &PopulatedFrom) -> bool {
    let hit = POPULATED
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(&paths::resolve1(dir)))
        .map(|prev| prev == from)
        .unwrap_or(false);
    hit && Path::new(dir).is_dir()
}

fn mark_populated(dir: &str, from: PopulatedFrom) {
    POPULATED
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(paths::resolve1(dir), from);
}

/// Clean `dest` and copy the skill directory `src` into it, unless this run
/// already did exactly that.
fn populate_from_directory(
    dest: &str,
    src: &str,
    agent_type: Option<AgentType>,
) -> std::io::Result<()> {
    let from = PopulatedFrom::Dir(paths::resolve1(src), agent_type == Some("eve"));
    if already_populated(dest, &from) {
        return Ok(());
    }
    clean_and_create_directory(dest)?;
    copy_directory(src, dest, agent_type)?;
    mark_populated(dest, from);
    Ok(())
}

/// Clean `dest` and write the in-memory `files` into it, unless this run
/// already did exactly that.
fn populate_from_files(
    dest: &str,
    files: &[SnapshotFile],
    agent_type: AgentType,
) -> std::io::Result<()> {
    let from = PopulatedFrom::Files(files.as_ptr() as usize, files.len(), agent_type == "eve");
    if already_populated(dest, &from) {
        return Ok(());
    }
    clean_and_create_directory(dest)?;
    write_files(dest, files, agent_type)?;
    mark_populated(dest, from);
    Ok(())
}

/// Run `f` over `items`, on a small worker pool when there are enough of them.
/// Stops at, and returns, the first error.
fn for_each_parallel<T: Sync>(
    items: &[T],
    f: impl Fn(&T) -> std::io::Result<()> + Sync,
) -> std::io::Result<()> {
    if items.len() < 16 {
        return items.iter().try_for_each(f);
    }
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(2, 16);
    let next = AtomicUsize::new(0);
    let first_err: Mutex<Option<std::io::Error>> = Mutex::new(None);
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                if first_err.lock().unwrap().is_some() {
                    break;
                }
                let Some(item) = items.get(next.fetch_add(1, Ordering::Relaxed)) else {
                    break;
                };
                if let Err(e) = f(item) {
                    first_err.lock().unwrap().get_or_insert(e);
                    break;
                }
            });
        }
    });
    match first_err.into_inner().unwrap() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

fn realpath(p: &str) -> Option<String> {
    let r = std::fs::canonicalize(p).ok()?;
    Some(strip_verbatim(&r.to_string_lossy()))
}

/// Remove the `\\?\` prefix Windows canonicalization adds.
fn strip_verbatim(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("\\\\?\\UNC\\") {
        return format!("\\\\{}", rest);
    }
    if let Some(rest) = p.strip_prefix("\\\\?\\") {
        return rest.to_string();
    }
    if let Some(rest) = p.strip_prefix("\\??\\") {
        return rest.to_string();
    }
    p.to_string()
}

fn resolve_parent_symlinks(p: &str) -> String {
    let resolved = paths::resolve1(p);
    let dir = paths::dirname(&resolved);
    let base = paths::basename(&resolved);
    match realpath(&dir) {
        Some(real) => join(&[real.as_str(), base.as_str()]),
        None => resolved,
    }
}

fn make_link(target: &str, link: &str, relative_target: &str) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        let _ = relative_target;
        junction::create(target, link)
    }
    #[cfg(not(windows))]
    {
        let _ = target;
        std::os::unix::fs::symlink(relative_target, link)
    }
}

/// Create a symlink (a junction on Windows). Returns `false` when the caller
/// should fall back to copying.
fn create_symlink(target: &str, link_path: &str) -> bool {
    let resolved_target = paths::resolve1(target);
    let resolved_link = paths::resolve1(link_path);
    let real_target = realpath(&resolved_target).unwrap_or_else(|| resolved_target.clone());
    let real_link = realpath(&resolved_link).unwrap_or_else(|| resolved_link.clone());
    if real_target == real_link {
        return true;
    }
    if resolve_parent_symlinks(target) == resolve_parent_symlinks(link_path) {
        return true;
    }
    forget_populated(link_path);
    if let Ok(m) = std::fs::symlink_metadata(link_path) {
        if m.file_type().is_symlink() {
            if let Ok(existing) = std::fs::read_link(link_path) {
                let existing = strip_verbatim(&existing.to_string_lossy());
                if paths::resolve(&[paths::dirname(link_path).as_str(), existing.as_str()])
                    == resolved_target
                {
                    return true;
                }
            }
        }
        if remove_all(link_path).is_err() {
            return false;
        }
    }
    let link_dir = paths::dirname(link_path);
    if std::fs::create_dir_all(&link_dir).is_err() {
        return false;
    }
    let real_link_dir = resolve_parent_symlinks(&link_dir);
    let relative_path = paths::relative(&real_link_dir, target);
    make_link(&resolved_target, link_path, &relative_path).is_ok()
}

fn is_excluded(name: &str, is_dir: bool) -> bool {
    name == "metadata.json" || (is_dir && matches!(name, ".git" | "__pycache__" | "__pypackages__"))
}

/// Keep only the frontmatter fields Eve understands.
pub fn strip_ignored_eve_frontmatter(raw: &str) -> String {
    let (data, content) = match parse_frontmatter(raw) {
        Ok(fm) => (fm.data, fm.content),
        Err(_) => (Map::new(), raw.to_string()),
    };
    let mut eve = Map::new();
    if let Some(Value::String(s)) = data.get("name") {
        eve.insert("name".into(), Value::String(s.clone()));
    }
    if let Some(Value::String(s)) = data.get("description") {
        eve.insert("description".into(), Value::String(s.clone()));
    }
    if let Some(Value::String(s)) = data.get("license") {
        eve.insert("license".into(), Value::String(s.clone()));
    }
    if let Some(v) = data.get("compatibility") {
        eve.insert("compatibility".into(), v.clone());
    }
    if let Some(v @ (Value::String(_) | Value::Number(_))) = data.get("version") {
        eve.insert("version".into(), v.clone());
    }
    if let Some(v @ Value::Object(_)) = data.get("metadata") {
        eve.insert("metadata".into(), v.clone());
    }
    let body = content
        .strip_prefix("\r\n")
        .or_else(|| content.strip_prefix('\n'))
        .unwrap_or(&content)
        .to_string();
    if eve.is_empty() {
        return body;
    }
    let fm = stringify_yaml(&Value::Object(eve));
    format!("---\n{}\n---\n{}", fm.trim_end(), body)
}

fn copy_file_preserving_mode(src: &str, dest: &str) -> std::io::Result<()> {
    std::fs::copy(src, dest)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(src)?.permissions().mode() & 0o777;
        std::fs::set_permissions(dest, std::fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

/// Recursive copy of a directory following symlinks, with no exclusions
/// (Node's `cp(..., { dereference: true, recursive: true })`). Directories are
/// created now; files are queued in `files`.
fn cp_dereference(src: &str, dest: &str, files: &mut Vec<(String, String)>) -> std::io::Result<()> {
    let meta = std::fs::metadata(src)?;
    if meta.is_dir() {
        std::fs::create_dir_all(dest)?;
        for e in std::fs::read_dir(src)? {
            let e = e?;
            let name = e.file_name().to_string_lossy().to_string();
            cp_dereference(
                &join(&[src, name.as_str()]),
                &join(&[dest, name.as_str()]),
                files,
            )?;
        }
    } else {
        files.push((src.to_string(), dest.to_string()));
    }
    Ok(())
}

/// Copy a skill directory. The tree walk (exclusions, links, Eve rewrites,
/// directory creation) is sequential; file copies then run in parallel, as
/// the TS implementation's concurrent `copyDirectory` does.
fn copy_directory(src: &str, dest: &str, agent_type: Option<AgentType>) -> std::io::Result<()> {
    let mut files = Vec::new();
    collect_directory(src, dest, agent_type, &mut files)?;
    for_each_parallel(&files, |(s, d)| copy_file_preserving_mode(s, d))
}

fn collect_directory(
    src: &str,
    dest: &str,
    agent_type: Option<AgentType>,
    files: &mut Vec<(String, String)>,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        let ft = entry.file_type()?;
        if is_excluded(&name, ft.is_dir()) {
            continue;
        }
        let src_path = join(&[src, name.as_str()]);
        let dest_path = join(&[dest, name.as_str()]);
        if ft.is_dir() {
            collect_directory(&src_path, &dest_path, agent_type, files)?;
            continue;
        }
        if agent_type == Some("eve") && name.to_lowercase() == "skill.md" {
            let content = crate::skills::read_utf8_lossy(&src_path)?;
            std::fs::write(&dest_path, strip_ignored_eve_frontmatter(&content))?;
            continue;
        }
        match cp_dereference(&src_path, &dest_path, files) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && ft.is_symlink() => {
                crate::errln!("Skipping broken symlink: {}", src_path);
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn to_eve_flat_skill_file_name(install_name: &str) -> String {
    format!("{}.md", sanitize_name(install_name))
}

fn get_eve_flat_skill_markdown(files: &[SnapshotFile]) -> String {
    if let Some(f) = files
        .iter()
        .find(|f| paths::basename(&f.path).to_lowercase() == "skill.md")
    {
        return strip_ignored_eve_frontmatter(&String::from_utf8_lossy(&f.contents));
    }
    if let Some(f) = files
        .iter()
        .find(|f| paths::extname(&f.path).to_lowercase() == ".md")
    {
        return strip_ignored_eve_frontmatter(&String::from_utf8_lossy(&f.contents));
    }
    String::new()
}

pub fn is_skill_installed(
    skill_name: &str,
    agent_type: AgentType,
    global: bool,
    cwd: Option<&str>,
    eve_subagent: Option<&str>,
) -> bool {
    let a = agent(agent_type);
    let sanitized = sanitize_name(skill_name);
    if global && a.global_skills_dir.is_none() {
        return false;
    }
    let target_base = if global {
        a.global_skills_dir.clone().unwrap()
    } else if agent_type == "eve" && eve_subagent.map(|s| !s.is_empty()).unwrap_or(false) {
        get_eve_subagent_skills_dir(eve_subagent.unwrap(), cwd)
    } else {
        join(&[base_dir(false, cwd).as_str(), a.skills_dir])
    };
    let skill_dir = join(&[target_base.as_str(), sanitized.as_str()]);
    if !is_path_safe(&target_base, &skill_dir) {
        return false;
    }
    exists(&skill_dir)
}

pub fn get_install_path(
    skill_name: &str,
    agent_type: AgentType,
    global: bool,
    cwd: Option<&str>,
    eve_subagent: Option<&str>,
) -> Result<String, String> {
    let sanitized = sanitize_name(skill_name);
    let base = get_agent_base_dir(agent_type, global, cwd, eve_subagent);
    let p = join(&[base.as_str(), sanitized.as_str()]);
    if !is_path_safe(&base, &p) {
        return Err("Invalid skill name: potential path traversal detected".into());
    }
    Ok(p)
}

/// The canonical `.agents/skills/<skill>` path (Eve uses its own dir).
pub fn get_canonical_path(
    skill_name: &str,
    global: bool,
    cwd: Option<&str>,
    agent_type: Option<AgentType>,
    eve_subagent: Option<&str>,
) -> Result<String, String> {
    let sanitized = sanitize_name(skill_name);
    let base = if agent_type == Some("eve") {
        get_agent_base_dir("eve", global, cwd, eve_subagent)
    } else {
        get_canonical_skills_dir(global, cwd)
    };
    let p = join(&[base.as_str(), sanitized.as_str()]);
    if !is_path_safe(&base, &p) {
        return Err("Invalid skill name: potential path traversal detected".into());
    }
    Ok(p)
}

struct Dirs {
    canonical_base: String,
    canonical_dir: String,
    agent_base: String,
    agent_dir: String,
}

fn compute_dirs(
    skill_name: &str,
    agent_type: AgentType,
    is_global: bool,
    cwd: &str,
    mode: InstallMode,
    eve_subagent: Option<&str>,
) -> Dirs {
    let canonical_base = if agent_type == "eve" && mode == InstallMode::Symlink {
        get_agent_base_dir(agent_type, is_global, Some(cwd), eve_subagent)
    } else {
        get_canonical_skills_dir(is_global, Some(cwd))
    };
    let canonical_dir = join(&[canonical_base.as_str(), skill_name]);
    let agent_base = get_agent_base_dir(agent_type, is_global, Some(cwd), eve_subagent);
    let agent_dir = join(&[agent_base.as_str(), skill_name]);
    Dirs {
        canonical_base,
        canonical_dir,
        agent_base,
        agent_dir,
    }
}

fn traversal_error(d: &Dirs, mode: InstallMode) -> Option<InstallResult> {
    if !is_path_safe(&d.canonical_base, &d.canonical_dir)
        || !is_path_safe(&d.agent_base, &d.agent_dir)
    {
        return Some(InstallResult::fail(
            &d.agent_dir,
            mode,
            "Invalid skill name: potential path traversal detected".into(),
        ));
    }
    None
}

fn no_global_support(agent_type: AgentType, mode: InstallMode) -> InstallResult {
    InstallResult::fail(
        "",
        mode,
        format!(
            "{} does not support global skill installation",
            agent(agent_type).display_name
        ),
    )
}

fn io_err(e: std::io::Error) -> String {
    e.to_string()
}

/// Install a skill from a directory on disk.
pub fn install_skill_for_agent(
    skill: &Skill,
    agent_type: AgentType,
    options: &InstallOptions,
) -> InstallResult {
    let a = agent(agent_type);
    let is_global = options.global;
    let cwd = options
        .cwd
        .clone()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(sys::cwd);
    let mode = options.mode.unwrap_or(InstallMode::Symlink);
    if is_global && a.global_skills_dir.is_none() {
        return no_global_support(agent_type, mode);
    }
    let raw_name = if skill.name.is_empty() {
        paths::basename(&skill.path)
    } else {
        skill.name.clone()
    };
    let skill_name = sanitize_name(&raw_name);
    let d = compute_dirs(
        &skill_name,
        agent_type,
        is_global,
        &cwd,
        mode,
        options.eve_subagent.as_deref(),
    );
    if let Some(e) = traversal_error(&d, mode) {
        return e;
    }

    let run = || -> Result<InstallResult, String> {
        if paths_overlap(&skill.path, &d.agent_dir) {
            let mut r = InstallResult::ok(&d.agent_dir, None, mode);
            r.skipped = true;
            return Ok(r);
        }
        if mode == InstallMode::Copy {
            populate_from_directory(&d.agent_dir, &skill.path, Some(agent_type)).map_err(io_err)?;
            return Ok(InstallResult::ok(&d.agent_dir, None, InstallMode::Copy));
        }
        if paths_overlap(&skill.path, &d.canonical_dir) {
            let mut r = InstallResult::ok(
                &d.canonical_dir,
                Some(&d.canonical_dir),
                InstallMode::Symlink,
            );
            r.skipped = true;
            return Ok(r);
        }
        populate_from_directory(&d.canonical_dir, &skill.path, Some(agent_type)).map_err(io_err)?;
        if is_global && is_universal_agent(agent_type) {
            return Ok(InstallResult::ok(
                &d.canonical_dir,
                Some(&d.canonical_dir),
                InstallMode::Symlink,
            ));
        }
        if should_skip_project_agent_symlink(
            agent_type,
            is_global,
            &cwd,
            options.create_missing_agent_root,
        ) {
            let mut r = InstallResult::ok(
                &d.canonical_dir,
                Some(&d.canonical_dir),
                InstallMode::Symlink,
            );
            r.skipped = true;
            r.skip_reason = Some("missing-agent-project-directory");
            return Ok(r);
        }
        if !create_symlink(&d.canonical_dir, &d.agent_dir) {
            populate_from_directory(&d.agent_dir, &skill.path, Some(agent_type)).map_err(io_err)?;
            let mut r =
                InstallResult::ok(&d.agent_dir, Some(&d.canonical_dir), InstallMode::Symlink);
            r.symlink_failed = true;
            return Ok(r);
        }
        Ok(InstallResult::ok(
            &d.agent_dir,
            Some(&d.canonical_dir),
            InstallMode::Symlink,
        ))
    };
    run().unwrap_or_else(|e| InstallResult::fail(&d.agent_dir, mode, e))
}

fn write_files(
    target_dir: &str,
    files: &[SnapshotFile],
    agent_type: AgentType,
) -> std::io::Result<()> {
    // Snapshot paths are unique, so after creating the parent directories in
    // order the writes are independent and can run in parallel.
    let mut writes: Vec<(String, &SnapshotFile)> = Vec::new();
    for f in files {
        let full = join(&[target_dir, f.path.as_str()]);
        if !is_path_safe(target_dir, &full) {
            continue;
        }
        let parent = paths::dirname(&full);
        if parent != target_dir {
            std::fs::create_dir_all(&parent)?;
        }
        writes.push((full, f));
    }
    for_each_parallel(&writes, |(full, f)| {
        if agent_type == "eve" && paths::basename(&f.path).to_lowercase() == "skill.md" {
            if let Ok(text) = std::str::from_utf8(&f.contents) {
                return std::fs::write(full, strip_ignored_eve_frontmatter(text));
            }
        }
        std::fs::write(full, &f.contents)
    })
}

/// Install an in-memory skill (well-known or blob snapshot).
fn install_files(
    install_name: &str,
    files: &[SnapshotFile],
    agent_type: AgentType,
    options: &InstallOptions,
    skip_missing_project_dirs: bool,
) -> InstallResult {
    let a = agent(agent_type);
    let is_global = options.global;
    let cwd = options
        .cwd
        .clone()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(sys::cwd);
    let mode = options.mode.unwrap_or(InstallMode::Symlink);
    if is_global && a.global_skills_dir.is_none() {
        return no_global_support(agent_type, mode);
    }
    let skill_name = sanitize_name(install_name);
    let d = compute_dirs(
        &skill_name,
        agent_type,
        is_global,
        &cwd,
        mode,
        options.eve_subagent.as_deref(),
    );
    if let Some(e) = traversal_error(&d, mode) {
        return e;
    }
    let run = || -> Result<InstallResult, String> {
        if mode == InstallMode::Copy {
            populate_from_files(&d.agent_dir, files, agent_type).map_err(io_err)?;
            return Ok(InstallResult::ok(&d.agent_dir, None, InstallMode::Copy));
        }
        populate_from_files(&d.canonical_dir, files, agent_type).map_err(io_err)?;
        if is_global && is_universal_agent(agent_type) {
            return Ok(InstallResult::ok(
                &d.canonical_dir,
                Some(&d.canonical_dir),
                InstallMode::Symlink,
            ));
        }
        if skip_missing_project_dirs
            && should_skip_project_agent_symlink(
                agent_type,
                is_global,
                &cwd,
                options.create_missing_agent_root,
            )
        {
            let mut r = InstallResult::ok(
                &d.canonical_dir,
                Some(&d.canonical_dir),
                InstallMode::Symlink,
            );
            r.skipped = true;
            r.skip_reason = Some("missing-agent-project-directory");
            return Ok(r);
        }
        if !create_symlink(&d.canonical_dir, &d.agent_dir) {
            populate_from_files(&d.agent_dir, files, agent_type).map_err(io_err)?;
            let mut r =
                InstallResult::ok(&d.agent_dir, Some(&d.canonical_dir), InstallMode::Symlink);
            r.symlink_failed = true;
            return Ok(r);
        }
        Ok(InstallResult::ok(
            &d.agent_dir,
            Some(&d.canonical_dir),
            InstallMode::Symlink,
        ))
    };
    run().unwrap_or_else(|e| InstallResult::fail(&d.agent_dir, mode, e))
}

/// Install a well-known skill with all of its files.
pub fn install_well_known_skill_for_agent(
    install_name: &str,
    files: &[SnapshotFile],
    agent_type: AgentType,
    options: &InstallOptions,
) -> InstallResult {
    install_files(install_name, files, agent_type, options, false)
}

/// Install a blob-downloaded skill snapshot.
pub fn install_blob_skill_for_agent(
    install_name: &str,
    files: &[SnapshotFile],
    agent_type: AgentType,
    options: &InstallOptions,
) -> InstallResult {
    let a = agent(agent_type);
    let mode = options.mode.unwrap_or(InstallMode::Symlink);
    if options.global && a.global_skills_dir.is_none() {
        return no_global_support(agent_type, mode);
    }
    let is_eve_packaged = files
        .iter()
        .any(|f| paths::basename(&f.path).to_lowercase() == "skill.md");
    if agent_type == "eve" && !is_eve_packaged {
        let cwd = options
            .cwd
            .clone()
            .filter(|c| !c.is_empty())
            .unwrap_or_else(sys::cwd);
        let agent_base = get_agent_base_dir(
            agent_type,
            options.global,
            Some(&cwd),
            options.eve_subagent.as_deref(),
        );
        let flat = join(&[
            agent_base.as_str(),
            to_eve_flat_skill_file_name(install_name).as_str(),
        ]);
        if !is_path_safe(&agent_base, &flat) {
            return InstallResult::fail(
                &flat,
                mode,
                "Invalid skill name: potential path traversal detected".into(),
            );
        }
        let run = || -> std::io::Result<()> {
            std::fs::create_dir_all(&agent_base)?;
            let _ = remove_all(&flat);
            std::fs::write(&flat, get_eve_flat_skill_markdown(files))
        };
        return match run() {
            Ok(()) => InstallResult::ok(&flat, None, InstallMode::Copy),
            Err(e) => InstallResult::fail(&flat, mode, e.to_string()),
        };
    }
    install_files(install_name, files, agent_type, options, true)
}

#[derive(Clone, Debug)]
pub struct InstalledSkill {
    pub name: String,
    pub description: String,
    pub path: String,
    pub canonical_path: String,
    pub scope: &'static str,
    pub agents: Vec<AgentType>,
}

struct Scope {
    global: bool,
    path: String,
    agent_type: Option<AgentType>,
}

/// All installed skills in canonical and agent-specific directories.
pub fn list_installed_skills(
    global: Option<bool>,
    cwd: Option<&str>,
    agent_filter: Option<&[AgentType]>,
) -> Vec<InstalledSkill> {
    let cwd = cwd.map(|s| s.to_string()).unwrap_or_else(sys::cwd);
    let mut skills: Vec<InstalledSkill> = Vec::new();
    let mut scopes: Vec<Scope> = Vec::new();
    let detected = detect_installed_agents();
    let to_check: Vec<AgentType> = match agent_filter {
        Some(f) => detected.into_iter().filter(|a| f.contains(a)).collect(),
        None => detected,
    };
    let scope_types: Vec<bool> = match global {
        None => vec![false, true],
        Some(g) => vec![g],
    };
    for is_global in scope_types {
        scopes.push(Scope {
            global: is_global,
            path: get_canonical_skills_dir(is_global, Some(&cwd)),
            agent_type: None,
        });
        for at in &to_check {
            let a = agent(at);
            if is_global && a.global_skills_dir.is_none() {
                continue;
            }
            let dir = if is_global {
                a.global_skills_dir.clone().unwrap()
            } else {
                join(&[cwd.as_str(), a.skills_dir])
            };
            if !scopes
                .iter()
                .any(|s| s.path == dir && s.global == is_global)
            {
                scopes.push(Scope {
                    global: is_global,
                    path: dir,
                    agent_type: Some(at),
                });
            }
            if *at == "eve" && !is_global {
                for sub in get_eve_subagents(&cwd) {
                    let sub_dir = get_eve_subagent_skills_dir(&sub, Some(&cwd));
                    if !scopes
                        .iter()
                        .any(|s| s.path == sub_dir && s.global == is_global)
                    {
                        scopes.push(Scope {
                            global: is_global,
                            path: sub_dir,
                            agent_type: Some(at),
                        });
                    }
                }
            }
        }
        for a in agents::agents() {
            if to_check.contains(&a.name) {
                continue;
            }
            if is_global && a.global_skills_dir.is_none() {
                continue;
            }
            let dir = if is_global {
                a.global_skills_dir.clone().unwrap()
            } else {
                join(&[cwd.as_str(), a.skills_dir])
            };
            if scopes
                .iter()
                .any(|s| s.path == dir && s.global == is_global)
            {
                continue;
            }
            if exists(&dir) {
                scopes.push(Scope {
                    global: is_global,
                    path: dir,
                    agent_type: Some(a.name),
                });
            }
        }
    }

    let find = |skills: &Vec<InstalledSkill>, key_scope: &str, name: &str| {
        skills
            .iter()
            .position(|s| s.scope == key_scope && s.name == name)
    };

    let whitespace_re = regex::Regex::new(r"\s+").unwrap();
    for scope in &scopes {
        let Ok(entries) = std::fs::read_dir(&scope.path) else {
            continue;
        };
        for entry in entries.flatten() {
            let entry_name = entry.file_name().to_string_lossy().to_string();
            let skill_dir = join(&[scope.path.as_str(), entry_name.as_str()]);
            if !is_dir_entry_or_symlink_to_dir(&entry, &skill_dir) {
                continue;
            }
            let md = join(&[skill_dir.as_str(), "SKILL.md"]);
            if std::fs::metadata(&md).is_err() {
                continue;
            }
            let Some(skill) = parse_skill_md(&md, false) else {
                continue;
            };
            let scope_key: &'static str = if scope.global { "global" } else { "project" };

            if let Some(at) = scope.agent_type {
                match find(&skills, scope_key, &skill.name) {
                    Some(i) => {
                        if !skills[i].agents.contains(&at) {
                            skills[i].agents.push(at);
                        }
                    }
                    None => skills.push(InstalledSkill {
                        name: skill.name.clone(),
                        description: skill.description.clone(),
                        path: skill_dir.clone(),
                        canonical_path: skill_dir.clone(),
                        scope: scope_key,
                        agents: vec![at],
                    }),
                }
                continue;
            }

            let sanitized = sanitize_name(&skill.name);
            let mut installed_agents: Vec<AgentType> = Vec::new();
            for at in &to_check {
                let a = agent(at);
                if scope.global && a.global_skills_dir.is_none() {
                    continue;
                }
                let agent_base = get_agent_base_dir(at, scope.global, Some(&cwd), None);
                let alt: String = {
                    let lower = skill.name.to_lowercase();
                    let dashed = whitespace_re.replace_all(&lower, "-").into_owned();
                    dashed
                        .chars()
                        .filter(|c| !matches!(c, '/' | '\\' | ':' | '\0'))
                        .collect()
                };
                let mut possible: Vec<String> = Vec::new();
                for n in [entry_name.clone(), sanitized.clone(), alt] {
                    if !possible.contains(&n) {
                        possible.push(n);
                    }
                }
                let mut found = possible.iter().any(|n| {
                    let d = join(&[agent_base.as_str(), n.as_str()]);
                    is_path_safe(&agent_base, &d) && exists(&d)
                });
                if !found {
                    if let Ok(agent_entries) = std::fs::read_dir(&agent_base) {
                        for ae in agent_entries.flatten() {
                            let cand = join(&[
                                agent_base.as_str(),
                                ae.file_name().to_string_lossy().as_ref(),
                            ]);
                            if !is_dir_entry_or_symlink_to_dir(&ae, &cand)
                                || !is_path_safe(&agent_base, &cand)
                            {
                                continue;
                            }
                            let cand_md = join(&[cand.as_str(), "SKILL.md"]);
                            if std::fs::metadata(&cand_md).is_err() {
                                continue;
                            }
                            if let Some(cs) = parse_skill_md(&cand_md, false) {
                                if cs.name == skill.name {
                                    found = true;
                                    break;
                                }
                            }
                        }
                    }
                }
                if found {
                    installed_agents.push(at);
                }
            }
            match find(&skills, scope_key, &skill.name) {
                Some(i) => {
                    for at in installed_agents {
                        if !skills[i].agents.contains(&at) {
                            skills[i].agents.push(at);
                        }
                    }
                }
                None => skills.push(InstalledSkill {
                    name: skill.name.clone(),
                    description: skill.description.clone(),
                    path: skill_dir.clone(),
                    canonical_path: skill_dir.clone(),
                    scope: scope_key,
                    agents: installed_agents,
                }),
            }
        }
    }
    skills
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_names() {
        assert_eq!(sanitize_name("My Skill"), "my-skill");
        assert_eq!(sanitize_name("../../etc/passwd"), "etc-passwd");
        assert_eq!(sanitize_name("ce:review"), "ce-review");
        assert_eq!(sanitize_name("..."), "unnamed-skill");
        assert_eq!(sanitize_name(".hidden"), "hidden");
        assert_eq!(sanitize_name("a_b.c"), "a_b.c");
        assert_eq!(sanitize_name("日本語"), "unnamed-skill");
        assert_eq!(sanitize_name(&"a".repeat(300)).len(), 255);
    }

    #[test]
    fn eve_frontmatter_is_filtered() {
        let raw = "---\nname: x\ndescription: y\nallowed-tools: Bash\nmetadata:\n  internal: true\n---\n\n# Body\n";
        assert_eq!(
            strip_ignored_eve_frontmatter(raw),
            "---\nname: x\ndescription: y\nmetadata:\n  internal: true\n---\n# Body\n"
        );
        assert_eq!(
            strip_ignored_eve_frontmatter("---\nother: 1\n---\nBody"),
            "Body"
        );
    }

    #[test]
    fn copy_install_and_symlink_install() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().to_string_lossy().to_string();
        let src = join(&[root.as_str(), "src", "my-skill"]);
        std::fs::create_dir_all(join(&[src.as_str(), ".git"])).unwrap();
        std::fs::write(
            join(&[src.as_str(), "SKILL.md"]),
            "---\nname: my-skill\ndescription: d\n---\n",
        )
        .unwrap();
        std::fs::write(join(&[src.as_str(), "metadata.json"]), "{}").unwrap();
        std::fs::write(join(&[src.as_str(), ".git", "HEAD"]), "x").unwrap();
        let project = join(&[root.as_str(), "project"]);
        std::fs::create_dir_all(&project).unwrap();
        let skill = Skill {
            name: "my-skill".into(),
            description: "d".into(),
            path: src.clone(),
            ..Default::default()
        };

        let r = install_skill_for_agent(
            &skill,
            "claude-code",
            &InstallOptions {
                cwd: Some(project.clone()),
                mode: Some(InstallMode::Symlink),
                ..Default::default()
            },
        );
        assert!(r.success, "{:?}", r.error);
        let canonical = join(&[project.as_str(), ".agents", "skills", "my-skill"]);
        assert_eq!(r.canonical_path.as_deref(), Some(canonical.as_str()));
        assert!(exists(&join(&[canonical.as_str(), "SKILL.md"])));
        assert!(!exists(&join(&[canonical.as_str(), "metadata.json"])));
        assert!(!exists(&join(&[canonical.as_str(), ".git"])));
        assert!(exists(&join(&[
            project.as_str(),
            ".claude",
            "skills",
            "my-skill",
            "SKILL.md"
        ])));

        // Removing the agent link (a junction on Windows) must delete only the
        // link and leave the canonical copy intact.
        let link = join(&[project.as_str(), ".claude", "skills", "my-skill"]);
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        crate::git::remove_all(&link).unwrap();
        assert!(std::fs::symlink_metadata(&link).is_err());
        assert!(exists(&join(&[canonical.as_str(), "SKILL.md"])));

        // Non-universal agent without a project dir is skipped
        let r = install_skill_for_agent(
            &skill,
            "windsurf",
            &InstallOptions {
                cwd: Some(project.clone()),
                mode: Some(InstallMode::Symlink),
                ..Default::default()
            },
        );
        assert!(r.skipped);
        assert_eq!(r.skip_reason, Some("missing-agent-project-directory"));

        let r = install_skill_for_agent(
            &skill,
            "windsurf",
            &InstallOptions {
                cwd: Some(project.clone()),
                mode: Some(InstallMode::Copy),
                ..Default::default()
            },
        );
        assert!(r.success);
        assert!(exists(&join(&[
            project.as_str(),
            ".windsurf",
            "skills",
            "my-skill",
            "SKILL.md"
        ])));
    }

    #[test]
    fn shared_canonical_dir_is_copied_once_per_run() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().to_string_lossy().to_string();
        let src = join(&[root.as_str(), "src", "big"]);
        std::fs::create_dir_all(join(&[src.as_str(), "ref"])).unwrap();
        std::fs::write(
            join(&[src.as_str(), "SKILL.md"]),
            "---
name: big
description: d
---
",
        )
        .unwrap();
        for i in 0..40 {
            std::fs::write(
                join(&[src.as_str(), "ref", &format!("{}.md", i)]),
                format!("file {}", i),
            )
            .unwrap();
        }
        let project = join(&[root.as_str(), "project"]);
        std::fs::create_dir_all(&project).unwrap();
        let skill = Skill {
            name: "big".into(),
            description: "d".into(),
            path: src,
            ..Default::default()
        };
        let opts = InstallOptions {
            cwd: Some(project.clone()),
            mode: Some(InstallMode::Symlink),
            ..Default::default()
        };
        let marker = join(&[project.as_str(), ".agents", "skills", "big", "ref", "0.md"]);

        reset_populated();
        assert!(install_skill_for_agent(&skill, "codex", &opts).success);
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "file 0");
        let refs = join(&[project.as_str(), ".agents", "skills", "big", "ref"]);
        assert_eq!(std::fs::read_dir(&refs).unwrap().count(), 40);

        // A second universal agent in the same run reuses the populated directory.
        std::fs::write(&marker, "untouched").unwrap();
        assert!(install_skill_for_agent(&skill, "cursor", &opts).success);
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "untouched");

        // A new run starts from scratch and copies again.
        reset_populated();
        assert!(install_skill_for_agent(&skill, "cursor", &opts).success);
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "file 0");
    }
}
