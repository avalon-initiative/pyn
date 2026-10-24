//! Applying hunks as `git apply` does: exact context only, searched outward from the line the hunk names.

use super::apply_parse::FilePatch;
use super::apply_ws::blank;

pub(super) struct Done {
    pub bytes: Vec<u8>,
    /// Patch line of a hunk that adds blank lines at the end of the file.
    pub blank_at_eof: Option<usize>,
}

fn lines(bytes: &[u8]) -> Vec<Vec<u8>> {
    bytes
        .split_inclusive(|b| *b == b'\n')
        .map(<[u8]>::to_vec)
        .collect()
}

fn matches(img: &[Vec<u8>], pre: &[&[u8]], at: usize, begin: bool, end: bool) -> bool {
    if pre.len() + at > img.len() || (begin && at != 0) {
        return false;
    }
    if end && at != img.len() - pre.len() {
        return false;
    }
    pre.iter()
        .zip(&img[at..])
        .all(|(want, have)| *want == have.as_slice())
}

/// The line where `pre` matches, trying `hint` first, then one line further or earlier in turn.
fn find_pos(img: &[Vec<u8>], pre: &[&[u8]], hint: usize, begin: bool, end: bool) -> Option<usize> {
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
        if matches(img, pre, current, begin, end) {
            return Some(current);
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
pub(super) fn apply_hunks(base: &[u8], patch: &FilePatch) -> Result<Done, usize> {
    let mut img = lines(base);
    let mut blank_at_eof = None;
    for hunk in &patch.hunks {
        let pre: Vec<&[u8]> = hunk
            .lines
            .iter()
            .filter(|(m, _)| *m != b'+')
            .map(|(_, l)| l.as_slice())
            .collect();
        let post: Vec<Vec<u8>> = hunk
            .lines
            .iter()
            .filter(|(m, _)| *m != b'-')
            .map(|(_, l)| l.clone())
            .collect();
        let begin = hunk.oldpos == 0 || hunk.oldpos == 1;
        let end = hunk.trailing == 0;
        let hint = hunk.newpos.saturating_sub(1);
        let Some(at) = find_pos(&img, &pre, hint, begin, end) else {
            return Err(hunk.oldpos);
        };
        let mut new_blank = 0;
        for (marker, line) in &hunk.lines {
            match marker {
                b'+' if blank(line) => new_blank += 1,
                _ => new_blank = 0,
            }
        }
        if new_blank > 0 && at + pre.len() >= img.len() {
            blank_at_eof = Some(hunk.line);
        }
        let end_at = at + pre.len();
        img.splice(at..end_at, post);
    }
    Ok(Done {
        bytes: img.concat(),
        blank_at_eof,
    })
}
