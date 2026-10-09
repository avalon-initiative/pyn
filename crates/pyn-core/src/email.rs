use std::sync::Mutex;

use async_trait::async_trait;

use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailMessage {
    pub to: String,
    pub subject: String,
    pub body: String,
}

/// Delivers mail the server sends on its own behalf, such as address verification.
#[async_trait]
pub trait EmailSender: Send + Sync {
    async fn send(&self, message: EmailMessage) -> Result<()>;

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
}
