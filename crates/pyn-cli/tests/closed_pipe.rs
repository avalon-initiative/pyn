//! A reader that goes away early ends a command quietly, as `pyn ... | head` needs.

mod common;

use common::start;

#[test]
fn a_closed_output_pipe_ends_every_kind_of_command_quietly() {
    let env = start();
    env.seed("alice", "Source/a.cpp", &"int a;\n".repeat(20_000), None);
    let home = env.dir("home");
    let ws = env.dir("ws");
    env.ok(
        &home,
        "alice",
        &["clone", &env.source(common::REPO), ws.to_str().unwrap()],
    );
    let ws = ws.canonicalize().unwrap();
    std::fs::write(ws.join("Source/b.cpp"), "b").unwrap();
    for (cwd, args) in [
        (&home, &["usage", "alice"][..]),
        (&home, &["files"]),
        (&home, &["--json", "files"]),
        (&home, &["get", "Source/a.cpp"]),
        (&ws, &["log"]),
        (&ws, &["status", "--short"]),
    ] {
        let out = env.run_into_closed_pipe(cwd, "alice", args);
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(0), "pyn {args:?}: {err}");
        assert!(
            !err.contains("panicked") && !err.to_lowercase().contains("broken pipe"),
            "pyn {args:?}: {err}"
        );
    }
}
