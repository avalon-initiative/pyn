//! The one check every path from a repository or a server passes before it is written, removed or renamed in a work
//! tree. It follows git's `verify_path` and is stricter: the folding rules apply on every platform.

use std::fmt;

use pyn_object::{fold_name, names_dir};

/// Most `/`-separated components a path may have.
pub const MAX_DEPTH: usize = 1024;
/// Most bytes a path may have.
pub const MAX_PATH_BYTES: usize = 4096;

/// Files of the `.pyn` folder that a repository tracks; everything else under that name is local state.
const PYN_SHARED: [&[u8]; 2] = [b".pyn/pyn.toml", b".pyn/ignore"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Empty,
    EmptyComponent,
    CurrentDir,
    ParentDir,
    Absolute,
    DrivePrefix,
    Backslash,
    Control,
    Reserved,
    WindowsName,
    TooLong,
    TooDeep,
    LinkEmpty,
    LinkAbsolute,
    LinkEscapes,
}

impl Reason {
    fn text(self) -> &'static str {
        match self {
            Self::Empty => "the path is empty",
            Self::EmptyComponent => "the path has an empty component",
            Self::CurrentDir => "the path has a `.` component",
            Self::ParentDir => "the path has a `..` component",
            Self::Absolute => "the path is absolute",
            Self::DrivePrefix => "the path starts with a drive or share prefix",
            Self::Backslash => "the path has a backslash",
            Self::Control => "the path has a control character",
            Self::Reserved => "the path has a reserved component",
            Self::WindowsName => "the path has a component this platform cannot store",
            Self::TooLong => "the path is too long",
            Self::TooDeep => "the path is nested too deeply",
            Self::LinkEmpty => "the symbolic link has no target",
            Self::LinkAbsolute => "the symbolic link target is absolute",
            Self::LinkEscapes => "the symbolic link target leaves the work tree",
        }
    }
}

/// A path that must not be materialized, with the reason; displays the path escaped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsafePath {
    pub path: String,
    pub reason: Reason,
}

impl UnsafePath {
    fn new(path: &[u8], reason: Reason) -> Self {
        Self {
            path: String::from_utf8_lossy(path).into_owned(),
            reason,
        }
    }
}

impl fmt::Display for UnsafePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "refusing path {:?}: {}", self.path, self.reason.text())
    }
}

impl std::error::Error for UnsafePath {}

/// Checks `path` (`/`-separated, from the work tree root) with this platform's rules.
pub fn check_path(path: &[u8]) -> Result<(), UnsafePath> {
    check_path_as(path, cfg!(windows))
}

/// Checks every path, naming the first that fails; nothing should be written until all pass.
pub fn check_paths<'a>(paths: impl IntoIterator<Item = &'a [u8]>) -> Result<(), UnsafePath> {
    paths.into_iter().try_for_each(check_path)
}

/// Like [`check_path`], with the Windows-only rules switched by `windows`.
pub fn check_path_as(path: &[u8], windows: bool) -> Result<(), UnsafePath> {
    let bad = |reason| Err(UnsafePath::new(path, reason));
    if path.is_empty() {
        return bad(Reason::Empty);
    }
    if path.len() > MAX_PATH_BYTES {
        return bad(Reason::TooLong);
    }
    if path[0] == b'/' {
        return bad(Reason::Absolute);
    }
    if path.contains(&b'\\') {
        return bad(Reason::Backslash);
    }
    if has_control(path) {
        return bad(Reason::Control);
    }
    let mut depth = 0;
    for (i, part) in path.split(|b| *b == b'/').enumerate() {
        depth += 1;
        if depth > MAX_DEPTH {
            return bad(Reason::TooDeep);
        }
        match part {
            b"" => return bad(Reason::EmptyComponent),
            b"." => return bad(Reason::CurrentDir),
            b".." => return bad(Reason::ParentDir),
            _ => {}
        }
        if i == 0 && has_drive(part) {
            return bad(Reason::DrivePrefix);
        }
        let folded = fold_name(part);
        if names_dir(&folded, "git")
            || (i == 0 && names_dir(&folded, "pyn") && !PYN_SHARED.contains(&path))
        {
            return bad(Reason::Reserved);
        }
        if windows && windows_unstorable(part) {
            return bad(Reason::WindowsName);
        }
    }
    Ok(())
}

/// Checks the target of a symbolic link at `link`: relative, and never above the work tree root lexically.
pub fn check_link_target(link: &[u8], target: &[u8]) -> Result<(), UnsafePath> {
    let bad = |reason| Err(UnsafePath::new(link, reason));
    if target.is_empty() {
        return bad(Reason::LinkEmpty);
    }
    if target[0] == b'/' || target[0] == b'\\' || has_drive(target) {
        return bad(Reason::LinkAbsolute);
    }
    if target.contains(&b'\\') || has_control(target) {
        return bad(Reason::Control);
    }
    let mut depth = link.iter().filter(|b| **b == b'/').count();
    for part in target.split(|b| *b == b'/') {
        match part {
            b"" | b"." => {}
            b".." => {
                if depth == 0 {
                    return bad(Reason::LinkEscapes);
                }
                depth -= 1;
            }
            _ => depth += 1,
        }
    }
    Ok(())
}

fn has_control(bytes: &[u8]) -> bool {
    bytes.iter().any(|b| *b < 0x20 || *b == 0x7f)
        || bytes
            .windows(2)
            .any(|w| w[0] == 0xC2 && (0x80..=0x9f).contains(&w[1]))
}

fn has_drive(part: &[u8]) -> bool {
    part.len() >= 2 && part[0].is_ascii_alphabetic() && part[1] == b':'
}

fn windows_unstorable(part: &[u8]) -> bool {
    if part.iter().any(|b| b":<>\"|?*".contains(b)) {
        return true;
    }
    if part.last().is_some_and(|b| *b == b'.' || *b == b' ') {
        return true;
    }
    let text = String::from_utf8_lossy(part);
    let stem = text.split('.').next().unwrap_or("").trim_end_matches(' ');
    let upper = stem.to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.as_bytes()[3].is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reason(path: &[u8], windows: bool) -> Option<Reason> {
        check_path_as(path, windows).err().map(|e| e.reason)
    }

    #[test]
    fn ordinary_paths_pass_everywhere() {
        for p in [
            "a",
            "a/b/c.txt",
            "Content/World/Main.umap",
            ".gitignore",
            ".github/workflows/ci.yml",
            ".gitattributes",
            "dir/.pynx",
            "a.b/c d/e~f",
            "git~x",
            "dir/pyn~1",
            "\u{e9}t\u{e9}/\u{65e5}\u{672c}",
            ".pyn/pyn.toml",
            ".pyn/ignore",
        ] {
            for windows in [false, true] {
                assert_eq!(reason(p.as_bytes(), windows), None, "{p} {windows}");
            }
        }
    }

    #[test]
    fn structural_forms_are_refused() {
        let cases: [(&[u8], Reason); 17] = [
            (b"", Reason::Empty),
            (b"/", Reason::Absolute),
            (b"/etc/passwd", Reason::Absolute),
            (b"a//b", Reason::EmptyComponent),
            (b"a/", Reason::EmptyComponent),
            (b".", Reason::CurrentDir),
            (b"a/./b", Reason::CurrentDir),
            (b"..", Reason::ParentDir),
            (b"../x", Reason::ParentDir),
            (b"a/../b", Reason::ParentDir),
            (b"a/b/..", Reason::ParentDir),
            (b"C:/x", Reason::DrivePrefix),
            (b"c:x", Reason::DrivePrefix),
            (b"a\\b", Reason::Backslash),
            (b"\\\\server\\share", Reason::Backslash),
            (b"//server/share", Reason::Absolute),
            (b"a\\", Reason::Backslash),
        ];
        for (p, want) in cases {
            for windows in [false, true] {
                assert_eq!(reason(p, windows), Some(want), "{p:?}");
            }
        }
    }

    #[test]
    fn control_characters_are_refused() {
        for p in [
            &b"a\0b"[..],
            b"a\nb",
            b"a\tb",
            b"a\rb",
            b"a\x1bb",
            b"\x7f",
            "a\u{85}b".as_bytes(),
            "a\u{9f}".as_bytes(),
        ] {
            assert_eq!(reason(p, false), Some(Reason::Control), "{p:?}");
        }
        assert_eq!(reason("a\u{a0}b".as_bytes(), false), None);
    }

    #[test]
    fn reserved_names_are_refused_after_folding() {
        for p in [
            ".git",
            ".git/config",
            "a/.git",
            "a/b/.git/hooks/x",
            ".GIT",
            ".Git/x",
            "a/.gIt",
            ".git.",
            ".git ",
            ".git. .",
            "GIT~1",
            "git~1/x",
            "a/git~12",
            ".git::$INDEX_ALLOCATION",
            ".git:stream",
            ".g\u{200c}it",
            ".gi\u{200d}t/x",
            "\u{feff}.git",
            ".\u{202e}git",
            "a/.gi\u{206a}t",
            ".pyn",
            ".pyn/local_only/config.toml",
            ".PYN/pyn.toml",
            ".pyn/other",
            ".pyn/pyn.toml/x",
            ".Pyn",
            "PYN~1/x",
            ".p\u{200c}yn",
            ".pyn.",
        ] {
            for windows in [false, true] {
                assert_eq!(
                    reason(p.as_bytes(), windows),
                    Some(Reason::Reserved),
                    "{p} {windows}"
                );
            }
        }
    }

    #[test]
    fn a_pyn_name_below_the_root_is_an_ordinary_name() {
        for p in ["a/.pyn", "a/.pyn/local_only", "a/.PYN/x"] {
            assert_eq!(reason(p.as_bytes(), false), None, "{p}");
        }
    }

    #[test]
    fn windows_rules_apply_only_on_windows() {
        for p in [
            "ab:c",
            "a/b:c",
            "a/CON",
            "con",
            "a/nul.txt",
            "PRN.tar.gz",
            "aux ",
            "COM1",
            "lpt9.log",
            "a/b.",
            "a/b ",
            "a/x?",
            "a/x*y",
            "a/<x>",
            "a/\"x",
            "a/x|y",
        ] {
            assert_eq!(reason(p.as_bytes(), false), None, "{p}");
            assert_eq!(reason(p.as_bytes(), true), Some(Reason::WindowsName), "{p}");
        }
        for p in ["console", "com", "COM10", "lpt", "a/auxiliary", "nulls"] {
            assert_eq!(reason(p.as_bytes(), true), None, "{p}");
        }
    }

    #[test]
    fn length_and_depth_are_capped() {
        let deep = vec!["a"; MAX_DEPTH].join("/");
        assert_eq!(reason(deep.as_bytes(), false), None);
        let deeper = vec!["a"; MAX_DEPTH + 1].join("/");
        assert_eq!(reason(deeper.as_bytes(), false), Some(Reason::TooDeep));
        assert_eq!(reason(&vec![b'a'; MAX_PATH_BYTES], false), None);
        assert_eq!(
            reason(&vec![b'a'; MAX_PATH_BYTES + 1], false),
            Some(Reason::TooLong)
        );
    }

    #[test]
    fn a_set_fails_on_the_first_bad_path_and_names_it() {
        let ok: [&[u8]; 2] = [b"a", b"b/c"];
        assert!(check_paths(ok).is_ok());
        let err = check_paths([&b"a"[..], b"x/.git/y", b"../z"]).unwrap_err();
        assert_eq!(err.path, "x/.git/y");
        assert!(err.to_string().contains("x/.git/y"));
    }

    #[test]
    fn display_escapes_control_characters() {
        let err = check_path(b"a\x1b[31mb").unwrap_err();
        assert!(!err.to_string().contains('\x1b'), "{err}");
    }

    #[test]
    fn link_targets_stay_inside_the_work_tree() {
        let ok = [
            ("l", "a"),
            ("l", "a/b"),
            ("d/l", "../x"),
            ("d/l", "../d/x"),
            ("a/b/l", "../../c"),
            ("l", "./a/../b"),
            ("l", "a/.."),
            ("d/l", "x/../../y"),
        ];
        for (link, target) in ok {
            assert_eq!(
                check_link_target(link.as_bytes(), target.as_bytes()),
                Ok(()),
                "{link} -> {target}"
            );
        }
        let bad = [
            ("l", "", Reason::LinkEmpty),
            ("l", "/etc", Reason::LinkAbsolute),
            ("l", "\\x", Reason::LinkAbsolute),
            ("l", "C:/x", Reason::LinkAbsolute),
            ("l", "..", Reason::LinkEscapes),
            ("l", "../x", Reason::LinkEscapes),
            ("d/l", "../../x", Reason::LinkEscapes),
            ("d/l", "a/../../../x", Reason::LinkEscapes),
            ("l", "a\\b", Reason::Control),
            ("l", "a\0b", Reason::Control),
        ];
        for (link, target, want) in bad {
            let got = check_link_target(link.as_bytes(), target.as_bytes());
            assert_eq!(got.map_err(|e| e.reason), Err(want), "{link} -> {target}");
        }
    }
}
