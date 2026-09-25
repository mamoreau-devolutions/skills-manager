//! Process-level helpers: environment, home/temp directories, output routing
//! and exit handling that mirror Node's `process` / `os` behavior.

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Mutex;

/// When set, everything written through [`write_out`] goes to stderr. This is
/// how `add --json` keeps stdout reserved for the final JSON value.
static REDIRECT_STDOUT: AtomicBool = AtomicBool::new(false);
static EXIT_CODE: AtomicI32 = AtomicI32::new(0);

type ExitHook = Box<dyn FnOnce() + Send>;
static EXIT_HOOKS: Mutex<Vec<ExitHook>> = Mutex::new(Vec::new());

pub fn set_stdout_redirect(on: bool) {
    REDIRECT_STDOUT.store(on, Ordering::SeqCst);
}

pub fn stdout_redirected() -> bool {
    REDIRECT_STDOUT.load(Ordering::SeqCst)
}

/// Write to (possibly redirected) stdout.
pub fn write_out(s: &str) {
    if stdout_redirected() {
        let mut err = std::io::stderr().lock();
        let _ = err.write_all(s.as_bytes());
        let _ = err.flush();
    } else {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(s.as_bytes());
        let _ = out.flush();
    }
}

/// Write directly to the real stdout, bypassing redirection.
pub fn write_real_stdout(s: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(s.as_bytes());
    let _ = out.flush();
}

pub fn write_err(s: &str) {
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(s.as_bytes());
    let _ = err.flush();
}

#[macro_export]
macro_rules! outln {
    () => { $crate::sys::write_out("\n") };
    ($($arg:tt)*) => { $crate::sys::write_out(&format!("{}\n", format_args!($($arg)*))) };
}

#[macro_export]
macro_rules! out {
    ($($arg:tt)*) => { $crate::sys::write_out(&format!($($arg)*)) };
}

#[macro_export]
macro_rules! errln {
    () => { $crate::sys::write_err("\n") };
    ($($arg:tt)*) => { $crate::sys::write_err(&format!("{}\n", format_args!($($arg)*))) };
}

pub fn set_exit_code(code: i32) {
    EXIT_CODE.store(code, Ordering::SeqCst);
}

pub fn exit_code() -> i32 {
    EXIT_CODE.load(Ordering::SeqCst)
}

/// Register a hook that runs before the process exits via [`exit`].
pub fn on_exit(hook: ExitHook) {
    EXIT_HOOKS.lock().unwrap().push(hook);
}

pub fn clear_exit_hooks() {
    EXIT_HOOKS.lock().unwrap().clear();
}

/// Equivalent of `process.exit(code)`: runs exit hooks, then terminates
/// immediately (pending telemetry is intentionally not awaited, as in Node).
pub fn exit(code: i32) -> ! {
    let hooks: Vec<ExitHook> = std::mem::take(&mut *EXIT_HOOKS.lock().unwrap());
    for hook in hooks {
        hook();
    }
    crate::ui::on_process_exit(code);
    crate::ui::restore_terminal();
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    std::process::exit(code);
}

/// `process.env.NAME` when set and non-empty (JS truthiness).
pub fn env(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(v) if !v.is_empty() => Some(v),
        _ => None,
    }
}

/// `process.env.NAME` when set, even if empty.
pub fn env_raw(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

pub fn env_truthy(name: &str) -> bool {
    env(name).is_some()
}

/// `process.env.NAME?.trim() || fallback`
pub fn env_trimmed(name: &str) -> Option<String> {
    env_raw(name)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub fn cwd() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string())
}

/// `os.homedir()` (libuv `uv_os_homedir`).
pub fn homedir() -> String {
    #[cfg(windows)]
    {
        if let Some(p) = env("USERPROFILE") {
            return p;
        }
        if let (Some(d), Some(p)) = (env("HOMEDRIVE"), env("HOMEPATH")) {
            return format!("{}{}", d, p);
        }
        "C:\\".to_string()
    }
    #[cfg(not(windows))]
    {
        if let Some(p) = env("HOME") {
            return p;
        }
        "/".to_string()
    }
}

/// `os.tmpdir()`
pub fn tmpdir() -> String {
    #[cfg(windows)]
    {
        let mut p = env("TEMP").or_else(|| env("TMP")).unwrap_or_else(|| {
            format!(
                "{}\\temp",
                env("SystemRoot")
                    .or_else(|| env("windir"))
                    .unwrap_or_else(|| "C:\\Windows".into())
            )
        });
        if p.len() > 1 && p.ends_with('\\') && !p.ends_with(":\\") {
            p.pop();
        }
        p
    }
    #[cfg(not(windows))]
    {
        let mut p = env("TMPDIR")
            .or_else(|| env("TMP"))
            .or_else(|| env("TEMP"))
            .unwrap_or_else(|| "/tmp".into());
        if p.len() > 1 && p.ends_with('/') {
            p.pop();
        }
        p
    }
}

/// `fs.mkdtemp(join(tmpdir(), prefix))`
pub fn mkdtemp(prefix: &str) -> std::io::Result<String> {
    let base = tmpdir();
    std::fs::create_dir_all(&base)?;
    for _ in 0..100 {
        let suffix = random_suffix(6);
        let dir = crate::paths::join(&[base.as_str(), &format!("{}{}", prefix, suffix)]);
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("failed to create temp dir"))
}

fn random_suffix(len: usize) -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut out = String::new();
    let mut h = RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    h.write_u32(std::process::id());
    let mut v = h.finish();
    for _ in 0..len {
        out.push(CHARS[(v % CHARS.len() as u64) as usize] as char);
        v /= CHARS.len() as u64;
        if v == 0 {
            v = RandomState::new().build_hasher().finish();
        }
    }
    out
}

pub fn stdin_is_tty() -> bool {
    std::io::stdin().is_terminal()
}

pub fn stdout_is_tty() -> bool {
    std::io::stdout().is_terminal()
}

pub fn terminal_columns() -> Option<u16> {
    if !stdout_is_tty() {
        return None;
    }
    crossterm::terminal::size()
        .ok()
        .map(|(c, _)| c)
        .filter(|c| *c > 0)
}

pub fn terminal_rows() -> Option<u16> {
    if !stdout_is_tty() {
        return None;
    }
    crossterm::terminal::size()
        .ok()
        .map(|(_, r)| r)
        .filter(|r| *r > 0)
}

pub fn now_iso() -> String {
    // `new Date().toISOString()` → 2024-01-02T03:04:05.678Z
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let millis = now.as_millis() as i64;
    let secs = millis.div_euclid(1000);
    let ms = millis.rem_euclid(1000);
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Civil-from-days (Howard Hinnant)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y, mo, d, h, m, s, ms
    )
}

pub fn is_windows() -> bool {
    cfg!(windows)
}
