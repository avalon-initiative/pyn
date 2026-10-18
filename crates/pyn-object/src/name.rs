//! Folding of a path component the way case-insensitive, HFS+ and NTFS file systems equate names.

/// Code points that HFS+ ignores when comparing names.
fn hfs_ignorable(c: char) -> bool {
    matches!(c, '\u{200c}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{206a}'..='\u{206f}' | '\u{feff}')
}

/// A name as those file systems would equate it: ignorables, a stream suffix and trailing dots and spaces dropped.
pub fn fold_name(part: &[u8]) -> String {
    let text = String::from_utf8_lossy(part);
    let mut name: String = text.chars().filter(|c| !hfs_ignorable(*c)).collect();
    if let Some(at) = name.find(':') {
        name.truncate(at);
    }
    name.trim_end_matches(['.', ' ']).to_ascii_lowercase()
}

/// Whether the folded name is `.base` or its NTFS short name `base~N`.
pub fn names_dir(folded: &str, base: &str) -> bool {
    folded.strip_prefix('.') == Some(base)
        || folded
            .strip_prefix(base)
            .and_then(|rest| rest.strip_prefix('~'))
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether a path component is git's metadata directory `.git` or its short name `git~N`, however folded.
pub fn is_git_dir_name(part: &[u8]) -> bool {
    names_dir(&fold_name(part), "git")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_names_fold_and_short_names_match() {
        for bad in [
            ".git",
            ".GIT",
            ".git.",
            "GIT~1",
            "git~12",
            ".g\u{200c}it",
            ".git:s",
        ] {
            assert!(is_git_dir_name(bad.as_bytes()), "{bad}");
        }
        for ok in [".gitignore", "git~", "git~x", "git", "a.git", "digit~1"] {
            assert!(!is_git_dir_name(ok.as_bytes()), "{ok}");
        }
    }
}
