//! `pyn login` and sensitive commands with a second factor, against the in-process server.

mod common;

use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use common::{Env, start_with_accounts_and_clock};
use hmac::{Hmac, KeyInit, Mac};
use pyn_core::{Clock, ManualClock};
use pyn_proto as api;
use serde_json::json;
use sha1::Sha1;

const PASSWORD: &str = "correct horse battery";

fn base32_decode(text: &str) -> Vec<u8> {
    let (mut buffer, mut bits, mut out) = (0u32, 0u32, Vec::new());
    for c in text.bytes() {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'2'..=b'7' => c - b'2' + 26,
            other => panic!("not base32: {other}"),
        };
        buffer = (buffer << 5) | u32::from(value);
        bits += 5;
        if bits >= 8 {
            out.push((buffer >> (bits - 8)) as u8);
            bits -= 8;
        }
    }
    out
}

fn app_code(seed: &[u8], unix: i64) -> String {
    let mut mac = <Hmac<Sha1> as KeyInit>::new_from_slice(seed).unwrap();
    mac.update(&((unix / 30) as u64).to_be_bytes());
    let h = mac.finalize().into_bytes();
    let o = (h[19] & 15) as usize;
    let n = (u32::from(h[o] & 127) << 24)
        | (u32::from(h[o + 1]) << 16)
        | (u32::from(h[o + 2]) << 8)
        | u32::from(h[o + 3]);
    format!("{:06}", n % 1_000_000)
}

struct Setup {
    env: Env,
    clock: Arc<ManualClock>,
    seed: Vec<u8>,
    recovery: Vec<String>,
}

impl Setup {
    /// A code for a time step no earlier code used.
    fn next_code(&self) -> String {
        self.clock.advance(Duration::seconds(30));
        app_code(&self.seed, self.clock.now().timestamp())
    }
}

/// An account `alice` with an authenticator app turned on, set up over the API.
fn setup() -> Setup {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2030, 1, 1, 12, 0, 0).unwrap(),
    ));
    let env = start_with_accounts_and_clock(&["alice"], clock.clone());
    let http = reqwest::blocking::Client::new();
    let post = |token: Option<&str>, path: &str, body: serde_json::Value| {
        let mut req = http.post(format!("{}{path}", env.url)).json(&body);
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        let r = req.send().unwrap();
        assert!(r.status().is_success(), "{path}: {}", r.text().unwrap());
        r
    };
    let made: api::CreatedToken = post(
        None,
        "/v1/login",
        json!({"username": "alice", "password": PASSWORD}),
    )
    .json()
    .unwrap();
    let begun: api::TotpEnrolment = post(
        Some(&made.token),
        "/v1/me/two-factor/totp",
        json!({"password": PASSWORD}),
    )
    .json()
    .unwrap();
    let seed = base32_decode(&begun.secret);
    let code = app_code(&seed, clock.now().timestamp());
    let done: api::TotpConfirmed = post(
        Some(&made.token),
        &format!("/v1/me/two-factor/totp/{}/confirm", begun.id),
        json!({"code": code}),
    )
    .json()
    .unwrap();
    Setup {
        env,
        clock,
        seed,
        recovery: done.recovery_codes.unwrap(),
    }
}

fn login(s: &Setup, input: &str) -> std::process::Output {
    let home = s.env.dir("home");
    s.env.run_stdin(
        &home,
        "alice",
        &["login", "alice", "--password-stdin"],
        input,
    )
}

fn text(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn login_asks_for_the_code_and_signs_in() {
    let s = setup();
    let out = login(&s, &format!("{PASSWORD}\n{}\n", s.next_code()));
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("signed in as alice"));
    let home = s.env.dir("home");
    assert!(s.env.run(&home, "alice", &["whoami"]).status.success());
}

#[test]
fn a_wrong_code_is_asked_for_again_a_bounded_number_of_times() {
    let s = setup();
    let good = s.next_code();
    let out = login(&s, &format!("{PASSWORD}\n000000\n{good}\n"));
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("not accepted"));

    let out = login(&s, &format!("{PASSWORD}\n000000\n000001\n000002\n"));
    assert!(!out.status.success());
    assert!(
        text(&out).contains("second_factor_invalid"),
        "{}",
        text(&out)
    );
}

#[test]
fn a_recovery_code_signs_in() {
    let s = setup();
    let out = login(&s, &format!("{PASSWORD}\n{}\n", s.recovery[0]));
    assert!(out.status.success(), "{}", text(&out));
}

#[test]
fn no_code_on_standard_input_fails_without_signing_in() {
    let s = setup();
    let out = login(&s, &format!("{PASSWORD}\n"));
    assert!(!out.status.success());
    assert!(text(&out).contains("no code"), "{}", text(&out));
}

#[test]
fn too_many_wrong_codes_show_the_wait() {
    let s = setup();
    for _ in 0..2 {
        login(&s, &format!("{PASSWORD}\n000000\n000001\n000002\n"));
    }
    let out = login(&s, &format!("{PASSWORD}\n000003\n000004\n000005\n"));
    let said = text(&out);
    assert!(said.contains("too_many_attempts"), "{said}");
    assert!(said.contains("try again in"), "{said}");
}

#[test]
fn a_stale_sign_in_is_checked_again_before_a_sensitive_command() {
    let s = setup();
    let out = login(&s, &format!("{PASSWORD}\n{}\n", s.next_code()));
    assert!(out.status.success(), "{}", text(&out));
    let home = s.env.dir("home");
    let args = [
        "token",
        "create",
        "ci",
        "--permissions",
        "account_ssh_keys:write",
    ];

    let fresh = s.env.run(&home, "alice", &args);
    assert!(fresh.status.success(), "fresh sign-in: {}", text(&fresh));

    s.clock.advance(Duration::minutes(16));
    let none = s.env.run_stdin(&home, "alice", &args, "");
    assert!(!none.status.success());
    assert!(text(&none).contains("no code"), "{}", text(&none));

    let code = s.next_code();
    let out = s.env.run_stdin(&home, "alice", &args, &format!("{code}\n"));
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("second-factor check"));

    let listed = s.env.ok(&home, "alice", &["token", "list"]);
    assert!(listed.contains("ci"), "{listed}");
    let revoke = s
        .env
        .run_stdin(&home, "alice", &["token", "revoke", "x"], "");
    assert!(
        !text(&revoke).contains("fresh_check_required"),
        "{}",
        text(&revoke)
    );
}
