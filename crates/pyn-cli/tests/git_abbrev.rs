//! Abbreviations on git homes, with real git as the oracle for both object formats.

mod common;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use common::gitfix::{FORMATS, Repo, copy_tree};

fn git(repo: &Repo, dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("HOME", &repo.home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("XDG_CONFIG_HOME")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn setting(repo: &Repo, value: &str) {
    writeln!(
        OpenOptions::new()
            .append(true)
            .open(repo.main.join(".git/config"))
            .unwrap(),
        "\n[core]\n\tabbrev = {value}"
    )
    .unwrap();
}

fn same(repo: &Repo, args: &[&str]) -> String {
    let expected = git(repo, &repo.main, args);
    assert!(!expected.is_empty(), "positive rendering control: {args:?}");
    assert_eq!(repo.ok(args), expected, "{} {args:?}", repo.format);
    expected
}

#[test]
fn core_abbrev_numeric_disabled_auto_and_unset_match_git() {
    // Catches ignoring each config arm, treating auto as full, and truncating disabled SHA-256 ids.
    for format in FORMATS {
        let full = if format == "sha1" { 40 } else { 64 };
        for (value, width) in [
            (None, 7),
            (Some("4"), 4),
            (Some("12"), 12),
            (Some("999"), full),
            (Some("012"), 10),
            (Some("0x0c"), 12),
            (Some("010"), 8),
            (Some("0X0C"), 12),
            (Some("+12"), 12),
            (Some("+0x0c"), 12),
            (Some("4k"), full),
            (Some("4K"), full),
            (Some("4m"), full),
            (Some("1g"), full),
            (Some("40"), 40),
            (Some("64"), full),
            (Some("no"), full),
            (Some("NO"), full),
            (Some("off"), full),
            (Some("Off"), full),
            (Some("false"), full),
            (Some("FALSE"), full),
            (Some(""), full),
            (Some("auto"), 7),
            (Some("AUTO"), 7),
        ] {
            let repo = Repo::new(format);
            if let Some(value) = value {
                setting(&repo, value);
            }
            let out = same(&repo, &["log", "-n", "1", "--format=%h %t %p"]);
            assert_eq!(out.split_whitespace().next().unwrap().len(), width);
            same(&repo, &["show", "--format=%h %t %p", "HEAD~1"]);
            same(&repo, &["log", "-n", "1"]);
            same(&repo, &["branch", "-v"]);
            assert!(same(&repo, &["diff", "HEAD~2", "HEAD~1"]).contains("\nindex "));
            same(&repo, &["log", "-n", "1", "--oneline"]);
        }
    }
}

#[test]
fn abbrev_flags_override_config_and_bare_retains_it_like_git() {
    // Catches reversed precedence, a bare flag resetting config, and no/zero bypassing Git's minimum.
    for format in FORMATS {
        let repo = Repo::new(format);
        setting(&repo, "12");
        for (flag, width) in [
            ("--abbrev=9", 9),
            ("--abbrev", 12),
            ("--abbrev=no", 4),
            ("--abbrev=", 4),
            ("--abbrev=0", 4),
            ("--abbrev=3", 4),
            ("--abbrev=-1", if format == "sha1" { 40 } else { 64 }),
            ("--abbrev=-0", 4),
            ("--abbrev=+12", 12),
            ("--abbrev=12junk", 12),
            ("--abbrev=012", 12),
            ("--abbrev=0x0c", 4),
            ("--abbrev=4294967296", 4),
            ("--abbrev=999", if format == "sha1" { 40 } else { 64 }),
            (
                "--abbrev=999999999999999999999999",
                if format == "sha1" { 40 } else { 64 },
            ),
            ("--no-abbrev", if format == "sha1" { 40 } else { 64 }),
        ] {
            let out = same(&repo, &["log", "-n", "1", "--format=%h", flag]);
            assert_eq!(out.trim_end().len(), width, "{flag}");
            same(&repo, &["show", "--format=%h", flag, "HEAD~1"]);
        }
        assert!(
            same(
                &repo,
                &[
                    "log",
                    "-n",
                    "1",
                    "-p",
                    "--format=%h",
                    "--abbrev=9",
                    "HEAD~1"
                ],
            )
            .contains("\nindex ")
        );
        same(
            &repo,
            &["log", "-n", "1", "--format=%h", "--abbrev=9", "--no-abbrev"],
        );
        same(
            &repo,
            &["log", "-n", "1", "--format=%h", "--no-abbrev", "--abbrev=9"],
        );
        setting(&repo, "no");
        let out = same(&repo, &["log", "-n", "1", "--format=%h", "--abbrev=9"]);
        assert_eq!(out.trim_end().len(), 9);
        let out = same(&repo, &["log", "-n", "1", "--format=%h", "--abbrev"]);
        assert_eq!(out.trim_end().len(), if format == "sha1" { 40 } else { 64 });
        setting(&repo, "999");
        let out = same(&repo, &["log", "-n", "1", "--format=%h", "--abbrev=9"]);
        assert_eq!(out.trim_end().len(), 9);
    }
}

#[test]
fn invalid_core_abbrev_is_rejected_even_with_an_explicit_override() {
    // Catches clamping invalid config instead of rejecting it, and skipping validation under an override.
    for format in FORMATS {
        for value in [
            Some("3"),
            Some("0"),
            Some("nonsense"),
            Some("08"),
            Some("12junk"),
            Some("++12"),
            Some("0x+0c"),
            Some("+0x+0c"),
            Some("2147483648"),
            Some("4g"),
            None,
        ] {
            let repo = Repo::new(format);
            same(&repo, &["log", "-n", "1", "--format=%h"]);
            match value {
                Some(value) => setting(&repo, value),
                None => writeln!(
                    OpenOptions::new()
                        .append(true)
                        .open(repo.main.join(".git/config"))
                        .unwrap(),
                    "\n[core]\n\tabbrev"
                )
                .unwrap(),
            }
            let out = Command::new("git")
                .args(["log", "-1", "--format=%h", "--abbrev=9"])
                .current_dir(&repo.main)
                .env("HOME", &repo.home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .unwrap();
            assert!(!out.status.success());
            let (_, err) = repo.fails(&["log", "-n", "1", "--format=%h", "--abbrev=9"]);
            assert!(err.contains("core.abbrev"), "{err}");
        }
    }
}

fn alternate(repo: &Repo, name: &str) -> std::path::PathBuf {
    let path = repo.home.join(name);
    fs::create_dir_all(path.join("info")).unwrap();
    fs::create_dir_all(path.join("pack")).unwrap();
    path
}

fn link(objects: &Path, alternate: &Path) {
    fs::create_dir_all(objects.join("info")).unwrap();
    fs::write(
        objects.join("info/alternates"),
        format!("{}\n", alternate.display()),
    )
    .unwrap();
}

#[test]
fn alternate_only_revision_and_duplicate_objects_resolve_in_loose_and_packed_stores() {
    // Catches missing recursive alternates, scanning only packs/only loose, and counting duplicates as ambiguity.
    for format in FORMATS {
        for packed in [false, true] {
            let repo = Repo::new(format);
            let id = git(&repo, &repo.main, &["rev-parse", "HEAD"])
                .trim()
                .to_string();
            let objects = repo.main.join(".git/objects");
            if packed {
                git(&repo, &repo.main, &["repack", "-ad"]);
            }
            let alt = alternate(&repo, "alternate");
            let nested = alternate(&repo, "nested");
            copy_tree(&objects, &nested);
            link(&alt, &nested);
            link(&objects, &alt);
            same(&repo, &["show", "--no-patch", "--format=%h %s", &id[..12]]);
            // Leave the only copy of every object in the nested alternate.
            for entry in fs::read_dir(&objects).unwrap() {
                let entry = entry.unwrap();
                if entry.file_name() != "info" {
                    fs::remove_dir_all(entry.path()).unwrap();
                }
            }
            same(&repo, &["show", "--no-patch", "--format=%h %s", &id[..12]]);
        }
    }
}

#[test]
fn a_collision_in_an_alternate_extends_output_and_duplicate_ids_do_not() {
    // Catches checking only commit ids, ignoring alternate loose objects or packs, and extending for the object itself.
    for format in FORMATS {
        for packed in [false, true] {
            let repo = Repo::new(format);
            setting(&repo, "4");
            let id = git(&repo, &repo.main, &["rev-parse", "HEAD"])
                .trim()
                .to_string();
            let objects = repo.main.join(".git/objects");
            let alt = alternate(&repo, "alternate");
            copy_tree(&objects, &alt);
            link(&objects, &alt);
            let out = same(&repo, &["log", "-n", "1", "--format=%h"]);
            assert_eq!(out.trim_end(), &id[..4]);
            let algorithm = if format == "sha1" {
                pyn_object::Algorithm::Sha1
            } else {
                pyn_object::Algorithm::Sha256
            };
            let body = (0..2_000_000)
                .find_map(|n| {
                    let data = format!("alternate collision {n}\n").into_bytes();
                    let other =
                        pyn_object::hash_object(algorithm, pyn_object::Kind::Blob, &data).unwrap();
                    (other.to_hex().starts_with(&id[..4]) && !other.to_hex().starts_with(&id[..5]))
                        .then_some(data)
                })
                .expect("a deterministic four-digit collision");
            let other = pyn_git::ObjectDb::open(&alt, algorithm)
                .unwrap()
                .write(pyn_object::Kind::Blob, &body)
                .unwrap()
                .to_hex();
            if packed {
                let prefix = alt.join("pack/pack");
                let mut child = Command::new("git")
                    .arg("pack-objects")
                    .arg(prefix)
                    .current_dir(&repo.main)
                    .env("HOME", &repo.home)
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .spawn()
                    .unwrap();
                writeln!(child.stdin.take().unwrap(), "{other}").unwrap();
                assert!(child.wait().unwrap().success());
                fs::remove_file(alt.join(&other[..2]).join(&other[2..])).unwrap();
            }
            let out = same(&repo, &["log", "-n", "1", "--format=%h"]);
            assert_eq!(out.trim_end(), &id[..5]);
        }
    }
}

#[test]
fn packed_object_count_raises_the_default_abbreviation_above_seven() {
    // Catches a fixed-seven fallback, the wrong bit/hex rounding, and ignoring alternate pack counts.
    for format in FORMATS {
        let repo = Repo::new(format);
        let before = same(&repo, &["log", "-n", "1", "--format=%h"]);
        assert_eq!(before.trim_end().len(), 7);
        // One compact pack at runtime instead of duplicating a large archived fixture.
        let mut child = Command::new("git")
            .args(["fast-import", "--quiet"])
            .current_dir(&repo.main)
            .env("HOME", &repo.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        {
            let mut input = child.stdin.take().unwrap();
            for n in 0..16384 {
                let data = format!("abbrev-count-{n}\n");
                write!(input, "blob\ndata {}\n{data}\n", data.len()).unwrap();
            }
            writeln!(input, "done").unwrap();
        }
        assert!(child.wait().unwrap().success());
        for value in [None, Some("auto")] {
            if let Some(value) = value {
                setting(&repo, value);
            }
            let out = same(&repo, &["log", "-n", "1", "--format=%h"]);
            assert_eq!(out.trim_end().len(), 8);
            same(&repo, &["show", "--format=%h", "HEAD~1"]);
            same(&repo, &["diff", "HEAD~2", "HEAD~1"]);
        }
        let objects = repo.main.join(".git/objects");
        let alt = alternate(&repo, "count-alternate");
        fs::rename(objects.join("pack"), alt.join("packed")).unwrap();
        fs::rename(alt.join("packed"), alt.join("pack")).unwrap();
        let bridge = alternate(&repo, "count-bridge");
        link(&objects, &bridge);
        link(&bridge, &alt);
        let out = same(&repo, &["log", "-n", "1", "--format=%h"]);
        assert_eq!(out.trim_end().len(), 8);
    }
}

#[test]
fn repeated_abbrev_flags_use_the_last_numeric_or_bare_value() {
    // Kills self-conflict, first-value-wins and treating a repeated bare flag as its previous numeric value.
    for format in FORMATS {
        let repo = Repo::new(format);
        setting(&repo, "12");
        for (flags, width) in [
            (["--abbrev=9", "--abbrev=5"], 5),
            (["--abbrev=5", "--abbrev=9"], 9),
            (["--abbrev=9", "--abbrev"], 12),
            (["--abbrev", "--abbrev=9"], 9),
            (["--abbrev", "--abbrev"], 12),
        ] {
            let out = same(
                &repo,
                &["log", "-n", "1", "--format=%h", flags[0], flags[1]],
            );
            assert_eq!(out.trim_end().len(), width);
            same(
                &repo,
                &["show", "--format=%h", flags[0], flags[1], "HEAD~1"],
            );
        }
    }
}

#[test]
fn no_abbrev_keeps_patch_indices_at_the_configured_width() {
    // Kills propagating the full-header sentinel as an explicit full-width patch request.
    for format in FORMATS {
        for value in [None, Some("12"), Some("auto")] {
            let repo = Repo::new(format);
            if let Some(value) = value {
                setting(&repo, value);
            }
            let out = same(&repo, &["show", "--format=%h", "--no-abbrev", "HEAD~1"]);
            assert_eq!(
                out.lines().next().unwrap().len(),
                if format == "sha1" { 40 } else { 64 }
            );
            let indices: Vec<_> = out
                .lines()
                .filter_map(|line| line.strip_prefix("index "))
                .collect();
            assert!(!indices.is_empty());
            for index in indices {
                for id in index.split_whitespace().next().unwrap().split("..") {
                    assert_eq!(id.len(), if value == Some("12") { 12 } else { 7 });
                }
            }
            same(
                &repo,
                &[
                    "log",
                    "-n",
                    "1",
                    "-p",
                    "--format=%h",
                    "--no-abbrev",
                    "HEAD~1",
                ],
            );
        }
    }
}

#[test]
fn many_loose_objects_do_not_raise_the_default_abbreviation() {
    // Kills using total objects instead of packed objects for the size heuristic.
    for format in FORMATS {
        let repo = Repo::new(format);
        assert_eq!(
            same(&repo, &["log", "-n", "1", "--format=%h"])
                .trim_end()
                .len(),
            7
        );
        let algorithm = pyn_object::Algorithm::from_name(format).unwrap();
        let db = pyn_git::ObjectDb::open(&repo.main.join(".git/objects"), algorithm).unwrap();
        for n in 0..16384 {
            db.write(
                pyn_object::Kind::Blob,
                format!("loose object {n}\n").as_bytes(),
            )
            .unwrap();
        }
        let count = git(&repo, &repo.main, &["count-objects", "-v"]);
        assert!(count.lines().any(|line| line == "in-pack: 0"), "{count}");
        let loose: usize = count
            .lines()
            .find_map(|line| line.strip_prefix("count: "))
            .unwrap()
            .parse()
            .unwrap();
        assert!(loose >= 16384);
        assert_eq!(
            same(&repo, &["log", "-n", "1", "--format=%h"])
                .trim_end()
                .len(),
            7
        );
        setting(&repo, "auto");
        assert_eq!(
            same(&repo, &["log", "-n", "1", "--format=%h"])
                .trim_end()
                .len(),
            7
        );
    }
}

fn patch_prompt_indices(repo: &Repo, program: &str) -> Vec<String> {
    let mut child = Command::new(program)
        .args(["add", "-p", "--", "README.md"])
        .current_dir(&repo.main)
        .env("HOME", &repo.home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("PYN_SERVER")
        .env_remove("PYN_REPO")
        .env_remove("PYN_DIR")
        .env("PYN_CONFIG_DIR", repo.home.join("pyn-config"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"q\n").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|line| line.starts_with("index "))
        .map(str::to_string)
        .collect()
}

#[test]
fn patch_editor_headers_honor_the_nondefault_width() {
    // Kills a patch-mode header that still uses seven despite the configured twelve.
    for format in FORMATS {
        let repo = Repo::new(format);
        setting(&repo, "12");
        let expected = patch_prompt_indices(&repo, "git");
        assert!(!expected.is_empty());
        assert_eq!(
            patch_prompt_indices(&repo, env!("CARGO_BIN_EXE_pyn")),
            expected
        );
        for line in expected {
            assert!(
                line.split_whitespace()
                    .nth(1)
                    .unwrap()
                    .split("..")
                    .all(|id| id.len() == 12)
            );
        }
    }
}

#[test]
fn null_ids_extend_for_zero_prefix_collisions_in_diff_and_patch_editor_headers() {
    // Kills minimum-width zero repetition in normal diffs and interactive patch headers.
    for format in FORMATS {
        let repo = Repo::new(format);
        setting(&repo, "4");
        assert!(same(&repo, &["show", "--format=%h", "HEAD~1"]).contains("..0000\n"));
        common::gitfix::write_zero_prefix_blob(&repo.main, format);
        assert!(same(&repo, &["show", "--format=%h", "HEAD~1"]).contains("..00000\n"));
        assert!(same(&repo, &["diff", "HEAD~2", "HEAD~1"]).contains("..00000\n"));
        fs::remove_file(repo.main.join("README.md")).unwrap();
        let expected = patch_prompt_indices(&repo, "git");
        assert!(!expected.is_empty());
        assert!(
            expected.iter().any(|line| line.ends_with("..00000")),
            "{expected:?}"
        );
        assert_eq!(
            patch_prompt_indices(&repo, env!("CARGO_BIN_EXE_pyn")),
            expected
        );
    }
}
