//! Colored diff output as git paints it: the palette (`color.diff.<slot>`), `--color`, and the colored patch lines.

use std::io::IsTerminal;

use pyn_diff::whitespace::{self, Paint, Rule};
use pyn_diff::{Hunk, Op};

pub const RESET: &str = "\x1b[m";
const BOLD: &str = "\x1b[1m";
const CYAN: &str = "\x1b[36m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED_BACKGROUND: &str = "\x1b[41m";

/// What `--color[=<when>]` and `--no-color` asked for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Choice {
    /// Neither flag: `color.diff`, then `color.ui`.
    #[default]
    Config,
    Always,
    Never,
    Auto,
}

/// The escape sequence for a color setting such as `bold red` or `#ff8800`; `None` when it does not parse.
pub fn parse_color(spec: &str) -> Option<String> {
    if spec.trim().eq_ignore_ascii_case("reset") {
        return Some(RESET.to_string());
    }
    let mut attrs = 0u32;
    let mut colors: Vec<Option<String>> = Vec::new();
    for word in spec.split_whitespace() {
        if let Some(c) = parse_hue(word) {
            if colors.len() == 2 {
                return None;
            }
            colors.push(c);
        } else {
            attrs |= 1 << parse_attr(word)?;
        }
    }
    let mut parts: Vec<String> = (0..32)
        .filter(|i| attrs & (1 << i) != 0)
        .map(|i| i.to_string())
        .collect();
    for (n, color) in colors.iter().enumerate() {
        if let Some(code) = color {
            parts.push(code_of(code, if n == 0 { '3' } else { '4' }));
        }
    }
    Some(if parts.is_empty() {
        String::new()
    } else {
        format!("\x1b[{}m", parts.join(";"))
    })
}

/// `Some(None)` is `normal`, which prints nothing.
fn parse_hue(word: &str) -> Option<Option<String>> {
    const NAMES: [&str; 8] = [
        "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
    ];
    let lower = word.to_ascii_lowercase();
    if lower == "normal" {
        return Some(None);
    }
    if let Some(i) = NAMES.iter().position(|n| *n == lower) {
        return Some(Some(i.to_string()));
    }
    if let Some(i) = lower
        .strip_prefix("bright")
        .and_then(|n| NAMES.iter().position(|x| *x == n))
    {
        return Some(Some(format!("b{i}")));
    }
    if let Some(hex) = lower.strip_prefix('#')
        && hex.len() == 6
        && hex.chars().all(|c| c.is_ascii_hexdigit())
    {
        let n = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        return Some(Some(format!("rgb;{};{};{}", n(0)?, n(2)?, n(4)?)));
    }
    let n: i64 = lower.parse().ok()?;
    match n {
        -1 => Some(None),
        0..=7 => Some(Some(n.to_string())),
        8..=15 => Some(Some(format!("b{}", n - 8))),
        16..=255 => Some(Some(format!("8;5;{n}"))),
        _ => None,
    }
}

fn code_of(hue: &str, base: char) -> String {
    if let Some(rest) = hue.strip_prefix("rgb;") {
        return format!("{base}8;2;{rest}");
    }
    if let Some(bright) = hue.strip_prefix('b') {
        return if base == '3' {
            format!("9{bright}")
        } else {
            format!("10{bright}")
        };
    }
    if hue.starts_with("8;5;") {
        return format!("{base}{hue}");
    }
    format!("{base}{hue}")
}

fn parse_attr(word: &str) -> Option<u32> {
    let lower = word.to_ascii_lowercase();
    let (negated, name) = match lower
        .strip_prefix("no-")
        .or_else(|| lower.strip_prefix("no"))
    {
        Some(n) if !n.is_empty() => (true, n),
        _ => (false, lower.as_str()),
    };
    let code = match name {
        "bold" => 1,
        "dim" => 2,
        "italic" => 3,
        "ul" => 4,
        "blink" => 5,
        "reverse" => 7,
        "strike" => 9,
        _ => return None,
    };
    Some(if !negated {
        code
    } else if code <= 2 {
        22
    } else {
        code + 20
    })
}

/// The escape sequences `color.diff.<slot>` settle for each kind of line.
#[derive(Debug, Clone)]
pub struct Palette {
    pub meta: String,
    pub frag: String,
    pub func: String,
    pub old: String,
    pub new: String,
    pub context: String,
    pub whitespace: String,
    pub commit: String,
}

impl Palette {
    pub fn load(setting: &dyn Fn(&str) -> Option<String>) -> Self {
        let slot = |name: &str, default: &str| {
            setting(&format!("color.diff.{name}"))
                .and_then(|v| parse_color(&v))
                .unwrap_or_else(|| default.to_string())
        };
        Self {
            meta: slot("meta", BOLD),
            frag: slot("frag", CYAN),
            func: slot("func", ""),
            old: slot("old", RED),
            new: slot("new", GREEN),
            context: slot("context", ""),
            whitespace: slot("whitespace", RED_BACKGROUND),
            commit: slot("commit", YELLOW),
        }
    }
}

/// Whether output is colored: the flag, else `color.diff`, else `color.ui`, with `auto` meaning a terminal.
pub fn enabled(choice: Choice, setting: &dyn Fn(&str) -> Option<String>) -> bool {
    let auto =
        || std::io::stdout().is_terminal() && std::env::var("TERM").map_or(true, |t| t != "dumb");
    match choice {
        Choice::Always => true,
        Choice::Never => false,
        Choice::Auto => auto(),
        Choice::Config => {
            let value = setting("color.diff").or_else(|| setting("color.ui"));
            match value.as_deref().map(str::to_ascii_lowercase).as_deref() {
                None => auto(),
                Some("always") => true,
                Some("never" | "false" | "no" | "off" | "0") => false,
                Some(_) => auto(),
            }
        }
    }
}

/// The whitespace rule `core.whitespace` sets, else git's default.
pub fn whitespace_rule(setting: &dyn Fn(&str) -> Option<String>) -> Rule {
    setting("core.whitespace")
        .and_then(|v| Rule::parse(&v))
        .unwrap_or_default()
}

/// Where whitespace errors are painted: `diff.wsErrorHighlight`, as bits.
pub const WSEH_NEW: u32 = 1;
pub const WSEH_OLD: u32 = 2;
pub const WSEH_CONTEXT: u32 = 4;

/// The kinds of line `--ws-error-highlight` and `diff.wsErrorHighlight` name; `None` when one is unknown.
pub fn parse_highlight(spec: &str) -> Option<u32> {
    let mut bits = 0;
    for part in spec.split(',') {
        bits |= match part.trim() {
            "none" => 0,
            "default" => WSEH_NEW,
            "all" => WSEH_NEW | WSEH_OLD | WSEH_CONTEXT,
            "new" => WSEH_NEW,
            "old" => WSEH_OLD,
            "context" => WSEH_CONTEXT,
            _ => return None,
        };
    }
    Some(bits)
}

impl Style {
    /// The style `opts` and the repository's settings call for; `None` when output is not colored.
    pub fn resolve(
        opts: &crate::diff_opts::Opts,
        setting: &dyn Fn(&str) -> Option<String>,
    ) -> Option<Self> {
        if !enabled(opts.color, setting) {
            return None;
        }
        Some(Self {
            palette: Palette::load(setting),
            rule: whitespace_rule(setting),
            highlight: opts
                .ws_highlight
                .or_else(|| setting("diff.wserrorhighlight").and_then(|v| parse_highlight(&v)))
                .unwrap_or(WSEH_NEW),
        })
    }
}

/// Everything a colored patch needs.
#[derive(Debug, Clone)]
pub struct Style {
    pub palette: Palette,
    pub rule: Rule,
    pub highlight: u32,
}

impl Style {
    /// A metadata line (`diff --git`, `index`, `---`, `+++`, mode and rename lines) in the meta color.
    pub fn meta(&self, line: &str) -> String {
        format!("{}{}{}\n", self.palette.meta, line, RESET)
    }

    /// The text of a hunk header, painted.
    pub fn hunk_header(&self, h: &Hunk) -> String {
        let p = &self.palette;
        let full = h.header();
        let line = full.trim_end_matches('\n');
        let at = line.rfind("@@").map_or(line.len(), |i| i + 2);
        let (range, rest) = line.split_at(at);
        let mut out = format!("{}{}{}", p.frag, range, RESET);
        let body = rest.trim_start_matches(|c: char| c.is_ascii_whitespace());
        let gap = &rest[..rest.len() - body.len()];
        if !gap.is_empty() {
            out.push_str(&format!("{}{}{}", p.context, gap, RESET));
        }
        if !body.is_empty() {
            out.push_str(&format!("{}{}{}", p.func, body, RESET));
        }
        out.push('\n');
        out
    }

    /// `sign` and `line` in `set`, closing with a reset before any carriage return and newline.
    fn plain_line(&self, set: &str, sign: Option<char>, line: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let (body, newline) = match line.strip_suffix(b"\n") {
            Some(b) => (b, true),
            None => (line, false),
        };
        let (body, cr) = match body.strip_suffix(b"\r") {
            Some(b) => (b, true),
            None => (body, false),
        };
        if !body.is_empty() || sign.is_some() {
            out.extend_from_slice(set.as_bytes());
            if let Some(c) = sign {
                out.push(c as u8);
            }
            out.extend_from_slice(body);
            out.extend_from_slice(RESET.as_bytes());
        }
        if cr {
            out.push(b'\r');
        }
        if newline {
            out.push(b'\n');
        }
        out
    }

    /// One patch line: `sign` then the text, with whitespace errors painted where `highlight` says.
    fn line(&self, op: Op, line: &[u8], blank_eof: bool) -> Vec<u8> {
        let p = &self.palette;
        let (set, sign, kind) = match op {
            Op::Equal => (&p.context, ' ', WSEH_CONTEXT),
            Op::Delete => (&p.old, '-', WSEH_OLD),
            Op::Insert => (&p.new, '+', WSEH_NEW),
        };
        let ws = (self.highlight & kind != 0 && !p.whitespace.is_empty()).then_some(&p.whitespace);
        let Some(ws) = ws else {
            return self.plain_line(set, Some(sign), line);
        };
        if blank_eof && op == Op::Insert {
            return self.plain_line(ws, Some(sign), line);
        }
        let mut out = self.plain_line(set, Some(sign), b"");
        let paint = Paint {
            set,
            reset: RESET,
            ws,
        };
        out.extend(whitespace::emit(line, &self.rule, &paint));
        out
    }

    /// The hunks of a file, colored; `eof` is `blank_at_eof`'s answer for the file.
    pub fn hunks(&self, hunks: &[Hunk], eof: Option<(usize, usize)>) -> Vec<u8> {
        let mut out = Vec::new();
        for h in hunks {
            out.extend(self.hunk_header(h).into_bytes());
            // git counts from the numbers its header prints, which sit one line early for an empty side.
            let mut pre = h.old_start - usize::from(h.old_count == 0);
            let mut post = h.new_start - usize::from(h.new_count == 0);
            for (op, text) in &h.lines {
                if *op != Op::Insert {
                    pre += 1;
                }
                if *op != Op::Delete {
                    post += 1;
                }
                let blank = *op == Op::Insert
                    && whitespace::is_blank(text)
                    && eof.is_some_and(|(a, b)| a <= pre && b <= post);
                out.extend(self.line(*op, text, blank));
                if !text.ends_with(b"\n") {
                    out.extend(self.no_newline());
                }
            }
        }
        out
    }

    /// A context line of word diff output: the text in the context color.
    pub fn words_context(&self, line: &[u8]) -> Vec<u8> {
        self.plain_line(&self.palette.context, None, line)
    }

    fn no_newline(&self) -> Vec<u8> {
        let mut out = vec![b'\n'];
        out.extend(self.plain_line(
            &self.palette.context,
            Some('\\'),
            b" No newline at end of file\n",
        ));
        out
    }
}

/// `whitespace::blank_at_eof` when the rule cares.
pub fn blank_at_eof(old: &[u8], new: &[u8], rule: &Rule) -> Option<(usize, usize)> {
    if rule.bits & whitespace::BLANK_AT_EOF == 0 {
        return None;
    }
    whitespace::blank_at_eof(old, new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_spell_the_escape_sequences_git_does() {
        assert_eq!(parse_color("red").unwrap(), "\x1b[31m");
        assert_eq!(parse_color("bold red").unwrap(), "\x1b[1;31m");
        assert_eq!(parse_color("red blue").unwrap(), "\x1b[31;44m");
        assert_eq!(
            parse_color("#ff0000 brightcyan ul").unwrap(),
            "\x1b[4;38;2;255;0;0;106m"
        );
        assert_eq!(parse_color("202").unwrap(), "\x1b[38;5;202m");
        assert_eq!(parse_color("reset").unwrap(), "\x1b[m");
        assert_eq!(parse_color("normal").unwrap(), "");
        assert!(parse_color("loud").is_none());
        assert!(parse_color("red green blue").is_none());
    }

    #[test]
    fn color_is_chosen_by_the_flag_then_the_settings() {
        let none = |_: &str| None;
        assert!(enabled(Choice::Always, &none));
        assert!(!enabled(Choice::Never, &none));
        let ui = |k: &str| (k == "color.ui").then(|| "always".to_string());
        assert!(enabled(Choice::Config, &ui));
        let diff_wins = |k: &str| match k {
            "color.ui" => Some("always".to_string()),
            "color.diff" => Some("false".to_string()),
            _ => None,
        };
        assert!(!enabled(Choice::Config, &diff_wins));
        assert!(enabled(Choice::Always, &diff_wins));
    }

    #[test]
    fn the_palette_reads_color_diff_slots() {
        let setting = |k: &str| (k == "color.diff.old").then(|| "blue bold".to_string());
        let p = Palette::load(&setting);
        assert_eq!(p.old, "\x1b[1;34m");
        assert_eq!(p.new, "\x1b[32m");
        assert_eq!(p.frag, "\x1b[36m");
    }

    #[test]
    fn colored_lines_close_each_run_and_mark_whitespace_errors() {
        let style = Style {
            palette: Palette::load(&|_: &str| None),
            rule: Rule::default(),
            highlight: WSEH_NEW,
        };
        let hunk = Hunk {
            old_start: 1,
            old_count: 1,
            new_start: 1,
            new_count: 1,
            heading: " fn a".into(),
            lines: vec![
                (Op::Equal, b"c\n".to_vec()),
                (Op::Delete, b"x\n".to_vec()),
                (Op::Insert, b"y \n".to_vec()),
            ],
        };
        let out = String::from_utf8(style.hunks(&[hunk], None)).unwrap();
        assert_eq!(
            out,
            "\x1b[36m@@ -1 +1 @@\x1b[m \x1b[mfn a\x1b[m\n c\x1b[m\n\
             \x1b[31m-x\x1b[m\n\x1b[32m+\x1b[m\x1b[32my\x1b[m\x1b[41m \x1b[m\n"
        );
    }
}
