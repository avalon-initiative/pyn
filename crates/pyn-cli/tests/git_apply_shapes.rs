//! `pyn apply` and `pyn am` when a patch needs a folder where a file is tracked, or a file where a folder is, or a
//! path below a symlink, compared with real git on twin repositories: exit status class and the resulting state.
//! Real git is the oracle in these tests only.

mod patchfix;

#[path = "common/oracle.rs"]
mod oracle;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use patchfix::{FORMATS, Twin, commit, run, snapshot, write};

fn history(dir: &Path) {
    let mut tick = 1_700_000_000;
    write(dir, "d", "plain\n");
    write(dir, "e", "other\n");
    write(dir, "dd/g", "g\n");
    symlink("e", dir.join("ln")).unwrap();
    commit(dir, &mut tick, "T <t@example.com>", "base");
    run(dir, &["tag", "base"]);
}

fn add_file(name: &str) -> String {
    format!(
        "diff --git a/{name} b/{name}\nnew file mode 100644\nindex 0000000..e69de29\n--- /dev/null\n+++ b/{name}\n@@ -0,0 +1 @@\n+n\n"
    )
}

fn twin(format: &'static str) -> Twin {
    Twin::new(format, history)
}

/// A state with the unmerged stage of a moved-aside file read at its original path: git versions differ on where
/// `am --3way` leaves it (`d` or `d~HEAD`).
fn stages_at_origin(state: &str) -> Vec<String> {
    let mut lines: Vec<String> = state
        .replace("\td~HEAD\n", "\td\n")
        .lines()
        .map(str::to_string)
        .collect();
    lines.sort();
    lines
}

/// Both tools refuse or both succeed, and the repositories end up the same.
fn alike(t: &Twin, args: &[&str], stdin: &[u8]) {
    let want = t.run_git("", args, stdin);
    let got = t.run_pyn("", args, stdin);
    assert_eq!(
        oracle::status_class(got.code),
        oracle::status_class(want.code),
        "{} {args:?}: pyn stderr {}",
        t.format,
        got.err()
    );
    assert_eq!(
        stages_at_origin(&snapshot(&t.pyn)),
        stages_at_origin(&snapshot(&t.git)),
        "{} {args:?}: state",
        t.format
    );
}

#[test]
fn apply_refuses_a_shape_clash_like_git() {
    for format in FORMATS {
        for name in ["d/f", "dd", "ln/f", "d/f/x"] {
            for mode in [None, Some("--index"), Some("--cached")] {
                let t = twin(format);
                let mut args = vec!["apply"];
                args.extend(mode);
                alike(&t, &args, add_file(name).as_bytes());
            }
        }
    }
}

fn commit_all(t: &Twin, message: &str) {
    t.both(&["add", "-A"]);
    t.both(&[
        "-c",
        "gc.auto=0",
        "-c",
        "maintenance.auto=false",
        "commit",
        "-q",
        "-m",
        message,
    ]);
}

/// A mailbox with one commit that adds `name`; `clear` is removed in the same commit or in an earlier one.
fn mbox(t: &Twin, name: &str, clear: &str, together: bool) -> String {
    t.both(&["checkout", "-q", "-b", "p", "base"]);
    t.each_dir(|dir| {
        let _ = fs::remove_file(dir.join(clear));
        let _ = fs::remove_dir_all(dir.join(clear));
    });
    if !together {
        commit_all(t, "clear");
    }
    t.each_dir(|dir| write(dir, name, "n\n"));
    commit_all(t, "add it");
    let text = run(&t.git, &["format-patch", "-1", "--stdout"]);
    t.both(&["checkout", "-q", "base"]);
    t.both(&["branch", "-q", "-D", "p"]);
    t.both(&["clean", "-fdq"]);
    text
}

#[test]
fn am_handles_a_shape_clash_like_git() {
    for format in FORMATS {
        for (name, clear) in [("d/f", "d"), ("dd", "dd")] {
            for together in [false, true] {
                for args in [vec!["am"], vec!["am", "--3way"]] {
                    let t = twin(format);
                    let text = mbox(&t, name, clear, together);
                    fs::write(t.root.join("m.mbox"), &text).unwrap();
                    let file = t.root.join("m.mbox").display().to_string();
                    let mut full = args.clone();
                    full.push(&file);
                    alike(&t, &full, b"");
                }
            }
        }
    }
}
