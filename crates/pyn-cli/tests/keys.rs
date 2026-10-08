//! Linking SSH public keys to an account with the real `pyn` binary.

mod common;

use std::path::PathBuf;

use common::start;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../pyn-core/tests/fixtures/keys")
        .join(name)
}

#[test]
fn keys_are_added_listed_and_removed_from_the_command_line() {
    let env = start();
    let home = env.dir("home");
    let ed1 = fixture("ed1.pub");

    let out = env.ok(
        &home,
        "alice",
        &["key", "add", ed1.to_str().unwrap(), "--title", "laptop"],
    );
    let fingerprint = std::fs::read_to_string(fixture("fingerprints.txt"))
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("ed1 ").map(str::to_string))
        .unwrap();
    assert!(
        out.contains("laptop") && out.contains(&fingerprint),
        "{out}"
    );

    let again = env.fails(&home, "bob", &["key", "add", ed1.to_str().unwrap()]);
    assert!(
        again.contains("key_in_use"),
        "another account cannot link the same key: {again}"
    );

    let listed = env.ok(&home, "alice", &["key", "list"]);
    assert!(
        listed.contains("laptop")
            && listed.contains("ssh-ed25519")
            && listed.contains("never used"),
        "{listed}"
    );
    assert!(
        env.ok(&home, "bob", &["key", "list"]).trim().is_empty(),
        "only your own keys"
    );

    let id = listed.split('\t').next().unwrap().to_string();
    let out = env.ok(&home, "alice", &["key", "remove", &id]);
    assert!(out.contains("removed"), "{out}");
    assert!(env.ok(&home, "alice", &["key", "list"]).trim().is_empty());
    let gone = env.fails(&home, "alice", &["key", "remove", &id]);
    assert!(gone.contains("key_not_found"), "{gone}");
}

#[test]
fn without_a_file_the_usual_public_key_is_used_and_bad_keys_are_refused() {
    let env = start();
    let home = env.dir("home");
    let missing = env.fails(&home, "alice", &["key", "add"]);
    assert!(
        missing.contains("no public key found") || missing.contains("home directory"),
        "{missing}"
    );

    let ssh = home.join(".ssh");
    std::fs::create_dir_all(&ssh).unwrap();
    std::fs::copy(fixture("ed2.pub"), ssh.join("id_ed25519.pub")).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_pyn"))
        .args(["key", "add"])
        .current_dir(&home)
        .env("HOME", &home)
        .env("PYN_SERVER", &env.url)
        .env("PYN_USER", "alice")
        .env("PYN_CONFIG_DIR", home.join("cfg"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("bob@desktop"),
        "the title comes from the key's comment"
    );

    let weak = env.fails(
        &home,
        "alice",
        &["key", "add", fixture("rsa1024.pub").to_str().unwrap()],
    );
    assert!(weak.contains("at least 2048"), "{weak}");
    let dsa = env.fails(
        &home,
        "alice",
        &["key", "add", fixture("dsa.pub").to_str().unwrap()],
    );
    assert!(dsa.contains("not accepted"), "{dsa}");
}
