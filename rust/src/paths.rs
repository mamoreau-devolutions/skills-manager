//! Node.js `path` module semantics (posix and win32) operating on strings.
//!
//! The TypeScript CLI relies heavily on `path.join`/`resolve`/`normalize` and
//! then performs string prefix checks with `path.sep` for path-traversal
//! protection. `std::path` does not normalize `..` segments, so reproducing
//! Node's behavior exactly is what keeps those safety checks equivalent.

use crate::sys;

#[cfg(windows)]
pub const SEP: &str = "\\";
#[cfg(not(windows))]
pub const SEP: &str = "/";

#[cfg(windows)]
const WIN: bool = true;
#[cfg(not(windows))]
const WIN: bool = false;

fn is_sep_char(c: char) -> bool {
    if WIN {
        c == '/' || c == '\\'
    } else {
        c == '/'
    }
}

/// Resolve `.` and `..` segments (Node's internal `normalizeString`).
fn normalize_string(path: &str, allow_above_root: bool, separator: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for segment in path.split(is_sep_char) {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            if let Some(last) = out.last() {
                if *last != ".." {
                    out.pop();
                    continue;
                }
            }
            if allow_above_root {
                out.push("..");
            }
            continue;
        }
        out.push(segment);
    }
    out.join(separator)
}

/// Split a win32 path into (device, is_absolute, rest).
fn win32_root(path: &str) -> (Option<String>, bool, &str) {
    let chars: Vec<char> = path.chars().take(3).collect();
    if chars.is_empty() {
        return (None, false, path);
    }
    if is_sep_char(chars[0]) {
        if chars.len() > 1 && is_sep_char(chars[1]) {
            // Possible UNC root: \\server\share
            let rest = &path[2..];
            let mut parts = rest.splitn(3, is_sep_char);
            let server = parts.next().unwrap_or("");
            let share = parts.next();
            if !server.is_empty() {
                if let Some(share) = share {
                    if !share.is_empty() {
                        let device = format!("\\\\{}\\{}", server, share);
                        let consumed = 2 + server.len() + 1 + share.len();
                        let tail = &path[consumed.min(path.len())..];
                        return (Some(device), true, tail);
                    }
                }
            }
        }
        return (None, true, &path[1..]);
    }
    if chars.len() >= 2 && chars[1] == ':' && chars[0].is_ascii_alphabetic() {
        let device = path[..2].to_string();
        if chars.len() >= 3 && is_sep_char(chars[2]) {
            return (Some(device), true, &path[3..]);
        }
        return (Some(device), false, &path[2..]);
    }
    (None, false, path)
}

pub fn is_absolute(path: &str) -> bool {
    if WIN {
        let (device, abs, _) = win32_root(path);
        abs && (device.is_some() || path.starts_with(is_sep_char))
    } else {
        path.starts_with('/')
    }
}

pub fn normalize(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    if WIN {
        let (device, abs, rest) = win32_root(path);
        let mut tail = normalize_string(rest, !abs, "\\");
        if tail.is_empty() && !abs {
            tail = ".".to_string();
        }
        if !tail.is_empty() && path.ends_with(is_sep_char) && !(device.is_some() && rest.is_empty())
        {
            tail.push('\\');
        }
        match device {
            None => {
                if abs {
                    format!("\\{}", tail)
                } else {
                    tail
                }
            }
            Some(d) => {
                if abs {
                    format!("{}\\{}", d, tail)
                } else {
                    format!("{}{}", d, tail)
                }
            }
        }
    } else {
        let abs = path.starts_with('/');
        let trailing = path.ends_with('/');
        let mut p = normalize_string(path, !abs, "/");
        if p.is_empty() {
            if abs {
                return "/".to_string();
            }
            return if trailing {
                "./".to_string()
            } else {
                ".".to_string()
            };
        }
        if trailing {
            p.push('/');
        }
        if abs {
            format!("/{}", p)
        } else {
            p
        }
    }
}

/// `path.join(...parts)`
pub fn join<S: AsRef<str>>(parts: &[S]) -> String {
    let non_empty: Vec<&str> = parts
        .iter()
        .map(|p| p.as_ref())
        .filter(|p| !p.is_empty())
        .collect();
    if non_empty.is_empty() {
        return ".".to_string();
    }
    let joined = non_empty.join(if WIN { "\\" } else { "/" });
    normalize(&joined)
}

/// Convenience two-argument join.
pub fn join2(a: &str, b: &str) -> String {
    join(&[a, b])
}

/// `path.resolve(...parts)` relative to the process working directory.
pub fn resolve<S: AsRef<str>>(parts: &[S]) -> String {
    let cwd = sys::cwd();
    if WIN {
        let mut resolved_device: Option<String> = None;
        let mut resolved_tail = String::new();
        let mut resolved_abs = false;
        let mut candidates: Vec<String> = parts.iter().map(|p| p.as_ref().to_string()).collect();
        candidates.insert(0, cwd.clone());
        for path in candidates.iter().rev() {
            if path.is_empty() {
                continue;
            }
            let (device, abs, rest) = win32_root(path);
            if let (Some(d), Some(rd)) = (&device, &resolved_device) {
                if !d.eq_ignore_ascii_case(rd) {
                    continue;
                }
            }
            if resolved_device.is_none() {
                if let Some(d) = &device {
                    resolved_device = Some(d.clone());
                }
            }
            if !resolved_abs {
                resolved_tail = if resolved_tail.is_empty() {
                    rest.to_string()
                } else {
                    format!("{}\\{}", rest, resolved_tail)
                };
                resolved_abs = abs;
            }
            if resolved_abs && resolved_device.is_some() {
                break;
            }
        }
        if resolved_device.is_none() {
            let (d, _, _) = win32_root(&cwd);
            resolved_device = d;
        }
        let tail = normalize_string(&resolved_tail, !resolved_abs, "\\");
        let device = resolved_device.unwrap_or_default();
        if resolved_abs {
            format!("{}\\{}", device, tail)
        } else {
            let s = format!("{}{}", device, tail);
            if s.is_empty() {
                ".".to_string()
            } else {
                s
            }
        }
    } else {
        let mut resolved = String::new();
        let mut abs = false;
        for p in parts.iter().rev() {
            let p = p.as_ref();
            if p.is_empty() {
                continue;
            }
            resolved = if resolved.is_empty() {
                p.to_string()
            } else {
                format!("{}/{}", p, resolved)
            };
            if p.starts_with('/') {
                abs = true;
                break;
            }
        }
        if !abs {
            resolved = if resolved.is_empty() {
                cwd
            } else {
                format!("{}/{}", cwd, resolved)
            };
        }
        let tail = normalize_string(&resolved, false, "/");
        format!("/{}", tail)
    }
}

pub fn resolve1(p: &str) -> String {
    resolve(&[p])
}

/// `path.relative(from, to)`
pub fn relative(from: &str, to: &str) -> String {
    let from_r = resolve1(from);
    let to_r = resolve1(to);
    let (cmp_from, cmp_to) = if WIN {
        (from_r.to_lowercase(), to_r.to_lowercase())
    } else {
        (from_r.clone(), to_r.clone())
    };
    if cmp_from == cmp_to {
        return String::new();
    }
    if WIN {
        let (fd, _, _) = win32_root(&from_r);
        let (td, _, _) = win32_root(&to_r);
        let same = match (&fd, &td) {
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            (None, None) => true,
            _ => false,
        };
        if !same {
            return to_r;
        }
    }
    let split = |s: &str| -> Vec<String> {
        s.split(is_sep_char)
            .filter(|x| !x.is_empty())
            .map(|x| x.to_string())
            .collect()
    };
    let from_parts = split(&from_r);
    let to_parts = split(&to_r);
    let from_cmp = split(&cmp_from);
    let to_cmp = split(&cmp_to);
    let mut common = 0;
    while common < from_cmp.len() && common < to_cmp.len() && from_cmp[common] == to_cmp[common] {
        common += 1;
    }
    let mut out: Vec<String> = Vec::new();
    for _ in common..from_parts.len() {
        out.push("..".to_string());
    }
    for part in &to_parts[common..] {
        out.push(part.clone());
    }
    out.join(SEP)
}

fn strip_trailing_seps(path: &str) -> &str {
    let mut end = path.len();
    while end > 1 && path[..end].ends_with(is_sep_char) {
        end -= 1;
    }
    &path[..end]
}

/// `path.dirname(p)`
pub fn dirname(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    if WIN {
        let (_, _, rest) = win32_root(path);
        let root_len = path.len() - rest.len();
        let root = &path[..root_len];
        let rest_trimmed = {
            let mut r = rest;
            while r.ends_with(is_sep_char) {
                r = &r[..r.len() - 1];
            }
            r
        };
        match rest_trimmed.rfind(is_sep_char) {
            Some(idx) => format!("{}{}", root, &rest_trimmed[..idx]),
            None => {
                if root.is_empty() {
                    ".".to_string()
                } else {
                    root.to_string()
                }
            }
        }
    } else {
        let trimmed = strip_trailing_seps(path);
        if trimmed == "/" {
            return "/".to_string();
        }
        match trimmed.rfind('/') {
            Some(0) => "/".to_string(),
            Some(idx) => {
                let mut d = &trimmed[..idx];
                while d.len() > 1 && d.ends_with('/') {
                    d = &d[..d.len() - 1];
                }
                d.to_string()
            }
            None => ".".to_string(),
        }
    }
}

/// `path.basename(p)`
pub fn basename(path: &str) -> String {
    let rest = if WIN { win32_root(path).2 } else { path };
    let mut r = rest;
    while r.ends_with(is_sep_char) {
        r = &r[..r.len() - 1];
    }
    match r.rfind(is_sep_char) {
        Some(idx) => r[idx + 1..].to_string(),
        None => r.to_string(),
    }
}

/// `path.extname(p)`
pub fn extname(path: &str) -> String {
    let base = basename(path);
    match base.rfind('.') {
        Some(0) | None => String::new(),
        Some(idx) => base[idx..].to_string(),
    }
}

/// Convert platform separators to forward slashes.
pub fn to_posix(path: &str) -> String {
    if WIN {
        path.replace('\\', "/")
    } else {
        path.to_string()
    }
}

/// `normalize(resolve(target))` starts with `normalize(resolve(base)) + sep`
/// or equals it. Mirrors the many `isPathSafe` helpers in the TS source.
pub fn is_path_safe(base: &str, target: &str) -> bool {
    let nb = normalize(&resolve1(base));
    let nt = normalize(&resolve1(target));
    nt.starts_with(&format!("{}{}", nb, SEP)) || nt == nb
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn posix_semantics() {
        assert_eq!(join(&["/a/b", "../c"]), "/a/c");
        assert_eq!(join(&["a", "", "b/"]), "a/b/");
        assert_eq!(normalize("/a//b/./c/.."), "/a/b");
        assert_eq!(dirname("/a/b/"), "/a");
        assert_eq!(dirname("a"), ".");
        assert_eq!(dirname("/a"), "/");
        assert_eq!(basename("/a/b/"), "b");
        assert_eq!(relative("/a/b/c", "/a/d"), "../../d");
        assert_eq!(resolve(&["/a", "b", "../c"]), "/a/c");
        assert_eq!(extname("x/SKILL.md"), ".md");
        assert_eq!(extname(".hidden"), "");
    }

    #[cfg(windows)]
    #[test]
    fn win32_semantics() {
        assert_eq!(join(&["C:\\a\\b", "../c"]), "C:\\a\\c");
        assert_eq!(normalize("C:/a//b/./c/.."), "C:\\a\\b");
        assert_eq!(dirname("C:\\a\\b"), "C:\\a");
        assert_eq!(dirname("C:\\a"), "C:\\");
        assert_eq!(basename("C:\\a\\b\\"), "b");
        assert_eq!(relative("C:\\a\\b\\c", "C:\\a\\d"), "..\\..\\d");
        assert_eq!(relative("C:\\a", "D:\\b"), "D:\\b");
        assert_eq!(resolve(&["C:\\a", "b", "..\\c"]), "C:\\a\\c");
        assert!(is_absolute("C:\\x"));
        assert!(is_absolute("\\x"));
        assert!(!is_absolute("C:x"));
        assert!(is_path_safe("C:\\a", "C:\\a\\b"));
        assert!(!is_path_safe("C:\\a", "C:\\a\\..\\b"));
    }
}
