use async_trait::async_trait;
use pyn_core::{EmailMessage, EmailSender};

/// Records each message as queued without sending it; the body, which holds links, is logged only when asked.
pub struct LogEmailSender {
    log_bodies: bool,
}

impl LogEmailSender {
    pub fn new(log_bodies: bool) -> Self {
        Self { log_bodies }
    }
}

/// The part of an address after the last `@`, or a placeholder.
fn domain(address: &str) -> &str {
    address.rsplit_once('@').map_or("unknown", |(_, d)| d)
}

#[async_trait]
impl EmailSender for LogEmailSender {
    async fn send(&self, message: EmailMessage) -> pyn_core::Result<()> {
        let to_domain = domain(&message.to);
        if self.log_bodies {
            tracing::info!(to_domain, subject = %message.subject, "email queued (log only)\n{}", message.body);
        } else {
            tracing::info!(to_domain, subject = %message.subject, "email queued (log only)");
        }
        Ok(())
    }

    fn delivers(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::domain;

    #[test]
    fn only_the_domain_is_kept() {
        assert_eq!(domain("maya@example.org"), "example.org");
        assert_eq!(domain("no-at-sign"), "unknown");
    }
}
