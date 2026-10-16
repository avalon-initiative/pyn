//! Git over the system `ssh`: the remote runs `git-upload-pack` or `git-receive-pack` and the pipe carries the protocol.

use std::io::{self, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, ExitStatus, Stdio};

use pyn_git::Config;
use pyn_gitwire::{Error as WireError, ProtocolVersion, Result, Service, Session, Transport};

use crate::env::Env;
use crate::url::SshUrl;

/// How the ssh client takes a port and options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    /// OpenSSH and compatibles: `-p`, `-o`.
    Ssh,
    /// Host and command only.
    Simple,
    /// PuTTY's plink: `-P`.
    Plink,
    /// TortoiseGit's plink: `-P` and `-batch`.
    TortoisePlink,
}

impl Variant {
    fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "ssh" => Some(Self::Ssh),
            "simple" => Some(Self::Simple),
            "plink" | "putty" => Some(Self::Plink),
            "tortoiseplink" => Some(Self::TortoisePlink),
            _ => None,
        }
    }

    fn from_program(program: &str) -> Option<Self> {
        let first = program.split_whitespace().next()?.trim_matches(['\'', '"']);
        let base = first.rsplit('/').next()?;
        let base = base.strip_suffix(".exe").unwrap_or(base);
        Self::from_name(base)
    }
}

/// Bound the connect and detect a dead peer (about two minutes without a reply) on the default client.
const LIVENESS_OPTIONS: [&str; 3] = [
    "ConnectTimeout=30",
    "ServerAliveInterval=30",
    "ServerAliveCountMax=4",
];

#[derive(Debug, Clone, PartialEq, Eq)]
enum Program {
    /// `ssh` from `PATH`.
    Default,
    /// `GIT_SSH`: run as is.
    Direct(String),
    /// `GIT_SSH_COMMAND` or `core.sshCommand`: a shell command line.
    Shell(String),
}

#[derive(Debug, Clone)]
pub struct SshTransport {
    url: SshUrl,
    program: Program,
    variant: Variant,
    env: Env,
}

impl SshTransport {
    /// Picks the ssh client: `GIT_SSH_COMMAND`, `core.sshCommand`, `GIT_SSH`, then `ssh`.
    pub fn new(url: SshUrl, config: &Config, env: &Env) -> Self {
        let program = if let Some(c) = env.non_empty("GIT_SSH_COMMAND") {
            Program::Shell(c.to_owned())
        } else if let Some(c) = config.get("core.sshcommand").filter(|c| !c.is_empty()) {
            Program::Shell(c.to_owned())
        } else if let Some(p) = env.non_empty("GIT_SSH") {
            Program::Direct(p.to_owned())
        } else {
            Program::Default
        };
        let named = match &program {
            Program::Default => Some("ssh"),
            Program::Direct(p) | Program::Shell(p) => Some(p.as_str()),
        };
        let variant = env
            .non_empty("GIT_SSH_VARIANT")
            .and_then(Variant::from_name)
            .or_else(|| config.get("ssh.variant").and_then(Variant::from_name))
            .or_else(|| named.and_then(Variant::from_program))
            .unwrap_or(Variant::Ssh);
        Self {
            url,
            program,
            variant,
            env: env.clone(),
        }
    }

    pub fn variant(&self) -> Variant {
        self.variant
    }

    /// The arguments after the program: options, `[user@]host`, then the remote command.
    pub fn args(&self, service: Service, version: ProtocolVersion) -> Result<Vec<String>> {
        let mut args = Vec::new();
        let u = &self.url;
        match self.variant {
            Variant::Ssh => {
                if !matches!(self.program, Program::Shell(_)) {
                    for o in LIVENESS_OPTIONS {
                        args.extend(["-o".into(), o.into()]);
                    }
                }
                if version != ProtocolVersion::V0 {
                    args.extend(["-o".into(), "SendEnv=GIT_PROTOCOL".into()]);
                }
                if let Some(p) = u.port {
                    args.extend(["-p".into(), p.to_string()]);
                }
                args.push("--".into());
            }
            Variant::Plink | Variant::TortoisePlink => {
                if self.variant == Variant::TortoisePlink {
                    args.push("-batch".into());
                }
                if let Some(p) = u.port {
                    args.extend(["-P".into(), p.to_string()]);
                }
            }
            Variant::Simple => {
                if u.port.is_some() {
                    return Err(WireError::Usage(
                        "the 'simple' ssh variant cannot set a port; use ssh.variant or GIT_SSH_VARIANT with a client that can"
                            .into(),
                    ));
                }
            }
        }
        let host = match &u.user {
            Some(user) => format!("{user}@{}", u.host),
            None => u.host.clone(),
        };
        args.push(host);
        args.push(remote_command(service, &u.path));
        Ok(args)
    }

    fn command(&self, args: &[String]) -> std::process::Command {
        let mut c = match &self.program {
            Program::Default => self.env.command("ssh"),
            Program::Direct(p) => self.env.command(p),
            Program::Shell(cmd) => {
                let mut c = self.env.command("sh");
                c.arg("-c").arg(format!("{cmd} \"$@\"")).arg(cmd);
                c
            }
        };
        c.args(args);
        c
    }
}

/// `git-upload-pack '<path>'` with the path quoted for the remote shell.
pub fn remote_command(service: Service, path: &str) -> String {
    format!("{} {}", service.name(), shell_quote(path))
}

/// Single-quotes `s`; a quote or `!` inside is closed, escaped and reopened (git's `sq_quote`).
pub fn shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\'' | '!' => {
                out.push_str("'\\");
                out.push(c);
                out.push('\'');
            }
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

impl Transport for SshTransport {
    fn open(&mut self, service: Service, version: ProtocolVersion) -> Result<Box<dyn Session>> {
        let args = self.args(service, version)?;
        let mut cmd = self.command(&args);
        match version {
            ProtocolVersion::V0 => cmd.env_remove("GIT_PROTOCOL"),
            ProtocolVersion::V1 => cmd.env("GIT_PROTOCOL", "version=1"),
            ProtocolVersion::V2 => cmd.env("GIT_PROTOCOL", "version=2"),
        };
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = cmd.spawn().map_err(|e| self.spawn_error(&e))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        Ok(Box::new(SshSession {
            child,
            stdin,
            stdout,
            target: crate::url::RemoteUrl::Ssh(self.url.clone()).display(),
            service,
            shell: matches!(self.program, Program::Shell(_)),
            sent: false,
        }))
    }
}

impl SshTransport {
    fn spawn_error(&self, e: &io::Error) -> WireError {
        let named = match &self.program {
            Program::Default => "ssh",
            Program::Direct(p) | Program::Shell(p) => p,
        };
        if e.kind() == io::ErrorKind::NotFound {
            return WireError::Connection(format!(
                "cannot run '{named}': it was not found on PATH. ssh remotes use the system ssh client; \
                 install OpenSSH, or set GIT_SSH_COMMAND (or core.sshCommand) to another client"
            ));
        }
        WireError::Connection(format!("cannot run '{named}': {e}"))
    }
}

struct SshSession {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    target: String,
    service: Service,
    shell: bool,
    sent: bool,
}

impl SshSession {
    /// The typed error for an ssh that ended badly, from its exit status.
    fn exit_error(&self, status: ExitStatus) -> WireError {
        let to = &self.target;
        match status.code() {
            Some(255) => WireError::Connection(format!(
                "ssh could not connect to or sign in at {to} (exit status 255); see the ssh message above"
            )),
            Some(127) if self.shell => WireError::Connection(format!(
                "exit status 127 for {to}: the ssh command was not found, or the remote has no {}; \
                 the remote needs git installed to serve a repository over ssh",
                self.service.name()
            )),
            Some(127) => WireError::Connection(format!(
                "{} was not found on the remote ({to}, exit status 127); the remote needs git installed to serve a repository over ssh",
                self.service.name()
            )),
            Some(code) => WireError::Connection(format!(
                "the remote {} at {to} failed (exit status {code}); see the message above",
                self.service.name()
            )),
            None => WireError::Connection(format!("ssh to {to} was terminated by a signal")),
        }
    }

    fn closed(&mut self) -> io::Error {
        match self.child.wait() {
            Ok(status) if !status.success() => self.exit_error(status).into(),
            _ => io::Error::new(io::ErrorKind::BrokenPipe, "the ssh connection closed"),
        }
    }
}

struct Out<'a>(&'a mut SshSession);

impl Read for Out<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let s = &mut *self.0;
        let n = s.stdout.as_mut().expect("open").read(buf)?;
        if n == 0 && !buf.is_empty() {
            let status = s.child.wait()?;
            if !status.success() {
                return Err(s.exit_error(status).into());
            }
        }
        Ok(n)
    }
}

impl Session for SshSession {
    fn advertisement(&mut self) -> Result<Box<dyn Read + '_>> {
        Ok(Box::new(Out(self)))
    }

    fn send(&mut self, body: &mut dyn Read) -> Result<()> {
        self.sent = true;
        let stdin = self.stdin.as_mut().expect("open");
        match io::copy(body, stdin).and_then(|_| stdin.flush()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Err(self.closed().into()),
            Err(e) => Err(e.into()),
        }
    }

    fn response(&mut self) -> Result<Box<dyn Read + '_>> {
        Ok(Box::new(Out(self)))
    }
}

impl Drop for SshSession {
    /// Without a request the server waits for one: a flush tells it there is none.
    fn drop(&mut self) {
        if let (false, Some(stdin)) = (self.sent, self.stdin.as_mut()) {
            let _ = stdin.write_all(b"0000");
        }
        self.stdin.take();
        self.stdout.take();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::url::RemoteUrl;

    fn url(s: &str) -> SshUrl {
        match RemoteUrl::parse(s).unwrap() {
            RemoteUrl::Ssh(u) => u,
            other => panic!("{other:?}"),
        }
    }

    fn transport(s: &str, env: &Env, config: &str) -> SshTransport {
        SshTransport::new(url(s), &Config::parse(config.as_bytes()).unwrap(), env)
    }

    #[test]
    fn quoting_survives_a_shell() {
        assert_eq!(shell_quote("/srv/r.git"), "'/srv/r.git'");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote("a!b"), "'a'\\!'b'");
        assert_eq!(
            remote_command(Service::ReceivePack, "x;rm -rf ~"),
            "git-receive-pack 'x;rm -rf ~'"
        );
    }

    #[test]
    fn openssh_arguments() {
        let t = transport("ssh://git@host:2222/srv/r.git", &Env::default(), "");
        assert_eq!(t.variant(), Variant::Ssh);
        assert_eq!(
            t.args(Service::UploadPack, ProtocolVersion::V2).unwrap(),
            [
                "-o",
                "ConnectTimeout=30",
                "-o",
                "ServerAliveInterval=30",
                "-o",
                "ServerAliveCountMax=4",
                "-o",
                "SendEnv=GIT_PROTOCOL",
                "-p",
                "2222",
                "--",
                "git@host",
                "git-upload-pack '/srv/r.git'"
            ]
        );
        assert_eq!(
            t.args(Service::ReceivePack, ProtocolVersion::V0).unwrap(),
            [
                "-o",
                "ConnectTimeout=30",
                "-o",
                "ServerAliveInterval=30",
                "-o",
                "ServerAliveCountMax=4",
                "-p",
                "2222",
                "--",
                "git@host",
                "git-receive-pack '/srv/r.git'"
            ]
        );
        let t = transport("host:r.git", &Env::default(), "");
        assert_eq!(
            t.args(Service::UploadPack, ProtocolVersion::V0).unwrap(),
            [
                "-o",
                "ConnectTimeout=30",
                "-o",
                "ServerAliveInterval=30",
                "-o",
                "ServerAliveCountMax=4",
                "--",
                "host",
                "git-upload-pack 'r.git'"
            ]
        );
    }

    #[test]
    fn other_variants() {
        let env = Env::default().with("GIT_SSH_VARIANT", "plink");
        let t = transport("ssh://h:22/p", &env, "");
        assert_eq!(
            t.args(Service::UploadPack, ProtocolVersion::V2).unwrap(),
            ["-P", "22", "h", "git-upload-pack '/p'"]
        );
        let env = Env::default().with("GIT_SSH", "/opt/TortoisePlink.exe");
        let t = transport("ssh://h:22/p", &env, "");
        assert_eq!(t.variant(), Variant::TortoisePlink);
        assert_eq!(
            t.args(Service::UploadPack, ProtocolVersion::V0).unwrap(),
            ["-batch", "-P", "22", "h", "git-upload-pack '/p'"]
        );
        let env = Env::default().with("GIT_SSH_VARIANT", "simple");
        let t = transport("ssh://h/p", &env, "");
        assert_eq!(
            t.args(Service::UploadPack, ProtocolVersion::V2).unwrap(),
            ["h", "git-upload-pack '/p'"]
        );
        let t = transport("ssh://h:2/p", &env, "");
        assert!(t.args(Service::UploadPack, ProtocolVersion::V2).is_err());
    }

    #[test]
    fn program_and_variant_precedence() {
        let t = transport("h:p", &Env::default(), "");
        assert_eq!(t.program, Program::Default);
        let env = Env::default().with("GIT_SSH", "/bin/mine");
        assert_eq!(
            transport("h:p", &env, "").program,
            Program::Direct("/bin/mine".into())
        );
        let config = "[core]\n\tsshCommand = ssh -i key\n";
        assert_eq!(
            transport("h:p", &env, config).program,
            Program::Shell("ssh -i key".into())
        );
        let env = env.with("GIT_SSH_COMMAND", "/usr/bin/plink -batch");
        let t = transport("h:p", &env, config);
        assert_eq!(t.program, Program::Shell("/usr/bin/plink -batch".into()));
        assert_eq!(t.variant(), Variant::Plink);
        let t = transport("h:p", &Env::default(), "[ssh]\n\tvariant = simple\n");
        assert_eq!(t.variant(), Variant::Simple);
        let env = Env::default().with("GIT_SSH_COMMAND", "my-wrapper");
        assert_eq!(transport("h:p", &env, "").variant(), Variant::Ssh);
    }

    #[test]
    fn a_missing_client_is_actionable() {
        let env = Env::default().with("PATH", "/nonexistent");
        let mut t = transport("h:p", &env, "");
        let Err(e) = t.open(Service::UploadPack, ProtocolVersion::V2) else {
            panic!("expected an error")
        };
        let msg = e.to_string();
        assert!(
            msg.contains("ssh") && msg.contains("GIT_SSH_COMMAND"),
            "{msg}"
        );
    }
}
