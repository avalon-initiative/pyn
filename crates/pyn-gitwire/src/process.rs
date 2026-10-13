//! A transport that runs `git upload-pack` / `git receive-pack` on a local repository, in both
//! shapes: one duplex pipe, or stateless-rpc (one process per request, as smart HTTP does).
//! It needs the `git` binary and exists to exercise the client; real connections live elsewhere.

use std::ffi::OsString;
use std::io::{self, Cursor, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use crate::caps::ProtocolVersion;
use crate::error::{Error, Result};
use crate::transport::{Service, Session, Transport};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Duplex,
    StatelessRpc,
}

#[derive(Debug, Clone)]
pub struct LocalGit {
    repo: PathBuf,
    shape: Shape,
    git: OsString,
    version: Option<ProtocolVersion>,
}

impl LocalGit {
    pub fn new(repo: impl Into<PathBuf>, shape: Shape) -> Self {
        Self {
            repo: repo.into(),
            shape,
            git: "git".into(),
            version: None,
        }
    }

    /// Requests this protocol version regardless of what the client prefers.
    pub fn force_version(mut self, version: ProtocolVersion) -> Self {
        self.version = Some(version);
        self
    }

    fn command(&self, service: Service, version: ProtocolVersion, extra: &[&str]) -> Command {
        let sub = match service {
            Service::UploadPack => "upload-pack",
            Service::ReceivePack => "receive-pack",
        };
        let mut c = Command::new(&self.git);
        c.args(["-c", "gc.auto=0", "-c", "maintenance.auto=false", sub])
            .args(extra)
            .arg(&self.repo);
        c.env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null");
        match version {
            ProtocolVersion::V0 => c.env_remove("GIT_PROTOCOL"),
            ProtocolVersion::V1 => c.env("GIT_PROTOCOL", "version=1"),
            ProtocolVersion::V2 => c.env("GIT_PROTOCOL", "version=2"),
        };
        c
    }
}

impl Transport for LocalGit {
    fn open(&mut self, service: Service, version: ProtocolVersion) -> Result<Box<dyn Session>> {
        let version = self.version.unwrap_or(version);
        match self.shape {
            Shape::Duplex => {
                let mut cmd = self.command(service, version, &[]);
                cmd.stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null());
                let mut child = cmd.spawn()?;
                let stdin = child.stdin.take();
                let stdout = child.stdout.take().expect("piped stdout");
                Ok(Box::new(DuplexSession {
                    child,
                    stdin,
                    stdout: Some(stdout),
                }))
            }
            Shape::StatelessRpc => Ok(Box::new(StatelessSession {
                transport: self.clone(),
                service,
                version,
                current: None,
            })),
        }
    }
}

struct DuplexSession {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
}

impl Session for DuplexSession {
    fn advertisement(&mut self) -> Result<Box<dyn Read + '_>> {
        Ok(Box::new(self.stdout.as_mut().expect("open")))
    }

    fn send(&mut self, body: &mut dyn Read) -> Result<()> {
        let stdin = self.stdin.as_mut().expect("open");
        io::copy(body, stdin)?;
        Ok(stdin.flush()?)
    }

    fn response(&mut self) -> Result<Box<dyn Read + '_>> {
        Ok(Box::new(self.stdout.as_mut().expect("open")))
    }
}

impl Drop for DuplexSession {
    fn drop(&mut self) {
        self.stdin.take();
        self.stdout.take();
        let _ = self.child.wait();
    }
}

struct StatelessSession {
    transport: LocalGit,
    service: Service,
    version: ProtocolVersion,
    current: Option<ChildOutput>,
}

impl Session for StatelessSession {
    fn advertisement(&mut self) -> Result<Box<dyn Read + '_>> {
        let out = self
            .transport
            .command(
                self.service,
                self.version,
                &["--stateless-rpc", "--advertise-refs"],
            )
            .output()?;
        if out.stdout.is_empty() && !out.status.success() {
            return Err(Error::Remote(
                String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            ));
        }
        Ok(Box::new(Cursor::new(out.stdout)))
    }

    fn send(&mut self, body: &mut dyn Read) -> Result<()> {
        self.current = None;
        let mut data = Vec::new();
        body.read_to_end(&mut data)?;
        let mut cmd = self
            .transport
            .command(self.service, self.version, &["--stateless-rpc"]);
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(&data);
        });
        self.current = Some(ChildOutput {
            child,
            stdout: Some(stdout),
            writer: Some(writer),
        });
        Ok(())
    }

    fn response(&mut self) -> Result<Box<dyn Read + '_>> {
        match self.current.as_mut() {
            Some(c) => Ok(Box::new(c)),
            None => Err(Error::Usage("no request sent".into())),
        }
    }
}

struct ChildOutput {
    child: Child,
    stdout: Option<ChildStdout>,
    writer: Option<std::thread::JoinHandle<()>>,
}

impl Read for &mut ChildOutput {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stdout.as_mut().expect("open").read(buf)
    }
}

impl Drop for ChildOutput {
    fn drop(&mut self) {
        self.stdout.take();
        if let Some(w) = self.writer.take() {
            let _ = w.join();
        }
        let _ = self.child.wait();
    }
}
