//! Download a SKILL.md or archive URL into a temp directory
//! (port of download-source.ts).

use crate::archive::{read_zip_archive, ArchiveError, ArchiveLimits};
use crate::frontmatter::parse_frontmatter;
use crate::git::{parse_int_prefix, remove_all};
use crate::paths::{self, join};
use crate::sys;
use std::io::Read;
use std::time::Duration;

const DEFAULT_DOWNLOAD_MAX_BYTES: u64 = 10 * 1024 * 1024;
const DEFAULT_EXTRACT_MAX_BYTES: u64 = 25 * 1024 * 1024;
const DEFAULT_EXTRACT_MAX_FILES: u64 = 1000;
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum DownloadKind {
    SkillMd,
    Archive,
}

pub struct DownloadedSource {
    pub root_dir: String,
    pub temp_dir: String,
    pub kind: DownloadKind,
}

#[derive(Default, Clone, Copy)]
pub struct DownloadOptions {
    pub download_max_bytes: Option<u64>,
    pub extract_max_bytes: Option<u64>,
    pub extract_max_files: Option<u64>,
}

struct Limits {
    download_max_bytes: u64,
    extract_max_bytes: u64,
    extract_max_files: u64,
}

fn env_limit(name: &str, fallback: u64) -> u64 {
    match sys::env(name).and_then(|v| parse_int_prefix(&v)) {
        Some(v) if v > 0 => v as u64,
        _ => fallback,
    }
}

fn limits(o: &DownloadOptions) -> Limits {
    let or = |v: Option<u64>, d: u64| v.filter(|x| *x > 0).unwrap_or(d);
    Limits {
        download_max_bytes: env_limit(
            "SKILLS_DOWNLOAD_MAX_BYTES",
            or(o.download_max_bytes, DEFAULT_DOWNLOAD_MAX_BYTES),
        ),
        extract_max_bytes: env_limit(
            "SKILLS_EXTRACT_MAX_BYTES",
            or(o.extract_max_bytes, DEFAULT_EXTRACT_MAX_BYTES),
        ),
        extract_max_files: env_limit(
            "SKILLS_EXTRACT_MAX_FILES",
            or(o.extract_max_files, DEFAULT_EXTRACT_MAX_FILES),
        ),
    }
}

fn validate_archive_path(p: &str) -> Option<String> {
    let n = p.replace('\\', "/");
    let n = n.strip_prefix("./").map(|s| s.to_string()).unwrap_or(n);
    if n.is_empty() || n.ends_with('/') {
        return Some(n);
    }
    if n.starts_with('/') {
        return None;
    }
    let b = n.as_bytes();
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'/' {
        return None;
    }
    if n.split('/').any(|s| s == "..") {
        return None;
    }
    Some(n)
}

fn is_valid_skill_markdown(bytes: &[u8]) -> bool {
    let content = String::from_utf8_lossy(bytes);
    match parse_frontmatter(&content) {
        Ok(fm) => {
            fm.data.get("name").map(|v| v.is_string()).unwrap_or(false)
                && fm
                    .data
                    .get("description")
                    .map(|v| v.is_string())
                    .unwrap_or(false)
        }
        Err(_) => false,
    }
}

enum ExtractFail {
    Validation(String),
    Other,
}

fn extract_zip(data: &[u8], dir: &str, l: &Limits) -> Result<(), ExtractFail> {
    let files = read_zip_archive(
        data,
        &ArchiveLimits {
            max_extracted_bytes: l.extract_max_bytes,
            max_entries: l.extract_max_files,
        },
    )
    .map_err(|e| match e {
        ArchiveError::Validation(m) => ExtractFail::Validation(m),
        ArchiveError::Invalid(_) => ExtractFail::Other,
    })?;
    for (p, contents) in files {
        let target = join(&[dir, p.as_str()]);
        if !paths::is_path_safe(dir, &target) {
            return Err(ExtractFail::Validation(format!(
                "Archive contains unsafe path: {}",
                p
            )));
        }
        if p.ends_with('/') {
            continue;
        }
        std::fs::create_dir_all(paths::dirname(&target)).map_err(|_| ExtractFail::Other)?;
        std::fs::write(&target, contents).map_err(|_| ExtractFail::Other)?;
    }
    Ok(())
}

fn extract_tar(data: &[u8], dir: &str, l: &Limits) -> Result<(), ExtractFail> {
    let reader: Box<dyn Read> = if data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b {
        Box::new(flate2::read::MultiGzDecoder::new(data))
    } else {
        Box::new(data)
    };
    let mut archive = tar::Archive::new(reader);
    let entries = archive.entries().map_err(|_| ExtractFail::Other)?;
    let mut count: u64 = 0;
    let mut bytes: u64 = 0;
    for entry in entries {
        let mut entry = entry.map_err(|_| ExtractFail::Other)?;
        let raw_path = entry
            .path()
            .map_err(|_| ExtractFail::Other)?
            .to_string_lossy()
            .to_string();
        let safe = validate_archive_path(&raw_path).ok_or_else(|| {
            ExtractFail::Validation(format!("Archive contains unsafe path: {}", raw_path))
        })?;
        let target = join(&[dir, safe.as_str()]);
        if !paths::is_path_safe(dir, &target) {
            return Err(ExtractFail::Validation(format!(
                "Archive contains unsafe path: {}",
                raw_path
            )));
        }
        count += 1;
        if count > l.extract_max_files {
            return Err(ExtractFail::Validation(format!(
                "Archive contains too many files ({}). Maximum is {}. Set SKILLS_EXTRACT_MAX_FILES to override.",
                count, l.extract_max_files
            )));
        }
        let size = entry.header().size().unwrap_or(0);
        bytes += size;
        if bytes > l.extract_max_bytes {
            return Err(ExtractFail::Validation(format!(
                "Archive extracts to more than {} bytes. Set SKILLS_EXTRACT_MAX_BYTES to override.",
                l.extract_max_bytes
            )));
        }
        let et = entry.header().entry_type();
        if et.is_file() || et == tar::EntryType::Continuous {
            std::fs::create_dir_all(paths::dirname(&target)).map_err(|_| ExtractFail::Other)?;
            let mut contents = Vec::new();
            entry
                .read_to_end(&mut contents)
                .map_err(|_| ExtractFail::Other)?;
            std::fs::write(&target, contents).map_err(|_| ExtractFail::Other)?;
        } else if et.is_dir() {
            std::fs::create_dir_all(&target).map_err(|_| ExtractFail::Other)?;
        }
    }
    if count == 0 {
        return Err(ExtractFail::Other);
    }
    Ok(())
}

fn try_extract(data: &[u8], dir: &str, l: &Limits) -> Result<bool, String> {
    let is_zip = data.len() >= 2 && data[0] == 0x50 && data[1] == 0x4b;
    let result = if is_zip {
        extract_zip(data, dir, l)
    } else {
        extract_tar(data, dir, l)
    };
    match result {
        Ok(()) => Ok(true),
        Err(f) => {
            let _ = remove_all(dir);
            let _ = std::fs::create_dir_all(dir);
            match f {
                ExtractFail::Validation(m) => Err(m),
                ExtractFail::Other => Ok(false),
            }
        }
    }
}

fn single_top_level_directory(dir: &str) -> Option<String> {
    let entries: Vec<_> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.file_name() != "__MACOSX")
        .collect();
    if entries.len() != 1 || !entries[0].file_type().map(|t| t.is_dir()).unwrap_or(false) {
        return None;
    }
    Some(join(&[
        dir,
        entries[0].file_name().to_string_lossy().as_ref(),
    ]))
}

fn download(url: &str, l: &Limits) -> Result<Vec<u8>, String> {
    let too_large = || {
        format!(
            "Download is larger than {} bytes. Set SKILLS_DOWNLOAD_MAX_BYTES to override.",
            l.download_max_bytes
        )
    };
    let resp = crate::http::get(url)
        .timeout(FETCH_TIMEOUT)
        .max_bytes(l.download_max_bytes)
        .send()
        .map_err(|e| {
            if e.starts_with("too-large:") {
                too_large()
            } else {
                format!("fetch failed: {}", e)
            }
        })?;
    if !resp.ok() {
        return Err(format!("Download failed with HTTP {}", resp.status));
    }
    Ok(resp.body)
}

/// Download `url` and prepare it as a local skill source.
pub fn download_source(url: &str, options: DownloadOptions) -> Result<DownloadedSource, String> {
    let l = limits(&options);
    let temp_dir = sys::mkdtemp("skills-download-").map_err(|e| e.to_string())?;
    let result = (|| -> Result<DownloadedSource, String> {
        let data = download(url, &l)?;
        std::fs::write(join(&[temp_dir.as_str(), "source.download"]), &data)
            .map_err(|e| e.to_string())?;
        if data.is_empty() {
            return Err("Downloaded URL is empty".into());
        }
        if is_valid_skill_markdown(&data) {
            let skill_dir = join(&[temp_dir.as_str(), "skill"]);
            std::fs::create_dir_all(&skill_dir).map_err(|e| e.to_string())?;
            std::fs::write(join(&[skill_dir.as_str(), "SKILL.md"]), &data)
                .map_err(|e| e.to_string())?;
            return Ok(DownloadedSource {
                root_dir: skill_dir,
                temp_dir: temp_dir.clone(),
                kind: DownloadKind::SkillMd,
            });
        }
        let extract_dir = join(&[temp_dir.as_str(), "extract"]);
        std::fs::create_dir_all(&extract_dir).map_err(|e| e.to_string())?;
        if try_extract(&data, &extract_dir, &l)? {
            let root = single_top_level_directory(&extract_dir).unwrap_or(extract_dir);
            return Ok(DownloadedSource {
                root_dir: root,
                temp_dir: temp_dir.clone(),
                kind: DownloadKind::Archive,
            });
        }
        Err("Downloaded URL is not a valid SKILL.md file or supported archive".into())
    })();
    if result.is_err() {
        let _ = remove_all(&temp_dir);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_path_validation() {
        assert_eq!(validate_archive_path("./a/b").as_deref(), Some("a/b"));
        assert_eq!(validate_archive_path("dir/").as_deref(), Some("dir/"));
        assert!(validate_archive_path("/etc/passwd").is_none());
        assert!(validate_archive_path("C:/x").is_none());
        assert!(validate_archive_path("a/../../b").is_none());
    }

    #[test]
    fn extracts_zip_and_tar() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().to_string_lossy().to_string();
        let l = limits(&DownloadOptions::default());
        let z = crate::archive::tests::build_zip(&[(
            "pkg/SKILL.md",
            b"---\nname: a\ndescription: b\n---\n",
        )]);
        assert!(try_extract(&z, &dir, &l).unwrap());
        assert_eq!(
            single_top_level_directory(&dir),
            Some(join(&[dir.as_str(), "pkg"]))
        );

        let t2 = tempfile::tempdir().unwrap();
        let dir2 = t2.path().to_string_lossy().to_string();
        let mut builder = tar::Builder::new(Vec::new());
        let data = b"hello";
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_cksum();
        builder
            .append_data(&mut header, "x/SKILL.md", &data[..])
            .unwrap();
        let tarball = builder.into_inner().unwrap();
        assert!(try_extract(&tarball, &dir2, &l).unwrap());
        assert!(std::path::Path::new(&join(&[dir2.as_str(), "x", "SKILL.md"])).exists());

        assert!(!try_extract(b"just some text that is not an archive", &dir2, &l).unwrap());
    }
}
