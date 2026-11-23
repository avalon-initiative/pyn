//! `pyn-server --check-config` and the unknown-variable warning, through the real binary.

use std::fs::File;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn check(env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pyn-server"))
        .arg("--check-config")
        .env_clear()
        .envs(env.iter().copied())
        .output()
        .unwrap()
}

fn out(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn lines(output: &Output) -> Vec<String> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

fn problems(output: &Output) -> Vec<String> {
    lines(output)
        .into_iter()
        .filter(|l| l.starts_with("PYN_"))
        .collect()
}

#[test]
fn a_clean_environment_is_ok_and_says_what_it_did_not_check() {
    let output = check(&[]);
    assert!(output.status.success(), "{}", out(&output));
    let lines = lines(&output);
    assert_eq!(lines[0], "configuration ok");
    assert!(lines.last().unwrap().starts_with("not checked:"));
    assert!(lines.last().unwrap().contains("PYN_DATABASE_URL"));
    assert_eq!(lines.len(), 2, "{lines:?}");
}

#[test]
fn every_problem_is_reported_not_only_the_first() {
    let output = check(&[
        ("PYN_SESSION_DAYS", "many"),
        ("PYN_SESSION_REMEMBER_DAYS", "forever"),
        ("PYN_TRUSTED_PROXIES", "3"),
        ("PYN_S3_REGION", "eu-west-1"),
        ("PYN_MAILER", "console"),
        ("PYN_SHUTDOWN_DRAIN_DELAY_SECS", "7200"),
    ]);
    assert_eq!(output.status.code(), Some(1), "{}", out(&output));
    let found = problems(&output);
    assert_eq!(found.len(), 6, "{found:?}");
    for name in [
        "PYN_SESSION_DAYS",
        "PYN_SESSION_REMEMBER_DAYS",
        "PYN_TRUSTED_PROXIES",
        "PYN_S3_REGION",
        "PYN_MAILER",
        "PYN_SHUTDOWN_DRAIN_DELAY_SECS",
    ] {
        assert!(found.iter().any(|l| l.contains(name)), "{name}: {found:?}");
    }
    let lines = lines(&output);
    assert!(
        lines.contains(&"6 problem(s) found".to_string()),
        "{lines:?}"
    );
    assert!(lines.last().unwrap().starts_with("not checked:"));
}

#[test]
fn the_messages_match_the_real_start() {
    let env = [("PYN_TRUSTED_PROXIES", "3")];
    let output = check(&env);
    let expected = "PYN_TRUSTED_PROXIES only applies with PYN_TRUST_FORWARDED_FOR=true";
    assert!(lines(&output).contains(&expected.to_string()));
    let start = Command::new(env!("CARGO_BIN_EXE_pyn-server"))
        .env_clear()
        .envs(env)
        .output()
        .unwrap();
    assert_eq!(start.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&start.stderr).contains(expected));
}

#[test]
fn a_real_start_with_several_problems_points_at_the_check() {
    let start = Command::new(env!("CARGO_BIN_EXE_pyn-server"))
        .env_clear()
        .env("PYN_TRUSTED_PROXIES", "3")
        .env("PYN_SHUTDOWN_GRACE_SECS", "soon")
        .output()
        .unwrap();
    assert_eq!(start.status.code(), Some(1));
    let err = String::from_utf8_lossy(&start.stderr);
    assert!(err.contains("PYN_SHUTDOWN_GRACE_SECS must be"), "{err}");
    assert!(err.contains("1 more problem(s)") && err.contains("--check-config"));
}

#[test]
fn no_secret_value_is_ever_printed() {
    let secrets = [
        "sk-hunter2-secret",
        "smtp-pw-hunter3",
        "oidc-hunter4",
        "s3-hunter5",
        "db-hunter6",
        "key-hunter7",
        "tok hunter8",
    ];
    let output = check(&[
        ("PYN_SECRET_KEY", secrets[0]),
        ("PYN_MAILER", "smtp"),
        ("PYN_SMTP_HOST", "smtp.example.org"),
        ("PYN_SMTP_FROM", "pyn@example.org"),
        ("PYN_SMTP_PASSWORD", secrets[1]),
        ("PYN_OIDC_CLIENT_SECRET", secrets[2]),
        ("PYN_S3_BUCKET", "b"),
        (
            "PYN_S3_ENDPOINT",
            &format!("https://user:{}@s3.example", secrets[3]),
        ),
        ("PYN_S3_ACCESS_KEY_ID", "a"),
        ("PYN_S3_SECRET_ACCESS_KEY", secrets[5]),
        (
            "PYN_DATABASE_URL",
            &format!("mysql://u:{}@h/db", secrets[4]),
        ),
        ("PYN_SETUP_TOKEN", secrets[6]),
    ]);
    assert_eq!(output.status.code(), Some(1));
    let text = out(&output);
    for secret in secrets {
        assert!(!text.contains(secret), "{secret} leaked: {text}");
    }
    for name in [
        "PYN_SECRET_KEY",
        "PYN_SMTP_USER",
        "PYN_OIDC_ISSUER",
        "PYN_S3_ENDPOINT",
        "PYN_DATABASE_URL",
        "PYN_SETUP_TOKEN",
    ] {
        assert!(text.contains(name), "{name} missing: {text}");
    }
}

#[test]
fn an_unknown_variable_is_a_warning_with_a_suggestion() {
    let output = check(&[
        ("PYN_PUBLIC_UR", "http://localhost"),
        ("PYN_NOT_A_SETTING_AT_ALL", "x"),
        ("PYN_SERVER_VERSION", "v0.1.0"),
        ("PYN_TOKEN", "t"),
        ("PYN_INSTALL_COLOR", "1"),
    ]);
    assert!(output.status.success(), "{}", out(&output));
    let lines = lines(&output);
    assert!(lines.contains(
        &"warning: PYN_PUBLIC_UR is not a setting this server reads; did you mean PYN_PUBLIC_URL?"
            .to_string()
    ));
    assert!(lines.contains(
        &"warning: PYN_NOT_A_SETTING_AT_ALL is not a setting this server reads".to_string()
    ));
    let warnings = lines.iter().filter(|l| l.starts_with("warning:")).count();
    assert_eq!(warnings, 2, "{lines:?}");
    assert!(lines.contains(&"configuration ok".to_string()));
}

#[test]
fn nothing_is_opened_or_contacted() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = taken.local_addr().unwrap().to_string();
    let started = Instant::now();
    let output = check(&[
        ("PYN_ADDR", &addr),
        ("PYN_DATABASE_URL", "postgres://u:p@127.0.0.1:1/none"),
        ("PYN_DATA_DIR", "/nonexistent/pyn-data"),
        ("PYN_UPDATE_CHECK", "on"),
    ]);
    assert!(output.status.success(), "{}", out(&output));
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn a_policy_file_and_secret_files_are_read_like_the_real_start() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.toml");
    let output = check(&[("PYN_CONFIG", missing.to_str().unwrap())]);
    assert_eq!(problems(&output).len(), 1, "{}", out(&output));
    assert!(problems(&output)[0].starts_with("PYN_CONFIG: "));
    let good = dir.path().join("pyn.toml");
    std::fs::write(&good, "[meta]\ndefault = \"shared\"\n").unwrap();
    let output = check(&[("PYN_CONFIG", good.to_str().unwrap())]);
    assert!(output.status.success(), "{}", out(&output));
    let output = check(&[
        ("PYN_S3_BUCKET", "b"),
        ("PYN_S3_ENDPOINT", "https://s3.example"),
        ("PYN_S3_ACCESS_KEY_ID_FILE", missing.to_str().unwrap()),
        ("PYN_S3_SECRET_ACCESS_KEY", "x"),
    ]);
    assert!(
        problems(&output)[0].starts_with("PYN_S3_ACCESS_KEY_ID_FILE: cannot read"),
        "{}",
        out(&output)
    );
}

#[test]
fn several_copies_need_a_database_and_shared_storage() {
    let output = check(&[("PYN_INSTANCES", "2")]);
    assert!(problems(&output)[0].starts_with("PYN_INSTANCES above 1 needs"));
    let output = check(&[
        ("PYN_INSTANCES", "2"),
        ("PYN_DATABASE_URL", "postgres://u:p@db/pyn"),
        ("PYN_DATA_DIR", "/data"),
    ]);
    assert!(output.status.success(), "{}", out(&output));
}

#[test]
fn the_flag_takes_no_arguments_and_the_help_documents_it() {
    let output = Command::new(env!("CARGO_BIN_EXE_pyn-server"))
        .args(["--check-config", "extra"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let help = Command::new(env!("CARGO_BIN_EXE_pyn-server"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&help.stdout).contains("--check-config"));
}

/// The uncommented `NAME=value` lines of the shipped `.env.example`, as the environment Compose would hand over.
fn example_env() -> Vec<(String, String)> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/.env.example");
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn compose_environment() -> Vec<(String, String)> {
    let mut env = example_env();
    for (k, v) in [
        ("PYN_DATABASE_URL", "postgres://pyn:pw@db:5432/pyn"),
        ("PYN_DATA_DIR", "/data"),
        ("PYN_TRUST_FORWARDED_FOR", "true"),
    ] {
        env.push((k.into(), v.into()));
    }
    env
}

fn check_with(env: &[(String, String)]) -> Output {
    let pairs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    check(&pairs)
}

#[test]
fn the_shipped_env_example_fails_only_on_its_placeholders() {
    let env = compose_environment();
    assert!(env.iter().any(|(k, _)| k == "PYN_SERVER_VERSION"));
    let output = check_with(&env);
    assert_eq!(output.status.code(), Some(1), "{}", out(&output));
    let found = problems(&output);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].starts_with("PYN_SECRET_KEY"), "{found:?}");
    assert!(
        !out(&output).contains("warning:"),
        "compose variables must not be reported: {}",
        out(&output)
    );
}

#[test]
fn the_shipped_env_example_passes_once_the_secret_is_filled_in() {
    let mut env = compose_environment();
    env.retain(|(k, _)| k != "PYN_SECRET_KEY");
    env.push(("PYN_SECRET_KEY".into(), KEY.into()));
    let output = check_with(&env);
    assert!(output.status.success(), "{}", out(&output));
    assert_eq!(lines(&output)[0], "configuration ok");
    assert_eq!(lines(&output).len(), 2, "{}", out(&output));
}

#[test]
fn a_normal_start_warns_about_unknown_variables_and_still_starts() {
    let dir = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let addr = format!("127.0.0.1:{port}");
    let log = dir.path().join("out.log");
    let mut child = Command::new(env!("CARGO_BIN_EXE_pyn-server"))
        .env_clear()
        .env("PYN_ADDR", &addr)
        .env("PYN_REGISTRTION", "open")
        .env("PYN_SERVER_VERSION", "v0.1.0")
        .stdout(File::create(&log).unwrap())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while std::net::TcpStream::connect(&addr).is_err() {
        assert!(Instant::now() < deadline, "no start");
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("PYN_REGISTRTION is not a setting this server reads"));
    assert!(text.contains("did you mean PYN_REGISTRATION?"));
    assert!(!text.contains("PYN_SERVER_VERSION"), "{text}");
}
