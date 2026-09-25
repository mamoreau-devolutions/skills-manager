//! Two color implementations, matching the two used by the TS CLI:
//!
//! * [`pc`]: a faithful port of `picocolors` (used directly by the CLI code),
//!   including its nested-close replacement and its color-support detection
//!   (which, notably, enables colors unconditionally on Windows).
//! * [`style`]: Node's `util.styleText` as used by `@clack/prompts`, which
//!   only colors when stdout is a TTY (or `FORCE_COLOR` is set).

use std::sync::OnceLock;

fn picocolors_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        let argv: Vec<String> = std::env::args().collect();
        let no_color = crate::sys::env_truthy("NO_COLOR") || argv.iter().any(|a| a == "--no-color");
        let force = crate::sys::env_truthy("FORCE_COLOR")
            || argv.iter().any(|a| a == "--color")
            || cfg!(windows)
            || (crate::sys::stdout_is_tty()
                && std::env::var("TERM").map(|t| t != "dumb").unwrap_or(true))
            || crate::sys::env_truthy("CI");
        !no_color && force
    })
}

fn styletext_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        if let Ok(force) = std::env::var("FORCE_COLOR") {
            return force != "0" && force != "false";
        }
        crate::sys::stdout_is_tty()
            && std::env::var("NO_COLOR")
                .map(|v| v.is_empty())
                .unwrap_or(true)
            && std::env::var("TERM").map(|t| t != "dumb").unwrap_or(true)
    })
}

fn replace_close(s: &str, close: &str, replace: &str, mut index: usize) -> String {
    let mut result = String::new();
    let mut cursor = 0;
    loop {
        result.push_str(&s[cursor..index]);
        result.push_str(replace);
        cursor = index + close.len();
        match s[cursor..].find(close) {
            Some(i) => index = cursor + i,
            None => break,
        }
    }
    result.push_str(&s[cursor..]);
    result
}

fn formatter(input: &str, open: &str, close: &str, replace: &str) -> String {
    // picocolors: string.indexOf(close, open.length)
    let start = open.len().min(input.len());
    let idx = if input.is_char_boundary(start) {
        input[start..].find(close).map(|i| i + start)
    } else {
        input.find(close)
    };
    match idx {
        Some(i) => format!(
            "{}{}{}",
            open,
            replace_close(input, close, replace, i),
            close
        ),
        None => format!("{}{}{}", open, input, close),
    }
}

pub mod pc {
    use super::*;

    macro_rules! color_fn {
        ($name:ident, $open:expr, $close:expr) => {
            pub fn $name<S: AsRef<str>>(s: S) -> String {
                if !picocolors_enabled() {
                    return s.as_ref().to_string();
                }
                formatter(s.as_ref(), $open, $close, $open)
            }
        };
        ($name:ident, $open:expr, $close:expr, $replace:expr) => {
            pub fn $name<S: AsRef<str>>(s: S) -> String {
                if !picocolors_enabled() {
                    return s.as_ref().to_string();
                }
                formatter(s.as_ref(), $open, $close, $replace)
            }
        };
    }

    color_fn!(bold, "\x1b[1m", "\x1b[22m", "\x1b[22m\x1b[1m");
    color_fn!(dim, "\x1b[2m", "\x1b[22m", "\x1b[22m\x1b[2m");
    color_fn!(underline, "\x1b[4m", "\x1b[24m");
    color_fn!(inverse, "\x1b[7m", "\x1b[27m");
    color_fn!(strikethrough, "\x1b[9m", "\x1b[29m");
    color_fn!(black, "\x1b[30m", "\x1b[39m");
    color_fn!(red, "\x1b[31m", "\x1b[39m");
    color_fn!(green, "\x1b[32m", "\x1b[39m");
    color_fn!(yellow, "\x1b[33m", "\x1b[39m");
    color_fn!(cyan, "\x1b[36m", "\x1b[39m");
    color_fn!(white, "\x1b[37m", "\x1b[39m");
    color_fn!(bg_red, "\x1b[41m", "\x1b[49m");
    color_fn!(bg_cyan, "\x1b[46m", "\x1b[49m");
}

pub mod style {
    use super::*;

    macro_rules! style_fn {
        ($name:ident, $open:expr, $close:expr) => {
            pub fn $name<S: AsRef<str>>(s: S) -> String {
                if !styletext_enabled() {
                    return s.as_ref().to_string();
                }
                format!("{}{}{}", $open, s.as_ref(), $close)
            }
        };
    }

    style_fn!(gray, "\x1b[90m", "\x1b[39m");
    style_fn!(dim, "\x1b[2m", "\x1b[22m");
    style_fn!(green, "\x1b[32m", "\x1b[39m");
    style_fn!(red, "\x1b[31m", "\x1b[39m");
    style_fn!(cyan, "\x1b[36m", "\x1b[39m");
    style_fn!(yellow, "\x1b[33m", "\x1b[39m");
    style_fn!(blue, "\x1b[34m", "\x1b[39m");
    style_fn!(magenta, "\x1b[35m", "\x1b[39m");
    style_fn!(reset, "\x1b[0m", "\x1b[0m");
    style_fn!(strikethrough, "\x1b[9m", "\x1b[29m");
    style_fn!(inverse, "\x1b[7m", "\x1b[27m");
    style_fn!(hidden, "\x1b[8m", "\x1b[28m");
}

/// Hard-coded ANSI constants used by cli.ts / list.ts / update.ts / find.ts.
pub mod ansi {
    pub const RESET: &str = "\x1b[0m";
    pub const BOLD: &str = "\x1b[1m";
    pub const DIM: &str = "\x1b[38;5;102m";
    pub const TEXT: &str = "\x1b[38;5;145m";
    pub const CYAN: &str = "\x1b[36m";
    pub const YELLOW: &str = "\x1b[33m";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picocolors_nested_replacement() {
        // bold(dim('x')) must re-open bold after dim's shared close code
        let inner = formatter("x", "\x1b[2m", "\x1b[22m", "\x1b[22m\x1b[2m");
        let outer = formatter(&inner, "\x1b[1m", "\x1b[22m", "\x1b[22m\x1b[1m");
        assert_eq!(outer, "\x1b[1m\x1b[2mx\x1b[22m\x1b[1m\x1b[22m");
    }
}
