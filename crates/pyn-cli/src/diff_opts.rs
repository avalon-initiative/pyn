//! The `diff` options that shape the output beyond its kind: whitespace handling, hunk layout, hash lengths and prefixes.

use clap::Args;

use crate::diff_color::Choice;
use crate::word_diff::{Mode, Request};

/// The options that shape a patch, taken by every command that prints one.
#[derive(Args, Debug, Default, Clone)]
pub struct PatchFlags {
    /// Ignore whitespace when comparing lines.
    #[arg(short = 'w', long = "ignore-all-space")]
    all_space: bool,
    /// Ignore changes in the amount of whitespace.
    #[arg(short = 'b', long = "ignore-space-change")]
    space_change: bool,
    /// Ignore whitespace at the end of a line.
    #[arg(long = "ignore-space-at-eol")]
    space_at_eol: bool,
    /// Ignore a carriage return at the end of a line.
    #[arg(long = "ignore-cr-at-eol")]
    cr_at_eol: bool,
    /// Ignore changes that only add or remove blank lines.
    #[arg(long = "ignore-blank-lines")]
    blank_lines: bool,
    /// Show the whole function around each change.
    #[arg(short = 'W', long = "function-context")]
    function_context: bool,
    /// Merge hunks that are at most this many lines apart beyond the context.
    #[arg(long = "inter-hunk-context", value_name = "N")]
    inter_hunk: Option<usize>,
    /// Show whole object ids on the `index` lines.
    #[arg(long = "full-index")]
    full_index: bool,
    /// Prefix for the old file names in the patch (`a/` by default).
    #[arg(long = "src-prefix", value_name = "PREFIX")]
    src_prefix: Option<String>,
    /// Prefix for the new file names in the patch (`b/` by default).
    #[arg(long = "dst-prefix", value_name = "PREFIX")]
    dst_prefix: Option<String>,
    /// Leave the `a/` and `b/` prefixes off the file names in the patch.
    #[arg(long = "no-prefix")]
    no_prefix: bool,
    /// Treat every file as text.
    #[arg(short = 'a', long)]
    text: bool,
    /// Spend extra time to find the smallest set of changes.
    #[arg(long)]
    minimal: bool,
    /// Choose the algorithm that finds the changes: `default` (`myers`) or `minimal`.
    #[arg(long = "diff-algorithm", value_name = "ALGORITHM")]
    diff_algorithm: Option<String>,
    /// Slide changed blocks to where they read best (the default).
    #[arg(long = "indent-heuristic", overrides_with = "no_indent_heuristic")]
    indent_heuristic: bool,
    /// Leave changed blocks where the diff first put them.
    #[arg(long = "no-indent-heuristic", overrides_with = "indent_heuristic")]
    no_indent_heuristic: bool,
    /// Accepted: no external diff program is run.
    #[arg(long = "no-ext-diff")]
    no_ext_diff: bool,
    /// Accepted: no text conversion is applied.
    #[arg(long = "no-textconv")]
    no_textconv: bool,
    /// Swap the two sides of the comparison.
    #[arg(short = 'R')]
    swap_sides: bool,
    /// Keep only the files changed in these ways: `A` added, `C` copied, `D` deleted, `M` modified, `R` renamed, `T` type changed; lower case leaves them out.
    #[arg(long = "diff-filter", value_name = "KINDS")]
    diff_filter: Option<String>,
    /// Color the output: `always`, `never` or `auto` (a terminal); `always` without a value.
    #[arg(long, value_name = "WHEN", num_args = 0..=1, require_equals = true, default_missing_value = "always", overrides_with = "no_color")]
    color: Option<String>,
    /// Do not color the output.
    #[arg(long = "no-color", overrides_with = "color")]
    no_color: bool,
    /// Show changes word by word: `plain`, `color`, `porcelain` or `none`; `plain` without a value.
    #[arg(long = "word-diff", value_name = "MODE", num_args = 0..=1, require_equals = true, default_missing_value = "plain")]
    word_diff: Option<String>,
    /// What counts as a word in word diff output (an extended regular expression); implies `--word-diff`.
    #[arg(long = "word-diff-regex", value_name = "REGEX")]
    word_diff_regex: Option<String>,
    /// `--word-diff=color`, optionally with a word regular expression.
    #[arg(long = "color-words", value_name = "REGEX", num_args = 0..=1, require_equals = true, default_missing_value = "")]
    color_words: Option<String>,
    /// Where whitespace errors are painted in colored output: `new`, `old`, `context`, `all`, `none`.
    #[arg(long = "ws-error-highlight", value_name = "KIND")]
    ws_error_highlight: Option<String>,
    /// Width of the whole diffstat.
    #[arg(long = "stat-width", value_name = "N")]
    stat_width: Option<String>,
    /// Width of the file names in the diffstat.
    #[arg(long = "stat-name-width", value_name = "N")]
    stat_name_width: Option<String>,
    /// Width of the graph in the diffstat.
    #[arg(long = "stat-graph-width", value_name = "N")]
    stat_graph_width: Option<String>,
    /// Number of files the diffstat lists.
    #[arg(long = "stat-count", value_name = "N")]
    stat_count: Option<String>,
    /// Note creations, deletions and mode changes next to the names in the diffstat.
    #[arg(long = "compact-summary")]
    compact_summary: bool,
    /// Show where the change falls by folder, optionally `=<params>` (`lines`, `files`, `cumulative`, a cut-off).
    #[arg(long, value_name = "PARAMS", num_args = 0..=1, require_equals = true, default_missing_value = "")]
    dirstat: Option<String>,
    /// `--dirstat=files`, optionally with more parameters.
    #[arg(long = "dirstat-by-file", value_name = "PARAMS", num_args = 0..=1, require_equals = true, default_missing_value = "")]
    dirstat_by_file: Option<String>,
    /// `--dirstat=cumulative`.
    #[arg(long)]
    cumulative: bool,
}

/// The options only `diff`, `show` and `stash show` take.
#[derive(Args, Debug, Default, Clone)]
pub struct OutputFlags {
    #[command(flatten)]
    pub patch_flags: PatchFlags,
    /// Show each change as a line of modes, ids and a status letter.
    #[arg(long, conflicts_with_all = ["stat", "shortstat", "numstat", "name_only", "name_status", "check"])]
    raw: bool,
    /// End each record with a NUL instead of a newline (with `--raw`, `--name-only`, `--name-status` and `--numstat`).
    #[arg(short = 'z')]
    nul: bool,
    /// Show object ids shortened to at least N digits.
    #[arg(long, value_name = "N", num_args = 0..=1, require_equals = true, default_missing_value = "")]
    abbrev: Option<String>,
    /// Show whole object ids wherever ids are shortened.
    #[arg(long = "no-abbrev", conflicts_with = "abbrev")]
    no_abbrev: bool,
    /// Show the patch (the default).
    #[arg(short = 'p', long)]
    patch: bool,
    /// Show the diffstat, then the patch.
    #[arg(long = "patch-with-stat")]
    patch_with_stat: bool,
    /// Show the raw lines, then the patch.
    #[arg(long = "patch-with-raw")]
    patch_with_raw: bool,
}

/// The resolved options a rendering needs.
#[derive(Clone, Debug)]
pub struct Opts {
    pub ws: pyn_diff::Ws,
    pub inter_hunk: usize,
    pub function: bool,
    pub nul: bool,
    pub full_index: bool,
    /// `--abbrev=<n>` already clamped to git's range; `None` is the default length.
    pub abbrev: Option<usize>,
    /// `--no-abbrev`: whole ids in the raw format.
    pub no_abbrev: bool,
    pub src_prefix: String,
    pub dst_prefix: String,
    pub text: bool,
    pub color: Choice,
    pub ws_highlight: Option<u32>,
    pub stat: StatSpec,
    pub compact_summary: bool,
    pub word: Option<crate::word_diff::Request>,
    pub reverse: bool,
    /// `--diff-filter`: the letters wanted and the letters left out.
    pub filter: Option<(String, String)>,
    pub dirstat: Option<crate::dirstat::Spec>,
    /// Which of these a flag chose, so the configuration does not.
    pub chosen: Chosen,
}

#[derive(Clone, Debug, Default)]
pub struct Chosen {
    pub src_prefix: bool,
    pub dst_prefix: bool,
    pub indent_heuristic: bool,
    pub algorithm: bool,
}

/// The sizes `--stat=<width>[,<name-width>[,<count>]]` and the `--stat-*` options set; zero or unset means the default.
#[derive(Clone, Debug, Default)]
pub struct StatSpec {
    pub width: Option<usize>,
    pub name_width: Option<usize>,
    pub graph_width: Option<usize>,
    pub count: Option<usize>,
}

impl StatSpec {
    /// Reads the stat options among `args` in order, so a later one wins as in git.
    pub fn from_args(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut spec = Self::default();
        let mut args = args.into_iter().skip(1).peekable();
        let number = |name: &str, text: &str| {
            text.parse::<usize>()
                .map_err(|_| format!("option `{name}' expects a numerical value"))
        };
        while let Some(arg) = args.next() {
            if arg == "--" {
                break;
            }
            if let Some(list) = arg.strip_prefix("--stat=") {
                let bad = || format!("invalid --stat value: '{list}'");
                let mut parts = list.splitn(3, ',');
                let first = parts.next().unwrap_or("");
                spec.width = Some(if first.is_empty() {
                    0
                } else {
                    first.parse().map_err(|_| bad())?
                });
                if let Some(n) = parts.next() {
                    spec.name_width = Some(n.parse().map_err(|_| bad())?);
                }
                if let Some(c) = parts.next() {
                    spec.count = Some(c.parse().map_err(|_| bad())?);
                }
                continue;
            }
            for name in [
                "stat-width",
                "stat-name-width",
                "stat-graph-width",
                "stat-count",
            ] {
                let flag = format!("--{name}");
                let value = if arg == flag {
                    args.next().unwrap_or_default()
                } else if let Some(v) = arg.strip_prefix(&format!("{flag}=")) {
                    v.to_string()
                } else {
                    continue;
                };
                let n = number(name, &value)?;
                match name {
                    "stat-width" => spec.width = Some(n),
                    "stat-name-width" => spec.name_width = Some(n),
                    "stat-graph-width" => spec.graph_width = Some(n),
                    _ => spec.count = Some(n),
                }
            }
        }
        Ok(spec)
    }
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            ws: pyn_diff::Ws::default(),
            inter_hunk: 0,
            function: false,
            nul: false,
            full_index: false,
            abbrev: None,
            no_abbrev: false,
            src_prefix: "a/".into(),
            dst_prefix: "b/".into(),
            text: false,
            color: Choice::Config,
            ws_highlight: None,
            stat: StatSpec::default(),
            compact_summary: false,
            word: None,
            reverse: false,
            filter: None,
            dirstat: None,
            chosen: Chosen::default(),
        }
    }
}

const MIN_ABBREV: usize = 4;
/// The length of the `index` and raw ids when no `--abbrev` says otherwise.
const DEFAULT_ABBREV: usize = 7;

/// `--diff-filter=<kinds>`: upper case letters to keep, lower case to leave out.
fn parse_filter(text: &str) -> (String, String) {
    let (mut keep, mut skip) = (String::new(), String::new());
    for c in text.chars() {
        match c {
            'A' | 'C' | 'D' | 'M' | 'R' | 'T' | 'U' | 'X' | 'B' | '*' => keep.push(c),
            'a' | 'c' | 'd' | 'm' | 'r' | 't' | 'u' | 'x' | 'b' => {
                skip.push(c.to_ascii_uppercase())
            }
            other => crate::usage_error(&format!(
                "unknown change class '{other}' in --diff-filter={text}"
            )),
        }
    }
    (keep, skip)
}

impl PatchFlags {
    fn algorithm(&self) -> Option<&str> {
        match self.diff_algorithm.as_deref() {
            None | Some("default" | "myers" | "minimal") => self.diff_algorithm.as_deref(),
            Some(other) => crate::usage_error(&format!(
                "`--diff-algorithm={other}` is not built; only default, myers and minimal are"
            )),
        }
    }

    fn word_request(&self) -> Option<Request> {
        let regex = self
            .word_diff_regex
            .clone()
            .or_else(|| self.color_words.clone().filter(|r| !r.is_empty()));
        let mode = match self.word_diff.as_deref() {
            Some("plain") => Some(Mode::Plain),
            Some("color") => Some(Mode::Color),
            Some("porcelain") => Some(Mode::Porcelain),
            Some("none") => return None,
            Some(other) => crate::usage_error(&format!(
                "option `word-diff' expects one of `plain', `color', `porcelain' or `none', not '{other}'"
            )),
            None if self.color_words.is_some() => Some(Mode::Color),
            None => None,
        };
        // `--word-diff-regex` alone turns plain word diff on.
        match (mode, regex) {
            (None, None) => None,
            (mode, regex) => Some(Request {
                mode: mode.unwrap_or(Mode::Plain),
                regex,
            }),
        }
    }

    /// Whether the last option that decides color asked for word diff colors, which force color on.
    fn color_forced() -> bool {
        let mut forced = false;
        for arg in std::env::args().skip(1) {
            if arg == "--" {
                break;
            }
            if arg == "--word-diff=color"
                || arg == "--color-words"
                || arg.starts_with("--color-words=")
            {
                forced = true;
            } else if arg == "--no-color" || arg == "--color" || arg.starts_with("--color=") {
                forced = false;
            }
        }
        forced
    }

    /// Whether `--diff-filter` was given.
    pub fn filters(&self) -> bool {
        self.diff_filter.is_some()
    }

    pub fn all_space(&self) -> bool {
        self.all_space
    }

    /// Whether an option that implies `--stat` was given.
    pub fn stat_implied(&self) -> bool {
        self.compact_summary
            || self.stat_width.is_some()
            || self.stat_name_width.is_some()
            || self.stat_graph_width.is_some()
            || self.stat_count.is_some()
    }

    fn dirstat_spec(&self) -> Option<crate::dirstat::Spec> {
        use crate::dirstat::{Measure, Spec};
        if self.dirstat.is_none() && self.dirstat_by_file.is_none() && !self.cumulative {
            return None;
        }
        let mut spec = Spec::default();
        if self.dirstat_by_file.is_some() {
            spec.measure = Measure::Files;
        }
        if self.cumulative {
            spec.cumulative = true;
        }
        let params = self.dirstat.as_deref().or(self.dirstat_by_file.as_deref());
        Some(
            spec.with_params(params.unwrap_or(""))
                .unwrap_or_else(|e| crate::usage_error(&e)),
        )
    }

    /// Whether `--dirstat` or an option that implies it was given.
    pub fn dirstat_given(&self) -> bool {
        self.dirstat.is_some() || self.dirstat_by_file.is_some() || self.cumulative
    }

    /// The first flag given other than `-w`, for a command that cannot honor any.
    pub fn given(&self) -> Option<&'static str> {
        [
            ("-b", self.space_change),
            ("--ignore-space-at-eol", self.space_at_eol),
            ("--ignore-cr-at-eol", self.cr_at_eol),
            ("--ignore-blank-lines", self.blank_lines),
            ("-W", self.function_context),
            ("--inter-hunk-context", self.inter_hunk.is_some()),
            ("--full-index", self.full_index),
            ("--src-prefix", self.src_prefix.is_some()),
            ("--dst-prefix", self.dst_prefix.is_some()),
            ("--no-prefix", self.no_prefix),
            ("-a", self.text),
            ("--minimal", self.minimal),
            ("--diff-algorithm", self.diff_algorithm.is_some()),
            ("--indent-heuristic", self.indent_heuristic),
            ("--no-indent-heuristic", self.no_indent_heuristic),
            ("--no-ext-diff", self.no_ext_diff),
            ("--no-textconv", self.no_textconv),
            ("-R", self.swap_sides),
            ("--stat-width", self.stat_width.is_some()),
            ("--stat-name-width", self.stat_name_width.is_some()),
            ("--stat-graph-width", self.stat_graph_width.is_some()),
            ("--stat-count", self.stat_count.is_some()),
            ("--compact-summary", self.compact_summary),
            ("--dirstat", self.dirstat.is_some()),
            ("--dirstat-by-file", self.dirstat_by_file.is_some()),
            ("--cumulative", self.cumulative),
            ("--diff-filter", self.diff_filter.is_some()),
            ("--color", self.color.is_some()),
            ("--no-color", self.no_color),
            ("--ws-error-highlight", self.ws_error_highlight.is_some()),
            ("--word-diff", self.word_diff.is_some()),
            ("--word-diff-regex", self.word_diff_regex.is_some()),
            ("--color-words", self.color_words.is_some()),
        ]
        .into_iter()
        .find(|(_, on)| *on)
        .map(|(f, _)| f)
    }

    pub fn opts(&self) -> Opts {
        let mut o = Opts {
            ws: pyn_diff::Ws {
                all: self.all_space,
                change: self.space_change,
                eol: self.space_at_eol,
                cr: self.cr_at_eol,
                blank_lines: self.blank_lines,
                minimal: self.minimal || self.algorithm() == Some("minimal"),
                no_indent_heuristic: self.no_indent_heuristic,
            },
            compact_summary: self.compact_summary,
            stat: StatSpec::from_args(std::env::args()).unwrap_or_else(|e| crate::usage_error(&e)),
            dirstat: self.dirstat_spec(),
            chosen: Chosen {
                src_prefix: if self.swap_sides {
                    self.no_prefix || self.dst_prefix.is_some()
                } else {
                    self.no_prefix || self.src_prefix.is_some()
                },
                dst_prefix: if self.swap_sides {
                    self.no_prefix || self.src_prefix.is_some()
                } else {
                    self.no_prefix || self.dst_prefix.is_some()
                },
                indent_heuristic: self.indent_heuristic || self.no_indent_heuristic,
                algorithm: self.minimal || self.diff_algorithm.is_some(),
            },
            reverse: self.swap_sides,
            filter: self.diff_filter.as_deref().map(parse_filter),
            inter_hunk: self.inter_hunk.unwrap_or(0),
            function: self.function_context,
            full_index: self.full_index,
            text: self.text,
            ..Opts::default()
        };
        o.color = match (self.no_color, self.color.as_deref()) {
            (true, _) => Choice::Never,
            (_, None) => Choice::Config,
            (_, Some("always" | "true")) => Choice::Always,
            (_, Some("never" | "false")) => Choice::Never,
            (_, Some("auto")) => Choice::Auto,
            (_, Some(other)) => {
                crate::usage_error(&format!("bad color value '{other}' for --color"))
            }
        };
        o.word = self.word_request();
        if o.word.as_ref().is_some_and(|w| w.mode == Mode::Color) && Self::color_forced() {
            o.color = Choice::Always;
        }
        if let Some(spec) = &self.ws_error_highlight {
            match crate::diff_color::parse_highlight(spec) {
                Some(bits) => o.ws_highlight = Some(bits),
                None => {
                    crate::usage_error(&format!("unknown value after ws-error-highlight=: {spec}"))
                }
            }
        }
        if self.no_prefix && (self.src_prefix.is_some() || self.dst_prefix.is_some()) {
            crate::usage_error("`--no-prefix` with `--src-prefix` or `--dst-prefix` is not built");
        }
        if self.no_prefix {
            o.src_prefix.clear();
            o.dst_prefix.clear();
        }
        if let Some(p) = &self.src_prefix {
            o.src_prefix = p.clone();
        }
        if let Some(p) = &self.dst_prefix {
            o.dst_prefix = p.clone();
        }
        if self.swap_sides {
            std::mem::swap(&mut o.src_prefix, &mut o.dst_prefix);
        }
        o
    }
}

impl OutputFlags {
    pub fn opts(&self) -> Opts {
        let mut o = Opts {
            nul: self.nul,
            no_abbrev: self.no_abbrev,
            ..self.patch_flags.opts()
        };
        o.abbrev = match self.abbrev.as_deref() {
            None | Some("") => None,
            Some(n) => match n.parse::<usize>() {
                Ok(n) => Some(n.max(MIN_ABBREV)),
                Err(_) => crate::usage_error(&format!(
                    "option `abbrev' expects a numerical value, not '{n}'"
                )),
            },
        };
        o
    }

    /// The first option given that only a git repository honors.
    pub fn git_only(&self) -> Option<&'static str> {
        [
            ("--raw", self.raw),
            ("-z", self.nul),
            ("--abbrev", self.abbrev.is_some()),
            ("--no-abbrev", self.no_abbrev),
        ]
        .into_iter()
        .find(|(_, on)| *on)
        .map(|(f, _)| f)
        .or_else(|| self.patch_flags.given())
    }

    pub fn stat_implied(&self) -> bool {
        self.patch_flags.stat_implied()
    }

    pub fn dirstat_given(&self) -> bool {
        self.patch_flags.dirstat_given()
    }

    pub fn raw(&self) -> bool {
        self.raw
    }

    pub fn patch_with_stat(&self) -> bool {
        self.patch_with_stat
    }

    pub fn patch_with_raw(&self) -> bool {
        self.patch_with_raw
    }

    /// Whether `-p` or an option that implies it was given.
    pub fn patch(&self) -> bool {
        self.patch || self.patch_with_stat || self.patch_with_raw
    }
}

impl Opts {
    /// These options with what `diff.noprefix`, `diff.indentHeuristic` and `diff.algorithm` add where no flag chose.
    pub fn with_config(&self, setting: &dyn Fn(&str) -> Option<String>) -> Self {
        let mut o = self.clone();
        let on = |key: &str| {
            setting(key).map(|v| {
                matches!(
                    v.to_ascii_lowercase().as_str(),
                    "true" | "yes" | "on" | "1" | ""
                )
            })
        };
        if on("diff.noprefix") == Some(true) {
            if !o.chosen.src_prefix {
                o.src_prefix.clear();
            }
            if !o.chosen.dst_prefix {
                o.dst_prefix.clear();
            }
        }
        if !o.chosen.indent_heuristic && on("diff.indentheuristic") == Some(false) {
            o.ws.no_indent_heuristic = true;
        }
        if !o.chosen.algorithm {
            match setting("diff.algorithm").as_deref() {
                None | Some("default" | "myers") => {}
                Some("minimal") => o.ws.minimal = true,
                Some(other) => crate::usage_error(&format!(
                    "`diff.algorithm={other}` is not built; only default, myers and minimal are"
                )),
            }
        }
        o
    }

    /// The id shortened for an `index` line, or whole with `--full-index`.
    pub fn index_id(&self, id: &str) -> String {
        if self.full_index {
            return id.to_string();
        }
        self.short(id)
    }

    /// The id shortened as `--abbrev` says (seven digits by default).
    pub fn short(&self, id: &str) -> String {
        let n = self.abbrev.unwrap_or(DEFAULT_ABBREV).min(id.len());
        id[..n].to_string()
    }

    /// The id as the raw format shows it.
    pub fn raw_id(&self, id: &str) -> String {
        if self.no_abbrev {
            id.to_string()
        } else {
            self.short(id)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        std::iter::once("pyn")
            .chain(list.iter().copied())
            .map(String::from)
            .collect()
    }

    #[test]
    fn stat_sizes_are_read_in_order_and_the_last_one_wins() {
        let spec = StatSpec::from_args(args(&[
            "diff",
            "--stat=80,30,4",
            "--stat-width",
            "60",
            "--stat-graph-width=9",
        ]))
        .unwrap();
        assert_eq!(
            (spec.width, spec.name_width, spec.count, spec.graph_width),
            (Some(60), Some(30), Some(4), Some(9))
        );
        let blank = StatSpec::from_args(args(&["--stat=,5"])).unwrap();
        assert_eq!((blank.width, blank.name_width), (Some(0), Some(5)));
        assert!(StatSpec::from_args(args(&["--stat=wide"])).is_err());
        assert!(StatSpec::from_args(args(&["--stat-count=x"])).is_err());
        let after_dashes = StatSpec::from_args(args(&["--", "--stat-width=5"])).unwrap();
        assert_eq!(after_dashes.width, None);
    }

    #[test]
    fn diff_filter_keeps_upper_case_and_drops_lower_case() {
        assert_eq!(parse_filter("AM"), ("AM".into(), String::new()));
        assert_eq!(parse_filter("Ad"), ("A".into(), "D".into()));
    }

    #[test]
    fn ids_are_cut_to_the_abbreviation_and_whole_with_full_index() {
        let o = Opts {
            abbrev: Some(10),
            ..Opts::default()
        };
        assert_eq!(o.short("0123456789abcdef"), "0123456789");
        assert_eq!(Opts::default().short("0123456789abcdef"), "0123456");
        let full = Opts {
            full_index: true,
            ..Opts::default()
        };
        assert_eq!(full.index_id("0123456789abcdef"), "0123456789abcdef");
    }

    #[test]
    fn configuration_fills_in_what_no_flag_chose() {
        let setting = |k: &str| match k {
            "diff.noprefix" => Some("true".to_string()),
            "diff.indentheuristic" => Some("false".to_string()),
            "diff.algorithm" => Some("minimal".to_string()),
            _ => None,
        };
        let o = Opts::default().with_config(&setting);
        assert!(o.src_prefix.is_empty() && o.dst_prefix.is_empty());
        assert!(o.ws.no_indent_heuristic && o.ws.minimal);
        let chosen = Opts {
            chosen: Chosen {
                src_prefix: true,
                indent_heuristic: true,
                ..Chosen::default()
            },
            ..Opts::default()
        };
        let o = chosen.with_config(&setting);
        assert_eq!((o.src_prefix.as_str(), o.dst_prefix.as_str()), ("a/", ""));
        assert!(!o.ws.no_indent_heuristic);
    }
}
