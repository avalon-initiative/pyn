//! `EmailSender` over SMTP (rustls, aws-lc-rs); one bounded attempt per message.

mod diagnose;

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use async_trait::async_trait;
use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use pyn_core::{EmailMessage, EmailSender, PynError};

const ATTEMPT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SmtpTls {
    /// Plain connection upgraded with STARTTLS, which must succeed.
    #[default]
    StartTls,
    /// TLS from the first byte (usually port 465).
    Tls,
    /// No encryption; only for a trusted local relay.
    None,
}

impl FromStr for SmtpTls {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "starttls" => Ok(Self::StartTls),
            "tls" => Ok(Self::Tls),
            "none" => Ok(Self::None),
            other => Err(format!(
                "unknown TLS mode {other:?}; use starttls, tls or none"
            )),
        }
    }
}

#[derive(Clone)]
pub struct SmtpSettings {
    pub host: String,
    pub port: u16,
    pub user: Option<String>,
    pub password: Option<String>,
    pub from: String,
    pub tls: SmtpTls,
}

impl fmt::Debug for SmtpSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SmtpSettings")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("from", &self.from)
            .field("tls", &self.tls)
            .finish()
    }
}

pub use diagnose::{explain, host_mismatch};

pub struct SmtpSender {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
    settings: SmtpSettings,
}

impl SmtpSender {
    /// Builds the transport; connects only when a message is sent.
    pub fn new(settings: &SmtpSettings) -> Result<Self, String> {
        let from: Mailbox = settings
            .from
            .parse()
            .map_err(|e| format!("the sender address {:?} is not valid: {e}", settings.from))?;
        let builder = match settings.tls {
            SmtpTls::StartTls => {
                AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&settings.host)
            }
            SmtpTls::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(&settings.host),
            SmtpTls::None => Ok(AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(
                &settings.host,
            )),
        }
        .map_err(|e| format!("the SMTP host {:?} is not usable: {e}", settings.host))?;
        let mut builder = builder.port(settings.port).timeout(Some(ATTEMPT));
        if let (Some(user), Some(password)) = (&settings.user, &settings.password) {
            builder = builder.credentials(Credentials::new(user.clone(), password.clone()));
        }
        Ok(Self {
            transport: builder.build(),
            from,
            settings: settings.clone(),
        })
    }
}

fn failed(why: impl fmt::Display) -> PynError {
    PynError::EmailDeliveryFailed(why.to_string())
}

#[async_trait]
impl EmailSender for SmtpSender {
    async fn send(&self, message: EmailMessage) -> pyn_core::Result<()> {
        let to: Mailbox = message
            .to
            .parse()
            .map_err(|e| failed(format!("recipient address: {e}")))?;
        let mail = Message::builder()
            .from(self.from.clone())
            .to(to)
            .subject(message.subject)
            .body(message.body)
            .map_err(failed)?;
        match tokio::time::timeout(ATTEMPT * 2, self.transport.send(mail)).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(failed(explain(&self.settings, &e.to_string()))),
            Err(_) => Err(failed(explain(&self.settings, "timed out"))),
        }
    }

    async fn check(&self) -> Result<(), String> {
        match tokio::time::timeout(ATTEMPT * 2, self.transport.test_connection()).await {
            Ok(Ok(true)) => Ok(()),
            Ok(Ok(false)) => Err(explain(&self.settings, "the server did not answer the connection test")),
            Ok(Err(e)) => Err(explain(&self.settings, &e.to_string())),
            Err(_) => Err(explain(&self.settings, "timed out")),
        }
    }
}
