//! Loosened comparators for tests that use real git as an oracle (decision #718): compare facts, not whole output.
//! Exact bytes are asserted only where they are the contract (plumbing output, formats, on-disk state).
#![allow(dead_code)]

/// Exit status class: success, usage error (129) or failure (git has moved between 1 and 128 across releases).
pub fn status_class(code: i32) -> i32 {
    match code {
        0 | 129 => code,
        _ => 1,
    }
}

/// Blame-style output with the leading object id cut to seven characters and the padding after it collapsed.
/// A boundary mark `^` is kept and a blanked id column is one space.
pub fn blame_lines(out: &[u8]) -> String {
    let text = String::from_utf8_lossy(out);
    let mut result = Vec::new();
    for line in text.lines() {
        let (mark, rest) = match line.strip_prefix('^') {
            Some(rest) => ("^", rest),
            None => ("", line),
        };
        let blank = rest.trim_start_matches(' ');
        if rest.starts_with(' ') && (blank.starts_with('(') || blank.starts_with("\t(")) {
            result.push(format!(" {blank}"));
            continue;
        }
        let hex = rest.chars().take_while(char::is_ascii_hexdigit).count();
        if hex < 6 {
            result.push(line.to_string());
            continue;
        }
        let tail = rest[hex..].trim_start_matches(' ');
        result.push(format!("{mark}{} {tail}", &rest[..7]));
    }
    result.join("\n")
}

/// Every run of 7 to 64 hex digits cut to seven, so abbreviation width does not matter.
pub fn short_ids(text: &str) -> String {
    let mut out = String::new();
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if (7..=64).contains(&run.len()) {
            out.push_str(&run[..7]);
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in text.chars() {
        if c.is_ascii_hexdigit() {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

/// Runs of whitespace collapsed to one space per line, and blank lines dropped.
pub fn squeezed(text: &str) -> String {
    text.lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Panics unless `text` contains every fact, with ids abbreviated and whitespace squeezed on both sides.
pub fn assert_has_facts(text: &str, facts: &[&str], what: &str) {
    let hay = squeezed(&short_ids(text));
    for fact in facts {
        let need = squeezed(&short_ids(fact));
        assert!(hay.contains(&need), "{what}: missing {fact:?} in {text:?}");
    }
}

/// Graph output reduced to the commit text of each commit row, so connector rows and columns do not matter.
pub fn graph_commits(text: &str) -> String {
    text.lines()
        .filter_map(|l| l.find(|c: char| c.is_alphanumeric()).map(|at| &l[at..]))
        .collect::<Vec<_>>()
        .join("\n")
}

/// An fsck complaint about an unreadable object with the wording of the reason dropped.
pub fn fsck_line(line: &str) -> String {
    match line.find(": object could not be parsed") {
        Some(at) => format!("{}: object corrupt or missing", &line[..at]),
        None => line.to_string(),
    }
}
