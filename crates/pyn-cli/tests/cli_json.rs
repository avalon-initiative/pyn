//! `--json` output: the versioned shapes in docs/cli-json.md, the server's error codes and the unchanged exit codes.

mod common;

use common::{REPO, start};
use serde_json::Value;

fn json(out: &str) -> Value {
    serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("not JSON ({e}): {out}"))
}

#[test]
fn listings_and_results_are_versioned_json() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "a", None);
    let home = env.dir("home");

    let lock = json(&env.ok(&home, "alice", &["lock", "Content/m.umap", "--json"]));
    assert_eq!(lock["version"], 1);
    assert_eq!(lock["lock"]["owner"], "alice");
    assert_eq!(lock["lock"]["path"], "Content/m.umap");

    let locks = json(&env.ok(&home, "alice", &["locks", "--json"]));
    assert_eq!(locks["locks"][0]["path"], "Content/m.umap");
    let mine = json(&env.ok(&home, "alice", &["locks", "--mine", "--json"]));
    assert_eq!(mine["locks"][0]["name"], "game");

    let files = json(&env.ok(&home, "alice", &["files", "--json"]));
    assert!(
        files["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["path"] == "Source/a.cpp" && f["mode"] == "shared" && f["revision"] == 1)
    );
    let ls = json(&env.ok(&home, "alice", &["ls", "--json"]));
    assert_eq!(ls["path"], "");
    assert!(ls["entries"].as_array().unwrap().len() >= 2);

    let summary = json(&env.ok(&home, "alice", &["summary", "--json"]));
    assert_eq!(summary["summary"]["locks"][0]["owner"], "alice");

    let me = json(&env.ok(&home, "alice", &["whoami", "--json"]));
    assert_eq!(
        (me["user"].as_str(), me["repo"].as_str()),
        (Some("alice"), Some(REPO))
    );
    assert!(me["permissions"].is_array());

    let unlock = json(&env.ok(&home, "alice", &["unlock", "Content/m.umap", "--json"]));
    assert_eq!(unlock["unlocked"], "Content/m.umap");
}

#[test]
fn a_refused_lock_carries_the_code_and_the_holder_with_the_same_exit_code() {
    let env = start();
    let home = env.dir("home");
    env.ok(&home, "bob", &["lock", "Content/m.umap"]);

    let plain = env.run(&home, "alice", &["lock", "Content/m.umap"]);
    let out = env.run(&home, "alice", &["lock", "Content/m.umap", "--json"]);
    assert_eq!(out.status.code(), plain.status.code());
    let doc = json(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(doc["version"], 1);
    assert_eq!(doc["error"]["code"], "lock_held");
    assert_eq!(doc["error"]["status"], 409);
    assert_eq!(doc["error"]["lock"]["owner"], "bob");
    assert!(doc["error"]["lock"]["expires_at"].is_string());
}

#[test]
fn status_lists_changed_files_with_state_tokens_and_counts() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "a one", None);
    let ws = env.dir("ws");
    env.ok(
        &env.dir("home"),
        "alice",
        &["clone", &env.source(REPO), ws.to_str().unwrap()],
    );
    let ws = ws.canonicalize().unwrap();
    std::fs::write(ws.join("Source/a.cpp"), "edited").unwrap();
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);

    let doc = json(&env.ok(&ws, "alice", &["status", "--json"]));
    assert_eq!(doc["version"], 1);
    let files = doc["files"].as_array().unwrap();
    let a = files.iter().find(|f| f["path"] == "Source/a.cpp").unwrap();
    assert_eq!(a["states"][0], "modified");
    let m = files
        .iter()
        .find(|f| f["path"] == "Content/m.umap")
        .unwrap();
    assert_eq!(m["lock"]["owner"], "alice");
    assert_eq!(m["lock"]["mine"], true);
    assert_eq!(doc["counts"]["modified"], 1);
    assert_eq!(doc["counts"]["locked_by_you"], 1);
}

#[test]
fn other_failures_and_unsupported_commands() {
    let env = start();
    let home = env.dir("home");
    let out = env.run(&home, "alice", &["unlock", "Content/none.umap", "--json"]);
    assert_eq!(out.status.code(), Some(128));
    let doc = json(&String::from_utf8_lossy(&out.stdout));
    assert!(doc["error"]["code"].is_string());

    let out = env.run(&home, "alice", &["log", "--json"]);
    assert_eq!(out.status.code(), Some(129));
}
