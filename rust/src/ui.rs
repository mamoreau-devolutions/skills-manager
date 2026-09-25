//! A port of the subset of `@clack/prompts` used by the CLI: intro/outro,
//! cancel, log.*, note, spinner, select, confirm and multiselect.
//!
//! Output formatting follows clack 1.x. Interactive prompts use raw terminal
//! mode via crossterm. When stdin is not a TTY, prompts behave like clack on
//! EOF: they render a cancelled frame and report cancellation.

use crate::color::style;
use crate::sys;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

// ─── Symbols ───

fn unicode_supported() -> bool {
    static U: OnceLock<bool> = OnceLock::new();
    *U.get_or_init(|| {
        if !cfg!(windows) {
            return std::env::var("TERM").map(|t| t != "linux").unwrap_or(true);
        }
        let term = std::env::var("TERM").unwrap_or_default();
        let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();
        sys::env_truthy("WT_SESSION")
            || sys::env_truthy("TERMINUS_SUBLIME")
            || std::env::var("ConEmuTask")
                .map(|v| v == "{cmd::Cmder}")
                .unwrap_or(false)
            || term_program == "Terminus-Sublime"
            || term_program == "vscode"
            || term == "xterm-256color"
            || term == "alacritty"
            || term == "rxvt-unicode"
            || term == "rxvt-unicode-256color"
            || std::env::var("TERMINAL_EMULATOR")
                .map(|v| v == "JetBrains-JediTerm")
                .unwrap_or(false)
            || sys::env_truthy("CI")
    })
}

fn u(unicode: &'static str, fallback: &'static str) -> &'static str {
    if unicode_supported() {
        unicode
    } else {
        fallback
    }
}

pub fn s_step_active() -> &'static str {
    u("◆", "*")
}
pub fn s_step_cancel() -> &'static str {
    u("■", "x")
}
pub fn s_step_submit() -> &'static str {
    u("◇", "o")
}
pub fn s_bar_start() -> &'static str {
    u("┌", "T")
}
pub fn s_bar() -> &'static str {
    u("│", "|")
}
pub fn s_bar_end() -> &'static str {
    u("└", "—")
}
pub fn s_radio_active() -> &'static str {
    u("●", ">")
}
pub fn s_radio_inactive() -> &'static str {
    u("○", " ")
}
fn s_checkbox_active() -> &'static str {
    u("◻", "[•]")
}
fn s_checkbox_selected() -> &'static str {
    u("◼", "[+]")
}
fn s_checkbox_inactive() -> &'static str {
    u("◻", "[ ]")
}
fn s_bar_h() -> &'static str {
    u("─", "-")
}
fn s_corner_top_right() -> &'static str {
    u("╮", "+")
}
fn s_connect_left() -> &'static str {
    u("├", "+")
}
fn s_corner_bottom_right() -> &'static str {
    u("╯", "+")
}
fn s_info() -> &'static str {
    u("●", "•")
}
fn s_success() -> &'static str {
    u("◆", "*")
}
fn s_warn() -> &'static str {
    u("▲", "!")
}
fn s_error() -> &'static str {
    u("■", "x")
}

// ─── Width helpers ───

/// Strip ANSI escape sequences (Node's `stripVTControlCharacters`).
pub fn strip_ansi(s: &str) -> String {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    let re = R.get_or_init(|| {
        regex::Regex::new(r"[\x1b\x{9b}][\[\]()#;?]*(?:(?:(?:(?:;[-a-zA-Z\d/#&.:=?%@~_]+)*|[a-zA-Z\d]+(?:;[-a-zA-Z\d/#&.:=?%@~_]*)*)?(?:\x07|\x1b\\|\x{9c}))|(?:(?:\d{1,4}(?:;\d{0,4})*)?[\dA-PR-TZcf-nq-uy=><~]))").unwrap()
    });
    re.replace_all(s, "").into_owned()
}

/// Approximate terminal display width of plain text (same table as the TS
/// `approxStringWidth` in search-multiselect.ts).
pub fn approx_string_width(plain: &str) -> usize {
    plain
        .chars()
        .map(|ch| {
            let code = ch as u32;
            if code == 0 {
                return 0;
            }
            let wide = (0x1100..=0x115f).contains(&code)
                || (0x231a..=0x231b).contains(&code)
                || (0x2329..=0x232a).contains(&code)
                || (0x23e9..=0x23ec).contains(&code)
                || code == 0x23f0
                || code == 0x23f3
                || (0x25fd..=0x25fe).contains(&code)
                || (0x2614..=0x2615).contains(&code)
                || (0x2648..=0x2653).contains(&code)
                || code == 0x267f
                || code == 0x2693
                || code == 0x26a1
                || (0x26aa..=0x26ab).contains(&code)
                || (0x26bd..=0x26be).contains(&code)
                || (0x26c4..=0x26c5).contains(&code)
                || code == 0x26ce
                || code == 0x26d4
                || code == 0x26ea
                || (0x26f2..=0x26f3).contains(&code)
                || code == 0x26f5
                || code == 0x26fa
                || code == 0x26fd
                || code == 0x2705
                || (0x270a..=0x270b).contains(&code)
                || code == 0x2728
                || code == 0x274c
                || code == 0x274e
                || (0x2753..=0x2755).contains(&code)
                || code == 0x2757
                || (0x2795..=0x2797).contains(&code)
                || code == 0x27b0
                || code == 0x27bf
                || (0x2b1b..=0x2b1c).contains(&code)
                || code == 0x2b50
                || code == 0x2b55
                || ((0x2e80..=0xa4cf).contains(&code) && code != 0x303f)
                || (0xa960..=0xa97c).contains(&code)
                || (0xac00..=0xd7a3).contains(&code)
                || (0xf900..=0xfaff).contains(&code)
                || (0xfe10..=0xfe19).contains(&code)
                || (0xfe30..=0xfe6f).contains(&code)
                || (0xff00..=0xff60).contains(&code)
                || (0xffe0..=0xffe6).contains(&code)
                || (0x1f000..=0x1f9ff).contains(&code);
            if wide {
                2
            } else {
                1
            }
        })
        .sum()
}

pub fn visible_width(s: &str) -> usize {
    approx_string_width(&strip_ansi(s))
}

pub fn visual_rows_for_line(line: &str, columns: usize) -> usize {
    let cols = columns.max(1);
    let w = visible_width(line);
    w.div_ceil(cols).max(1)
}

pub fn count_visual_rows(lines: &[String], columns: Option<usize>) -> usize {
    let cols = columns
        .filter(|c| *c > 0)
        .or_else(|| sys::terminal_columns().map(|c| c as usize))
        .unwrap_or(80);
    lines.iter().map(|l| visual_rows_for_line(l, cols)).sum()
}

fn columns() -> usize {
    sys::terminal_columns().map(|c| c as usize).unwrap_or(80)
}

/// Closing SGR code for an opening one (ansi-styles `codes` map).
fn sgr_close(code: u32) -> Option<u32> {
    match code {
        0 => Some(0),
        1 | 2 => Some(22),
        3 => Some(23),
        4 => Some(24),
        7 => Some(27),
        8 => Some(28),
        9 => Some(29),
        53 => Some(55),
        30..=37 | 90..=97 => Some(39),
        40..=47 | 100..=107 => Some(49),
        _ => None,
    }
}

fn char_width(c: char) -> usize {
    if (c as u32) < 0x20 || (0x7f..0xa0).contains(&(c as u32)) {
        return 0;
    }
    approx_string_width(&c.to_string())
}

/// wrap-ansi's `wrapWord`: hard-wrap one word across rows.
fn wrap_word(rows: &mut Vec<String>, word: &str, columns: usize) {
    let chars: Vec<char> = word.chars().collect();
    let mut inside_escape = false;
    let mut visible = visible_width(rows.last().unwrap());
    for (index, &c) in chars.iter().enumerate() {
        let len = char_width(c);
        if visible + len <= columns {
            rows.last_mut().unwrap().push(c);
        } else {
            rows.push(c.to_string());
            visible = 0;
        }
        if c == '\x1b' || c == '\u{9b}' {
            inside_escape = true;
        }
        if inside_escape {
            if c == 'm' {
                inside_escape = false;
            }
            continue;
        }
        visible += len;
        if visible == columns && index < chars.len() - 1 {
            rows.push(String::new());
            visible = 0;
        }
    }
    if visible == 0 && !rows.last().unwrap().is_empty() && rows.len() > 1 {
        let last = rows.pop().unwrap();
        rows.last_mut().unwrap().push_str(&last);
    }
}

/// wrap-ansi `exec` for one line with `{ hard: true, trim: false }`.
fn wrap_line(line: &str, columns: usize) -> String {
    let words: Vec<&str> = line.split(' ').collect();
    let lengths: Vec<usize> = words.iter().map(|w| visible_width(w)).collect();
    let mut rows: Vec<String> = vec![String::new()];
    for (index, word) in words.iter().enumerate() {
        let mut row_len = visible_width(rows.last().unwrap());
        if index != 0 {
            if row_len >= columns {
                rows.push(String::new());
                row_len = 0;
            }
            rows.last_mut().unwrap().push(' ');
            row_len += 1;
        }
        if lengths[index] > columns {
            let remaining = columns as isize - row_len as isize;
            let starting_this = 1
                + ((lengths[index] as isize - remaining - 1) as f64 / columns as f64).floor()
                    as isize;
            let starting_next = (lengths[index] as isize - 1) / columns as isize;
            if starting_next < starting_this {
                rows.push(String::new());
            }
            wrap_word(&mut rows, word, columns);
            continue;
        }
        if row_len + lengths[index] > columns && row_len > 0 && lengths[index] > 0 {
            rows.push(String::new());
        }
        rows.last_mut().unwrap().push_str(word);
    }
    let pre = rows.join("\n");
    // Re-open the active SGR style across inserted line breaks.
    let sgr = regex::Regex::new(r"^\x1b\[(\d+)m").unwrap();
    let chars: Vec<(usize, char)> = pre.char_indices().collect();
    let mut out = String::new();
    let mut escape_code: Option<u32> = None;
    for (i, &(byte_idx, c)) in chars.iter().enumerate() {
        out.push(c);
        if c == '\x1b' {
            if let Some(cap) = sgr.captures(&pre[byte_idx..]) {
                let code: u32 = cap[1].parse().unwrap_or(0);
                escape_code = if code == 39 { None } else { Some(code) };
            }
        }
        let close = escape_code.and_then(sgr_close);
        let active = escape_code.filter(|c| *c != 0);
        if chars.get(i + 1).map(|x| x.1) == Some('\n') {
            if let (Some(_), Some(cl)) = (active, close) {
                out.push_str(&format!("\x1b[{}m", cl));
            }
        } else if c == '\n' {
            if let (Some(code), Some(_)) = (active, close) {
                out.push_str(&format!("\x1b[{}m", code));
            }
        }
    }
    out
}

/// Port of `wrap-ansi` with `{ hard: true, trim: false }` (used by `note`).
pub fn wrap_ansi(s: &str, columns: usize) -> String {
    let columns = columns.max(1);
    s.replace("\r\n", "\n")
        .split('\n')
        .map(|l| wrap_line(l, columns))
        .collect::<Vec<_>>()
        .join("\n")
}

// ─── Static output ───

pub fn intro(title: &str) {
    sys::write_out(&format!("{}  {}\n", style::gray(s_bar_start()), title));
}

pub fn outro(message: &str) {
    sys::write_out(&format!(
        "{}\n{}  {}\n\n",
        style::gray(s_bar()),
        style::gray(s_bar_end()),
        message
    ));
}

pub fn cancel(message: &str) {
    sys::write_out(&format!(
        "{}  {}\n\n",
        style::gray(s_bar_end()),
        style::red(message)
    ));
}

fn log_with_symbol(message: &str, symbol: &str) {
    let bar = style::gray(s_bar());
    let mut lines: Vec<String> = vec![bar.clone()];
    let parts: Vec<&str> = message.split('\n').collect();
    if let Some((first, rest)) = parts.split_first() {
        if !first.is_empty() {
            lines.push(format!("{}  {}", symbol, first));
        } else {
            lines.push(symbol.to_string());
        }
        for p in rest {
            if !p.is_empty() {
                lines.push(format!("{}  {}", bar, p));
            } else {
                lines.push(bar.clone());
            }
        }
    }
    sys::write_out(&format!("{}\n", lines.join("\n")));
}

pub mod log {
    use super::*;

    pub fn message(msg: &str) {
        log_with_symbol(msg, &style::gray(s_bar()));
    }
    pub fn info(msg: &str) {
        log_with_symbol(msg, &style::blue(s_info()));
    }
    pub fn success(msg: &str) {
        log_with_symbol(msg, &style::green(s_success()));
    }
    pub fn step(msg: &str) {
        log_with_symbol(msg, &style::green(s_step_submit()));
    }
    pub fn warn(msg: &str) {
        log_with_symbol(msg, &style::yellow(s_warn()));
    }
    pub fn error(msg: &str) {
        log_with_symbol(msg, &style::red(s_error()));
    }
}

pub fn note(message: &str, title: &str) {
    let wrapped = wrap_ansi(message, columns().saturating_sub(6));
    let mut lines: Vec<String> = vec![String::new()];
    lines.extend(wrapped.split('\n').map(style::dim));
    lines.push(String::new());
    let title_len = visible_width(title);
    let len = lines
        .iter()
        .map(|l| visible_width(l))
        .max()
        .unwrap_or(0)
        .max(title_len)
        + 2;
    let body: Vec<String> = lines
        .iter()
        .map(|m| {
            format!(
                "{}  {}{}{}",
                style::gray(s_bar()),
                m,
                " ".repeat(len.saturating_sub(visible_width(m))),
                style::gray(s_bar())
            )
        })
        .collect();
    let top = format!(
        "{}  {} {}",
        style::green(s_step_submit()),
        style::reset(title),
        style::gray(format!(
            "{}{}",
            s_bar_h().repeat((len as isize - title_len as isize - 1).max(1) as usize),
            s_corner_top_right()
        ))
    );
    let bottom = style::gray(format!(
        "{}{}{}",
        s_connect_left(),
        s_bar_h().repeat(len + 2),
        s_corner_bottom_right()
    ));
    sys::write_out(&format!(
        "{}\n{}\n{}\n{}\n",
        style::gray(s_bar()),
        top,
        body.join("\n"),
        bottom
    ));
}

// ─── Spinner ───

/// Serializes spinner frame writes with stop/exit output.
static FRAME_LOCK: Mutex<()> = Mutex::new(());

/// The currently running spinner, used by the process-exit handler.
static ACTIVE_SPINNER: Mutex<Option<Arc<SpinnerShared>>> = Mutex::new(None);

struct SpinnerShared {
    message: Mutex<String>,
    running: AtomicBool,
    animated: AtomicBool,
    stop: AtomicBool,
}

pub struct Spinner {
    inert: bool,
    shared: Arc<SpinnerShared>,
    handle: Option<std::thread::JoinHandle<()>>,
}

fn stop_symbol(code: i32) -> String {
    match code {
        0 => style::green(s_step_submit()),
        1 => style::red(s_step_cancel()),
        _ => style::red(u("▲", "x")),
    }
}

/// clack's spinner `exit` handler: an active spinner is stopped with
/// "Canceled" (or "Something went wrong" for exit codes > 1).
pub fn on_process_exit(code: i32) {
    let active = ACTIVE_SPINNER.lock().unwrap().take();
    if let Some(shared) = active {
        let _g = FRAME_LOCK.lock().unwrap();
        shared.stop.store(true, Ordering::SeqCst);
        if shared.running.swap(false, Ordering::SeqCst) {
            let clear = if shared.animated.load(Ordering::SeqCst) {
                "\x1b[1G\x1b[J"
            } else {
                ""
            };
            let msg = if code > 1 {
                "Something went wrong"
            } else {
                "Canceled"
            };
            sys::write_out(&format!(
                "{}{}  {}\n\x1b[?25h",
                clear,
                stop_symbol(code),
                msg
            ));
        }
    }
}

impl Spinner {
    pub fn new() -> Self {
        Spinner {
            inert: false,
            shared: Arc::new(SpinnerShared {
                message: Mutex::new(String::new()),
                running: AtomicBool::new(false),
                animated: AtomicBool::new(false),
                stop: AtomicBool::new(false),
            }),
            handle: None,
        }
    }

    /// A spinner that prints nothing (used in `--json` mode).
    pub fn inert() -> Self {
        let mut s = Self::new();
        s.inert = true;
        s
    }

    fn animated() -> bool {
        sys::stdout_is_tty() && !sys::stdout_redirected()
    }

    pub fn start(&mut self, msg: &str) {
        if self.inert {
            return;
        }
        if self.shared.running.load(Ordering::SeqCst) {
            self.halt_thread();
            self.shared.running.store(false, Ordering::SeqCst);
        }
        *self.shared.message.lock().unwrap() = msg.to_string();
        self.shared.running.store(true, Ordering::SeqCst);
        self.shared.stop.store(false, Ordering::SeqCst);
        *ACTIVE_SPINNER.lock().unwrap() = Some(self.shared.clone());
        // clack hides the cursor for the spinner's lifetime, even without a TTY.
        sys::write_out(&format!("\x1b[?25l{}\n", style::gray(s_bar())));
        let animated = Self::animated();
        self.shared.animated.store(animated, Ordering::SeqCst);
        if !animated {
            return;
        }
        let shared = self.shared.clone();
        let frames: Vec<&'static str> = if unicode_supported() {
            vec!["◒", "◐", "◓", "◑"]
        } else {
            vec!["•", "o", "O", "0"]
        };
        let delay = if unicode_supported() { 80 } else { 120 };
        self.handle = Some(std::thread::spawn(move || {
            let mut i = 0usize;
            let mut dots = 0.0f64;
            loop {
                {
                    let _g = FRAME_LOCK.lock().unwrap();
                    if shared.stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let msg = shared.message.lock().unwrap().clone();
                    let d: String = ".".repeat(dots.floor() as usize).chars().take(3).collect();
                    let clear = if i > 0 { "\x1b[1G\x1b[J" } else { "" };
                    sys::write_out(&format!(
                        "{}{}  {}{}",
                        clear,
                        style::magenta(frames[i % frames.len()]),
                        msg,
                        d
                    ));
                }
                i += 1;
                dots = if dots < 4.0 { dots + 0.125 } else { 0.0 };
                std::thread::sleep(Duration::from_millis(delay));
            }
        }));
    }

    pub fn message(&mut self, msg: &str) {
        if self.inert {
            return;
        }
        *self.shared.message.lock().unwrap() = msg.to_string();
    }

    fn halt_thread(&mut self) {
        {
            let _g = FRAME_LOCK.lock().unwrap();
            self.shared.stop.store(true, Ordering::SeqCst);
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
            sys::write_out("\x1b[1G\x1b[J");
        }
    }

    pub fn stop(&mut self, msg: &str) {
        if self.inert || !self.shared.running.load(Ordering::SeqCst) {
            return;
        }
        self.halt_thread();
        self.shared.running.store(false, Ordering::SeqCst);
        let mut active = ACTIVE_SPINNER.lock().unwrap();
        if active
            .as_ref()
            .map(|a| Arc::ptr_eq(a, &self.shared))
            .unwrap_or(false)
        {
            *active = None;
        }
        drop(active);
        sys::write_out(&format!(
            "{}  {}\n\x1b[?25h",
            style::green(s_step_submit()),
            msg
        ));
    }
}

impl Default for Spinner {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        {
            let _g = FRAME_LOCK.lock().unwrap();
            self.shared.stop.store(true, Ordering::SeqCst);
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

// ─── Raw terminal input ───

static RAW_ENABLED: AtomicBool = AtomicBool::new(false);

pub fn restore_terminal() {
    if RAW_ENABLED.swap(false, Ordering::SeqCst) {
        let _ = crossterm::terminal::disable_raw_mode();
        sys::write_out("\x1b[?25h");
    }
}

pub struct RawMode;

impl RawMode {
    pub fn enable() -> Option<RawMode> {
        if !sys::stdin_is_tty() {
            return None;
        }
        #[cfg(windows)]
        {
            let _ = crossterm::ansi_support::supports_ansi();
        }
        crossterm::terminal::enable_raw_mode().ok()?;
        RAW_ENABLED.store(true, Ordering::SeqCst);
        Some(RawMode)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        if RAW_ENABLED.swap(false, Ordering::SeqCst) {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Space,
    Backspace,
    Escape,
    CtrlC,
    Tab,
    Char(char),
    Other,
}

fn map_key(ev: crossterm::event::KeyEvent) -> Option<Key> {
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
    if ev.kind == KeyEventKind::Release {
        return None;
    }
    let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
    let alt = ev.modifiers.contains(KeyModifiers::ALT);
    Some(match ev.code {
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Enter => Key::Enter,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Esc => Key::Escape,
        KeyCode::Tab => Key::Tab,
        KeyCode::Char('c') if ctrl => Key::CtrlC,
        KeyCode::Char(' ') if !ctrl && !alt => Key::Space,
        KeyCode::Char(c) if !ctrl && !alt => Key::Char(c),
        _ => Key::Other,
    })
}

/// Block until the next key press.
pub fn read_key() -> Key {
    loop {
        match crossterm::event::read() {
            Ok(crossterm::event::Event::Key(ev)) => {
                if let Some(k) = map_key(ev) {
                    return k;
                }
            }
            Ok(_) => continue,
            Err(_) => return Key::CtrlC,
        }
    }
}

/// Wait up to `timeout` for a key press.
pub fn poll_key(timeout: Duration) -> Option<Key> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match crossterm::event::poll(remaining) {
            Ok(true) => {
                if let Ok(crossterm::event::Event::Key(ev)) = crossterm::event::read() {
                    if let Some(k) = map_key(ev) {
                        return Some(k);
                    }
                }
            }
            _ => return None,
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
    }
}

/// Redraws a block of lines in place.
pub struct Frame {
    last_height: usize,
}

impl Frame {
    pub fn new() -> Self {
        Frame { last_height: 0 }
    }

    pub fn render(&mut self, text: &str) {
        let clear = if self.last_height > 0 {
            format!("\x1b[{}A\x1b[J", self.last_height)
        } else {
            String::new()
        };
        let body = text.trim_end_matches('\n');
        // `\r` is needed in raw mode, where `\n` does not return the carriage.
        let rendered = body.replace('\n', "\r\n");
        sys::write_out(&format!("\r{}{}\r\n", clear, rendered));
        let lines: Vec<String> = body.split('\n').map(|s| s.to_string()).collect();
        self.last_height = count_visual_rows(&lines, None);
    }
}

impl Default for Frame {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Prompts ───

#[derive(Clone, Copy, PartialEq)]
enum PromptState {
    Active,
    Submit,
    Cancel,
}

fn title_block(state: PromptState, message: &str) -> String {
    let sym = match state {
        PromptState::Active => style::cyan(s_step_active()),
        PromptState::Submit => style::green(s_step_submit()),
        PromptState::Cancel => style::red(s_step_cancel()),
    };
    let msg_lines: Vec<&str> = message.split('\n').collect();
    let mut out = format!("{}\n{}  {}", style::gray(s_bar()), sym, msg_lines[0]);
    for l in &msg_lines[1..] {
        out.push_str(&format!("\n{}  {}", style::gray(s_bar()), l));
    }
    out.push('\n');
    out
}

pub struct SelectOption<T> {
    pub value: T,
    pub label: String,
    pub hint: Option<String>,
}

impl<T> SelectOption<T> {
    pub fn new(value: T, label: &str, hint: Option<&str>) -> Self {
        SelectOption {
            value,
            label: label.to_string(),
            hint: hint.map(|h| h.to_string()),
        }
    }
}

/// `p.select`. Returns `None` when cancelled.
pub fn select<T: Clone>(message: &str, options: &[SelectOption<T>], initial: usize) -> Option<T> {
    let mut cursor = initial.min(options.len().saturating_sub(1));
    let render = |state: PromptState, cursor: usize| -> String {
        let mut s = title_block(state, message);
        match state {
            PromptState::Submit => {
                s.push_str(&format!(
                    "{}  {}\n",
                    style::gray(s_bar()),
                    style::dim(&options[cursor].label)
                ));
            }
            PromptState::Cancel => {
                s.push_str(&format!(
                    "{}  {}\n{}\n",
                    style::gray(s_bar()),
                    style::strikethrough(style::dim(&options[cursor].label)),
                    style::gray(s_bar())
                ));
            }
            PromptState::Active => {
                let bar = format!("{}  ", style::cyan(s_bar()));
                let rows: Vec<String> = options
                    .iter()
                    .enumerate()
                    .map(|(i, o)| {
                        if i == cursor {
                            let hint = o
                                .hint
                                .as_ref()
                                .map(|h| format!(" {}", style::dim(format!("({})", h))))
                                .unwrap_or_default();
                            format!(
                                "{}{} {}{}",
                                bar,
                                style::green(s_radio_active()),
                                o.label,
                                hint
                            )
                        } else {
                            format!(
                                "{}{} {}",
                                bar,
                                style::dim(s_radio_inactive()),
                                style::dim(&o.label)
                            )
                        }
                    })
                    .collect();
                s.push_str(&rows.join("\n"));
                s.push_str(&format!("\n{}\n", style::cyan(s_bar_end())));
            }
        }
        s
    };

    let mut frame = Frame::new();
    let raw = match RawMode::enable() {
        Some(r) => r,
        None => {
            frame.render(&render(PromptState::Cancel, cursor));
            return None;
        }
    };
    sys::write_out("\x1b[?25l");
    frame.render(&render(PromptState::Active, cursor));
    let result = loop {
        match read_key() {
            Key::Up | Key::Left | Key::Char('k') | Key::Char('h') => {
                cursor = if cursor == 0 {
                    options.len() - 1
                } else {
                    cursor - 1
                };
            }
            Key::Down | Key::Right | Key::Char('j') | Key::Char('l') => {
                cursor = (cursor + 1) % options.len();
            }
            Key::Enter => {
                frame.render(&render(PromptState::Submit, cursor));
                break Some(options[cursor].value.clone());
            }
            Key::Escape | Key::CtrlC => {
                frame.render(&render(PromptState::Cancel, cursor));
                break None;
            }
            _ => {}
        }
        frame.render(&render(PromptState::Active, cursor));
    };
    drop(raw);
    sys::write_out("\x1b[?25h");
    result
}

/// `p.confirm`. Returns `None` when cancelled.
pub fn confirm(message: &str, initial: bool) -> Option<bool> {
    let mut value = initial;
    let render = |state: PromptState, value: bool| -> String {
        let mut s = title_block(state, message);
        let label = if value { "Yes" } else { "No" };
        match state {
            PromptState::Submit => s.push_str(&format!(
                "{}  {}\n",
                style::gray(s_bar()),
                style::dim(label)
            )),
            PromptState::Cancel => s.push_str(&format!(
                "{}  {}\n{}\n",
                style::gray(s_bar()),
                style::strikethrough(style::dim(label)),
                style::gray(s_bar())
            )),
            PromptState::Active => {
                let yes = if value {
                    format!("{} Yes", style::green(s_radio_active()))
                } else {
                    format!("{} {}", style::dim(s_radio_inactive()), style::dim("Yes"))
                };
                let no = if value {
                    format!("{} {}", style::dim(s_radio_inactive()), style::dim("No"))
                } else {
                    format!("{} No", style::green(s_radio_active()))
                };
                s.push_str(&format!(
                    "{}  {} {} {}\n{}\n",
                    style::cyan(s_bar()),
                    yes,
                    style::dim("/"),
                    no,
                    style::cyan(s_bar_end())
                ));
            }
        }
        s
    };
    let mut frame = Frame::new();
    let raw = match RawMode::enable() {
        Some(r) => r,
        None => {
            frame.render(&render(PromptState::Cancel, value));
            return None;
        }
    };
    sys::write_out("\x1b[?25l");
    frame.render(&render(PromptState::Active, value));
    let result = loop {
        match read_key() {
            Key::Up
            | Key::Down
            | Key::Left
            | Key::Right
            | Key::Char('h')
            | Key::Char('j')
            | Key::Char('k')
            | Key::Char('l') => value = !value,
            Key::Char('y') | Key::Char('Y') => {
                value = true;
                frame.render(&render(PromptState::Submit, value));
                break Some(true);
            }
            Key::Char('n') | Key::Char('N') => {
                value = false;
                frame.render(&render(PromptState::Submit, value));
                break Some(false);
            }
            Key::Enter => {
                frame.render(&render(PromptState::Submit, value));
                break Some(value);
            }
            Key::Escape | Key::CtrlC => {
                frame.render(&render(PromptState::Cancel, value));
                break None;
            }
            _ => {}
        }
        frame.render(&render(PromptState::Active, value));
    };
    drop(raw);
    sys::write_out("\x1b[?25h");
    result
}

/// `p.multiselect`. Returns `None` when cancelled.
pub fn multiselect<T: Clone + PartialEq>(
    message: &str,
    options: &[SelectOption<T>],
    initial: &[T],
    required: bool,
) -> Option<Vec<T>> {
    let mut selected: Vec<bool> = options.iter().map(|o| initial.contains(&o.value)).collect();
    let mut cursor = 0usize;
    let mut warning = false;
    let render = |state: PromptState, cursor: usize, selected: &[bool], warning: bool| -> String {
        let mut s = title_block(state, message);
        let chosen: Vec<String> = options
            .iter()
            .zip(selected)
            .filter(|(_, s)| **s)
            .map(|(o, _)| o.label.clone())
            .collect();
        match state {
            PromptState::Submit => {
                let joined = chosen
                    .iter()
                    .map(style::dim)
                    .collect::<Vec<_>>()
                    .join(&style::dim(", "));
                s.push_str(&format!(
                    "{}  {}\n",
                    style::gray(s_bar()),
                    if chosen.is_empty() {
                        style::dim("none")
                    } else {
                        joined
                    }
                ));
            }
            PromptState::Cancel => {
                let label = chosen.join(", ");
                if label.trim().is_empty() {
                    s.push_str(&format!("{}\n", style::gray(s_bar())));
                } else {
                    s.push_str(&format!(
                        "{}  {}\n{}\n",
                        style::gray(s_bar()),
                        style::strikethrough(style::dim(label)),
                        style::gray(s_bar())
                    ));
                }
            }
            PromptState::Active => {
                let bar_color = if warning {
                    style::yellow(s_bar())
                } else {
                    style::cyan(s_bar())
                };
                let bar = format!("{}  ", bar_color);
                let rows: Vec<String> = options
                    .iter()
                    .enumerate()
                    .map(|(i, o)| {
                        let hint = o
                            .hint
                            .as_ref()
                            .map(|h| format!(" {}", style::dim(format!("({})", h))))
                            .unwrap_or_default();
                        let body = match (i == cursor, selected[i]) {
                            (true, true) => format!(
                                "{} {}{}",
                                style::green(s_checkbox_selected()),
                                o.label,
                                hint
                            ),
                            (true, false) => {
                                format!("{} {}{}", style::cyan(s_checkbox_active()), o.label, hint)
                            }
                            (false, true) => format!(
                                "{} {}",
                                style::green(s_checkbox_selected()),
                                style::dim(&o.label)
                            ),
                            (false, false) => format!(
                                "{} {}",
                                style::dim(s_checkbox_inactive()),
                                style::dim(&o.label)
                            ),
                        };
                        format!("{}{}", bar, body)
                    })
                    .collect();
                s.push_str(&rows.join("\n"));
                if warning {
                    s.push_str(&format!(
                        "\n{}  {}\n",
                        style::yellow(s_bar_end()),
                        style::yellow(format!(
                            "Please select at least one option.\n{}",
                            style::reset(style::dim(format!(
                                "Press {} to select, {} to submit",
                                style::gray(style::inverse(" space ")),
                                style::gray(style::inverse(" enter "))
                            )))
                        )),
                    ));
                } else {
                    s.push_str(&format!("\n{}\n", style::cyan(s_bar_end())));
                }
            }
        }
        s
    };
    let mut frame = Frame::new();
    let raw = match RawMode::enable() {
        Some(r) => r,
        None => {
            frame.render(&render(PromptState::Cancel, cursor, &selected, false));
            return None;
        }
    };
    sys::write_out("\x1b[?25l");
    frame.render(&render(PromptState::Active, cursor, &selected, warning));
    let result = loop {
        let key = read_key();
        warning = false;
        match key {
            Key::Up | Key::Left | Key::Char('k') | Key::Char('h') => {
                cursor = if cursor == 0 {
                    options.len() - 1
                } else {
                    cursor - 1
                }
            }
            Key::Down | Key::Right | Key::Char('j') | Key::Char('l') => {
                cursor = (cursor + 1) % options.len()
            }
            Key::Space => selected[cursor] = !selected[cursor],
            Key::Char('a') => {
                let all = selected.iter().all(|s| *s);
                for s in selected.iter_mut() {
                    *s = !all;
                }
            }
            Key::Enter => {
                if required && !selected.iter().any(|s| *s) {
                    warning = true;
                } else {
                    frame.render(&render(PromptState::Submit, cursor, &selected, false));
                    break Some(
                        options
                            .iter()
                            .zip(&selected)
                            .filter(|(_, s)| **s)
                            .map(|(o, _)| o.value.clone())
                            .collect(),
                    );
                }
            }
            Key::Escape | Key::CtrlC => {
                frame.render(&render(PromptState::Cancel, cursor, &selected, false));
                break None;
            }
            _ => {}
        }
        frame.render(&render(PromptState::Active, cursor, &selected, warning));
    };
    drop(raw);
    sys::write_out("\x1b[?25h");
    result
}
