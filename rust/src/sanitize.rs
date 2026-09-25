//! Sanitize untrusted strings before terminal output (port of sanitize.ts).
//!
//! Strips CSI/OSC/DCS/PM/APC sequences, simple two-byte escapes, C1 control
//! codes and raw control characters (except `\t` and `\n`), defending against
//! terminal escape injection from skill metadata or remote APIs.

use regex::Regex;
use std::sync::OnceLock;

struct Patterns {
    osc: Regex,
    dcs_pm_apc: Regex,
    csi: Regex,
    simple_esc: Regex,
    c1: Regex,
    control: Regex,
    newlines: Regex,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        osc: Regex::new(r"(?s)\x1b\].*?(?:\x07|\x1b\\)").unwrap(),
        dcs_pm_apc: Regex::new(r"(?s)\x1b[P^_].*?\x1b\\").unwrap(),
        csi: Regex::new(r"\x1b\[[\x30-\x3f]*[\x20-\x2f]*[\x40-\x7e]").unwrap(),
        simple_esc: Regex::new(r"\x1b[\x20-\x7e]").unwrap(),
        c1: Regex::new(r"[\x{80}-\x{9f}]").unwrap(),
        control: Regex::new(r"[\x00-\x08\x0b\x0c\x0d-\x1a\x1c-\x1f\x7f]").unwrap(),
        newlines: Regex::new(r"[\r\n]+").unwrap(),
    })
}

/// Strip all terminal escape sequences and dangerous control characters.
pub fn strip_terminal_escapes(s: &str) -> String {
    let p = patterns();
    let s = p.osc.replace_all(s, "");
    let s = p.dcs_pm_apc.replace_all(&s, "");
    let s = p.csi.replace_all(&s, "");
    let s = p.simple_esc.replace_all(&s, "");
    let s = p.c1.replace_all(&s, "");
    let s = p.control.replace_all(&s, "");
    s.into_owned()
}

/// Sanitize a metadata string for single-line terminal display.
pub fn sanitize_metadata(s: &str) -> String {
    let stripped = strip_terminal_escapes(s);
    let collapsed = patterns().newlines.replace_all(&stripped, " ");
    js_trim(&collapsed).to_string()
}

/// JavaScript `String.prototype.trim` (Unicode whitespace + line terminators).
pub fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_whitespace)
}

pub fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// Number of UTF-16 code units, i.e. JavaScript `string.length`.
pub fn js_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// JavaScript `string.slice(0, n)` on UTF-16 code units (never splits a
/// surrogate pair in a way that produces invalid UTF-8; a split pair is
/// dropped).
pub fn js_slice_to(s: &str, n: usize) -> String {
    let mut count = 0;
    let mut out = String::new();
    for c in s.chars() {
        let w = c.len_utf16();
        if count + w > n {
            break;
        }
        count += w;
        out.push(c);
    }
    out
}

/// JavaScript `string.padEnd(width)` using UTF-16 length.
pub fn js_pad_end(s: &str, width: usize) -> String {
    let len = js_len(s);
    if len >= width {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(width - len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_sequences() {
        assert_eq!(strip_terminal_escapes("\x1b[31mred\x1b[0m"), "red");
        assert_eq!(strip_terminal_escapes("a\x1b]0;title\x07b"), "ab");
        assert_eq!(
            strip_terminal_escapes("a\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\b"),
            "alinkb"
        );
        assert_eq!(strip_terminal_escapes("x\x1b7y\x1b8z"), "xyz");
        assert_eq!(strip_terminal_escapes("a\u{9b}b"), "ab");
        assert_eq!(strip_terminal_escapes("a\tb\nc\rd\x07e\x08f"), "a\tb\ncdef");
        assert_eq!(strip_terminal_escapes("\x1bPpayload\x1b\\after"), "after");
    }

    #[test]
    fn sanitizes_metadata() {
        assert_eq!(
            sanitize_metadata("  multi\nline\r\nname  "),
            "multi line name"
        );
        assert_eq!(sanitize_metadata("\x1b[2Jclear"), "clear");
    }
}
