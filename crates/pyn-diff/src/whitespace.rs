//! Git's whitespace rules (`core.whitespace`): finding the errors in a line and marking them up.

pub const BLANK_AT_EOL: u32 = 1;
pub const SPACE_BEFORE_TAB: u32 = 2;
pub const INDENT_WITH_NON_TAB: u32 = 4;
pub const CR_AT_EOL: u32 = 8;
pub const BLANK_AT_EOF: u32 = 16;
pub const TAB_IN_INDENT: u32 = 32;

/// Which errors count and how wide a tab is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rule {
    pub bits: u32,
    pub tab_width: usize,
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            bits: BLANK_AT_EOL | BLANK_AT_EOF | SPACE_BEFORE_TAB,
            tab_width: 8,
        }
    }
}

impl Rule {
    /// The rule `core.whitespace` (or the `whitespace` attribute) spells; `None` when it names something unknown.
    pub fn parse(spec: &str) -> Option<Self> {
        let mut rule = Self::default();
        for token in spec.split(',') {
            let token = token.trim();
            if token.is_empty() {
                continue;
            }
            let (negate, name) = match token.strip_prefix('-') {
                Some(n) => (true, n),
                None => (false, token),
            };
            if let Some(width) = name.strip_prefix("tabwidth=") {
                match width.parse::<usize>() {
                    Ok(w) if w > 0 && w < 256 => rule.tab_width = w,
                    _ => return None,
                }
                continue;
            }
            let bits = match name {
                "trailing-space" => BLANK_AT_EOL | BLANK_AT_EOF,
                "space-before-tab" => SPACE_BEFORE_TAB,
                "indent-with-non-tab" => INDENT_WITH_NON_TAB,
                "cr-at-eol" => CR_AT_EOL,
                "blank-at-eol" => BLANK_AT_EOL,
                "blank-at-eof" => BLANK_AT_EOF,
                "tab-in-indent" => TAB_IN_INDENT,
                _ => return None,
            };
            if negate {
                rule.bits &= !bits;
            } else {
                rule.bits |= bits;
            }
        }
        Some(rule)
    }

    fn has(&self, bit: u32) -> bool {
        self.bits & bit != 0
    }
}

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// The errors in `line` as a mask of the rule's bits.
pub fn check(line: &[u8], rule: &Rule) -> u32 {
    mark(line, rule, None)
}

/// `line` with its whitespace errors fixed as `ws_fix_copy` does, and whether git counts it as fixed (a carriage
/// return dropped before the newline alone does not count). Unchanged when `check` finds no error.
pub fn fix(line: &[u8], rule: &Rule) -> (Vec<u8>, bool) {
    if check(line, rule) & !BLANK_AT_EOF == 0 {
        return (line.to_vec(), false);
    }
    let mut len = line.len();
    let newline = len > 0 && line[len - 1] == b'\n';
    let mut cr = false;
    if newline {
        len -= 1;
        if len > 0 && line[len - 1] == b'\r' {
            cr = rule.has(CR_AT_EOL);
            len -= 1;
        }
    }
    let mut counted = false;
    if rule.has(BLANK_AT_EOL) {
        while len > 0 && is_space(line[len - 1]) {
            len -= 1;
            counted = true;
        }
    }
    let src = &line[..len];
    let (mut last_tab, mut last_space) = (None::<usize>, None::<usize>);
    let (mut need, mut i) = (false, 0);
    while i < src.len() {
        match src[i] {
            b'\t' => {
                last_tab = Some(i);
                need |= rule.has(SPACE_BEFORE_TAB) && last_space.is_some();
            }
            b' ' => {
                last_space = Some(i);
                let since = last_tab.map_or(i + 1, |t| i - t);
                need |= rule.has(INDENT_WITH_NON_TAB) && rule.tab_width <= since;
            }
            _ => break,
        }
        i += 1;
    }
    let mut out = Vec::with_capacity(line.len());
    if need {
        let mut last = last_tab.map_or(0, |t| t + 1);
        if rule.has(INDENT_WITH_NON_TAB) {
            last = match (last_tab, last_space) {
                (t, Some(s)) if t.is_none_or(|t| t < s) => s + 1,
                (Some(t), _) => t + 1,
                _ => 0,
            };
        }
        let mut spaces = 0;
        for &c in &src[..last] {
            if c == b' ' {
                spaces += 1;
                if spaces == rule.tab_width {
                    out.push(b'\t');
                    spaces = 0;
                }
            } else {
                spaces = 0;
                out.push(c);
            }
        }
        out.extend(std::iter::repeat_n(b' ', spaces));
        out.extend_from_slice(&src[last..]);
        counted = true;
    } else if rule.has(TAB_IN_INDENT) && last_tab.is_some() {
        let mut col = 0;
        let mut j = 0;
        while j < src.len() && matches!(src[j], b' ' | b'\t') {
            let n = if src[j] == b'\t' {
                rule.tab_width - col % rule.tab_width
            } else {
                1
            };
            out.extend(std::iter::repeat_n(b' ', n));
            col += n;
            j += 1;
        }
        out.extend_from_slice(&src[j..]);
        counted = true;
    } else {
        out.extend_from_slice(src);
    }
    if cr {
        out.push(b'\r');
    }
    if newline {
        out.push(b'\n');
    }
    (out, counted)
}

/// Colors for `emit`: `set` for the text, `ws` for errors, `reset` after each run.
pub struct Paint<'a> {
    pub set: &'a str,
    pub reset: &'a str,
    pub ws: &'a str,
}

/// `line` with its whitespace errors painted, as `ws_check_emit` writes it.
pub fn emit(line: &[u8], rule: &Rule, paint: &Paint) -> Vec<u8> {
    let mut out = Vec::new();
    mark(line, rule, Some((paint, &mut out)));
    out
}

fn mark(line: &[u8], rule: &Rule, mut sink: Option<(&Paint, &mut Vec<u8>)>) -> u32 {
    let mut len = line.len();
    let mut result = 0;
    let mut written = 0;
    let newline = len > 0 && line[len - 1] == b'\n';
    if newline {
        len -= 1;
    }
    let cr = rule.has(CR_AT_EOL) && len > 0 && line[len - 1] == b'\r';
    if cr {
        len -= 1;
    }
    let mut trailing = len;
    if rule.has(BLANK_AT_EOL) {
        while trailing > 0 && is_space(line[trailing - 1]) {
            trailing -= 1;
            result |= BLANK_AT_EOL;
        }
    }
    let mut i = 0;
    while i < trailing {
        if line[i] == b' ' {
            i += 1;
            continue;
        }
        if line[i] != b'\t' {
            break;
        }
        if rule.has(SPACE_BEFORE_TAB) && written < i {
            result |= SPACE_BEFORE_TAB;
            if let Some((p, out)) = sink.as_mut() {
                out.extend_from_slice(p.ws.as_bytes());
                out.extend_from_slice(&line[written..i]);
                out.extend_from_slice(p.reset.as_bytes());
                out.push(line[i]);
            }
        } else if rule.has(TAB_IN_INDENT) {
            result |= TAB_IN_INDENT;
            if let Some((p, out)) = sink.as_mut() {
                out.extend_from_slice(&line[written..i]);
                out.extend_from_slice(p.ws.as_bytes());
                out.push(line[i]);
                out.extend_from_slice(p.reset.as_bytes());
            }
        } else if let Some((_, out)) = sink.as_mut() {
            out.extend_from_slice(&line[written..=i]);
        }
        written = i + 1;
        i += 1;
    }
    if rule.has(INDENT_WITH_NON_TAB) && i - written >= rule.tab_width {
        result |= INDENT_WITH_NON_TAB;
        if let Some((p, out)) = sink.as_mut() {
            out.extend_from_slice(p.ws.as_bytes());
            out.extend_from_slice(&line[written..i]);
            out.extend_from_slice(p.reset.as_bytes());
        }
        written = i;
    }
    if let Some((p, out)) = sink.as_mut() {
        if trailing > written {
            out.extend_from_slice(p.set.as_bytes());
            out.extend_from_slice(&line[written..trailing]);
            out.extend_from_slice(p.reset.as_bytes());
        }
        if trailing != len {
            out.extend_from_slice(p.ws.as_bytes());
            out.extend_from_slice(&line[trailing..len]);
            out.extend_from_slice(p.reset.as_bytes());
        }
        if cr {
            out.push(b'\r');
        }
        if newline {
            out.push(b'\n');
        }
    }
    result
}

/// The wording of the errors in `bits`, joined as `--check` prints them.
pub fn describe(bits: u32) -> String {
    [
        (BLANK_AT_EOL, "trailing whitespace"),
        (SPACE_BEFORE_TAB, "space before tab in indent"),
        (INDENT_WITH_NON_TAB, "indent with spaces"),
        (TAB_IN_INDENT, "tab in indent"),
        (BLANK_AT_EOF, "new blank line at EOF"),
    ]
    .iter()
    .filter(|(bit, _)| bits & bit != 0)
    .map(|(_, text)| *text)
    .collect::<Vec<_>>()
    .join(", ")
}

/// Blank lines at the end of `buf`, as git counts them: a first line with nothing on it is never counted.
fn count_trailing_blank(buf: &[u8]) -> usize {
    if buf.is_empty() {
        return 0;
    }
    let mut ptr = buf.len() as isize - 1;
    if buf[ptr as usize] == b'\n' {
        ptr -= 1;
    }
    let mut count = 0;
    while ptr > 0 {
        let mut eol = ptr;
        while eol >= 0 && buf[eol as usize] != b'\n' {
            eol -= 1;
        }
        if !is_blank(&buf[(eol + 1) as usize..=ptr as usize]) {
            break;
        }
        count += 1;
        ptr = eol - 1;
    }
    count
}

fn count_lines(buf: &[u8]) -> usize {
    buf.split_inclusive(|c| *c == b'\n').count()
}

/// Where the trailing blank lines of the old and the new text begin (1-based), when the new text has more of them.
pub fn blank_at_eof(old: &[u8], new: &[u8]) -> Option<(usize, usize)> {
    let (l1, l2) = (count_trailing_blank(old), count_trailing_blank(new));
    (l2 > l1).then(|| (count_lines(old) - l1 + 1, count_lines(new) - l2 + 1))
}

/// Whether the line is only whitespace.
pub fn is_blank(line: &[u8]) -> bool {
    line.iter().all(|c| is_space(*c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_rule_finds_trailing_space_and_space_before_tab() {
        let r = Rule::default();
        assert_eq!(check(b"ok\n", &r), 0);
        assert_eq!(check(b"x \n", &r), BLANK_AT_EOL);
        assert_eq!(check(b" \tx\n", &r), SPACE_BEFORE_TAB);
        assert_eq!(check(b"\tx\n", &r), 0);
    }

    #[test]
    fn rules_parse_with_negation_and_tab_width() {
        let r = Rule::parse("-trailing-space,tab-in-indent,tabwidth=4").unwrap();
        assert_eq!(r.bits, SPACE_BEFORE_TAB | TAB_IN_INDENT);
        assert_eq!(r.tab_width, 4);
        assert!(Rule::parse("nonsense").is_none());
    }

    #[test]
    fn fix_strips_trailing_space_and_fixes_the_indent() {
        let fixed = |line: &[u8], spec: &str| fix(line, &Rule::parse(spec).unwrap());
        assert_eq!(fixed(b"x \t\n", "trailing-space"), (b"x\n".to_vec(), true));
        assert_eq!(fixed(b"x\r\n", "trailing-space").0, b"x\n");
        assert!(!fixed(b"x\r\n", "trailing-space").1);
        assert_eq!(fixed(b"x\r\n", "cr-at-eol").0, b"x\r\n");
        assert_eq!(fixed(b"  \tc\n", "space-before-tab").0, b"\tc\n");
        assert_eq!(fixed(b"        y\n", "indent-with-non-tab").0, b"\ty\n");
        assert_eq!(fixed(b"    x\n", "indent-with-non-tab").0, b"    x\n");
        assert_eq!(fixed(b"\t  z\n", "tab-in-indent").0, b"          z\n");
        assert_eq!(fixed(b"\ta\n", "tab-in-indent,tabwidth=4").0, b"    a\n");
        assert_eq!(
            fixed(
                b"    \t x \t \n",
                "blank-at-eol,indent-with-non-tab,tabwidth=2"
            )
            .0,
            b"\t\t\t x\n"
        );
        assert_eq!(
            fixed(
                b"    \t x\n",
                "indent-with-non-tab,-space-before-tab,tabwidth=2"
            )
            .0,
            b"    \t x\n"
        );
        assert_eq!(fixed(b"fine\n", "trailing-space").0, b"fine\n");
    }

    #[test]
    fn markup_paints_each_error_in_its_own_run() {
        let rule = Rule::default();
        let paint = Paint {
            set: "<s>",
            reset: "</>",
            ws: "<w>",
        };
        let out = emit(b" \tx \n", &rule, &paint);
        assert_eq!(out, b"<w> </>\t<s>x</><w> </>\n");
        assert_eq!(emit(b"ok\n", &rule, &paint), b"<s>ok</>\n");
    }

    #[test]
    fn blank_lines_at_the_end_are_counted_as_git_counts_them() {
        assert_eq!(blank_at_eof(b"a\n", b"a\n\n"), Some((2, 2)));
        assert_eq!(blank_at_eof(b"a\n\n", b"a\n\n\n"), Some((2, 2)));
        assert_eq!(blank_at_eof(b"\n", b"\n\n"), None);
        assert_eq!(blank_at_eof(b"a\n  \n", b"a\n \n"), None);
        assert_eq!(blank_at_eof(b"a\n", b"a\n  \n"), Some((2, 2)));
    }
}
