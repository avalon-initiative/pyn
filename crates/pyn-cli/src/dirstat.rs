//! `--dirstat`: the share of a change that falls in each folder, as git works it out.

/// What the damage of a file is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Measure {
    Changes,
    Lines,
    Files,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spec {
    pub measure: Measure,
    pub cumulative: bool,
    /// Folders with a smaller share, in tenths of a percent, are left out.
    pub permille: u64,
}

impl Default for Spec {
    fn default() -> Self {
        Self {
            measure: Measure::Changes,
            cumulative: false,
            permille: 30,
        }
    }
}

impl Spec {
    /// Applies the comma separated parameters of `--dirstat=<params>`.
    pub fn with_params(mut self, params: &str) -> Result<Self, String> {
        for p in params.split(',').filter(|p| !p.is_empty()) {
            match p {
                "changes" => self.measure = Measure::Changes,
                "lines" => self.measure = Measure::Lines,
                "files" => self.measure = Measure::Files,
                "cumulative" => self.cumulative = true,
                "noncumulative" => self.cumulative = false,
                _ => {
                    self.permille = cut_off(p).ok_or_else(|| {
                        format!("Failed to parse --dirstat/-X option parameter: '{p}'")
                    })?
                }
            }
        }
        Ok(self)
    }
}

/// `12`, `3.5`: a percentage in tenths, ignoring digits after the first decimal.
fn cut_off(text: &str) -> Option<u64> {
    let (whole, frac) = match text.split_once('.') {
        Some((w, f)) => (w, Some(f)),
        None => (text, None),
    };
    if whole.is_empty() || !whole.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut permille = whole.parse::<u64>().ok()? * 10;
    if let Some(f) = frac {
        if f.is_empty() || !f.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        permille += u64::from(f.as_bytes()[0] - b'0');
    }
    Some(permille)
}

/// The bytes of `new` that are not in `old` and the bytes of `old` that are not lost, by git's line-chunk hashes.
pub fn byte_damage(old: &[u8], new: &[u8]) -> u64 {
    let (a, b) = (
        crate::rename::span_counts(old),
        crate::rename::span_counts(new),
    );
    let copied: u64 = a
        .iter()
        .map(|(h, n)| b.get(h).map_or(0, |m| *n.min(m)))
        .sum();
    let added: u64 = b
        .iter()
        .map(|(h, m)| m.saturating_sub(a.get(h).copied().unwrap_or(0)))
        .sum();
    (old.len() as u64).saturating_sub(copied) + added
}

struct Walk<'a> {
    files: &'a [(String, u64)],
    at: usize,
    total: u64,
    spec: Spec,
    out: String,
}

impl Walk<'_> {
    fn gather(&mut self, base: &str) -> u64 {
        let mut sum = 0;
        let mut sources = 0;
        while let Some((name, damage)) = self.files.get(self.at) {
            let Some(rest) = name.strip_prefix(base) else {
                break;
            };
            let changes = match rest.find('/') {
                Some(slash) => {
                    let next = format!("{base}{}", &rest[..=slash]);
                    sources += 1;
                    self.gather(&next)
                }
                None => {
                    self.at += 1;
                    sources += 2;
                    *damage
                }
            };
            sum += changes;
        }
        if !base.is_empty() && sources != 1 && sum > 0 {
            let permille = sum * 1000 / self.total;
            if permille >= self.spec.permille {
                self.out
                    .push_str(&format!("{:>4}.{}% {base}\n", permille / 10, permille % 10));
                if !self.spec.cumulative {
                    return 0;
                }
            }
        }
        sum
    }
}

/// The dirstat lines for files and the damage each did.
pub fn render(mut files: Vec<(String, u64)>, spec: Spec) -> String {
    let total: u64 = files.iter().map(|(_, d)| d).sum();
    if total == 0 {
        return String::new();
    }
    files.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut walk = Walk {
        files: &files,
        at: 0,
        total,
        spec,
        out: String::new(),
    };
    walk.gather("");
    walk.out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(list: &[(&str, u64)]) -> Vec<(String, u64)> {
        list.iter().map(|(n, d)| (n.to_string(), *d)).collect()
    }

    #[test]
    fn parameters_set_the_measure_the_cut_off_and_cumulation() {
        let spec = Spec::default()
            .with_params("lines,cumulative,12.34")
            .unwrap();
        assert_eq!(spec.measure, Measure::Lines);
        assert!(spec.cumulative);
        assert_eq!(spec.permille, 123);
        assert!(Spec::default().with_params("files,noncumulative").is_ok());
        assert!(Spec::default().with_params("loud").is_err());
        assert!(Spec::default().with_params("1.").is_err());
    }

    #[test]
    fn a_folder_with_a_share_over_the_cut_off_is_listed_once() {
        let files = rows(&[("a/b/x", 30), ("a/b/y", 30), ("a/z", 20), ("top", 20)]);
        let spec = Spec::default();
        assert_eq!(render(files.clone(), spec), "  60.0% a/b/\n  20.0% a/\n");
        let cumulative = Spec {
            cumulative: true,
            ..spec
        };
        assert_eq!(render(files, cumulative), "  60.0% a/b/\n  80.0% a/\n");
    }

    #[test]
    fn a_folder_that_holds_only_one_folder_is_not_repeated() {
        let files = rows(&[("a/b/x", 10), ("c/y", 10)]);
        assert_eq!(render(files, Spec::default()), "  50.0% a/b/\n  50.0% c/\n");
    }

    #[test]
    fn no_damage_prints_nothing() {
        assert_eq!(render(rows(&[("a/b", 0)]), Spec::default()), "");
    }

    #[test]
    fn byte_damage_counts_what_a_file_loses_and_gains() {
        assert_eq!(byte_damage(b"one\ntwo\n", b"one\ntwo\n"), 0);
        assert_eq!(byte_damage(b"one\ntwo\n", b"one\nthree\n"), 4 + 6);
    }
}
