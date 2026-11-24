//! The SMTP sender against an in-process server that speaks just enough of the protocol.

use std::sync::{Arc, Mutex};

use pyn_core::{EmailMessage, EmailSender};
use pyn_smtp::{SmtpSender, SmtpSettings, SmtpTls};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

#[derive(Default, Clone)]
struct Seen {
    commands: Arc<Mutex<Vec<String>>>,
    data: Arc<Mutex<String>>,
}

#[derive(Clone, Copy)]
enum Behaviour {
    Accept,
    RejectRecipient,
    RejectAuth,
}

/// Serves one connection per message; offers AUTH PLAIN, and STARTTLS (which it then refuses) when asked.
async fn fake_server(offer_starttls: bool, behaviour: Behaviour) -> (u16, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Seen::default();
    let record = seen.clone();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let record = record.clone();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                write.write_all(b"220 fake ESMTP\r\n").await.unwrap();
                while let Ok(Some(line)) = lines.next_line().await {
                    record.commands.lock().unwrap().push(line.clone());
                    let upper = line.to_ascii_uppercase();
                    let reply: &[u8] = if upper.starts_with("EHLO") {
                        if offer_starttls {
                            b"250-fake\r\n250-STARTTLS\r\n250 AUTH PLAIN\r\n"
                        } else {
                            b"250-fake\r\n250 AUTH PLAIN\r\n"
                        }
                    } else if upper.starts_with("STARTTLS") {
                        b"454 TLS not available\r\n"
                    } else if upper.starts_with("AUTH PLAIN") {
                        match behaviour {
                            Behaviour::RejectAuth => b"535 5.7.8 Authentication credentials invalid\r\n",
                            _ => b"235 ok\r\n",
                        }
                    } else if upper.starts_with("MAIL FROM") {
                        b"250 ok\r\n"
                    } else if upper.starts_with("RCPT TO") {
                        match behaviour {
                            Behaviour::Accept | Behaviour::RejectAuth => b"250 ok\r\n",
                            Behaviour::RejectRecipient => b"550 no such user\r\n",
                        }
                    } else if upper.starts_with("DATA") {
                        write.write_all(b"354 go ahead\r\n").await.unwrap();
                        let mut body = String::new();
                        while let Ok(Some(l)) = lines.next_line().await {
                            if l == "." {
                                break;
                            }
                            body.push_str(&l);
                            body.push('\n');
                        }
                        *record.data.lock().unwrap() = body;
                        b"250 queued\r\n"
                    } else if upper.starts_with("QUIT") {
                        write.write_all(b"221 bye\r\n").await.unwrap();
                        break;
                    } else {
                        b"250 ok\r\n"
                    };
                    write.write_all(reply).await.unwrap();
                }
            });
        }
    });
    (port, seen)
}

fn settings(port: u16, tls: SmtpTls) -> SmtpSettings {
    SmtpSettings {
        host: "127.0.0.1".into(),
        port,
        user: Some("mailer".into()),
        password: Some("hunter2-secret".into()),
        from: "pyn <noreply@example.org>".into(),
        tls,
    }
}

fn message() -> EmailMessage {
    EmailMessage {
        to: "maya@example.net".into(),
        subject: "Confirm your email address".into(),
        body: "Open https://pyn.example/verify-email?token=abc123 now".into(),
    }
}

#[tokio::test]
async fn a_message_goes_through_the_whole_dialogue() {
    let (port, seen) = fake_server(false, Behaviour::Accept).await;
    let sender = SmtpSender::new(&settings(port, SmtpTls::None)).unwrap();
    sender.send(message()).await.unwrap();
    let commands = seen.commands.lock().unwrap().clone();
    let has = |prefix: &str| {
        commands
            .iter()
            .any(|c| c.to_ascii_uppercase().starts_with(prefix))
    };
    assert!(has("EHLO"), "{commands:?}");
    assert!(has("AUTH PLAIN"), "{commands:?}");
    assert!(
        commands
            .iter()
            .any(|c| c.contains("MAIL FROM:<noreply@example.org>"))
    );
    assert!(
        commands
            .iter()
            .any(|c| c.contains("RCPT TO:<maya@example.net>"))
    );
    assert!(has("DATA") && has("QUIT"), "{commands:?}");
    let data = seen.data.lock().unwrap().clone();
    assert!(
        data.contains("Subject: Confirm your email address"),
        "{data}"
    );
    assert!(data.contains("verify-email?token=abc123"), "{data}");
}

#[tokio::test]
async fn starttls_mode_refuses_a_server_that_will_not_upgrade() {
    let (port, seen) = fake_server(false, Behaviour::Accept).await;
    let sender = SmtpSender::new(&settings(port, SmtpTls::StartTls)).unwrap();
    let err = sender.send(message()).await.unwrap_err();
    assert_eq!(err.code(), "email_delivery_failed");
    let commands = seen.commands.lock().unwrap().clone();
    assert!(
        !commands
            .iter()
            .any(|c| c.to_ascii_uppercase().starts_with("AUTH")),
        "credentials must not be sent in the clear: {commands:?}"
    );
    assert!(seen.data.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_failed_starttls_never_falls_back_to_plain() {
    let (port, seen) = fake_server(true, Behaviour::Accept).await;
    let sender = SmtpSender::new(&settings(port, SmtpTls::StartTls)).unwrap();
    assert!(sender.send(message()).await.is_err());
    let commands = seen.commands.lock().unwrap().clone();
    assert!(
        !commands
            .iter()
            .any(|c| c.to_ascii_uppercase().starts_with("AUTH"))
    );
    assert!(seen.data.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_rejected_recipient_is_a_delivery_failure_without_the_password() {
    let (port, _) = fake_server(false, Behaviour::RejectRecipient).await;
    let sender = SmtpSender::new(&settings(port, SmtpTls::None)).unwrap();
    let err = sender.send(message()).await.unwrap_err();
    assert_eq!(err.code(), "email_delivery_failed");
    assert!(!err.to_string().contains("hunter2-secret"));
}

#[tokio::test]
async fn an_unreachable_host_fails_cleanly() {
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    };
    let sender = SmtpSender::new(&settings(port, SmtpTls::None)).unwrap();
    assert!(sender.send(message()).await.is_err());
}

#[test]
fn settings_never_show_the_password() {
    let shown = format!("{:?}", settings(25, SmtpTls::None));
    assert!(!shown.contains("hunter2-secret"), "{shown}");
    assert!(shown.contains("redacted"));
}

#[test]
fn a_bad_sender_address_is_refused_up_front() {
    let mut bad = settings(25, SmtpTls::None);
    bad.from = "not an address".into();
    assert!(SmtpSender::new(&bad).is_err());
}

#[test]
fn tls_modes_parse() {
    assert_eq!("starttls".parse::<SmtpTls>(), Ok(SmtpTls::StartTls));
    assert_eq!("tls".parse::<SmtpTls>(), Ok(SmtpTls::Tls));
    assert_eq!("none".parse::<SmtpTls>(), Ok(SmtpTls::None));
    assert!("maybe".parse::<SmtpTls>().is_err());
}

#[tokio::test]
async fn a_check_signs_in_without_sending() {
    let (port, seen) = fake_server(false, Behaviour::Accept).await;
    let sender = SmtpSender::new(&settings(port, SmtpTls::None)).unwrap();
    sender.check().await.unwrap();
    let commands = seen.commands.lock().unwrap().clone();
    let has = |prefix: &str| {
        commands
            .iter()
            .any(|c| c.to_ascii_uppercase().starts_with(prefix))
    };
    assert!(has("AUTH PLAIN"), "{commands:?}");
    assert!(!has("MAIL FROM") && !has("DATA"), "{commands:?}");
}

#[tokio::test]
async fn a_check_names_refused_credentials_and_a_mismatched_host() {
    let (port, _) = fake_server(false, Behaviour::RejectAuth).await;
    let mut s = settings(port, SmtpTls::None);
    s.user = Some("someone@gmail.com".into());
    let err = SmtpSender::new(&s).unwrap().check().await.unwrap_err();
    assert!(err.contains("user name or password"), "{err}");
    assert!(err.contains("smtp.gmail.com"), "the host is 127.0.0.1: {err}");
    assert!(!err.contains("hunter2-secret"), "{err}");
}

#[tokio::test]
async fn a_check_says_when_nothing_answers() {
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    };
    let err = SmtpSender::new(&settings(port, SmtpTls::None))
        .unwrap()
        .check()
        .await
        .unwrap_err();
    assert!(err.contains("could not connect to 127.0.0.1"), "{err}");
}
