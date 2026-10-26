//! Webhook subscription administration from the command line.

mod common;

use common::{cell_rows, start_uninitialised};

const TOKEN: &str = "0123456789abcdef0123456789abcdef";
const PASSWORD: &str = "correct horse battery";

fn text(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn set_up() -> (common::Env, std::path::PathBuf) {
    let env = start_uninitialised(TOKEN);
    let home = env.dir("home");
    let input = format!("{PASSWORD}\n");
    let out = env.run_stdin(
        &home,
        "-",
        &[
            "setup",
            "root",
            "--setup-token",
            TOKEN,
            "--registration",
            "open",
            "--password-stdin",
        ],
        &input,
    );
    assert!(out.status.success(), "{}", text(&out));
    let out = env.run_stdin(
        &home,
        "-",
        &["register", "alice", "--password-stdin"],
        &input,
    );
    assert!(out.status.success(), "{}", text(&out));
    (env, home)
}

#[test]
fn a_webhook_is_created_listed_changed_rotated_and_deleted() {
    let (env, home) = set_up();
    let made = env.run(
        &home,
        "root",
        &[
            "admin",
            "webhook",
            "create",
            "ci",
            "--url",
            "https://example.com/hook",
            "--event",
            "repository.created,owner.limits_changed",
        ],
    );
    assert!(made.status.success(), "{}", text(&made));
    let secret = String::from_utf8_lossy(&made.stdout).trim().to_string();
    assert!(secret.starts_with("whsec_"), "{secret}");
    assert!(text(&made).contains("only time the signing secret is shown"));

    let listed = cell_rows(&env.ok(&home, "root", &["admin", "webhook", "list"]));
    assert_eq!(
        listed[0],
        ["ID", "STATE", "SECRETS", "EVENTS", "NAME", "URL"]
    );
    assert_eq!(
        listed[1],
        [
            "1",
            "on",
            "1",
            "repository.created,owner.limits_changed",
            "ci",
            "https://example.com/hook"
        ]
    );
    assert!(!listed[1].join(" ").contains(&secret));

    let shown = env.ok(&home, "root", &["admin", "webhook", "show", "1"]);
    assert!(shown.contains("https://example.com/hook") && shown.contains("secret 1:"));
    assert!(!shown.contains(&secret));

    let out = env.ok(
        &home,
        "root",
        &[
            "admin",
            "webhook",
            "update",
            "1",
            "--disable",
            "--all-events",
            "--name",
            "builds",
        ],
    );
    assert!(out.contains("updated webhook 1"));
    let listed = cell_rows(&env.ok(&home, "root", &["admin", "webhook", "list"]));
    assert_eq!(listed[1][..5], ["1", "off", "1", "all", "builds"]);
    env.ok(
        &home,
        "root",
        &["admin", "webhook", "update", "1", "--enable"],
    );

    let rotated = env.run(&home, "root", &["admin", "webhook", "rotate", "1"]);
    assert!(rotated.status.success(), "{}", text(&rotated));
    let second = String::from_utf8_lossy(&rotated.stdout).trim().to_string();
    assert!(second.starts_with("whsec_") && second != secret);
    assert!(text(&rotated).contains("only time it is shown"));
    let err = env.fails(&home, "root", &["admin", "webhook", "rotate", "1"]);
    assert!(err.contains("webhook_secret_limit"), "{err}");

    let out = env.ok(&home, "root", &["admin", "webhook", "retire", "1", "1"]);
    assert!(out.contains("retired secret 1 of webhook 1"));
    let err = env.fails(&home, "root", &["admin", "webhook", "retire", "1", "2"]);
    assert!(err.contains("webhook_last_secret"), "{err}");

    let out = env.ok(&home, "root", &["admin", "webhook", "delete", "1"]);
    assert!(out.contains("deleted webhook 1"));
    let out = env.ok(&home, "root", &["admin", "webhook", "list"]);
    assert_eq!(out.trim(), "no webhooks");
}

#[test]
fn a_bad_url_and_a_missing_scope_are_refused() {
    let (env, home) = set_up();
    let err = env.fails(
        &home,
        "root",
        &[
            "admin",
            "webhook",
            "create",
            "ci",
            "--url",
            "http://example.com/",
        ],
    );
    assert!(
        err.contains("invalid_request") && err.contains("https"),
        "{err}"
    );

    let err = env.fails(&home, "root", &["admin", "webhook", "show", "7"]);
    assert!(err.contains("webhook_not_found"), "{err}");

    let made = env.run(
        &home,
        "root",
        &[
            "admin",
            "service-credential",
            "create",
            "limits",
            "--scopes",
            "manage_limits",
        ],
    );
    let narrow = String::from_utf8_lossy(&made.stdout).trim().to_string();
    let out = env.run_with_token(&home, &narrow, &["admin", "webhook", "list"]);
    assert!(
        text(&out).contains("service_scope_required"),
        "{}",
        text(&out)
    );

    let made = env.run(
        &home,
        "root",
        &[
            "admin",
            "service-credential",
            "create",
            "hooks",
            "--scopes",
            "manage_webhooks",
        ],
    );
    assert!(made.status.success(), "{}", text(&made));
    let scoped = String::from_utf8_lossy(&made.stdout).trim().to_string();
    let out = env.run_with_token(&home, &scoped, &["admin", "webhook", "list"]);
    assert!(out.status.success(), "{}", text(&out));
}

#[test]
fn deliveries_are_listed_filtered_and_redelivered() {
    let (env, home) = set_up();
    env.ok(
        &home,
        "root",
        &[
            "admin",
            "webhook",
            "create",
            "ci",
            "--url",
            "https://example.com/hook",
        ],
    );
    for repos in ["5", "6"] {
        env.ok(
            &home,
            "root",
            &["admin", "limits", "set", "alice", "--repos", repos],
        );
    }

    let listed = cell_rows(&env.ok(&home, "root", &["admin", "webhooks", "deliveries", "1"]));
    assert_eq!(
        listed[0],
        [
            "EVENT", "STATUS", "ATTEMPTS", "NEXT", "CODE", "DELIVERY", "ERROR"
        ]
    );
    assert_eq!(listed.len(), 3, "{listed:?}");
    assert_eq!(listed[1][1], "pending");
    let delivery = listed[1][5].clone();

    let out = env.ok(
        &home,
        "root",
        &[
            "admin",
            "webhook",
            "deliveries",
            "1",
            "--status",
            "delivered",
        ],
    );
    assert_eq!(out.trim(), "no deliveries");

    let paged = env.run(
        &home,
        "root",
        &["admin", "webhook", "deliveries", "1", "--limit", "1"],
    );
    assert!(paged.status.success(), "{}", text(&paged));
    assert!(text(&paged).contains("more: pass --before"));

    let out = env.ok(
        &home,
        "root",
        &["admin", "webhook", "redeliver", "1", &delivery],
    );
    assert!(out.contains(&format!("delivery {delivery} of webhook 1 queued again")));
    let err = env.fails(
        &home,
        "root",
        &[
            "admin",
            "webhook",
            "redeliver",
            "1",
            "00000000-0000-4000-8000-000000000000",
        ],
    );
    assert!(err.contains("webhook_delivery_not_found"), "{err}");
    let err = env.fails(
        &home,
        "root",
        &["admin", "webhook", "deliveries", "1", "--status", "x"],
    );
    assert!(err.contains("invalid_request"), "{err}");
}
