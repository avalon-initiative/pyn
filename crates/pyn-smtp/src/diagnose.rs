//! Plain-words reasons for an SMTP failure, with a hint when the host does not match the account's provider.

use crate::SmtpSettings;

/// Account domains of well-known providers and the host that serves them.
const PROVIDERS: [(&[&str], &str); 6] = [
    (&["gmail.com", "googlemail.com"], "smtp.gmail.com"),
    (
        &["outlook.com", "hotmail.com", "live.com", "msn.com"],
        "smtp-mail.outlook.com",
    ),
    (&["yahoo.com", "ymail.com"], "smtp.mail.yahoo.com"),
    (&["icloud.com", "me.com", "mac.com"], "smtp.mail.me.com"),
    (&["fastmail.com", "fastmail.fm"], "smtp.fastmail.com"),
    (&["mail.com"], "smtp.mail.com"),
];

fn domain_of(address: &str) -> Option<String> {
    let bare = address.rsplit_once('<').map_or(address, |(_, rest)| rest);
    let bare = bare.trim_end_matches('>');
    bare.rsplit_once('@').map(|(_, d)| d.to_ascii_lowercase())
}

/// The provider host the account domain implies, when it is a known one and differs from `settings.host`.
pub fn host_mismatch(settings: &SmtpSettings) -> Option<String> {
    let account = settings
        .user
        .as_deref()
        .filter(|u| u.contains('@'))
        .or(Some(settings.from.as_str()))
        .and_then(domain_of)?;
    let expected = PROVIDERS
        .iter()
        .find(|(domains, _)| domains.contains(&account.as_str()))
        .map(|(_, host)| *host)?;
    let host = settings.host.to_ascii_lowercase();
    (host != expected).then(|| {
        format!(
            "the account is at {account}, whose mail server is usually {expected}, but PYN_SMTP_HOST is {}",
            settings.host
        )
    })
}

/// What went wrong, from the error text the SMTP library gave, and what to look at.
pub fn explain(settings: &SmtpSettings, raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    let what = if lower.contains("535") || lower.contains("authentication") || lower.contains("credentials") {
        "the mail server refused the user name or password (check PYN_SMTP_USER and PYN_SMTP_PASSWORD; providers such as Gmail need an app password)".to_string()
    } else if lower.contains("timed out") || lower.contains("timeout") {
        format!(
            "no answer from {}:{} in time (check PYN_SMTP_HOST and PYN_SMTP_PORT, and that the network allows outgoing mail)",
            settings.host, settings.port
        )
    } else if lower.contains("refused") || lower.contains("unreachable") {
        format!(
            "could not connect to {}:{} (check PYN_SMTP_HOST and PYN_SMTP_PORT, and that the network allows outgoing mail)",
            settings.host, settings.port
        )
    } else if lower.contains("dns") || lower.contains("resolve") || lower.contains("lookup") || lower.contains("name or service") {
        format!(
            "the host name {} does not resolve (check PYN_SMTP_HOST)",
            settings.host
        )
    } else if lower.contains("tls") || lower.contains("certificate") || lower.contains("starttls") || lower.contains("handshake") {
        format!(
            "the secure connection to {}:{} failed (PYN_SMTP_TLS is {}; try starttls on 587 or tls on 465)",
            settings.host,
            settings.port,
            match settings.tls {
                crate::SmtpTls::StartTls => "starttls",
                crate::SmtpTls::Tls => "tls",
                crate::SmtpTls::None => "none",
            }
        )
    } else {
        format!("the mail server at {}:{} said: {raw}", settings.host, settings.port)
    };
    match host_mismatch(settings) {
        Some(hint) => format!("{what}; {hint}"),
        None => what,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SmtpTls;

    fn settings(host: &str, user: Option<&str>, from: &str) -> SmtpSettings {
        SmtpSettings {
            host: host.into(),
            port: 587,
            user: user.map(str::to_string),
            password: user.map(|_| "secret".into()),
            from: from.into(),
            tls: SmtpTls::StartTls,
        }
    }

    #[test]
    fn a_known_provider_with_the_wrong_host_is_named() {
        let s = settings("smtp.mail.com", Some("me@gmail.com"), "pyn@example.org");
        let hint = host_mismatch(&s).unwrap();
        assert!(hint.contains("smtp.gmail.com") && hint.contains("smtp.mail.com"), "{hint}");
        assert!(host_mismatch(&settings("SMTP.GMAIL.COM", Some("me@gmail.com"), "x@y.z")).is_none());
        assert!(host_mismatch(&settings("mail.example.org", Some("me@example.org"), "x@example.org")).is_none());
    }

    #[test]
    fn the_sender_address_stands_in_when_the_user_is_not_an_address() {
        let s = settings("smtp.example.org", Some("apikey"), "pyn <pyn@outlook.com>");
        assert!(host_mismatch(&s).unwrap().contains("smtp-mail.outlook.com"));
    }

    #[test]
    fn failures_are_explained_in_plain_words_and_never_echo_the_password() {
        let s = settings("smtp.mail.com", Some("me@gmail.com"), "me@gmail.com");
        let auth = explain(&s, "permanent error (535): Authentication credentials invalid");
        assert!(auth.contains("user name or password") && auth.contains("smtp.gmail.com"), "{auth}");
        assert!(!auth.contains("secret"));
        let down = explain(&settings("h", None, "a@b.c"), "Connection refused (os error 111)");
        assert!(down.contains("could not connect to h:587"), "{down}");
        let slow = explain(&settings("h", None, "a@b.c"), "timed out");
        assert!(slow.contains("no answer"), "{slow}");
        let other = explain(&settings("h", None, "a@b.c"), "weird");
        assert!(other.contains("said: weird"), "{other}");
    }
}
