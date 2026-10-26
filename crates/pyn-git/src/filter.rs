use pyn_object::Kind;

use crate::error::{Error, Result, invalid};

/// A partial-clone object filter (`--filter=<spec>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectFilter {
    BlobNone,
    /// Blobs of at least this many bytes are left out.
    BlobLimit(u64),
    /// Trees and blobs at this depth from the root tree or deeper are left out.
    TreeDepth(u32),
    /// Every part applies; the text is the spec as given, which is what is stored and sent.
    Combine(Vec<ObjectFilter>, String),
}

impl ObjectFilter {
    /// Parses git's filter-spec grammar; a spec git has but pyn does not build is `Unsupported`.
    pub fn parse(spec: &str) -> Result<Self> {
        let bad = || invalid("filter-spec", spec);
        if let Some(rest) = spec.strip_prefix("combine:") {
            let parts = rest
                .split('+')
                .map(|p| Self::parse(&decode(p).ok_or_else(bad)?))
                .collect::<Result<Vec<_>>>()?;
            return Ok(match parts.len() {
                1 => parts.into_iter().next().expect("one part"),
                _ => Self::Combine(parts, spec.to_owned()),
            });
        }
        if spec == "blob:none" {
            return Ok(Self::BlobNone);
        }
        if let Some(n) = spec.strip_prefix("blob:limit=") {
            return Ok(Self::BlobLimit(size(n).ok_or_else(bad)?));
        }
        if let Some(n) = spec.strip_prefix("tree:") {
            return Ok(Self::TreeDepth(n.parse().map_err(|_| bad())?));
        }
        if spec.starts_with("sparse:") || spec.starts_with("object:type=") {
            return Err(Error::Unsupported(format!("filter-spec '{spec}'")));
        }
        Err(bad())
    }

    /// The spec text sent on the wire and kept in `remote.<name>.partialclonefilter`.
    pub fn spec(&self) -> String {
        match self {
            Self::BlobNone => "blob:none".into(),
            Self::BlobLimit(n) => format!("blob:limit={n}"),
            Self::TreeDepth(n) => format!("tree:{n}"),
            Self::Combine(_, raw) => raw.clone(),
        }
    }

    /// The filter that repeated `--filter` options make together; none given is `None`.
    pub fn from_specs(specs: &[String]) -> Result<Option<Self>> {
        match specs {
            [] => Ok(None),
            [one] => Self::parse(one).map(Some),
            many => {
                let parts: Vec<String> = many.iter().map(|s| encode(s)).collect();
                Self::parse(&format!("combine:{}", parts.join("+"))).map(Some)
            }
        }
    }

    /// Whether the filter leaves out an object of this kind at this depth from the root tree.
    pub fn omits(&self, kind: Kind, depth: u32, blob_size: u64) -> bool {
        match self {
            Self::BlobNone => kind == Kind::Blob,
            Self::BlobLimit(n) => kind == Kind::Blob && blob_size >= *n,
            Self::TreeDepth(d) => matches!(kind, Kind::Tree | Kind::Blob) && depth >= *d,
            Self::Combine(parts, _) => parts.iter().any(|p| p.omits(kind, depth, blob_size)),
        }
    }

    /// What a connectivity check over a clone made with this filter can follow: commits, and trees to the same depth.
    pub fn for_connectivity(&self) -> Self {
        match self {
            Self::TreeDepth(_) => self.clone(),
            Self::Combine(parts, raw) => Self::Combine(
                parts.iter().map(Self::for_connectivity).collect(),
                raw.clone(),
            ),
            _ => Self::BlobNone,
        }
    }

    /// Whether blob sizes are needed to decide.
    pub fn needs_size(&self) -> bool {
        match self {
            Self::BlobLimit(_) => true,
            Self::Combine(parts, _) => parts.iter().any(Self::needs_size),
            _ => false,
        }
    }
}

fn size(text: &str) -> Option<u64> {
    let (digits, unit) = match text.char_indices().last()? {
        (i, c) if c.is_ascii_alphabetic() => (&text[..i], c.to_ascii_lowercase()),
        _ => (text, ' '),
    };
    let factor: u64 = match unit {
        ' ' => 1,
        'k' => 1 << 10,
        'm' => 1 << 20,
        'g' => 1 << 30,
        _ => return None,
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u64>().ok()?.checked_mul(factor)
}

fn decode(part: &str) -> Option<String> {
    let bytes = part.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = part.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn encode(part: &str) -> String {
    let mut out = String::new();
    for c in part.chars() {
        if matches!(c, '%' | '+' | ' ' | '\n' | '\r') {
            out.push_str(&format!("%{:02x}", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_parse_and_print_back() {
        for (spec, f) in [
            ("blob:none", ObjectFilter::BlobNone),
            ("blob:limit=100", ObjectFilter::BlobLimit(100)),
            ("blob:limit=2k", ObjectFilter::BlobLimit(2048)),
            ("tree:0", ObjectFilter::TreeDepth(0)),
        ] {
            assert_eq!(ObjectFilter::parse(spec).unwrap(), f);
        }
        let c = ObjectFilter::parse("combine:blob:none+tree:2").unwrap();
        assert_eq!(c.spec(), "combine:blob:none+tree:2");
        assert_eq!(
            ObjectFilter::parse("combine:tree:3").unwrap().spec(),
            "tree:3"
        );
        for bad in [
            "bogus",
            "blob:limit=",
            "blob:limit=1x",
            "tree:x",
            "combine:blob:none+%zz",
        ] {
            assert!(
                matches!(ObjectFilter::parse(bad), Err(Error::Invalid { .. })),
                "{bad}"
            );
        }
        assert!(matches!(
            ObjectFilter::parse("sparse:oid=abc"),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn omission_follows_the_kind_size_and_depth() {
        let limit = ObjectFilter::BlobLimit(10);
        assert!(limit.omits(Kind::Blob, 1, 10) && !limit.omits(Kind::Blob, 1, 9));
        assert!(!limit.omits(Kind::Tree, 5, 99));
        let depth = ObjectFilter::TreeDepth(1);
        assert!(!depth.omits(Kind::Tree, 0, 0) && depth.omits(Kind::Tree, 1, 0));
        assert!(!depth.omits(Kind::Commit, 9, 0));
    }
}
