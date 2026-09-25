//! Minimal YAML frontmatter parser (port of frontmatter.ts) plus a small YAML
//! emitter modeled on the `yaml` package's `stringify` defaults, used to
//! rewrite Eve SKILL.md frontmatter.

use regex::Regex;
use serde_json::{Map, Value};
use std::sync::OnceLock;

pub struct Frontmatter {
    pub data: Map<String, Value>,
    pub content: String,
}

fn fm_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?s)\A---\r?\n(.*?)\r?\n---\r?\n?(.*)\z").unwrap())
}

/// Parse frontmatter. Only plain YAML `---` blocks are supported (never
/// `---js`), so there is no code-execution path. Errors are YAML parse errors.
pub fn parse_frontmatter(raw: &str) -> Result<Frontmatter, String> {
    let caps = match fm_regex().captures(raw) {
        Some(c) => c,
        None => {
            return Ok(Frontmatter {
                data: Map::new(),
                content: raw.to_string(),
            });
        }
    };
    let yaml = caps.get(1).map(|m| m.as_str()).unwrap_or("");
    let content = caps.get(2).map(|m| m.as_str()).unwrap_or("").to_string();
    let value = parse_yaml(yaml)?;
    let data = match value {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    Ok(Frontmatter { data, content })
}

/// Base of the private-use range used to smuggle non-printable characters
/// through the YAML parser (see [`parse_yaml`]).
const SHADOW_BASE: u32 = 0xF0000;

/// Characters outside YAML's printable set. libyaml (serde_yaml) rejects them
/// anywhere in the stream; the `yaml` npm package accepts them in scalars and
/// the CLI strips them later with `sanitizeMetadata`.
fn is_non_printable(c: u32) -> bool {
    matches!(c, 0x00..=0x08 | 0x0B | 0x0C | 0x0E..=0x1F | 0x7F | 0x80..=0x84 | 0x86..=0x9F | 0xFFFE | 0xFFFF)
}

fn unshadow(s: &str) -> String {
    s.chars()
        .map(|c| {
            let code = c as u32;
            if code >= SHADOW_BASE && is_non_printable(code - SHADOW_BASE) {
                char::from_u32(code - SHADOW_BASE).unwrap_or(c)
            } else {
                c
            }
        })
        .collect()
}

fn unshadow_value(v: Value) -> Value {
    match v {
        Value::String(s) => Value::String(unshadow(&s)),
        Value::Array(a) => Value::Array(a.into_iter().map(unshadow_value).collect()),
        Value::Object(m) => Value::Object(
            m.into_iter()
                .map(|(k, v)| (unshadow(&k), unshadow_value(v)))
                .collect(),
        ),
        other => other,
    }
}

pub fn parse_yaml(src: &str) -> Result<Value, String> {
    if src.trim().is_empty() {
        return Ok(Value::Null);
    }
    if !src.chars().any(|c| is_non_printable(c as u32)) {
        let v: serde_yaml::Value = serde_yaml::from_str(src).map_err(|e| e.to_string())?;
        return Ok(yaml_to_json(v));
    }
    // Map non-printable characters onto private-use code points so libyaml
    // accepts them, then restore them in the parsed values.
    let shadowed: String = src
        .chars()
        .map(|c| {
            if is_non_printable(c as u32) {
                char::from_u32(SHADOW_BASE + c as u32).unwrap_or(c)
            } else {
                c
            }
        })
        .collect();
    let v: serde_yaml::Value =
        serde_yaml::from_str(&shadowed).map_err(|e| unshadow(&e.to_string()))?;
    Ok(unshadow_value(yaml_to_json(v)))
}

fn yaml_key(k: &serde_yaml::Value) -> String {
    match k {
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Bool(b) => b.to_string(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::Null => "".to_string(),
        other => serde_yaml::to_string(other)
            .unwrap_or_default()
            .trim()
            .to_string(),
    }
}

pub fn yaml_to_json(v: serde_yaml::Value) -> Value {
    match v {
        serde_yaml::Value::Null => Value::Null,
        serde_yaml::Value::Bool(b) => Value::Bool(b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::from(i)
            } else if let Some(u) = n.as_u64() {
                Value::from(u)
            } else if let Some(f) = n.as_f64() {
                serde_json::Number::from_f64(f)
                    .map(Value::Number)
                    .unwrap_or(Value::Null)
            } else {
                Value::Null
            }
        }
        serde_yaml::Value::String(s) => Value::String(s),
        serde_yaml::Value::Sequence(seq) => {
            Value::Array(seq.into_iter().map(yaml_to_json).collect())
        }
        serde_yaml::Value::Mapping(m) => {
            let mut out = Map::new();
            for (k, v) in m {
                out.insert(yaml_key(&k), yaml_to_json(v));
            }
            Value::Object(out)
        }
        serde_yaml::Value::Tagged(t) => yaml_to_json(t.value),
    }
}

// ─── YAML emitter ───

const LINE_WIDTH: usize = 80;

fn is_reserved_plain(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "true"
            | "false"
            | "null"
            | "~"
            | "yes"
            | "no"
            | "on"
            | "off"
            | ".nan"
            | ".inf"
            | "-.inf"
            | "+.inf"
    ) || s.parse::<f64>().is_ok()
        || (s.starts_with("0x") && s.len() > 2 && s[2..].chars().all(|c| c.is_ascii_hexdigit()))
        || (s.starts_with("0o") && s.len() > 2 && s[2..].chars().all(|c| ('0'..='7').contains(&c)))
}

fn needs_quotes(s: &str) -> bool {
    if s.is_empty() || is_reserved_plain(s) {
        return true;
    }
    let first = s.chars().next().unwrap();
    if "-?:,[]{}#&*!|>'\"%@`".contains(first) {
        // `-`, `?`, `:` are fine when followed by a non-space char
        if matches!(first, '-' | '?' | ':') {
            let second = s.chars().nth(1);
            if second.map(|c| c == ' ' || c == '\t').unwrap_or(true) {
                return true;
            }
        } else {
            return true;
        }
    }
    if s.starts_with(' ') || s.ends_with(' ') || s.starts_with('\t') || s.ends_with('\t') {
        return true;
    }
    if s.contains(": ") || s.contains(" #") || s.ends_with(':') {
        return true;
    }
    s.chars().any(|c| (c as u32) < 0x20 && c != '\t')
}

fn double_quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:02X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Fold a plain scalar at spaces so lines stay within LINE_WIDTH.
fn fold_plain(s: &str, first_col: usize, indent: usize) -> String {
    if first_col + s.chars().count() <= LINE_WIDTH {
        return s.to_string();
    }
    let mut out = String::new();
    let mut col = first_col;
    let words: Vec<&str> = s.split(' ').collect();
    for (i, word) in words.iter().enumerate() {
        let wlen = word.chars().count();
        if i == 0 {
            out.push_str(word);
            col += wlen;
            continue;
        }
        if col + 1 + wlen > LINE_WIDTH && !word.is_empty() && col > indent {
            out.push('\n');
            out.push_str(&" ".repeat(indent));
            out.push_str(word);
            col = indent + wlen;
        } else {
            out.push(' ');
            out.push_str(word);
            col += 1 + wlen;
        }
    }
    out
}

fn scalar(v: &Value, first_col: usize, indent: usize) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => {
            if s.contains('\n') {
                let chomp = if s.ends_with('\n') { "" } else { "-" };
                let body: Vec<String> = s
                    .trim_end_matches('\n')
                    .split('\n')
                    .map(|l| {
                        if l.is_empty() {
                            String::new()
                        } else {
                            format!("{}{}", " ".repeat(indent), l)
                        }
                    })
                    .collect();
                format!("|{}\n{}", chomp, body.join("\n"))
            } else if needs_quotes(s) || s.contains("  ") {
                double_quote(s)
            } else {
                fold_plain(s, first_col, indent)
            }
        }
        _ => String::new(),
    }
}

fn emit(v: &Value, indent: usize, out: &mut String) {
    match v {
        Value::Object(map) => {
            for (k, val) in map {
                let key = if needs_quotes(k) {
                    double_quote(k)
                } else {
                    k.clone()
                };
                out.push_str(&" ".repeat(indent));
                out.push_str(&key);
                match val {
                    Value::Object(m) if !m.is_empty() => {
                        out.push_str(":\n");
                        emit(val, indent + 2, out);
                    }
                    Value::Array(a) if !a.is_empty() => {
                        out.push_str(":\n");
                        emit(val, indent + 2, out);
                    }
                    Value::Object(_) => out.push_str(": {}\n"),
                    Value::Array(_) => out.push_str(": []\n"),
                    _ => {
                        let first_col = indent + key.chars().count() + 2;
                        out.push_str(": ");
                        out.push_str(&scalar(val, first_col, indent + 2));
                        out.push('\n');
                    }
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                out.push_str(&" ".repeat(indent));
                out.push_str("- ");
                match item {
                    Value::Object(m) if !m.is_empty() => {
                        let mut nested = String::new();
                        emit(item, indent + 2, &mut nested);
                        out.push_str(nested.trim_start());
                    }
                    Value::Array(a) if !a.is_empty() => {
                        let mut nested = String::new();
                        emit(item, indent + 2, &mut nested);
                        out.push_str(nested.trim_start());
                    }
                    Value::Object(_) => out.push_str("{}\n"),
                    Value::Array(_) => out.push_str("[]\n"),
                    _ => {
                        out.push_str(&scalar(item, indent + 2, indent + 2));
                        out.push('\n');
                    }
                }
            }
        }
        other => {
            out.push_str(&scalar(other, indent, indent));
            out.push('\n');
        }
    }
}

/// `yaml.stringify(value)`
pub fn stringify_yaml(v: &Value) -> String {
    let mut out = String::new();
    emit(v, 0, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter() {
        let fm = parse_frontmatter("---\nname: a\ndescription: b\n---\n# Body\n").unwrap();
        assert_eq!(fm.data["name"], "a");
        assert_eq!(fm.content, "# Body\n");
        let crlf = parse_frontmatter("---\r\nname: a\r\n---\r\nx").unwrap();
        assert_eq!(crlf.data["name"], "a");
        assert_eq!(crlf.content, "x");
        let none = parse_frontmatter("no frontmatter").unwrap();
        assert!(none.data.is_empty());
        assert!(parse_frontmatter("---\nname: [unclosed\n---\n").is_err());
    }

    #[test]
    fn accepts_control_characters_in_scalars() {
        let fm = parse_frontmatter(
            "---\nname: x\ndescription: Colored \x1b[31mred\x1b[0m text\x07\n---\n",
        )
        .unwrap();
        assert_eq!(
            fm.data["description"],
            "Colored \x1b[31mred\x1b[0m text\x07"
        );
        let quoted = parse_frontmatter("---\nname: \"a\x01b\"\ndescription: d\n---\n").unwrap();
        assert_eq!(quoted.data["name"], "a\x01b");
    }

    #[test]
    fn stringifies_nested_yaml() {
        let v: Value = serde_json::json!({"name": "x", "metadata": {"internal": true, "tags": ["a", "b"]}, "version": 2});
        assert_eq!(
            stringify_yaml(&v),
            "name: x\nmetadata:\n  internal: true\n  tags:\n    - a\n    - b\nversion: 2\n"
        );
        let q: Value = serde_json::json!({"description": "has: colon"});
        assert_eq!(stringify_yaml(&q), "description: \"has: colon\"\n");
    }
}
