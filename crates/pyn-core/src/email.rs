use std::sync::Mutex;

use async_trait::async_trait;

use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailMessage {
    pub to: String,
    pub subject: String,
    pub body: String,
}

/// Which kind of sender is configured, as an administrator sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailerKind {
    None,
    Log,
    Smtp,
}

/// Delivers mail the server sends on its own behalf, such as address verification.
#[async_trait]
pub trait EmailSender: Send + Sync {
    async fn send(&self, message: EmailMessage) -> Result<()>;

    fn kind(&self) -> MailerKind {
        MailerKind::Smtp
    }

    /// False for senders that never reach a real inbox, so the server can warn about it.
    fn delivers(&self) -> bool {
        true
    }
}

/// Drops every message.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullEmailSender;

#[async_trait]
impl EmailSender for NullEmailSender {
    async fn send(&self, _message: EmailMessage) -> Result<()> {
        Ok(())
    }

    fn kind(&self) -> MailerKind {
        MailerKind::None
    }

    fn delivers(&self) -> bool {
        false
    }
}

/// Keeps what was sent, for tests.
#[derive(Debug, Default)]
pub struct MemoryEmailSender {
    sent: Mutex<Vec<EmailMessage>>,
}

impl MemoryEmailSender {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn sent(&self) -> Vec<EmailMessage> {
        self.sent.lock().unwrap().clone()
    }
}

#[async_trait]
impl EmailSender for MemoryEmailSender {
    async fn send(&self, message: EmailMessage) -> Result<()> {
        self.sent.lock().unwrap().push(message);
        Ok(())
    }

    fn kind(&self) -> MailerKind {
        MailerKind::Log
    }
}

/// The message sent once an address is confirmed. Plain text, no links that track.
pub fn welcome_message(
    to: &str,
    user: &str,
    public_url: &str,
    pending_approval: bool,
) -> EmailMessage {
    let url = public_url.trim_end_matches('/');
    let next = if pending_approval {
        "An administrator has to approve the account before you can sign in; you do not need to do anything until then."
    } else {
        "You can sign in now."
    };
    EmailMessage {
        to: to.to_string(),
        subject: "Welcome to pyn".into(),
        body: format!(
            "Hello {user},\n\n\
             your email address is confirmed. pyn keeps your files' history and locks the ones that cannot be merged, \
             so two people never overwrite each other's work.\n\n\
             {next}\n\n\
             Security settings, including two-factor authentication and passkeys, are at {url}/_/settings/security. \
             If you need help, contact an administrator of this server.\n"
        ),
    }
}

/// The message that carries a server invitation to the person it is for.
pub fn invitation_message(
    to: &str,
    code: &str,
    public_url: &str,
    expires_days: i64,
) -> EmailMessage {
    let url = public_url.trim_end_matches('/');
    EmailMessage {
        to: to.to_string(),
        subject: "You are invited to pyn".into(),
        body: format!(
            "An administrator of {url} invited you to create an account.\n\n\
             Invitation code: {code}\n\n\
             Open {url} and enter the code when you sign up, or run:\n\n\
             pyn register <name> --invite {code} --server {url}\n\n\
             The code works once and expires in {expires_days} days. If you did not expect this, ignore this message.\n"
        ),
    }
}
