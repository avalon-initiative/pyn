use std::sync::Arc;

use async_trait::async_trait;
use pyn_core::{EmailMessage, EmailSender, MailerKind, NullEmailSender};
use pyn_smtp::{SmtpSender, SmtpSettings, SmtpTls};

/// First line of every message the logging sender writes; grep the server log for it.
pub const LOG_MARKER: &str = "PYN-MAILER-LOG";

/// Writes each whole message to the server log instead of sending it. The links in it are secrets, so
/// it is for testing and guarded like `PYN_DEV_AUTH`.
pub struct LogEmailSender;

#[async_trait]
impl EmailSender for LogEmailSender {
    async fn send(&self, message: EmailMessage) -> pyn_core::Result<()> {
        tracing::info!(
            "{LOG_MARKER} (not delivered)\nTo: {}\nSubject: {}\n\n{}",
            message.to,
            message.subject,
            message.body
        );
        Ok(())
    }

    fn delivers(&self) -> bool {
        false
    }

    fn kind(&self) -> MailerKind {
        MailerKind::Log
    }
}

/// What `PYN_MAILER` selects.
#[derive(Debug, Clone)]
pub enum MailerSetting {
    None,
    Log,
    Smtp(SmtpSettings),
}

impl MailerSetting {
    /// Reads `PYN_MAILER` and `PYN_SMTP_*` through `get`; unset `PYN_MAILER` means none.
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        match get("PYN_MAILER").as_deref() {
            None | Some("none") => Ok(Self::None),
            Some("log") => Ok(Self::Log),
            Some("smtp") => smtp_from_env(&get).map(Self::Smtp),
            Some(other) => Err(format!(
                "unknown PYN_MAILER {other:?}; use none, log or smtp"
            )),
        }
    }

    /// Whether a message could reach someone, which is what turns on email verification.
    pub fn is_configured(&self) -> bool {
        !matches!(self, Self::None)
    }

    pub fn is_log(&self) -> bool {
        matches!(self, Self::Log)
    }

    pub fn sender(&self) -> Result<Arc<dyn EmailSender>, String> {
        Ok(match self {
            Self::None => Arc::new(NullEmailSender),
            Self::Log => Arc::new(LogEmailSender),
            Self::Smtp(settings) => Arc::new(SmtpSender::new(settings)?),
        })
    }
}

fn smtp_from_env(get: &impl Fn(&str) -> Option<String>) -> Result<SmtpSettings, String> {
    let need = |name: &str| {
        get(name)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| format!("PYN_MAILER=smtp needs {name}"))
    };
    let port = match get("PYN_SMTP_PORT") {
        None => 587,
        Some(p) => p
            .parse::<u16>()
            .map_err(|_| format!("PYN_SMTP_PORT must be a port number, not {p:?}"))?,
    };
    let tls = match get("PYN_SMTP_TLS") {
        None => SmtpTls::default(),
        Some(t) => t.parse().map_err(|e| format!("PYN_SMTP_TLS: {e}"))?,
    };
    let user = get("PYN_SMTP_USER").filter(|v| !v.is_empty());
    let password = get("PYN_SMTP_PASSWORD").filter(|v| !v.is_empty());
    if user.is_some() != password.is_some() {
        return Err("PYN_SMTP_USER and PYN_SMTP_PASSWORD go together: set both or neither".into());
    }
    Ok(SmtpSettings {
        host: need("PYN_SMTP_HOST")?,
        port,
        user,
        password,
        from: need("PYN_SMTP_FROM")?,
        tls,
    })
}

/// Whether sign-up verifies addresses: on when a mailer exists, and refused when forced on without one.
pub fn email_verification(requested: bool, mailer: &MailerSetting) -> Result<bool, String> {
    if requested && !mailer.is_configured() {
        return Err(
            "PYN_EMAIL_VERIFICATION=true needs a mailer to send the links: set PYN_MAILER=smtp (or log, for testing)"
                .into(),
        );
    }
    Ok(requested)
}

/// The logging sender on a non-loopback listener is refused unless `acknowledged`.
pub fn check_log_mailer_bind(
    addr: &str,
    log_mailer: bool,
    acknowledged: bool,
) -> Result<(), String> {
    if !log_mailer || acknowledged || crate::is_loopback_addr(addr) {
        return Ok(());
    }
    Err(format!(
        "PYN_MAILER=log with PYN_ADDR={addr}: the verification links it writes to the log let whoever reads the log take over accounts. \
         Bind a loopback address, use PYN_MAILER=smtp, or set PYN_MAILER_LOG_ALLOW_NON_LOOPBACK=true to accept the risk"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn unset_and_none_send_nothing() {
        assert!(!MailerSetting::from_env(env(&[])).unwrap().is_configured());
        let none = env(&[("PYN_MAILER", "none")]);
        assert!(!MailerSetting::from_env(none).unwrap().is_configured());
    }

    #[test]
    fn smtp_defaults_to_submission_with_starttls() {
        let get = env(&[
            ("PYN_MAILER", "smtp"),
            ("PYN_SMTP_HOST", "smtp.example.org"),
            ("PYN_SMTP_FROM", "pyn@example.org"),
        ]);
        let MailerSetting::Smtp(s) = MailerSetting::from_env(get).unwrap() else {
            panic!("smtp expected")
        };
        assert_eq!((s.port, s.tls), (587, SmtpTls::StartTls));
        assert!(s.user.is_none() && s.password.is_none());
    }

    #[test]
    fn smtp_needs_a_host_and_a_sender_and_whole_credentials() {
        let missing = |extra: &[(&str, &str)]| {
            let mut pairs = vec![("PYN_MAILER", "smtp")];
            pairs.extend_from_slice(extra);
            MailerSetting::from_env(env(&pairs)).unwrap_err()
        };
        assert!(missing(&[]).contains("PYN_SMTP_HOST"));
        assert!(missing(&[("PYN_SMTP_HOST", "h")]).contains("PYN_SMTP_FROM"));
        let half = missing(&[
            ("PYN_SMTP_HOST", "h"),
            ("PYN_SMTP_FROM", "a@b.c"),
            ("PYN_SMTP_USER", "u"),
        ]);
        assert!(half.contains("go together"));
    }

    #[test]
    fn bad_values_are_named_without_echoing_the_password() {
        let base = [
            ("PYN_MAILER", "smtp"),
            ("PYN_SMTP_HOST", "h"),
            ("PYN_SMTP_FROM", "a@b.c"),
            ("PYN_SMTP_USER", "u"),
            ("PYN_SMTP_PASSWORD", "s3cret-value"),
        ];
        let mut port = base.to_vec();
        port.push(("PYN_SMTP_PORT", "http"));
        let err = MailerSetting::from_env(env(&port)).unwrap_err();
        assert!(err.contains("PYN_SMTP_PORT") && !err.contains("s3cret-value"));
        let mut tls = base.to_vec();
        tls.push(("PYN_SMTP_TLS", "ssl"));
        assert!(
            MailerSetting::from_env(env(&tls))
                .unwrap_err()
                .contains("PYN_SMTP_TLS")
        );
        let unknown = MailerSetting::from_env(env(&[("PYN_MAILER", "console")]));
        assert!(unknown.unwrap_err().contains("none, log or smtp"));
    }

    #[test]
    fn forced_verification_without_a_mailer_is_refused() {
        let none = MailerSetting::None;
        assert!(
            email_verification(true, &none)
                .unwrap_err()
                .contains("PYN_MAILER")
        );
        assert!(!email_verification(false, &none).unwrap());
        assert!(email_verification(true, &MailerSetting::Log).unwrap());
        assert!(!email_verification(false, &MailerSetting::Log).unwrap());
    }

    #[test]
    fn the_log_sender_is_refused_beyond_loopback_unless_acknowledged() {
        for addr in ["127.0.0.1:7878", "[::1]:7878", "localhost:7878"] {
            assert!(check_log_mailer_bind(addr, true, false).is_ok(), "{addr}");
        }
        for addr in ["0.0.0.0:7878", "10.1.2.3:7878", "[::]:7878"] {
            let err = check_log_mailer_bind(addr, true, false).unwrap_err();
            assert!(err.contains("PYN_MAILER_LOG_ALLOW_NON_LOOPBACK"), "{addr}");
            assert!(check_log_mailer_bind(addr, true, true).is_ok(), "{addr}");
            assert!(check_log_mailer_bind(addr, false, false).is_ok(), "{addr}");
        }
    }
}
