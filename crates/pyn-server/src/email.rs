use async_trait::async_trait;
use pyn_core::{EmailMessage, EmailSender};

/// Writes each message to the server log instead of sending it; for development.
pub struct LogEmailSender;

#[async_trait]
impl EmailSender for LogEmailSender {
    async fn send(&self, message: EmailMessage) -> pyn_core::Result<()> {
        tracing::info!(to = %message.to, subject = %message.subject, "email (log only)\n{}", message.body);
        Ok(())
    }

    fn delivers(&self) -> bool {
        false
    }
}
