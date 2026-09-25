//! Notion skills integration via the `ntn` CLI (port of notion-test.ts).

use crate::color::pc;
use crate::download_source::{download_source, DownloadOptions, DownloadedSource};
use crate::git::cleanup_temp_dir;
use crate::installer::sanitize_name;
use crate::paths::{self, join};
use crate::proc::{self, ProcError};
use crate::sanitize::{sanitize_metadata, strip_terminal_escapes};
use crate::search_multiselect::{search_multiselect, Options, SearchItem};
use crate::skills::{discover_skills, DiscoverOptions};
use crate::ui::{self, log};
use crate::urlutil;
use crate::{errln, outln, sys};
use regex::Regex;
use serde_json::{json, Value};
use std::time::Duration;

const NOTION_API_VERSION: &str = "2026-03-11";
const NTN_TIMEOUT: Duration = Duration::from_secs(30);
const NTN_MAX_BUFFER_BYTES: usize = 10 * 1024 * 1024;
const NOTION_DOWNLOAD_MAX_BYTES: u64 = 50 * 1024 * 1024;
const NOTION_EXTRACT_MAX_BYTES: u64 = 100 * 1024 * 1024;
const NOTION_EXTRACT_MAX_FILES: u64 = 5000;

#[derive(Clone, Debug)]
pub struct NotionPack {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version_id: String,
}

pub struct Prepared {
    pub root_dir: String,
    pub temp_dir: String,
    /// Set for pack installs; `None` for a single Notion page.
    pub pack_count: Option<usize>,
}

struct Directory {
    id: String,
    version_id: String,
    url: String,
}

fn assert_string(v: Option<&Value>, field: &str) -> Result<String, String> {
    match v {
        Some(Value::String(s)) if !s.is_empty() => Ok(s.clone()),
        _ => Err(format!(
            "Notion Agent Plugins response is missing {}",
            field
        )),
    }
}

fn optional_metadata(v: Option<&Value>, field: &str) -> Result<String, String> {
    match v {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(s)) => Ok(sanitize_metadata(s)),
        _ => Err(format!(
            "Notion Agent Plugins response has an invalid {}",
            field
        )),
    }
}

fn parse_pack(v: &Value) -> Result<Option<NotionPack>, String> {
    if !v.is_object() {
        return Err("Notion Agent Plugins response contains an invalid pack".into());
    }
    let Some(Value::String(raw_name)) = v.get("name") else {
        return Err("Notion Agent Plugins response is missing pack.name".into());
    };
    let name = sanitize_metadata(raw_name);
    if name.is_empty() {
        return Ok(None);
    }
    Ok(Some(NotionPack {
        id: assert_string(v.get("id"), "pack.id")?,
        name,
        description: optional_metadata(v.get("description"), "pack.description")?,
        version_id: assert_string(v.get("version_id"), "pack.version_id")?,
    }))
}

fn parse_directory(v: &Value, label: &str) -> Result<Directory, String> {
    if !v.is_object() {
        return Err(format!("Notion {} directory response is invalid", label));
    }
    Ok(Directory {
        id: assert_string(v.get("id"), &format!("{} directory id", label))?,
        version_id: assert_string(
            v.get("version_id"),
            &format!("{} directory version_id", label),
        )?,
        url: assert_string(v.get("url"), &format!("{} directory url", label))?,
    })
}

fn run_ntn(args: &[&str]) -> Result<String, String> {
    if sys::env_raw("SKILLS_DEBUG").as_deref() == Some("1") {
        errln!("[notion] ntn {}", args.join(" "));
    }
    match proc::command("ntn", args).timeout(NTN_TIMEOUT).max_output(NTN_MAX_BUFFER_BYTES).output() {
        Ok(out) if out.success() => Ok(out.stdout_str()),
        Ok(out) => {
            let stderr = strip_terminal_escapes(&out.stderr_str()).trim().to_string();
            if stderr.is_empty() {
                Err(format!("ntn api failed with exit code {}", out.status.map(|c| c.to_string()).unwrap_or_else(|| "unknown".into())))
            } else {
                Err(format!("ntn api failed: {}", stderr))
            }
        }
        Err(ProcError::NotFound) => Err("Notion CLI (ntn) is required. Install it from:\nhttps://developers.notion.com/cli/get-started/overview\nThen run `ntn login`.".into()),
        Err(ProcError::Timeout) => Err(format!("ntn api timed out after {} seconds", NTN_TIMEOUT.as_secs())),
        Err(ProcError::TooMuchOutput) => Err("ntn api output exceeded 10 MiB".into()),
        Err(ProcError::Other(e)) => Err(format!("Unable to start ntn: {}", strip_terminal_escapes(&e))),
    }
}

fn fetch_json(args: &[&str], label: &str) -> Result<Value, String> {
    let mut full = vec!["api"];
    full.extend_from_slice(args);
    full.extend_from_slice(&["--notion-version", NOTION_API_VERSION]);
    let out = run_ntn(&full)?;
    serde_json::from_str(&out).map_err(|_| format!("ntn returned invalid JSON for {}", label))
}

pub fn fetch_notion_packs() -> Result<Vec<NotionPack>, String> {
    let mut packs = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut args: Vec<String> = vec!["/v1/ai/plugins".into(), "page_size==100".into()];
        if let Some(c) = &cursor {
            args.push(format!("start_cursor=={}", c));
        }
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let value = fetch_json(&refs, "the Notion packs list")?;
        let Some(results) = value
            .get("results")
            .and_then(|r| r.as_array())
            .filter(|_| value.is_object())
        else {
            return Err("Notion Agent Plugins list response is invalid".into());
        };
        let Some(has_more) = value.get("has_more").and_then(|v| v.as_bool()) else {
            return Err("Notion Agent Plugins list response is missing has_more".into());
        };
        let next = match value.get("next_cursor") {
            None => {
                return Err("Notion Agent Plugins list response has an invalid next_cursor".into())
            }
            Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => {
                return Err("Notion Agent Plugins list response has an invalid next_cursor".into())
            }
        };
        for r in results {
            if let Some(p) = parse_pack(r)? {
                packs.push(p);
            }
        }
        if !has_more {
            break;
        }
        match next {
            Some(n) if !n.is_empty() && !seen.contains(&n) => {
                seen.push(n.clone());
                cursor = Some(n);
            }
            _ => return Err("Notion Agent Plugins pagination returned an invalid cursor".into()),
        }
    }
    Ok(packs)
}

fn fetch_pack_directory(pack: &NotionPack) -> Result<Directory, String> {
    let path = format!("/v1/ai/plugins/{}", urlutil::encode_uri_component(&pack.id));
    let label = format!("Notion pack {}", serde_json::to_string(&pack.name).unwrap());
    let v = fetch_json(&[&path], &label)?;
    let d = parse_directory(&v, "pack")?;
    if d.id != pack.id || d.version_id != pack.version_id {
        return Err(format!(
            "Notion pack {} changed while preparing installation",
            serde_json::to_string(&pack.name).unwrap()
        ));
    }
    Ok(d)
}

/// Workspace name; `None` on any failure so the probe never blocks installs.
pub fn fetch_workspace_name() -> Option<String> {
    let out = run_ntn(&[
        "api",
        "/v1/users/me",
        "--notion-version",
        NOTION_API_VERSION,
    ])
    .ok()?;
    let v: Value = serde_json::from_str(&out).ok()?;
    let name = v.get("bot")?.as_object()?.get("workspace_name")?.as_str()?;
    if name.is_empty() {
        None
    } else {
        Some(sanitize_metadata(name))
    }
}

fn in_workspace(ws: &Option<String>) -> String {
    ws.as_ref()
        .map(|w| format!(" in workspace {}", pc::cyan(w)))
        .unwrap_or_default()
}

pub fn is_notion_source(source: &str) -> bool {
    source.to_lowercase() == "notion"
}

/// Page ID from a Notion page URL.
pub fn parse_notion_skill_url(source: &str) -> Option<String> {
    let url = urlutil::parse(source)?;
    if url.scheme() != "https" && url.scheme() != "http" {
        return None;
    }
    if !Regex::new(r"(^|\.)notion\.(so|com)$")
        .unwrap()
        .is_match(&urlutil::hostname(&url).to_lowercase())
    {
        return None;
    }
    let last = url.path().split('/').rfind(|s| !s.is_empty())?.to_string();
    let decoded = urlutil::decode_uri_component(&last).unwrap_or(last);
    let re = Regex::new(
        r"(?:^|-)([0-9a-f]{32}|[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$",
    )
    .unwrap();
    let lower = decoded.to_lowercase();
    let raw = re.captures(&lower)?.get(1)?.as_str().replace('-', "");
    Some(format!(
        "{}-{}-{}-{}-{}",
        &raw[0..8],
        &raw[8..12],
        &raw[12..16],
        &raw[16..20],
        &raw[20..]
    ))
}

fn download_directory(url: &str) -> Result<DownloadedSource, String> {
    download_source(
        url,
        DownloadOptions {
            download_max_bytes: Some(NOTION_DOWNLOAD_MAX_BYTES),
            extract_max_bytes: Some(NOTION_EXTRACT_MAX_BYTES),
            extract_max_files: Some(NOTION_EXTRACT_MAX_FILES),
        },
    )
}

pub fn prepare_notion_skill_source(page_id: &str) -> Result<Prepared, String> {
    let mut spinner = ui::Spinner::new();
    spinner.start("Fetching Notion skill with ntn…");
    let result = (|| {
        let (dir, ws) = std::thread::scope(|s| {
            let ws = s.spawn(fetch_workspace_name);
            let path = format!("/v1/ai/skills/{}", urlutil::encode_uri_component(page_id));
            let dir = fetch_json(&[&path], &format!("Notion skill {}", page_id))
                .and_then(|v| parse_directory(&v, "skill"));
            (dir, ws.join().unwrap_or(None))
        });
        let dir = dir?;
        let downloaded = download_directory(&dir.url)?;
        Ok::<_, String>((downloaded, ws))
    })();
    match result {
        Ok((d, ws)) => {
            spinner.stop(&format!(
                "Downloaded skill from Notion{}",
                in_workspace(&ws)
            ));
            Ok(Prepared {
                root_dir: d.root_dir,
                temp_dir: d.temp_dir,
                pack_count: None,
            })
        }
        Err(e) => {
            spinner.stop(&pc::red("Failed to prepare Notion skill"));
            Err(e)
        }
    }
}

fn normalize_selector(v: &str) -> String {
    let lower = v.to_lowercase();
    let mut out = String::new();
    let mut in_run = false;
    for c in lower.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
            in_run = false;
        } else if !in_run {
            out.push('-');
            in_run = true;
        }
    }
    let out = out.strip_prefix('-').unwrap_or(&out).to_string();
    out.strip_suffix('-').unwrap_or(&out).to_string()
}

fn sorted_packs(packs: &[NotionPack]) -> Vec<NotionPack> {
    let mut v = packs.to_vec();
    v.sort_by(|a, b| crate::collate::locale_compare(&a.name, &b.name));
    v
}

fn copy_dir_all(src: &str, dest: &str) -> std::io::Result<()> {
    if std::path::Path::new(dest).exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("Target already exists: {}", dest),
        ));
    }
    std::fs::create_dir_all(dest)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let name = e.file_name().to_string_lossy().to_string();
        let s = join(&[src, name.as_str()]);
        let d = join(&[dest, name.as_str()]);
        if e.file_type()?.is_dir() {
            copy_dir_all(&s, &d)?;
        } else {
            std::fs::copy(&s, &d)?;
        }
    }
    Ok(())
}

pub fn prepare_notion_pack_source(
    yes: bool,
    list: bool,
    skill: Option<&[String]>,
) -> Result<Option<Prepared>, String> {
    let mut spinner = ui::Spinner::new();
    spinner.start("Fetching Notion packs with ntn…");
    let (packs, ws) = std::thread::scope(|s| {
        let ws = s.spawn(fetch_workspace_name);
        let packs = fetch_notion_packs();
        (packs, ws.join().unwrap_or(None))
    });
    let packs = match packs {
        Ok(p) => p,
        Err(e) => {
            spinner.stop(&pc::red("Failed to load Notion packs"));
            return Err(e);
        }
    };
    spinner.stop(&format!(
        "Found {} Notion pack{}{}",
        pc::green(packs.len().to_string()),
        if packs.len() == 1 { "" } else { "s" },
        in_workspace(&ws)
    ));
    if packs.is_empty() {
        return Err("Notion returned no packs for the authenticated workspace".into());
    }

    if list {
        outln!();
        log::step(&pc::bold("Available Notion packs"));
        for p in sorted_packs(&packs) {
            log::message(&pc::cyan(&p.name));
            if !p.description.is_empty() {
                log::message(&format!("  {}", pc::dim(&p.description)));
            }
        }
        ui::outro(&pc::dim(
            "Select a pack by running the command without --list.",
        ));
        return Ok(None);
    }

    let selected: Vec<NotionPack> = if let Some(sel) = skill.filter(|s| !s.is_empty()) {
        let chosen: Vec<NotionPack> = if sel.iter().any(|s| s == "*") {
            packs.clone()
        } else {
            let normalized: Vec<String> = sel.iter().map(|s| normalize_selector(s)).collect();
            packs
                .iter()
                .filter(|p| {
                    normalized.contains(&normalize_selector(&p.name))
                        || normalized.contains(&p.id.to_lowercase())
                })
                .cloned()
                .collect()
        };
        if chosen.is_empty() {
            return Err(format!(
                "No matching Notion packs found for: {}",
                sel.join(", ")
            ));
        }
        chosen
    } else if yes || !sys::stdin_is_tty() {
        packs.clone()
    } else {
        let sorted = sorted_packs(&packs);
        let items: Vec<SearchItem<usize>> = sorted
            .iter()
            .enumerate()
            .map(|(i, p)| {
                SearchItem::new(i, "[object Object]", &p.name).detail(Some(
                    if p.description.is_empty() {
                        "Notion plugin pack".into()
                    } else {
                        p.description.clone()
                    },
                ))
            })
            .collect();
        let mut o = Options::new(
            &format!(
                "Select Notion packs to install {}",
                pc::dim("(space to toggle)")
            ),
            items,
        );
        o.required = true;
        o.max_visible = 20;
        o.searchable = false;
        o.show_detail = true;
        o.show_selected_summary = false;
        match search_multiselect(o) {
            Some(idx) => idx.into_iter().map(|i| sorted[i].clone()).collect(),
            None => {
                ui::cancel("Selection cancelled");
                return Ok(None);
            }
        }
    };

    ui::note(
        &selected
            .iter()
            .map(|p| pc::cyan(&p.name))
            .collect::<Vec<_>>()
            .join("\n"),
        &format!(
            "Selected {} Notion pack{}",
            selected.len(),
            if selected.len() == 1 { "" } else { "s" }
        ),
    );

    let staging = sys::mkdtemp("skills-notion-").map_err(|e| e.to_string())?;
    let packs_dir = join(&[staging.as_str(), "packs"]);
    spinner.start("Preparing selected Notion packs…");
    let result = (|| -> Result<usize, String> {
        std::fs::create_dir_all(&packs_dir).map_err(|e| e.to_string())?;
        let mut plugins: Vec<Value> = Vec::new();
        let mut skill_count = 0;
        for (index, pack) in selected.iter().enumerate() {
            let mut downloaded: Option<DownloadedSource> = None;
            let r = (|| -> Result<(), String> {
                let dir = fetch_pack_directory(pack)?;
                let d = download_directory(&dir.url)?;
                let root = d.root_dir.clone();
                downloaded = Some(d);
                let skills = discover_skills(
                    &root,
                    None,
                    DiscoverOptions {
                        include_internal: true,
                        full_depth: true,
                        include_duplicate_names: false,
                    },
                )?;
                if skills.is_empty() {
                    return Err("pack contains no valid skills".into());
                }
                let name: String = sanitize_name(&pack.name).chars().take(100).collect();
                let dir_name = format!("pack-{}-{}", index + 1, name);
                copy_dir_all(&root, &join(&[packs_dir.as_str(), dir_name.as_str()]))
                    .map_err(|e| e.to_string())?;
                let skill_paths: Vec<String> = skills
                    .iter()
                    .map(|s| {
                        let rel = paths::relative(&root, &s.path)
                            .split(paths::SEP)
                            .collect::<Vec<_>>()
                            .join("/");
                        if rel.is_empty() {
                            "./.".to_string()
                        } else {
                            format!("./{}", rel)
                        }
                    })
                    .collect();
                plugins.push(json!({"name": pack.name, "source": format!("./{}", dir_name), "skills": skill_paths}));
                skill_count += skills.len();
                Ok(())
            })();
            if let Some(d) = &downloaded {
                let _ = cleanup_temp_dir(&d.temp_dir);
            }
            if let Err(e) = r {
                return Err(format!(
                    "Failed to prepare Notion pack {}: {}",
                    serde_json::to_string(&pack.name).unwrap(),
                    e
                ));
            }
        }
        let manifest_dir = join(&[staging.as_str(), ".claude-plugin"]);
        std::fs::create_dir_all(&manifest_dir).map_err(|e| e.to_string())?;
        let manifest = json!({"metadata": {"pluginRoot": "./packs"}, "plugins": plugins});
        std::fs::write(
            join(&[manifest_dir.as_str(), "marketplace.json"]),
            format!("{}\n", serde_json::to_string_pretty(&manifest).unwrap()),
        )
        .map_err(|e| e.to_string())?;
        Ok(skill_count)
    })();
    match result {
        Ok(count) => {
            spinner.stop(&format!(
                "Prepared {} Notion pack{} with {} skill{}",
                pc::green(selected.len().to_string()),
                if selected.len() == 1 { "" } else { "s" },
                pc::green(count.to_string()),
                if count == 1 { "" } else { "s" }
            ));
            Ok(Some(Prepared {
                root_dir: staging.clone(),
                temp_dir: staging,
                pack_count: Some(selected.len()),
            }))
        }
        Err(e) => {
            spinner.stop(&pc::red("Failed to prepare Notion packs"));
            let _ = cleanup_temp_dir(&staging);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_notion_urls() {
        assert_eq!(
            parse_notion_skill_url(
                "https://www.notion.so/team/My-Skill-0123456789abcdef0123456789abcdef"
            )
            .as_deref(),
            Some("01234567-89ab-cdef-0123-456789abcdef")
        );
        assert!(
            parse_notion_skill_url("https://example.com/0123456789abcdef0123456789abcdef")
                .is_none()
        );
        assert!(parse_notion_skill_url("owner/repo").is_none());
        assert!(is_notion_source("Notion"));
    }

    #[test]
    fn selectors() {
        assert_eq!(normalize_selector("My Pack!"), "my-pack");
    }
}
