//! Whitespace errors in added lines, reported the way `git apply --whitespace=<action>` does under the rules of
//! `core.whitespace` and the `whitespace` attribute.

use pyn_diff::whitespace::{self, BLANK_AT_EOF, Rule};

use super::apply_parse::diag;

/// Reports go to standard error; after this many git stops listing them (all of them with `error-all`).
const LISTED: usize = 5;

/// What `apply` does about whitespace errors: `strip` is `fix`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Nowarn,
    Warn,
    Fix,
    Error,
    ErrorAll,
}

impl Action {
    pub(super) fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "nowarn" => Self::Nowarn,
            "warn" => Self::Warn,
            "fix" | "strip" => Self::Fix,
            "error" => Self::Error,
            "error-all" => Self::ErrorAll,
            _ => return None,
        })
    }

    pub(super) fn reports(self) -> bool {
        self != Self::Nowarn
    }

    pub(super) fn dies(self) -> bool {
        matches!(self, Self::Error | Self::ErrorAll)
    }
}

/// Whitespace errors found so far, across every patch input, and the added lines that were fixed.
pub(super) struct Tally {
    pub errors: usize,
    pub fixed: usize,
    /// How many errors are listed; `error-all` (even if a later option changes the action) lists them all.
    pub limit: usize,
}

impl Tally {
    pub fn new(all: bool) -> Self {
        Self {
            errors: 0,
            fixed: 0,
            limit: if all { usize::MAX } else { LISTED },
        }
    }
}

pub(super) fn blank(line: &[u8]) -> bool {
    line.iter().all(u8::is_ascii_whitespace)
}

/// Prints the finding for one added line (without its `+`) unless too many came before; true if it is an error.
pub(super) fn check_added(
    line: &[u8],
    rule: &Rule,
    (label, line_no): (&str, usize),
    (limit, reported): (usize, usize),
) -> bool {
    let bits = whitespace::check(line, rule) & !BLANK_AT_EOF;
    if bits == 0 {
        return false;
    }
    if reported < limit {
        let shown = String::from_utf8_lossy(line.strip_suffix(b"\n").unwrap_or(line)).into_owned();
        diag!(
            "{label}:{line_no}: {}.\n{shown}",
            whitespace::describe(bits)
        );
    }
    true
}

/// The new-blank-line-at-EOF finding of a hunk, counted and listed like the others.
pub(super) fn report_eof(label: &str, line_no: usize, tally: &mut Tally) {
    tally.errors += 1;
    if tally.errors <= tally.limit {
        diag!("{label}:{line_no}: new blank line at EOF.\n+");
    }
}

/// The summary lines after all inputs; `error` modes end the command here.
pub(super) fn summarize(action: Action, tally: &Tally, applying: bool) {
    let errors = tally.errors;
    if errors == 0 {
        return;
    }
    if errors > tally.limit {
        let squelched = errors - tally.limit;
        diag!(
            "warning: squelched {squelched} whitespace error{}",
            if squelched == 1 { "" } else { "s" }
        );
    }
    let (line, verb) = if errors == 1 {
        ("line", "adds")
    } else {
        ("lines", "add")
    };
    if action.dies() {
        diag!("error: {errors} {line} {verb} whitespace errors.");
        std::process::exit(128);
    }
    if tally.fixed > 0 && applying {
        let n = tally.fixed;
        diag!(
            "warning: {n} line{} applied after fixing whitespace errors.",
            if n == 1 { "" } else { "s" }
        );
    } else {
        diag!("warning: {errors} {line} {verb} whitespace errors.");
    }
}
