use std::collections::BTreeMap;
use std::ffi::OsStr;

/// The environment a connection reads and its child processes get, so tests need not touch the process's own.
#[derive(Debug, Clone, Default)]
pub struct Env {
    vars: BTreeMap<String, String>,
}

impl Env {
    pub fn process() -> Self {
        Self {
            vars: std::env::vars_os()
                .map(|(k, v)| {
                    (
                        k.to_string_lossy().into_owned(),
                        v.to_string_lossy().into_owned(),
                    )
                })
                .collect(),
        }
    }

    pub fn with(mut self, key: &str, value: &str) -> Self {
        self.vars.insert(key.to_owned(), value.to_owned());
        self
    }

    pub fn without(mut self, key: &str) -> Self {
        self.vars.remove(key);
        self
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(String::as_str)
    }

    /// The value when it is set and not empty.
    pub fn non_empty(&self, key: &str) -> Option<&str> {
        self.get(key).filter(|v| !v.is_empty())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&OsStr, &OsStr)> {
        self.vars
            .iter()
            .map(|(k, v)| (OsStr::new(k), OsStr::new(v)))
    }

    /// A command that sees exactly this environment.
    pub fn command(&self, program: impl AsRef<OsStr>) -> std::process::Command {
        let mut c = std::process::Command::new(program);
        c.env_clear().envs(self.iter());
        c
    }

    /// The first directory on `PATH` that holds an executable file called `name`.
    pub fn find_program(&self, name: &str) -> Option<std::path::PathBuf> {
        self.get("PATH")?
            .split(':')
            .filter(|d| !d.is_empty())
            .map(|d| std::path::Path::new(d).join(name))
            .find(|p| is_executable(p))
    }
}

#[cfg(unix)]
fn is_executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &std::path::Path) -> bool {
    p.is_file()
}
