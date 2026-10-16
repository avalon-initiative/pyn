//! Credential helpers: `store` in process, everything else through git's helper protocol on stdin and stdout.

use std::io::{Read, Write};
use std::process::Stdio;

use super::Credential;
use super::store::StoreFiles;
use crate::env::Env;
use crate::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Get,
    Store,
    Erase,
}

impl Op {
    fn name(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::Store => "store",
            Self::Erase => "erase",
        }
    }
}

pub enum Helper {
    Store(StoreFiles),
    /// A command line the shell runs with the operation appended.
    Command(String),
}

/// The helper a `credential.helper` value names, or `None` when it names a program that is not installed.
pub fn resolve(value: &str, env: &Env) -> Option<Helper> {
    let value = value.trim();
    if let Some(cmd) = value.strip_prefix('!') {
        return Some(Helper::Command(cmd.to_owned()));
    }
    if value.starts_with('/') {
        return Some(Helper::Command(value.to_owned()));
    }
    let (name, args) = value.split_once(char::is_whitespace).unwrap_or((value, ""));
    if name == "store" {
        return Some(Helper::Store(StoreFiles::new(
            store_file_arg(args).as_deref(),
            env,
        )));
    }
    let program = format!("git-credential-{name}");
    env.find_program(&program)?;
    Some(Helper::Command(
        format!("{program} {args}").trim_end().to_owned(),
    ))
}

fn store_file_arg(args: &str) -> Option<String> {
    let mut words = args.split_whitespace();
    while let Some(w) = words.next() {
        if w == "--file" {
            return words.next().map(|p| p.trim_matches(['\'', '"']).to_owned());
        }
        if let Some(p) = w.strip_prefix("--file=") {
            return Some(p.trim_matches(['\'', '"']).to_owned());
        }
    }
    None
}

/// Runs one operation. A `get` fills `cred`; the return value is whether the helper asked to stop.
/// A helper that fails is ignored, as git does; a control character in either direction is an error.
pub fn run(helper: &Helper, op: Op, cred: &mut Credential, env: &Env) -> Result<bool> {
    cred.validate()?;
    match helper {
        Helper::Store(files) => {
            match op {
                Op::Get => {
                    if let Some(found) = files.get(cred) {
                        found.validate()?;
                        *cred = found;
                    }
                }
                Op::Store => {
                    let _ = files.store(cred);
                }
                Op::Erase => {
                    let _ = files.erase(cred);
                }
            }
            Ok(false)
        }
        Helper::Command(cmd) => run_command(cmd, op, cred, env),
    }
}

fn run_command(cmd: &str, op: Op, cred: &mut Credential, env: &Env) -> Result<bool> {
    let lines = cred.to_lines()?;
    let mut child = match env
        .command("sh")
        .arg("-c")
        .arg(format!("{cmd} {}", op.name()))
        .stdin(Stdio::piped())
        .stdout(if op == Op::Get {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return Ok(false),
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(lines.as_bytes());
    }
    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut out);
    }
    let _ = child.wait();
    if op == Op::Get {
        return cred.read_lines(&out);
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_values_resolve_by_kind() {
        let env = Env::default().with("PATH", "/nonexistent");
        assert!(matches!(resolve("!echo hi", &env), Some(Helper::Command(c)) if c == "echo hi"));
        assert!(
            matches!(resolve("/opt/h --x", &env), Some(Helper::Command(c)) if c == "/opt/h --x")
        );
        assert!(matches!(resolve("store", &env), Some(Helper::Store(_))));
        assert!(resolve("nosuch", &env).is_none());
        assert!(resolve("cache --timeout 60", &env).is_none());
    }

    #[test]
    fn store_takes_a_file_argument() {
        let env = Env::default().with("HOME", "/h");
        for v in ["store --file /tmp/c", "store --file=/tmp/c"] {
            let Some(Helper::Store(f)) = resolve(v, &env) else {
                panic!("{v}")
            };
            assert_eq!(f.paths(), [std::path::PathBuf::from("/tmp/c")]);
        }
    }

    #[cfg(unix)]
    #[test]
    fn external_programs_are_found_on_path_and_speak_the_protocol() {
        let tmp = tempfile::tempdir().unwrap();
        let prog = tmp.path().join("git-credential-fake");
        let log = tmp.path().join("log");
        crate::script::write_script(
            &prog,
            &format!(
                "for a in \"$@\"; do op=$a; done\ncat > '{log}.in'\necho \"$op\" >> '{log}'\nif [ \"$op\" = get ]; then printf 'username=u1\\npassword=p1\\nquit=1\\n'; fi\n",
                log = log.display()
            ),
        );
        let env = Env::default().with("PATH", &format!("{}:/usr/bin:/bin", tmp.path().display()));
        let h = resolve("fake --opt", &env).unwrap();
        let mut c = Credential {
            protocol: "https".into(),
            host: "h.example".into(),
            ..Credential::default()
        };
        assert!(run(&h, Op::Get, &mut c, &env).unwrap());
        assert_eq!(
            (c.username.as_deref(), c.password.as_deref()),
            (Some("u1"), Some("p1"))
        );
        assert_eq!(
            std::fs::read_to_string(format!("{}.in", log.display())).unwrap(),
            "protocol=https\nhost=h.example\n\n"
        );
        run(&h, Op::Store, &mut c, &env).unwrap();
        run(&h, Op::Erase, &mut c, &env).unwrap();
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            "get\nstore\nerase\n"
        );
        assert_eq!(
            std::fs::read_to_string(format!("{}.in", log.display())).unwrap(),
            "protocol=https\nhost=h.example\nusername=u1\npassword=p1\n\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn values_with_control_characters_never_reach_or_come_from_a_helper() {
        let env = Env::default().with("PATH", "/usr/bin:/bin");
        let tmp = tempfile::tempdir().unwrap();
        let seen = tmp.path().join("seen");
        let h = resolve(&format!("!cat > '{}'", seen.display()), &env).unwrap();
        let mut c = Credential {
            protocol: "https".into(),
            host: "h".into(),
            username: Some("u\nhost=other".into()),
            ..Credential::default()
        };
        assert!(run(&h, Op::Store, &mut c, &env).is_err());
        assert!(!seen.exists(), "the helper was not started");
        let h = resolve("!printf 'username=u\\npassword=a\\rb\\n'", &env).unwrap();
        let mut c = Credential::default();
        assert!(run(&h, Op::Get, &mut c, &env).is_err());
        assert!(c.username.is_none() && c.password.is_none());
    }

    #[test]
    fn a_shell_helper_and_a_failing_helper() {
        let env = Env::default().with("PATH", "/usr/bin:/bin");
        let h = resolve("!f() { echo username=bob; echo password=pw; }; f", &env).unwrap();
        let mut c = Credential::default();
        run(&h, Op::Get, &mut c, &env).unwrap();
        assert_eq!(c.username.as_deref(), Some("bob"));
        let h = resolve("!exit 3; echo", &env).unwrap();
        let mut c = Credential::default();
        run(&h, Op::Get, &mut c, &env).unwrap();
        assert!(c.username.is_none());
    }
}
