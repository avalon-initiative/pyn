//! A reader that goes away early (`pyn log | head`) ends the command quietly with status 0, on every command.

use std::io::{ErrorKind, Write};

/// The OS errors a write to a pipe with no reader fails with.
#[cfg(unix)]
const PIPE_ERRORS: &[i32] = &[32]; // EPIPE
#[cfg(windows)]
const PIPE_ERRORS: &[i32] = &[109, 232]; // ERROR_BROKEN_PIPE, ERROR_NO_DATA
#[cfg(not(any(unix, windows)))]
const PIPE_ERRORS: &[i32] = &[];

/// Turns the panic `print!` raises on a closed standard output into a quiet exit; other panics keep the default hook.
pub fn install() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if info.payload_as_str().is_some_and(is_closed_stdout_panic) {
            std::process::exit(0);
        }
        default(info);
    }));
}

/// Standard output for writers that return errors: a reader that has gone ends the process quietly.
pub fn stdout() -> Stdout {
    Stdout(std::io::stdout().lock())
}

pub struct Stdout(std::io::StdoutLock<'static>);

impl Write for Stdout {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        quiet(self.0.write(buf))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        quiet(self.0.flush())
    }
}

fn quiet<T>(result: std::io::Result<T>) -> std::io::Result<T> {
    if let Err(e) = &result
        && e.kind() == ErrorKind::BrokenPipe
    {
        std::process::exit(0);
    }
    result
}

fn is_closed_stdout_panic(message: &str) -> bool {
    message
        .strip_prefix("failed printing to stdout: ")
        .is_some_and(|error| {
            PIPE_ERRORS
                .iter()
                .any(|&code| std::io::Error::from_raw_os_error(code).to_string() == error)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_closed_pipe_on_stdout_is_quiet() {
        for &code in PIPE_ERRORS {
            let error = std::io::Error::from_raw_os_error(code);
            assert_eq!(error.kind(), ErrorKind::BrokenPipe);
            assert!(is_closed_stdout_panic(&format!(
                "failed printing to stdout: {error}"
            )));
            assert!(!is_closed_stdout_panic(&format!(
                "failed printing to stderr: {error}"
            )));
        }
        let full = std::io::Error::from(ErrorKind::StorageFull);
        assert!(!is_closed_stdout_panic(&format!(
            "failed printing to stdout: {full}"
        )));
        assert!(!is_closed_stdout_panic("index out of bounds"));
    }
}
