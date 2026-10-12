//! gitignore rules: per-folder `.gitignore` files, `.git/info/exclude` and the global excludes file.

use crate::wildmatch;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pattern: Vec<u8>,
    negated: bool,
    dir_only: bool,
    /// No slash in the pattern: it matches a name at any depth below its file.
    basename: bool,
}

/// The rules of one ignore file, in file order.
pub fn parse_rules(text: &[u8]) -> Vec<Rule> {
    let text = text.strip_prefix(b"\xef\xbb\xbf").unwrap_or(text);
    text.split(|b| *b == b'\n')
        .filter_map(|line| parse_line(line.strip_suffix(b"\r").unwrap_or(line)))
        .collect()
}

fn parse_line(line: &[u8]) -> Option<Rule> {
    if line.is_empty() || line[0] == b'#' {
        return None;
    }
    let mut line = trim_trailing_spaces(line);
    let negated = line.first() == Some(&b'!');
    if negated {
        line = &line[1..];
    }
    let dir_only = line.last() == Some(&b'/');
    if dir_only {
        line = &line[..line.len() - 1];
    }
    if line.is_empty() {
        return None;
    }
    let basename = !line.contains(&b'/');
    let pattern = line.strip_prefix(b"/").unwrap_or(line).to_vec();
    Some(Rule {
        pattern,
        negated,
        dir_only,
        basename,
    })
}

fn trim_trailing_spaces(line: &[u8]) -> &[u8] {
    let mut keep = 0;
    let mut i = 0;
    while i < line.len() {
        match line[i] {
            b'\\' => {
                i += 2;
                keep = i.min(line.len());
            }
            b' ' => i += 1,
            _ => {
                i += 1;
                keep = i;
            }
        }
    }
    &line[..keep]
}

impl Rule {
    fn matches(&self, rel: &[u8], is_dir: bool, fold: bool) -> bool {
        if self.dir_only && !is_dir {
            return false;
        }
        let text = if self.basename {
            rel.rsplit(|b| *b == b'/').next().unwrap_or(rel)
        } else {
            rel
        };
        if fold {
            wildmatch::matches(
                &self.pattern.to_ascii_lowercase(),
                &text.to_ascii_lowercase(),
                !self.basename,
            )
        } else {
            wildmatch::matches(&self.pattern, text, !self.basename)
        }
    }
}

/// The rules of the ignore file in one folder; `base` is that folder (`""` for the root, else `dir/`).
#[derive(Debug, Clone)]
pub struct Level {
    base: Vec<u8>,
    rules: Vec<Rule>,
}

impl Level {
    pub fn new(base: &[u8], rules: Vec<Rule>) -> Self {
        Self {
            base: base.to_vec(),
            rules,
        }
    }
}

/// The ignore sources that apply everywhere: `.git/info/exclude` over the global excludes file.
#[derive(Debug, Clone, Default)]
pub struct Ignore {
    exclude: Vec<Rule>,
    global: Vec<Rule>,
    fold: bool,
}

impl Ignore {
    pub fn new(exclude: Vec<Rule>, global: Vec<Rule>, fold_case: bool) -> Self {
        Self {
            exclude,
            global,
            fold: fold_case,
        }
    }

    /// Whether `path` (from the root) is ignored given the `.gitignore` levels of its enclosing folders, outermost
    /// first. The caller has already skipped ignored folders, since a file in one cannot be re-included.
    pub fn is_ignored(&self, levels: &[Level], path: &[u8], is_dir: bool) -> bool {
        for level in levels.iter().rev() {
            if let Some(hit) = verdict(&level.rules, &path[level.base.len()..], is_dir, self.fold) {
                return hit;
            }
        }
        verdict(&self.exclude, path, is_dir, self.fold)
            .or_else(|| verdict(&self.global, path, is_dir, self.fold))
            .unwrap_or(false)
    }
}

fn verdict(rules: &[Rule], rel: &[u8], is_dir: bool, fold: bool) -> Option<bool> {
    rules
        .iter()
        .rev()
        .find(|r| r.matches(rel, is_dir, fold))
        .map(|r| !r.negated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ignored(text: &str, path: &str, is_dir: bool) -> bool {
        let ignore = Ignore::default();
        ignore.is_ignored(
            &[Level::new(b"", parse_rules(text.as_bytes()))],
            path.as_bytes(),
            is_dir,
        )
    }

    #[test]
    fn names_match_at_any_depth_and_slashed_patterns_are_anchored() {
        assert!(ignored("*.log\n", "a/b/c.log", false));
        assert!(ignored("/top.txt\n", "top.txt", false));
        assert!(!ignored("/top.txt\n", "sub/top.txt", false));
        assert!(ignored("doc/*.txt\n", "doc/a.txt", false));
        assert!(!ignored("doc/*.txt\n", "x/doc/a.txt", false));
        assert!(ignored("**/gen\n", "x/y/gen", false));
    }

    #[test]
    fn trailing_slash_means_folders_only() {
        assert!(ignored("build/\n", "build", true));
        assert!(!ignored("build/\n", "build", false));
        assert!(ignored("build/\n", "pkg/build", true));
    }

    #[test]
    fn the_last_matching_rule_wins_and_negation_re_includes() {
        assert!(!ignored("*.log\n!keep.log\n", "keep.log", false));
        assert!(ignored("*.log\n!keep.log\n*.log\n", "keep.log", false));
        assert!(ignored("!keep.log\n*.log\n", "keep.log", false));
    }

    #[test]
    fn comments_blanks_escapes_and_trailing_spaces() {
        assert!(!ignored("# *.log\n\n", "a.log", false));
        assert!(ignored("\\#hash\n", "#hash", false));
        assert!(ignored("\\!bang\n", "!bang", false));
        assert!(ignored("name  \n", "name", false));
        assert!(ignored("sp\\ \n", "sp ", false));
        assert!(ignored("crlf\r\n", "crlf", false));
        assert!(ignored("\u{feff}bom\n", "bom", false));
    }

    #[test]
    fn deeper_files_take_precedence_over_shallower_ones_and_exclude() {
        let ignore = Ignore::new(parse_rules(b"*.tmp\n"), parse_rules(b"*.bak\n"), false);
        let root = Level::new(b"", parse_rules(b"*.dat\n"));
        let sub = Level::new(b"sub/", parse_rules(b"!x.dat\n/only-here\n"));
        let levels = [root, sub];
        assert!(ignore.is_ignored(&levels, b"a.dat", false));
        assert!(!ignore.is_ignored(&levels, b"sub/x.dat", false));
        assert!(ignore.is_ignored(&levels, b"sub/only-here", false));
        assert!(!ignore.is_ignored(&levels, b"sub/deeper/only-here", false));
        assert!(ignore.is_ignored(&levels, b"z.tmp", false));
        assert!(ignore.is_ignored(&levels, b"z.bak", false));
        let negating = [Level::new(b"", parse_rules(b"!z.tmp\n"))];
        assert!(!ignore.is_ignored(&negating, b"z.tmp", false));
    }

    #[test]
    fn case_folding_follows_core_ignorecase() {
        let rules = vec![Level::new(b"", parse_rules(b"README\n"))];
        assert!(!Ignore::default().is_ignored(&rules, b"readme", false));
        assert!(Ignore::new(vec![], vec![], true).is_ignored(&rules, b"readme", false));
    }
}
