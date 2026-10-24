//! Whitespace errors in added lines, reported the way `git apply --whitespace=warn` does with the default rules:
//! trailing whitespace, a space before a tab in the indent, and blank lines added at the end of a file.

use super::apply_parse::diag;

/// Reports go to standard error; after this many git stops listing them.
const LISTED: usize = 5;

fn trailing(line: &[u8]) -> bool {
    let body = line.strip_suffix(b"\n").unwrap_or(line);
    body.last()
        .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\r' | 0x0b | 0x0c))
}

fn space_before_tab(line: &[u8]) -> bool {
    let mut spaces = false;
    for b in line {
        match b {
            b' ' => spaces = true,
            b'\t' => return spaces,
            _ => return false,
        }
    }
    false
}

pub(super) fn blank(line: &[u8]) -> bool {
    line.iter().all(u8::is_ascii_whitespace)
}

/// Prints the finding for one added line (without its `+`) unless too many came before; true if it is an error.
pub(super) fn check_added(line: &[u8], label: &str, line_no: usize, reported: usize) -> bool {
    let mut reasons = Vec::new();
    if trailing(line) {
        reasons.push("trailing whitespace");
    }
    if space_before_tab(line) {
        reasons.push("space before tab in indent");
    }
    if reasons.is_empty() {
        return false;
    }
    if reported < LISTED {
        let shown = String::from_utf8_lossy(line.strip_suffix(b"\n").unwrap_or(line)).into_owned();
        diag!("{label}:{line_no}: {}.\n{shown}", reasons.join(", "));
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
