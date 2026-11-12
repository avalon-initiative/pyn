//! Whitespace errors in added lines, reported the way `git apply --whitespace=warn` does under the rules of
//! `core.whitespace` and the `whitespace` attribute.

use pyn_diff::whitespace::{self, BLANK_AT_EOF, Rule};

use super::apply_parse::diag;

/// Reports go to standard error; after this many git stops listing them.
const LISTED: usize = 5;

pub(super) fn blank(line: &[u8]) -> bool {
    line.iter().all(u8::is_ascii_whitespace)
}

/// Prints the finding for one added line (without its `+`) unless too many came before; true if it is an error.
pub(super) fn check_added(
    line: &[u8],
    rule: &Rule,
    label: &str,
    line_no: usize,
    reported: usize,
) -> bool {
    let bits = whitespace::check(line, rule) & !BLANK_AT_EOF;
    if bits == 0 {
        return false;
    }
    if reported < LISTED {
        let shown = String::from_utf8_lossy(line.strip_suffix(b"\n").unwrap_or(line)).into_owned();
        diag!(
            "{label}:{line_no}: {}.\n{shown}",
            whitespace::describe(bits)
        );
    }
    true
}

/// The summary lines after all reports.
pub(super) fn summarize(errors: usize) {
    if errors > LISTED {
        let squelched = errors - LISTED;
        diag!(
            "warning: squelched {squelched} whitespace error{}",
            if squelched == 1 { "" } else { "s" }
        );
    }
    if errors > 0 {
        diag!(
            "warning: {errors} line{} add{} whitespace errors.",
            if errors == 1 { "" } else { "s" },
            if errors == 1 { "s" } else { "" }
        );
    }
}
