use crate::{Algorithm, Error, Kind, ObjectId};

/// An author, committer or tagger line: `name <email> time +hhmm`. Name and email are raw bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    pub name: Vec<u8>,
    pub email: Vec<u8>,
    pub time: i64,
    /// Minutes east of UTC, e.g. `-330` for `-0530`.
    pub offset_minutes: i32,
}

impl Signature {
    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.name);
        out.extend_from_slice(b" <");
        out.extend_from_slice(&self.email);
        out.extend_from_slice(b"> ");
        let sign = if self.offset_minutes < 0 { '-' } else { '+' };
        let abs = self.offset_minutes.unsigned_abs();
        out.extend_from_slice(
            format!("{} {sign}{:02}{:02}", self.time, abs / 60, abs % 60).as_bytes(),
        );
    }

    fn parse(line: &[u8], what: &'static str) -> Result<Self, Error> {
        let bad = |why| Error::Malformed(what, why);
        let gt = line
            .iter()
            .rposition(|&b| b == b'>')
            .ok_or(bad("missing '>'"))?;
        let lt = line[..gt]
            .iter()
            .position(|&b| b == b'<')
            .ok_or(bad("missing '<'"))?;
        if lt == 0 || line[lt - 1] != b' ' {
            return Err(bad("missing space before '<'"));
        }
        let name = line[..lt - 1].to_vec();
        let email = line[lt + 1..gt].to_vec();
        let tail = std::str::from_utf8(&line[gt + 1..]).map_err(|_| bad("non-ASCII time"))?;
        let tail = tail.strip_prefix(' ').ok_or(bad("missing time"))?;
        let (time, tz) = tail.split_once(' ').ok_or(bad("missing timezone"))?;
        let time: i64 = time.parse().map_err(|_| bad("bad time"))?;
        let sign: i32 = match tz.as_bytes().first() {
            Some(b'+') => 1,
            Some(b'-') => -1,
            _ => return Err(bad("bad timezone")),
        };
        if tz.len() != 5 || !tz[1..].bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad("bad timezone"));
        }
        let hours: i32 = tz[1..3].parse().map_err(|_| bad("bad timezone"))?;
        let mins: i32 = tz[3..5].parse().map_err(|_| bad("bad timezone"))?;
        let sig = Self {
            name,
            email,
            time,
            offset_minutes: sign * (hours * 60 + mins),
        };
        let mut check = Vec::new();
        sig.encode(&mut check);
        if check != line {
            return Err(bad("non-canonical signature line"));
        }
        Ok(sig)
    }
}

type Header = (Vec<u8>, Vec<u8>);

/// Appends `name value`, folding embedded newlines into space-prefixed continuation lines.
fn push_header(out: &mut Vec<u8>, name: &[u8], value: &[u8]) {
    out.extend_from_slice(name);
    out.push(b' ');
    for &b in value {
        out.push(b);
        if b == b'\n' {
            out.push(b' ');
        }
    }
    out.push(b'\n');
}

/// Splits a header block into `(name, value)` pairs, unfolding continuation lines.
fn parse_headers(block: &[u8], what: &'static str) -> Result<Vec<Header>, Error> {
    let mut headers: Vec<Header> = Vec::new();
    let block = block
        .strip_suffix(b"\n")
        .ok_or(Error::Malformed(what, "unterminated header"))?;
    for line in block.split(|&b| b == b'\n') {
        if let Some(cont) = line.strip_prefix(b" ") {
            let (_, value) = headers
                .last_mut()
                .ok_or(Error::Malformed(what, "continuation without header"))?;
            value.push(b'\n');
            value.extend_from_slice(cont);
        } else {
            let sp = line
                .iter()
                .position(|&b| b == b' ')
                .ok_or(Error::Malformed(what, "header without value"))?;
            headers.push((line[..sp].to_vec(), line[sp + 1..].to_vec()));
        }
    }
    Ok(headers)
}

fn split_message<'a>(body: &'a [u8], what: &'static str) -> Result<(&'a [u8], &'a [u8]), Error> {
    let at = body
        .windows(2)
        .position(|w| w == b"\n\n")
        .ok_or(Error::Malformed(what, "no blank line before message"))?;
    Ok((&body[..at + 1], &body[at + 2..]))
}

fn take(
    headers: &mut std::vec::IntoIter<Header>,
    name: &[u8],
    what: &'static str,
) -> Result<Vec<u8>, Error> {
    match headers.next() {
        Some((n, v)) if n == name => Ok(v),
        _ => Err(Error::Malformed(what, "missing or misordered header")),
    }
}

fn parse_id(algorithm: Algorithm, v: &[u8], what: &'static str) -> Result<ObjectId, Error> {
    let s = std::str::from_utf8(v).map_err(|_| Error::Malformed(what, "bad object id"))?;
    if s.bytes().any(|b| b.is_ascii_uppercase()) {
        return Err(Error::Malformed(what, "uppercase object id"));
    }
    ObjectId::from_hex(algorithm, s)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub tree: ObjectId,
    pub parents: Vec<ObjectId>,
    pub author: Signature,
    pub committer: Signature,
    /// Headers after `committer`, in order: `encoding`, `mergetag`, `gpgsig` and the like. Values may span lines.
    pub extra_headers: Vec<Header>,
    pub message: Vec<u8>,
}

impl Commit {
    /// Encodes the body; the tree and every parent must use `algorithm`.
    pub fn encode(&self, algorithm: Algorithm) -> Result<Vec<u8>, Error> {
        self.tree.expect(algorithm)?;
        for p in &self.parents {
            p.expect(algorithm)?;
        }
        let mut out = Vec::new();
        push_header(&mut out, b"tree", self.tree.to_hex().as_bytes());
        for p in &self.parents {
            push_header(&mut out, b"parent", p.to_hex().as_bytes());
        }
        for (name, sig) in [
            (&b"author"[..], &self.author),
            (b"committer", &self.committer),
        ] {
            let mut v = Vec::new();
            sig.encode(&mut v);
            push_header(&mut out, name, &v);
        }
        for (name, value) in &self.extra_headers {
            push_header(&mut out, name, value);
        }
        out.push(b'\n');
        out.extend_from_slice(&self.message);
        Ok(out)
    }

    pub fn parse(algorithm: Algorithm, body: &[u8]) -> Result<Self, Error> {
        const W: &str = "commit";
        let (block, message) = split_message(body, W)?;
        let all = parse_headers(block, W)?;
        let mut it = all.into_iter().peekable();
        let tree = match it.next() {
            Some((n, v)) if n == b"tree" => parse_id(algorithm, &v, W)?,
            _ => return Err(Error::Malformed(W, "missing tree")),
        };
        let mut parents = Vec::new();
        while let Some((_, v)) = it.next_if(|(n, _)| n == b"parent") {
            parents.push(parse_id(algorithm, &v, W)?);
        }
        let mut rest = it.collect::<Vec<_>>().into_iter();
        let author = Signature::parse(&take(&mut rest, b"author", W)?, W)?;
        let committer = Signature::parse(&take(&mut rest, b"committer", W)?, W)?;
        let commit = Self {
            tree,
            parents,
            author,
            committer,
            extra_headers: rest.collect(),
            message: message.to_vec(),
        };
        if commit.encode(algorithm)? != body {
            return Err(Error::Malformed(W, "not canonical"));
        }
        Ok(commit)
    }

    pub fn id(&self, algorithm: Algorithm) -> Result<ObjectId, Error> {
        crate::hash_object(algorithm, Kind::Commit, &self.encode(algorithm)?)
    }

    /// The `gpgsig` (or `gpgsig-sha256`) header value, if present.
    pub fn gpgsig(&self) -> Option<&[u8]> {
        self.extra_headers
            .iter()
            .find(|(n, _)| n == b"gpgsig" || n == b"gpgsig-sha256")
            .map(|(_, v)| v.as_slice())
    }
}

/// An annotated tag. Any signature stays at the end of `message`, as git stores it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub object: ObjectId,
    pub kind: Kind,
    pub name: Vec<u8>,
    pub tagger: Option<Signature>,
    pub message: Vec<u8>,
}

impl Tag {
    /// Encodes the body; the target object must use `algorithm`.
    pub fn encode(&self, algorithm: Algorithm) -> Result<Vec<u8>, Error> {
        self.object.expect(algorithm)?;
        let mut out = Vec::new();
        push_header(&mut out, b"object", self.object.to_hex().as_bytes());
        push_header(&mut out, b"type", self.kind.name().as_bytes());
        push_header(&mut out, b"tag", &self.name);
        if let Some(t) = &self.tagger {
            let mut v = Vec::new();
            t.encode(&mut v);
            push_header(&mut out, b"tagger", &v);
        }
        out.push(b'\n');
        out.extend_from_slice(&self.message);
        Ok(out)
    }

    pub fn parse(algorithm: Algorithm, body: &[u8]) -> Result<Self, Error> {
        const W: &str = "tag";
        let (block, message) = split_message(body, W)?;
        let mut it = parse_headers(block, W)?.into_iter();
        let object = parse_id(algorithm, &take(&mut it, b"object", W)?, W)?;
        let kind = Kind::from_name(&take(&mut it, b"type", W)?)
            .ok_or(Error::Malformed(W, "unknown object type"))?;
        let name = take(&mut it, b"tag", W)?;
        let tagger = match it.next() {
            Some((n, v)) if n == b"tagger" => Some(Signature::parse(&v, W)?),
            None => None,
            _ => return Err(Error::Malformed(W, "unexpected header")),
        };
        if it.next().is_some() {
            return Err(Error::Malformed(W, "unexpected header"));
        }
        let tag = Self {
            object,
            kind,
            name,
            tagger,
            message: message.to_vec(),
        };
        if tag.encode(algorithm)? != body {
            return Err(Error::Malformed(W, "not canonical"));
        }
        Ok(tag)
    }

    pub fn id(&self, algorithm: Algorithm) -> Result<ObjectId, Error> {
        crate::hash_object(algorithm, Kind::Tag, &self.encode(algorithm)?)
    }
}
