//! Applying hunks as `git apply` does: exact context (with `--whitespace=fix`, also context equal once its
//! whitespace errors are fixed), searched outward from the line the hunk names.

use super::apply_parse::FilePatch;
use super::apply_ws::blank;
use pyn_diff::whitespace::{self, BLANK_AT_EOF, Rule};

/// How a patch's whitespace is handled while its hunks go in.
pub(super) struct Fixing<'a> {
    pub rule: &'a Rule,
    /// `--whitespace=fix`: fix added lines and match context after fixing.
    pub fix: bool,
}

pub(super) struct Done {
    pub bytes: Vec<u8>,
    /// Patch lines of the first blank line added by each hunk that ends the file in blank lines.
    pub blank_at_eof: Vec<usize>,
    /// Added lines changed by the whitespace fix.
    pub fixed: usize,
}

fn lines(bytes: &[u8]) -> Vec<Vec<u8>> {
    bytes
        .split_inclusive(|b| *b == b'\n')
        .map(<[u8]>::to_vec)
        .collect()
}

/// How `pre` matches the image at `at`: the lines it covers and whether it took whitespace fixing to match.
/// With `fix`, trailing blank lines of `pre` past the end of the file may be missing from it.
fn matches(
    img: &[Vec<u8>],
    pre: &[&[u8]],
    at: usize,
    (begin, end): (bool, bool),
    fixing: &Fixing,
) -> Option<(usize, bool)> {
    if at > img.len() || (begin && at != 0) {
        return None;
    }
    let avail = img.len() - at;
    let limit = if pre.len() <= avail {
        pre.len()
    } else if fixing.fix
        && fixing.rule.bits & BLANK_AT_EOF != 0
        && pre[avail..].iter().all(|l| blank(l))
    {
        avail
    } else {
        return None;
    };
    if end && at + limit != img.len() {
        return None;
    }
    let window = img[at..at + limit].iter().zip(pre);
    if limit == pre.len() && window.clone().all(|(have, want)| have.as_slice() == *want) {
        return Some((limit, false));
    }
    let same = |have: &Vec<u8>, want: &&[u8]| {
        whitespace::fix(want, fixing.rule).0 == whitespace::fix(have, fixing.rule).0
    };
    (fixing.fix && window.clone().all(|(h, w)| same(h, w))).then_some((limit, true))
}

/// The line where `pre` matches, trying `hint` first, then one line further or earlier in turn.
fn find_pos(
    img: &[Vec<u8>],
    pre: &[&[u8]],
    hint: usize,
    (begin, end): (bool, bool),
    fixing: &Fixing,
) -> Option<(usize, usize, bool)> {
    let n = img.len();
    let mut line = if begin {
        0
    } else if end {
        n.checked_sub(pre.len()).unwrap_or(n)
    } else {
        hint
    };
    line = line.min(n);
    let (mut back, mut forward, mut current) = (line, line, line);
    let mut i = 0usize;
    loop {
        if let Some((limit, fuzzy)) = matches(img, pre, current, (begin, end), fixing) {
            return Some((current, limit, fuzzy));
        }
        loop {
            if back == 0 && forward == n {
                return None;
            }
            if i & 1 == 1 {
                if back == 0 {
                    i += 1;
                    continue;
                }
                back -= 1;
                current = back;
            } else {
                if forward == n {
                    i += 1;
                    continue;
                }
                forward += 1;
                current = forward;
            }
            break;
        }
        i += 1;
    }
}

/// The content after the patch's hunks, or the recorded line of the first hunk that does not fit.
pub(super) fn apply_hunks(base: &[u8], patch: &FilePatch, fixing: &Fixing) -> Result<Done, usize> {
    let mut img = lines(base);
    let mut done = Done {
        bytes: Vec::new(),
        blank_at_eof: Vec::new(),
        fixed: 0,
    };
    for hunk in &patch.hunks {
        let pre: Vec<&[u8]> = hunk
            .lines
            .iter()
            .filter(|(m, _)| *m != b'+')
            .map(|(_, l)| l.as_slice())
            .collect();
        let begin = hunk.oldpos == 0 || hunk.oldpos == 1;
        let end = hunk.trailing == 0;
        let hint = hunk.newpos.saturating_sub(1);
        let Some((at, limit, fuzzy)) = find_pos(&img, &pre, hint, (begin, end), fixing) else {
            return Err(hunk.oldpos);
        };
        let mut post: Vec<Vec<u8>> = Vec::new();
        let (mut blanks, mut first_blank) = (0, 0);
        for (k, (marker, line)) in hunk.lines.iter().enumerate() {
            let blank_line = line.ends_with(b"\n") && blank(line);
            match marker {
                b'+' if blank_line => {
                    if blanks == 0 {
                        first_blank = hunk.numbers[k];
                    }
                    blanks += 1;
                }
                b' ' if blank_line && fixing.rule.bits & BLANK_AT_EOF != 0 => {}
                _ => blanks = 0,
            }
            match marker {
                b'-' => {}
                b'+' if fixing.fix => {
                    let (fixed, counted) = whitespace::fix(line, fixing.rule);
                    done.fixed += usize::from(counted);
                    post.push(fixed);
                }
                b' ' if fuzzy => post.push(whitespace::fix(line, fixing.rule).0),
                _ => post.push(line.clone()),
            }
        }
        let reaches_end = at + limit >= img.len();
        if blanks > 0 && reaches_end && fixing.rule.bits & BLANK_AT_EOF != 0 {
            done.blank_at_eof.push(first_blank);
            if fixing.fix {
                post.truncate(post.len().saturating_sub(blanks));
            }
        }
        img.splice(at..at + limit, post);
    }
    done.bytes = img.concat();
    Ok(done)
}
