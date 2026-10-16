//! Writes an executable shell script that can be run at once.
//! A concurrent fork may still hold the write descriptor, which fails `exec` with ETXTBSY (os error 26).
#![allow(dead_code)]

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Writes `#!/bin/sh` plus `body` to `path`, then waits until the file can be executed.
pub fn write_script(path: &Path, body: &str) {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o755)
        .open(path)
        .unwrap();
    write!(
        f,
        "#!/bin/sh\n[ -n \"$PYN_SCRIPT_PROBE\" ] && exit 0\n{body}"
    )
    .unwrap();
    f.sync_all().unwrap();
    drop(f);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match Command::new(path)
            .env("PYN_SCRIPT_PROBE", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
        {
            Err(e) if e.raw_os_error() == Some(26) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            r => {
                assert!(r.unwrap().success());
                return;
            }
        }
    }
}
