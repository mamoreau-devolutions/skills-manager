//! Approximation of `String.prototype.localeCompare` (ICU root collation).
//!
//! Hashes such as `computeSkillFolderHash` sort file paths with
//! `localeCompare`, whose order differs from byte order (case-insensitive at
//! the primary level, punctuation before digits before letters, lowercase
//! before uppercase as a tie-break). Matching it keeps the Rust port's
//! `computedHash` values identical to the TypeScript CLI's for typical paths.

use std::cmp::Ordering;

/// Primary weight for ASCII punctuation/symbols in CLDR root order.
const PUNCT_ORDER: &str = "_-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";

fn primary(c: char) -> Option<u32> {
    let code = c as u32;
    // Control characters are completely ignorable.
    if code < 0x20 && !matches!(c, '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r')
        || (0x7f..0xa0).contains(&code)
    {
        return None;
    }
    match c {
        '\t' => return Some(1),
        '\n' => return Some(2),
        '\u{0b}' => return Some(3),
        '\u{0c}' => return Some(4),
        '\r' => return Some(5),
        ' ' => return Some(6),
        _ => {}
    }
    if let Some(i) = PUNCT_ORDER.find(c) {
        return Some(100 + i as u32);
    }
    if c.is_ascii_digit() {
        return Some(200 + (code - '0' as u32));
    }
    if c.is_ascii_alphabetic() {
        return Some(300 + (c.to_ascii_lowercase() as u32 - 'a' as u32));
    }
    let lower: Vec<char> = c.to_lowercase().collect();
    let base = if lower.len() == 1 {
        lower[0] as u32
    } else {
        code
    };
    Some(1000 + base)
}

/// Tertiary weight: lowercase sorts before uppercase.
fn tertiary(c: char) -> u32 {
    if c.is_uppercase() {
        1
    } else {
        0
    }
}

pub fn locale_compare(a: &str, b: &str) -> Ordering {
    let pa: Vec<u32> = a.chars().filter_map(primary).collect();
    let pb: Vec<u32> = b.chars().filter_map(primary).collect();
    match pa.cmp(&pb) {
        Ordering::Equal => {}
        o => return o,
    }
    let ta: Vec<u32> = a
        .chars()
        .filter(|c| primary(*c).is_some())
        .map(tertiary)
        .collect();
    let tb: Vec<u32> = b
        .chars()
        .filter(|c| primary(*c).is_some())
        .map(tertiary)
        .collect();
    ta.cmp(&tb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_like_icu() {
        let mut v = vec![
            "SKILL.md",
            "references/a.md",
            "README.md",
            "_x",
            "1.txt",
            "b",
            "B",
            "a-b",
            "a_b",
            "ab",
        ];
        v.sort_by(|a, b| locale_compare(a, b));
        assert_eq!(
            v,
            vec![
                "_x",
                "1.txt",
                "a_b",
                "a-b",
                "ab",
                "b",
                "B",
                "README.md",
                "references/a.md",
                "SKILL.md"
            ]
        );
    }
}
