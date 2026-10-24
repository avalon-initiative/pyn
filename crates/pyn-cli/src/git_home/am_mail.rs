//! Reading patch emails as `git am` does: an mbox split into messages, and one message into the author, date,
//! commit message and patch. UTF-8 and ASCII bodies only.

use anyhow::{Result, bail};

/// One email read for `am`.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Mail {
    pub name: String,
    pub email: String,
    pub date: Option<String>,
    pub subject: String,
    pub message_id: Option<String>,
    /// The message after the subject, without leading or trailing blank lines.
    pub body: Vec<u8>,
    /// From the `---` line or the first diff line to the end.
    pub patch: Vec<u8>,
}

fn is_from_line(line: &[u8]) -> bool {
    if line.len() < 20 || !line.starts_with(b"From ") {
        return false;
    }
    let text = String::from_utf8_lossy(line);
    let Some(colon) = text.rfind(':') else {
        return false;
    };
    let b = text.as_bytes();
    colon >= 2
        && colon + 2 < b.len()
        && b[colon - 2].is_ascii_digit()
        && b[colon - 1].is_ascii_digit()
        && b[colon + 1].is_ascii_digit()
        && b[colon + 2].is_ascii_digit()
}

/// The messages of an mbox; input that does not begin with a `From ` line is one message. A file that does not
/// look like mail is refused (`detect`); standard input is taken as it comes.
pub(super) fn split_mbox(input: &[u8], detect: bool) -> Result<Vec<Vec<u8>>> {
    let mut start = 0;
    while input[start..].starts_with(b"\n") {
        start += 1;
    }
    let input = &input[start..];
    if detect
        && !input.starts_with(b"From ")
        && !input.starts_with(b"From: ")
        && !looks_like_mail(input)
    {
        bail!("Patch format detection failed.");
    }
    if !is_from_line(input.split(|b| *b == b'\n').next().unwrap_or_default()) {
        return Ok(vec![input.to_vec()]);
    }
    let mut mails: Vec<Vec<u8>> = Vec::new();
    for line in input.split_inclusive(|b| *b == b'\n') {
        if is_from_line(line) {
            mails.push(Vec::new());
        }
        if let Some(last) = mails.last_mut() {
            last.extend_from_slice(line);
        }
    }
    Ok(mails)
}

fn looks_like_mail(input: &[u8]) -> bool {
    input.split(|b| *b == b'\n').next().is_some_and(|l| {
        l.iter()
            .position(|b| *b == b':')
            .is_some_and(|i| i > 0 && !l[..i].contains(&b' '))
    })
}

fn q_decode(text: &str) -> Vec<u8> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let hex = (b[i] == b'=' && i + 2 < b.len())
            .then(|| std::str::from_utf8(&b[i + 1..i + 3]).ok())
            .flatten()
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (b[i], hex) {
            (_, Some(v)) => {
                out.push(v);
                i += 2;
            }
            (b'_', None) => out.push(b' '),
            (c, None) => out.push(c),
        }
        i += 1;
    }
    out
}

fn b_decode(text: &str) -> Vec<u8> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in text.bytes().filter(|c| *c != b'=') {
        let Some(v) = ALPHABET.iter().position(|a| *a == c) else {
            continue;
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    out
}

/// A header value with its RFC 2047 words decoded (UTF-8 and ASCII charsets).
fn decode_words(value: &str) -> Result<String> {
    let mut out = Vec::new();
    let mut rest = value;
    let mut after_word = false;
    while let Some(start) = rest.find("=?") {
        let plain = &rest[..start];
        let mut parts = rest[start + 2..].splitn(3, '?');
        let word = match (parts.next(), parts.next(), parts.next()) {
            (Some(cs), Some(enc), Some(tail)) => tail
                .find("?=")
                .map(|end| (cs, enc, &tail[..end], &tail[end + 2..])),
            _ => None,
        };
        let Some((charset, enc, data, after)) = word else {
            out.extend_from_slice(&rest.as_bytes()[..start + 2]);
            rest = &rest[start + 2..];
            after_word = false;
            continue;
        };
        if !(after_word && plain.trim().is_empty()) {
            out.extend_from_slice(plain.as_bytes());
        }
        if !charset.eq_ignore_ascii_case("utf-8") && !charset.eq_ignore_ascii_case("us-ascii") {
            bail!("pyn am cannot read the charset {charset} yet");
        }
        match enc {
            "q" | "Q" => out.extend(q_decode(data)),
            "b" | "B" => out.extend(b_decode(data)),
            _ => bail!("unknown header encoding {enc}"),
        }
        rest = after;
        after_word = true;
    }
    out.extend_from_slice(rest.as_bytes());
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// Name and address from a `From:` header value.
fn parse_from(value: &str) -> (String, String) {
    let value = value.trim();
    if let (Some(open), Some(close)) = (value.find('<'), value.rfind('>'))
        && open < close
    {
        let email = value[open + 1..close].trim().to_string();
        let mut name = value[..open].trim().to_string();
        if name.len() >= 2 && name.starts_with('"') && name.ends_with('"') {
            let inner = &name[1..name.len() - 1];
            let mut plain = String::new();
            let mut chars = inner.chars();
            while let Some(c) = chars.next() {
                if c == '\\' {
                    plain.extend(chars.next());
                } else {
                    plain.push(c);
                }
            }
            name = plain;
        }
        return (if name.is_empty() { email.clone() } else { name }, email);
    }
    if let (Some(open), Some(close)) = (value.find('('), value.rfind(')'))
        && open < close
    {
        return (
            value[open + 1..close].trim().to_string(),
            value[..open].trim().to_string(),
        );
    }
    (value.to_string(), value.to_string())
}

/// Removes `Re:` and bracketed tags such as `[PATCH 1/2]` from the front of a subject.
fn clean_subject(subject: &str) -> String {
    let mut s = subject.trim_start();
    loop {
        let lower = s.to_ascii_lowercase();
        if lower.starts_with("re:") {
            s = s[3..].trim_start();
        } else if s.starts_with('[')
            && let Some(close) = s.find(']')
        {
            s = s[close + 1..].trim_start();
        } else {
            break;
        }
    }
    s.trim().to_string()
}

fn patch_break(line: &[u8]) -> bool {
    line.starts_with(b"diff -")
        || line.starts_with(b"Index: ")
        || (line.starts_with(b"---")
            && (line.get(3) == Some(&b'\n')
                || (line.get(3) == Some(&b' ')
                    && line.get(4).is_some_and(|c| !c.is_ascii_whitespace()))))
}

/// Reads the headers into `(name, value)` pairs with continuation lines joined; returns where the body starts.
fn headers(mail: &[u8]) -> (Vec<(String, String)>, usize) {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut pos = 0;
    let mut first = true;
    while pos < mail.len() {
        let pos_before = pos;
        let end = mail[pos..]
            .iter()
            .position(|b| *b == b'\n')
            .map_or(mail.len(), |i| pos + i);
        let line = String::from_utf8_lossy(&mail[pos..end])
            .trim_end_matches('\r')
            .to_string();
        pos = (end + 1).min(mail.len());
        if line.is_empty() {
            break;
        }
        if first && line.starts_with("From ") {
            first = false;
            continue;
        }
        first = false;
        let header_like = line.split_once(':').is_some_and(|(k, _)| {
            !k.is_empty() && k.bytes().all(|b| (b'!'..=b'~').contains(&b) && b != b':')
        });
        if !header_like && !line.starts_with([' ', '\t']) {
            return (out, pos_before);
        }
        if line.starts_with([' ', '\t']) {
            if let Some((_, v)) = out.last_mut() {
                v.push(' ');
                v.push_str(line.trim());
            }
        } else if let Some((k, v)) = line.split_once(':') {
            out.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    (out, pos)
}

pub(super) fn parse_mail(mail: &[u8]) -> Result<Mail> {
    let (heads, body_at) = headers(mail);
    let get = |name: &str| {
        heads
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    if let Some(t) = get("content-type") {
        let lower = t.to_ascii_lowercase();
        if lower.contains("multipart") {
            bail!("pyn am does not read multipart messages yet");
        }
        if let Some(cs) = lower.split("charset=").nth(1) {
            let cs = cs.trim_matches(|c: char| c == '"' || c == ';' || c.is_whitespace());
            if !cs.is_empty() && !matches!(cs, "utf-8" | "us-ascii") {
                bail!("pyn am cannot read the charset {cs} yet");
            }
        }
    }
    if let Some(enc) = get("content-transfer-encoding")
        && !matches!(enc.to_ascii_lowercase().as_str(), "7bit" | "8bit")
    {
        bail!("pyn am cannot read the transfer encoding {enc} yet");
    }
    let (mut name, mut email) = (String::new(), String::new());
    if let Some(from) = get("from") {
        (name, email) = parse_from(&decode_words(&from)?);
    }
    let mut date = get("date");
    let mut subject = decode_words(&get("subject").unwrap_or_default())?;
    let message_id = get("message-id");
    let body = &mail[body_at..];
    // A header block at the top of the body overrides the email's own headers.
    let mut seen_any = false;
    let mut pending: Option<(String, String)> = None;
    let mut inbody_done = false;
    let mut message: Vec<u8> = Vec::new();
    let mut patch: Vec<u8> = Vec::new();
    for line in body.split_inclusive(|b| *b == b'\n') {
        if !patch.is_empty() || patch_break(line) {
            patch.extend_from_slice(line);
            continue;
        }
        if !inbody_done {
            let text = String::from_utf8_lossy(line)
                .trim_end_matches(['\n', '\r'])
                .to_string();
            if let Some((k, v)) = text.split_once(':')
                && matches!(k.to_ascii_lowercase().as_str(), "from" | "subject" | "date")
                && !k.contains(' ')
            {
                if let Some((pk, pv)) = pending.take() {
                    apply_inbody(&pk, &pv, &mut name, &mut email, &mut date, &mut subject)?;
                }
                pending = Some((k.to_ascii_lowercase(), v.trim().to_string()));
                seen_any = true;
                continue;
            }
            if text.starts_with([' ', '\t'])
                && let Some((_, pv)) = pending.as_mut()
            {
                pv.push(' ');
                pv.push_str(text.trim());
                continue;
            }
            if let Some((pk, pv)) = pending.take() {
                apply_inbody(&pk, &pv, &mut name, &mut email, &mut date, &mut subject)?;
            }
            inbody_done = true;
            if seen_any && text.is_empty() {
                continue;
            }
        }
        message.extend_from_slice(line);
    }
    if let Some((pk, pv)) = pending.take() {
        apply_inbody(&pk, &pv, &mut name, &mut email, &mut date, &mut subject)?;
    }
    Ok(Mail {
        name,
        email,
        date,
        subject: clean_subject(&subject),
        message_id,
        body: message,
        patch,
    })
}

fn apply_inbody(
    key: &str,
    value: &str,
    name: &mut String,
    email: &mut String,
    date: &mut Option<String>,
    subject: &mut String,
) -> Result<()> {
    let value = decode_words(value)?;
    match key {
        "from" => (*name, *email) = parse_from(&value),
        "date" => *date = Some(value),
        _ => *subject = value,
    }
    Ok(())
}
