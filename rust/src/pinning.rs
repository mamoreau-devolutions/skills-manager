//! Pinned lock entries (extension; not in the reference CLI).
//!
//! `skills add --pin <ref>` records `"pinned": true` right after `"ref"` in
//! the lock entry. The reference CLI ignores the extra field.

use crate::color::ansi::{DIM, RESET};
use crate::outln;
use crate::sanitize::sanitize_metadata;
use serde_json::Value;

/// The resolved value of `--pin latest`.
pub const PIN_LATEST: &str = "latest";

/// The pinned ref of a lock entry: `pinned` is `true` and `ref` is a
/// non-empty string.
pub fn pinned_ref(entry: &Value) -> Option<String> {
    if entry.get("pinned") != Some(&Value::Bool(true)) {
        return None;
    }
    entry
        .get("ref")
        .and_then(|v| v.as_str())
        .filter(|r| !r.is_empty())
        .map(|r| r.to_string())
}

pub fn is_pinned_entry(entry: &Value) -> bool {
    pinned_ref(entry).is_some()
}

/// A copy of the entry without `ref` and `pinned` (reinstall from the
/// source's default branch).
pub fn unpinned_entry(entry: &Value) -> Value {
    let mut e = entry.clone();
    if let Value::Object(m) = &mut e {
        m.shift_remove("ref");
        m.shift_remove("pinned");
    }
    e
}

/// What `skills update` does with one lock entry.
#[derive(Debug, PartialEq)]
pub enum PinAction {
    /// Not pinned: check it as usual.
    Check,
    /// Pinned; skip it and list it in the pinned notice.
    Skip(String),
    /// Pinned with `--force`: reinstall at the pinned ref (`--pin <ref>`).
    Force(String),
    /// Pinned with `--unpin`: reinstall from the default branch.
    Unpin,
}

pub fn pin_action(entry: &Value, force: bool, unpin: bool) -> PinAction {
    match pinned_ref(entry) {
        None => PinAction::Check,
        Some(_) if unpin => PinAction::Unpin,
        Some(r) if force => PinAction::Force(r),
        Some(r) => PinAction::Skip(r),
    }
}

/// Validate `skills add --pin <pin>` against the parsed source's kind and ref.
pub fn check_pin(kind: &str, source_ref: Option<&str>, pin: &str) -> Result<(), String> {
    if !matches!(kind, "github" | "gitlab" | "git") {
        return Err("--pin is only supported for GitHub, GitLab and Git sources".into());
    }
    if let Some(r) = source_ref {
        if r != pin {
            return Err(format!(
                "Conflicting refs: the source selects \"{}\" but --pin selects \"{}\". Provide one ref.",
                r, pin
            ));
        }
    }
    if pin == PIN_LATEST && kind != "github" {
        return Err("--pin latest is only supported for GitHub sources".into());
    }
    Ok(())
}

/// `Pinned skills (not updated; use --unpin to update them):` notice.
pub fn print_pinned_notice(pinned: &[(String, String)]) {
    if pinned.is_empty() {
        return;
    }
    outln!();
    outln!(
        "{}Pinned skills (not updated; use --unpin to update them):{}",
        DIM,
        RESET
    );
    for (name, r) in pinned {
        outln!(
            "  • {} {}({}){}",
            sanitize_metadata(name),
            DIM,
            sanitize_metadata(r),
            RESET
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pinned_entries() {
        let pinned = json!({"source": "o/r", "ref": "v1", "pinned": true, "x": 1});
        assert_eq!(pinned_ref(&pinned).as_deref(), Some("v1"));
        assert!(!is_pinned_entry(&json!({"ref": "v1"})));
        assert!(!is_pinned_entry(&json!({"ref": "", "pinned": true})));
        assert!(!is_pinned_entry(&json!({"ref": "v1", "pinned": "true"})));
        assert_eq!(
            serde_json::to_string(&unpinned_entry(&pinned)).unwrap(),
            r#"{"source":"o/r","x":1}"#
        );
    }

    #[test]
    fn pin_checks() {
        assert!(check_pin("github", None, "v1").is_ok());
        assert!(check_pin("git", Some("v1"), "v1").is_ok());
        assert!(check_pin("github", None, "latest").is_ok());
        assert_eq!(
            check_pin("local", None, "v1").unwrap_err(),
            "--pin is only supported for GitHub, GitLab and Git sources"
        );
        assert_eq!(
            check_pin("gitlab", Some("main"), "v1").unwrap_err(),
            "Conflicting refs: the source selects \"main\" but --pin selects \"v1\". Provide one ref."
        );
        assert_eq!(
            check_pin("gitlab", None, "latest").unwrap_err(),
            "--pin latest is only supported for GitHub sources"
        );
    }

    #[test]
    fn pin_actions() {
        let pinned = json!({"ref": "v1", "pinned": true});
        let plain = json!({"ref": "v1"});
        assert_eq!(pin_action(&plain, true, true), PinAction::Check);
        assert_eq!(
            pin_action(&pinned, false, false),
            PinAction::Skip("v1".into())
        );
        assert_eq!(
            pin_action(&pinned, true, false),
            PinAction::Force("v1".into())
        );
        assert_eq!(pin_action(&pinned, true, true), PinAction::Unpin);
        assert_eq!(pin_action(&pinned, false, true), PinAction::Unpin);
    }
}
