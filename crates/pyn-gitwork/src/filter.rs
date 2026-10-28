//! Filter drivers: `filter.<name>.clean`, `smudge`, `process` and `required`, run as git runs them, plus pyn's own
//! built-in `lfs` driver. A driver's commands are the user's own programs, started through `sh -c`.

use std::collections::HashMap;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;

use pyn_git::Config;

use crate::error::{Error, Result};
use crate::lfs::Store;

/// The driver a `filter=<name>` attribute selects.
#[derive(Debug, Clone)]
pub enum Driver {
    External(External),
    Lfs { required: bool },
}

#[derive(Debug, Clone, Default)]
pub struct External {
    pub name: String,
    pub clean: Option<String>,
    pub smudge: Option<String>,
    pub process: Option<String>,
    pub required: bool,
}

impl Driver {
    /// The driver named `name`: the built-in one for `lfs`, else one configured under `filter.<name>.*`.
    pub fn from_config(config: &Config, name: &str) -> Option<Self> {
        let key = |k: &str| format!("filter.{name}.{k}");
        let cmd = |k: &str| {
            config
                .get(&key(k))
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        let required = config
            .get_bool(&key("required"))
            .ok()
            .flatten()
            .unwrap_or(false);
        if name == "lfs" {
            return Some(Self::Lfs { required });
        }
        let known = ["clean", "smudge", "process", "required"]
            .iter()
            .any(|k| config.get(&key(k)).is_some());
        known.then(|| {
            Self::External(External {
                name: name.to_owned(),
                clean: cmd("clean"),
                smudge: cmd("smudge"),
                process: cmd("process"),
                required,
            })
        })
    }

    pub fn name(&self) -> &str {
        match self {
            Self::External(e) => &e.name,
            Self::Lfs { .. } => "lfs",
        }
    }

    pub fn required(&self) -> bool {
        match self {
            Self::External(e) => e.required,
            Self::Lfs { required } => *required,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Clean,
    Smudge,
}

impl Op {
    fn name(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Smudge => "smudge",
        }
    }
}

/// The running filters of one repository: the work tree to run them in, the LFS store and the long-running processes.
pub struct Filters {
    root: Option<PathBuf>,
    lfs: Store,
    procs: Mutex<HashMap<String, Slot>>,
}

impl std::fmt::Debug for Filters {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Filters").finish_non_exhaustive()
    }
}

enum Slot {
    Running(Process),
    Aborted,
}

impl Filters {
    pub fn new(root: Option<&Path>, common_dir: &Path) -> Self {
        Self {
            root: root.map(Path::to_path_buf),
            lfs: Store::new(common_dir),
            procs: Mutex::new(HashMap::new()),
        }
    }

    pub fn lfs(&self) -> &Store {
        &self.lfs
    }

    /// Applies the driver. `Ok(None)` means nothing was filtered (no command, or a failure the driver may ignore);
    /// a failure of a `required` driver is an error.
    pub fn apply(
        &self,
        driver: &Driver,
        op: Op,
        path: &[u8],
        data: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        let out = match driver {
            Driver::Lfs { .. } => {
                let r = match op {
                    Op::Clean => self.lfs.clean(data),
                    Op::Smudge => self.lfs.smudge(data),
                };
                match r {
                    Ok(v) => Some(v),
                    Err(e) => {
                        eprintln!("error: {e}");
                        None
                    }
                }
            }
            Driver::External(e) => self.external(e, op, path, data),
        };
        if out.is_none() && driver.required() {
            return Err(Error::Invalid(format!(
                "{}: {} filter '{}' failed",
                String::from_utf8_lossy(path),
                op.name(),
                driver.name()
            )));
        }
        Ok(out)
    }

    fn external(&self, e: &External, op: Op, path: &[u8], data: &[u8]) -> Option<Vec<u8>> {
        if let Some(process) = &e.process {
            return self.through_process(process, op, path, data);
        }
        let cmd = match op {
            Op::Clean => e.clean.as_ref(),
            Op::Smudge => e.smudge.as_ref(),
        }?;
        match run_once(cmd, path, self.root.as_deref(), data) {
            Ok(out) => Some(out),
            Err(why) => {
                eprintln!("error: external filter '{cmd}' failed{why}");
                None
            }
        }
    }

    fn through_process(&self, cmd: &str, op: Op, path: &[u8], data: &[u8]) -> Option<Vec<u8>> {
        let mut procs = self.procs.lock().unwrap_or_else(|e| e.into_inner());
        if !procs.contains_key(cmd) {
            match Process::start(cmd, self.root.as_deref()) {
                Ok(p) => procs.insert(cmd.to_owned(), Slot::Running(p)),
                Err(why) => {
                    eprintln!("error: initialization for subprocess '{cmd}' failed: {why}");
                    return None;
                }
            };
        }
        let Some(Slot::Running(proc)) = procs.get_mut(cmd) else {
            return None;
        };
        if !proc.supports(op) {
            return None;
        }
        match proc.filter(op, path, data) {
            Ok(Reply::Done(out)) => Some(out),
            Ok(Reply::Error) => {
                eprintln!("error: external filter '{cmd}' failed");
                None
            }
            Ok(Reply::Abort) => {
                procs.insert(cmd.to_owned(), Slot::Aborted);
                eprintln!("error: external filter '{cmd}' failed");
                None
            }
            Err(why) => {
                procs.remove(cmd);
                eprintln!("error: external filter '{cmd}' failed: {why}");
                None
            }
        }
    }
}

fn sq_quote(path: &[u8]) -> String {
    let mut out = String::from("'");
    for c in String::from_utf8_lossy(path).chars() {
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

/// Expands `%f` to the quoted path and `%%` to `%`.
fn expand(cmd: &str, path: &[u8]) -> String {
    let mut out = String::new();
    let mut chars = cmd.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, chars.peek()) {
            ('%', Some('f')) => {
                chars.next();
                out.push_str(&sq_quote(path));
            }
            ('%', Some('%')) => {
                chars.next();
                out.push('%');
            }
            _ => out.push(c),
        }
    }
    out
}

fn shell(cmd: &str, root: Option<&Path>) -> Command {
    let mut c = Command::new("sh");
    c.arg("-c").arg(cmd).stderr(Stdio::inherit());
    if let Some(root) = root {
        c.current_dir(root);
    }
    c
}

/// Runs one command over `data`; the error is the text to append to the diagnostic.
fn run_once(
    cmd: &str,
    path: &[u8],
    root: Option<&Path>,
    data: &[u8],
) -> std::result::Result<Vec<u8>, String> {
    let mut child = shell(&expand(cmd, path), root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!(": {e}"))?;
    let mut stdin = child.stdin.take().ok_or_else(String::new)?;
    let mut stdout = child.stdout.take().ok_or_else(String::new)?;
    let (written, out) = std::thread::scope(|s| {
        let writer = s.spawn(move || stdin.write_all(data));
        let mut out = Vec::new();
        let read = stdout.read_to_end(&mut out);
        (writer.join().ok().and_then(|r| r.err()), read.map(|_| out))
    });
    let status = child.wait().map_err(|e| format!(": {e}"))?;
    if !status.success() {
        return Err(status.code().map(|c| format!(" {c}")).unwrap_or_default());
    }
    if written.is_some() {
        return Err(": cannot feed the input".into());
    }
    out.map_err(|e| format!(": {e}"))
}

const MAX_PACKET: usize = 65516;

enum Reply {
    Done(Vec<u8>),
    Error,
    Abort,
}

struct Process {
    child: Child,
    to: Option<BufWriter<ChildStdin>>,
    from: BufReader<ChildStdout>,
    clean: bool,
    smudge: bool,
}

type Io<T> = std::io::Result<T>;

fn bad(msg: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, msg.to_owned())
}

fn write_packet(w: &mut impl Write, data: &[u8]) -> Io<()> {
    write!(w, "{:04x}", data.len() + 4)?;
    w.write_all(data)
}

fn write_flush(w: &mut impl Write) -> Io<()> {
    w.write_all(b"0000")
}

/// One packet: `None` is a flush.
fn read_packet(r: &mut impl Read) -> Io<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let n = std::str::from_utf8(&len)
        .ok()
        .and_then(|s| usize::from_str_radix(s, 16).ok())
        .ok_or_else(|| bad("bad packet length"))?;
    match n {
        0 => Ok(None),
        1..=3 => Err(bad("bad packet length")),
        _ => {
            let mut buf = vec![0u8; n - 4];
            r.read_exact(&mut buf)?;
            Ok(Some(buf))
        }
    }
}

fn text(mut packet: Vec<u8>) -> String {
    if packet.last() == Some(&b'\n') {
        packet.pop();
    }
    String::from_utf8_lossy(&packet).into_owned()
}

fn read_list(r: &mut impl Read) -> Io<Vec<String>> {
    let mut lines = Vec::new();
    while let Some(p) = read_packet(r)? {
        lines.push(text(p));
    }
    Ok(lines)
}

fn read_content(r: &mut impl Read) -> Io<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(p) = read_packet(r)? {
        out.extend_from_slice(&p);
    }
    Ok(out)
}

fn status_of(lines: &[String]) -> Option<&str> {
    lines.iter().find_map(|l| l.strip_prefix("status="))
}

impl Process {
    fn start(cmd: &str, root: Option<&Path>) -> Io<Self> {
        let mut child = shell(cmd, root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let to = child.stdin.take().ok_or_else(|| bad("no stdin"))?;
        let from = child.stdout.take().ok_or_else(|| bad("no stdout"))?;
        let mut p = Self {
            child,
            to: Some(BufWriter::new(to)),
            from: BufReader::new(from),
            clean: false,
            smudge: false,
        };
        p.handshake()?;
        Ok(p)
    }

    fn out(&mut self) -> Io<&mut BufWriter<ChildStdin>> {
        self.to.as_mut().ok_or_else(|| bad("process closed"))
    }

    fn handshake(&mut self) -> Io<()> {
        let w = self.out()?;
        write_packet(w, b"git-filter-client\n")?;
        write_packet(w, b"version=2\n")?;
        write_flush(w)?;
        w.flush()?;
        let hello = read_list(&mut self.from)?;
        if hello.first().map(String::as_str) != Some("git-filter-server")
            || !hello.iter().any(|l| l == "version=2")
        {
            return Err(bad("unsupported filter protocol"));
        }
        let w = self.out()?;
        write_packet(w, b"capability=clean\n")?;
        write_packet(w, b"capability=smudge\n")?;
        write_flush(w)?;
        w.flush()?;
        for line in read_list(&mut self.from)? {
            match line.as_str() {
                "capability=clean" => self.clean = true,
                "capability=smudge" => self.smudge = true,
                _ => {}
            }
        }
        Ok(())
    }

    fn supports(&self, op: Op) -> bool {
        match op {
            Op::Clean => self.clean,
            Op::Smudge => self.smudge,
        }
    }

    fn filter(&mut self, op: Op, path: &[u8], data: &[u8]) -> Io<Reply> {
        let w = self.out()?;
        write_packet(w, format!("command={}\n", op.name()).as_bytes())?;
        write_packet(w, &[b"pathname=".as_slice(), path, b"\n"].concat())?;
        write_flush(w)?;
        for chunk in data.chunks(MAX_PACKET) {
            write_packet(w, chunk)?;
        }
        write_flush(w)?;
        w.flush()?;
        let head = read_list(&mut self.from)?;
        match status_of(&head) {
            Some("success") => {}
            Some("abort") => return Ok(Reply::Abort),
            _ => return Ok(Reply::Error),
        }
        let content = read_content(&mut self.from)?;
        let tail = read_list(&mut self.from)?;
        match status_of(&tail) {
            None | Some("success") => Ok(Reply::Done(content)),
            Some("abort") => Ok(Reply::Abort),
            Some(_) => Ok(Reply::Error),
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.to = None;
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_f_is_quoted_and_percent_percent_is_literal() {
        assert_eq!(expand("x %f 100%%", b"a b"), "x 'a b' 100%");
        assert_eq!(expand("%f", b"it's!"), "'it'\\''s'\\!''");
        assert_eq!(expand("%x", b"a"), "%x");
    }

    #[test]
    fn packets_round_trip() {
        let mut buf = Vec::new();
        write_packet(&mut buf, b"abc\n").unwrap();
        write_flush(&mut buf).unwrap();
        let mut r = &buf[..];
        assert_eq!(read_packet(&mut r).unwrap(), Some(b"abc\n".to_vec()));
        assert_eq!(read_packet(&mut r).unwrap(), None);
    }

    #[test]
    fn driver_exists_only_when_configured() {
        let c = Config::parse(b"[filter \"a\"]\n\tclean = cat\n\trequired\n").unwrap();
        assert!(matches!(Driver::from_config(&c, "a"), Some(Driver::External(e)) if e.required));
        assert!(Driver::from_config(&c, "b").is_none());
        assert!(matches!(
            Driver::from_config(&c, "lfs"),
            Some(Driver::Lfs { .. })
        ));
    }
}
