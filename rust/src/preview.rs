//! `skills preview` / `skills show` — show one skill's files and SKILL.md
//! without installing it (extension; not in the reference CLI).
//!
//! Source resolution and skill selection are shared with `skills use`
//! ([`crate::use_cmd::resolve_skill`]).

use crate::collate::ordinal_cmp;
use crate::color::ansi::{BOLD, CYAN, DIM, RESET, YELLOW};
use crate::git::cleanup_temp_dir;
use crate::paths::join;
use crate::sanitize::sanitize_metadata;
use crate::ui::{self, SelectOption};
use crate::use_cmd::{resolve_skill, Materialized};
use crate::{errln, outln, sys};
use serde_json::{Map, Value};
use std::io::Write;

const SCRIPT_EXTENSIONS: &[&str] = &[
    ".sh", ".bash", ".zsh", ".fish", ".ps1", ".psm1", ".bat", ".cmd", ".py", ".js", ".mjs", ".cjs",
    ".ts", ".rb", ".pl", ".php", ".lua", ".exe", ".dll", ".so", ".dylib",
];

/// Bytes inspected for a NUL byte when deciding whether a file is binary.
const BINARY_SNIFF_BYTES: usize = 8000;

#[derive(Default, Clone, Debug)]
pub struct PreviewOptions {
    pub skill: Option<String>,
    pub file: Option<String>,
    pub full_depth: bool,
    pub json: bool,
    pub no_pager: bool,
    pub help: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PreviewFile {
    /// Path relative to the skill directory, with `/` separators.
    pub path: String,
    pub size: u64,
    pub script: bool,
}

pub fn get_preview_help() -> &'static str {
    "Usage: skills preview <source>[@<skill>] [options]\n\nShow a skill's files and SKILL.md without installing it.\n\nOptions:\n  -s, --skill <skill>   Select the skill to preview\n  --file <path>         Print one file of the skill instead of the overview\n  --full-depth          Search nested directories like skills add --full-depth\n  --json                Output as JSON\n  --no-pager            Do not page the output\n  -h, --help            Show this help message\n\nExamples:\n  skills preview vercel-labs/agent-skills@web-design-guidelines\n  skills preview ./my-skills --skill my-skill --file scripts/run.sh"
}

pub fn print_preview_help() {
    outln!("{}", get_preview_help());
}

/// Parse `preview` arguments. Returns (sources, options, errors).
pub fn parse_preview_options(args: &[String]) -> (Vec<String>, PreviewOptions, Vec<String>) {
    let mut source = Vec::new();
    let mut o = PreviewOptions::default();
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
            "--json" => o.json = true,
            "--no-pager" => o.no_pager = true,
            "--skill" | "-s" | "--file" => {
                let is_file = a == "--file";
                match args.get(i + 1) {
                    Some(v) if !v.is_empty() && !v.starts_with('-') => {
                        let slot = if is_file { &mut o.file } else { &mut o.skill };
                        if slot.is_some() {
                            errors.push(format!(
                                "Only one {} value can be provided",
                                if is_file { "--file" } else { "--skill" }
                            ));
                        } else {
                            *slot = Some(v.clone());
                        }
                        i += 1;
                    }
                    _ => errors.push(if is_file {
                        "--file requires a path".to_string()
                    } else {
                        format!("{} requires a skill name", a)
                    }),
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
    (source, o, errors)
}

/// `N B` below 1 KiB, then one decimal `X.Y KB` / `X.Y MB` (half away from zero).
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    if bytes < KB {
        return format!("{} B", bytes);
    }
    let (unit, div) = if bytes < MB { ("KB", KB) } else { ("MB", MB) };
    let tenths = (bytes as u128 * 10 + (div as u128) / 2) / div as u128;
    format!("{}.{} {}", tenths / 10, tenths % 10, unit)
}

/// Whether a file can run code, judged by its (lowercased) extension.
pub fn is_script_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    match name.rfind('.') {
        Some(i) if i > 0 => SCRIPT_EXTENSIONS.contains(&&name[i..]),
        _ => false,
    }
}

fn is_root_skill_md(path: &str) -> bool {
    path.eq_ignore_ascii_case("SKILL.md")
}

/// Flat order: the root SKILL.md first, then ordinal by path.
pub fn sort_files(files: &mut [PreviewFile]) {
    files.sort_by(|a, b| {
        is_root_skill_md(&b.path)
            .cmp(&is_root_skill_md(&a.path))
            .then_with(|| ordinal_cmp(&a.path, &b.path))
    });
}

/// All regular files under `dir`, recursively, in [`sort_files`] order.
pub fn list_files(dir: &str) -> Vec<PreviewFile> {
    fn walk(dir: &str, prefix: &str, out: &mut Vec<PreviewFile>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let full = join(&[dir, name.as_str()]);
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{}/{}", prefix, name)
            };
            let Ok(meta) = std::fs::metadata(&full) else {
                continue;
            };
            if meta.is_dir() {
                walk(&full, &rel, out);
            } else if meta.is_file() {
                out.push(PreviewFile {
                    script: is_script_path(&rel),
                    path: rel,
                    size: meta.len(),
                });
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, "", &mut out);
    sort_files(&mut out);
    out
}

#[derive(Default)]
struct TreeNode<'a> {
    files: Vec<(String, &'a PreviewFile)>,
    dirs: Vec<(String, TreeNode<'a>)>,
}

impl<'a> TreeNode<'a> {
    fn insert(&mut self, parts: &[&str], file: &'a PreviewFile) {
        if parts.len() == 1 {
            self.files.push((parts[0].to_string(), file));
            return;
        }
        let idx = match self.dirs.iter().position(|(n, _)| n == parts[0]) {
            Some(i) => i,
            None => {
                self.dirs.push((parts[0].to_string(), TreeNode::default()));
                self.dirs.len() - 1
            }
        };
        self.dirs[idx].1.insert(&parts[1..], file);
    }

    fn render(&mut self, depth: usize, out: &mut Vec<String>) {
        let root = depth == 0;
        self.files.sort_by(|a, b| {
            (root && is_root_skill_md(&b.0))
                .cmp(&(root && is_root_skill_md(&a.0)))
                .then_with(|| ordinal_cmp(&a.0, &b.0))
        });
        self.dirs.sort_by(|a, b| ordinal_cmp(&a.0, &b.0));
        let indent = "  ".repeat(depth + 1);
        for (name, f) in &self.files {
            out.push(format!(
                "{}{} ({}){}",
                indent,
                sanitize_metadata(name),
                format_size(f.size),
                if f.script { " [script]" } else { "" }
            ));
        }
        for (name, node) in &mut self.dirs {
            out.push(format!("{}{}/", indent, sanitize_metadata(name)));
            node.render(depth + 1, out);
        }
    }
}

/// Indented file tree: files before directories at each level, ordinal by
/// name, with the root SKILL.md first.
pub fn render_tree(files: &[PreviewFile]) -> Vec<String> {
    let mut root = TreeNode::default();
    for f in files {
        let parts: Vec<&str> = f.path.split('/').collect();
        root.insert(&parts, f);
    }
    let mut out = Vec::new();
    root.render(0, &mut out);
    out
}

fn is_fence(line: &str) -> Option<&'static str> {
    let t = line.trim_start();
    if t.starts_with("```") {
        Some("```")
    } else if t.starts_with("~~~") {
        Some("~~~")
    } else {
        None
    }
}

/// Light terminal highlighting for SKILL.md: frontmatter and fenced code DIM,
/// headings BOLD+CYAN.
pub fn highlight_markdown(text: &str) -> String {
    let mut out = String::new();
    let lines: Vec<&str> = text.split('\n').collect();
    let mut in_frontmatter = lines
        .first()
        .map(|l| l.trim_end() == "---")
        .unwrap_or(false);
    let mut fence: Option<&str> = None;
    for (i, raw) in lines.iter().enumerate() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let last = i + 1 == lines.len();
        if last && line.is_empty() {
            break;
        }
        let styled = if in_frontmatter {
            if i > 0 && line.trim_end() == "---" {
                in_frontmatter = false;
            }
            format!("{}{}{}", DIM, line, RESET)
        } else if let Some(marker) = fence {
            if line.trim_start().starts_with(marker) {
                fence = None;
            }
            format!("{}{}{}", DIM, line, RESET)
        } else if let Some(marker) = is_fence(line) {
            fence = Some(marker);
            format!("{}{}{}", DIM, line, RESET)
        } else if line.starts_with('#') {
            format!("{}{}{}{}", BOLD, CYAN, line, RESET)
        } else {
            line.to_string()
        };
        out.push_str(&styled);
        out.push('\n');
    }
    out
}

/// The overview printed by `skills preview` (without `--json`/`--file`).
pub fn render_overview(
    name: &str,
    description: &str,
    files: &[PreviewFile],
    skill_md: &str,
    highlight: bool,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("{}{}{}\n", BOLD, sanitize_metadata(name), RESET));
    out.push_str(&format!(
        "{}{}{}\n",
        DIM,
        sanitize_metadata(description),
        RESET
    ));
    out.push('\n');
    out.push_str("Files:\n");
    for line in render_tree(files) {
        out.push_str(&line);
        out.push('\n');
    }
    out.push('\n');
    let scripts = files.iter().filter(|f| f.script).count();
    if scripts > 0 {
        out.push_str(&format!(
            "{}⚠ This skill includes {} script file(s). Review them before installing.{}\n",
            YELLOW, scripts, RESET
        ));
        out.push('\n');
    }
    out.push_str(&format!("{}--- SKILL.md ---{}\n", DIM, RESET));
    if highlight {
        out.push_str(&highlight_markdown(skill_md));
    } else {
        out.push_str(skill_md);
        if !skill_md.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

fn render_json(m: &Materialized, source: &str, files: &[PreviewFile]) -> String {
    let mut o = Map::new();
    o.insert("name".into(), Value::String(sanitize_metadata(&m.name)));
    o.insert(
        "description".into(),
        Value::String(sanitize_metadata(&m.description)),
    );
    o.insert("source".into(), Value::String(source.to_string()));
    o.insert(
        "files".into(),
        Value::Array(
            files
                .iter()
                .map(|f| {
                    let mut e = Map::new();
                    e.insert("path".into(), Value::String(f.path.clone()));
                    e.insert("size".into(), Value::from(f.size));
                    e.insert("script".into(), Value::Bool(f.script));
                    Value::Object(e)
                })
                .collect(),
        ),
    );
    o.insert("skillMd".into(), Value::String(m.skill_md.clone()));
    serde_json::to_string_pretty(&Value::Object(o)).unwrap()
}

/// Normalize a `--file` argument: `\` → `/`, leading `./` removed.
pub fn normalize_file_arg(path: &str) -> String {
    let p = path.replace('\\', "/");
    p.strip_prefix("./").unwrap_or(&p).to_string()
}

/// Exact match first, then case-insensitive.
pub fn find_file<'a>(files: &'a [PreviewFile], wanted: &str) -> Option<&'a PreviewFile> {
    files.iter().find(|f| f.path == wanted).or_else(|| {
        let lower = wanted.to_lowercase();
        files.iter().find(|f| f.path.to_lowercase() == lower)
    })
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(BINARY_SNIFF_BYTES)].contains(&0)
}

fn find_on_path(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".into())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(|e| e.to_string())
            .collect()
    } else {
        Vec::new()
    };
    std::env::split_paths(&path).any(|dir| {
        let base = dir.join(program);
        base.is_file()
            || exts
                .iter()
                .any(|e| dir.join(format!("{}{}", program, e)).is_file())
    })
}

fn pager_command() -> Option<(String, Vec<String>)> {
    if let Some(p) = sys::env("PAGER") {
        let mut parts = p.split_whitespace().map(|s| s.to_string());
        if let Some(prog) = parts.next() {
            return Some((prog, parts.collect()));
        }
    }
    if find_on_path("less") {
        return Some(("less".into(), vec!["-R".into()]));
    }
    None
}

fn run_pager(text: &str) -> bool {
    let Some((prog, args)) = pager_command() else {
        return false;
    };
    let mut child = match std::process::Command::new(&prog)
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    if let Some(mut stdin) = child.stdin.take() {
        // A pager quit early closes the pipe; that is not an error.
        let _ = stdin.write_all(text.as_bytes());
    }
    let _ = child.wait();
    true
}

/// Print `text`, through a pager when stdout is a terminal and the text is
/// taller than the terminal (unless `--no-pager`).
fn page_or_print(text: &str, no_pager: bool) {
    if !no_pager && sys::stdout_is_tty() {
        let rows = sys::terminal_rows().unwrap_or(0) as usize;
        let lines = text.matches('\n').count();
        if rows > 0 && lines > rows && run_pager(text) {
            return;
        }
    }
    sys::write_out(text);
}

fn write_raw_stdout(bytes: &[u8]) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(bytes);
    let _ = out.flush();
}

fn file_picker(m: &Materialized, files: &[PreviewFile], no_pager: bool) {
    let others: Vec<&PreviewFile> = files
        .iter()
        .filter(|f| !is_root_skill_md(&f.path))
        .collect();
    if others.is_empty() || !sys::stdin_is_tty() {
        return;
    }
    let mut options: Vec<SelectOption<Option<usize>>> = others
        .iter()
        .enumerate()
        .map(|(i, f)| {
            SelectOption::new(
                Some(i),
                &sanitize_metadata(&f.path),
                Some(&format_size(f.size)),
            )
        })
        .collect();
    options.push(SelectOption::new(None, "Done", None));
    let mut initial = 0;
    loop {
        let Some(Some(i)) = ui::select("Open a file", &options, initial) else {
            return;
        };
        initial = i;
        let f = others[i];
        let bytes =
            std::fs::read(join(&[m.skill_dir.as_str(), f.path.as_str()])).unwrap_or_default();
        if is_binary(&bytes) {
            outln!("Binary file, not shown ({})", format_size(f.size));
            continue;
        }
        let mut text = String::from_utf8_lossy(&bytes).into_owned();
        if !text.ends_with('\n') {
            text.push('\n');
        }
        page_or_print(&text, no_pager);
    }
}

fn show(m: &Materialized, source: &str, o: &PreviewOptions) -> Result<(), String> {
    let files = list_files(&m.skill_dir);
    if let Some(wanted) = &o.file {
        let wanted = normalize_file_arg(wanted);
        let Some(f) = find_file(&files, &wanted) else {
            let mut lines = vec![
                format!("File not found in skill: {}", wanted),
                "Available files:".to_string(),
            ];
            lines.extend(
                files
                    .iter()
                    .map(|f| format!("  - {}", sanitize_metadata(&f.path))),
            );
            return Err(lines.join("\n"));
        };
        let bytes = std::fs::read(join(&[m.skill_dir.as_str(), f.path.as_str()]))
            .map_err(|e| e.to_string())?;
        if is_binary(&bytes) {
            return Err(format!("Cannot display binary file: {}", wanted));
        }
        if !o.no_pager && sys::stdout_is_tty() {
            page_or_print(&String::from_utf8_lossy(&bytes), false);
        } else {
            write_raw_stdout(&bytes);
        }
        return Ok(());
    }
    if o.json {
        outln!("{}", render_json(m, source, &files));
        return Ok(());
    }
    let tty = sys::stdout_is_tty();
    let text = render_overview(&m.name, &m.description, &files, &m.skill_md, tty);
    if tty {
        page_or_print(&text, o.no_pager);
        file_picker(m, &files, o.no_pager);
    } else {
        sys::write_out(&text);
    }
    Ok(())
}

fn fail(message: &str) -> ! {
    errln!("{}", message);
    sys::exit(1);
}

pub fn run_preview(args: &[String]) {
    let (sources, o, errors) = parse_preview_options(args);
    if o.help {
        print_preview_help();
        return;
    }
    if !errors.is_empty() {
        fail(&errors.join("\n"));
    }
    if sources.is_empty() {
        fail(&format!(
            "Missing required argument: source\n\n{}",
            get_preview_help()
        ));
    }
    if sources.len() > 1 {
        fail(&format!(
            "Expected one source, received {}: {}",
            sources.len(),
            sources.join(", ")
        ));
    }
    let source = &sources[0];
    let m = match resolve_skill(source, o.skill.as_deref(), o.full_depth, "preview") {
        Ok(m) => m,
        Err(e) => fail(&e),
    };
    let result = show(&m, source, &o);
    let _ = cleanup_temp_dir(&m.temp_root);
    if let Err(e) = result {
        fail(&e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn f(path: &str, size: u64) -> PreviewFile {
        PreviewFile {
            path: path.into(),
            size,
            script: is_script_path(path),
        }
    }

    #[test]
    fn parses_preview_options() {
        let (src, o, errs) = parse_preview_options(&s(&[
            "o/r",
            "-s",
            "x",
            "--file",
            "a.md",
            "--json",
            "--no-pager",
            "--full-depth",
        ]));
        assert_eq!(src, s(&["o/r"]));
        assert_eq!(o.skill.as_deref(), Some("x"));
        assert_eq!(o.file.as_deref(), Some("a.md"));
        assert!(o.json && o.no_pager && o.full_depth && errs.is_empty());
        let (_, _, errs) = parse_preview_options(&s(&["o/r", "--skill", "a", "-s", "b"]));
        assert_eq!(errs, s(&["Only one --skill value can be provided"]));
        let (_, _, errs) = parse_preview_options(&s(&["o/r", "--file", "a", "--file", "b"]));
        assert_eq!(errs, s(&["Only one --file value can be provided"]));
        let (_, _, errs) = parse_preview_options(&s(&["o/r", "-s"]));
        assert_eq!(errs, s(&["-s requires a skill name"]));
        let (_, _, errs) = parse_preview_options(&s(&["o/r", "--file", "--json"]));
        assert_eq!(errs, s(&["--file requires a path"]));
        let (_, _, errs) = parse_preview_options(&s(&["--bogus"]));
        assert_eq!(errs, s(&["Unknown option: --bogus"]));
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(1075), "1.0 KB");
        assert_eq!(format_size(1076), "1.1 KB");
        assert_eq!(format_size(1048576), "1.0 MB");
        assert_eq!(format_size(1572864), "1.5 MB");
    }

    #[test]
    fn detects_scripts() {
        assert!(is_script_path("scripts/run.sh"));
        assert!(is_script_path("Tool.PS1"));
        assert!(!is_script_path("notes.md"));
        assert!(!is_script_path(".sh"));
        assert!(!is_script_path("Makefile"));
    }

    #[test]
    fn renders_tree() {
        let mut files = vec![
            f("scripts/run.sh", 20),
            f("notes.md", 10),
            f("SKILL.md", 1229),
            f("assets/b.png", 2048),
            f("Zeta.md", 1),
            f("scripts/a/deep.txt", 3),
        ];
        sort_files(&mut files);
        assert_eq!(files[0].path, "SKILL.md");
        assert_eq!(
            render_tree(&files),
            s(&[
                "  SKILL.md (1.2 KB)",
                "  Zeta.md (1 B)",
                "  notes.md (10 B)",
                "  assets/",
                "    b.png (2.0 KB)",
                "  scripts/",
                "    run.sh (20 B) [script]",
                "    a/",
                "      deep.txt (3 B)",
            ])
        );
    }

    #[test]
    fn overview_layout() {
        let files = vec![f("SKILL.md", 12), f("run.py", 5)];
        let text = render_overview("x", "", &files, "---\nname: x\n---\nBody", false);
        let expected = format!(
            "{b}x{r}\n{d}{r}\n\nFiles:\n  SKILL.md (12 B)\n  run.py (5 B) [script]\n\n{y}⚠ This skill includes 1 script file(s). Review them before installing.{r}\n\n{d}--- SKILL.md ---{r}\n---\nname: x\n---\nBody\n",
            b = BOLD,
            d = DIM,
            r = RESET,
            y = YELLOW
        );
        assert_eq!(text, expected);
    }

    #[test]
    fn finds_files() {
        let files = vec![f("SKILL.md", 1), f("Docs/A.md", 1)];
        assert_eq!(normalize_file_arg(".\\Docs\\A.md"), "Docs/A.md");
        assert_eq!(find_file(&files, "docs/a.md").unwrap().path, "Docs/A.md");
        assert!(find_file(&files, "nope").is_none());
    }
}
