//! Git's abbreviation length and uniqueness policy, shared by display and revision lookup.

use pyn_object::ObjectId;

use crate::{Error, Repo, Result};

/// A display's minimum width and the object ids that can make its prefixes ambiguous.
pub struct Abbreviations {
    len: usize,
    hex_len: usize,
    ids: Vec<String>,
}

impl Abbreviations {
    pub fn minimum_len(&self) -> usize {
        self.len
    }

    /// The shortest unique prefix at least as long as the selected minimum.
    pub fn short(&self, id: &ObjectId) -> String {
        self.short_hex(&id.to_hex())
    }

    /// The null sentinel is abbreviated against the same object ids as real objects.
    pub fn short_null(&self) -> String {
        self.short_hex(&"0".repeat(self.hex_len))
    }

    pub fn short_hex(&self, hex: &str) -> String {
        let mut len = self.len.min(hex.len());
        let start = self
            .ids
            .partition_point(|other| other.as_str() < &hex[..len]);
        for other in &self.ids[start..] {
            if !other.starts_with(&hex[..self.len.min(hex.len())]) {
                break;
            }
            if other != hex {
                let shared = hex
                    .bytes()
                    .zip(other.bytes())
                    .take_while(|(a, b)| a == b)
                    .count();
                len = len.max(shared + 1);
            }
        }
        hex[..len.min(hex.len())].to_string()
    }
}

fn configured_length(value: &str, hex: usize) -> Result<usize> {
    let text = value.trim_start();
    let text = text.strip_prefix('+').unwrap_or(text);
    let (digits, multiplier) = match text.as_bytes().last() {
        Some(b'k' | b'K') => (&text[..text.len() - 1], 1u64 << 10),
        Some(b'm' | b'M') => (&text[..text.len() - 1], 1u64 << 20),
        Some(b'g' | b'G') => (&text[..text.len() - 1], 1u64 << 30),
        _ => (text, 1),
    };
    let (radix, digits) = if digits.starts_with("0x") || digits.starts_with("0X") {
        (16, &digits[2..])
    } else if digits.starts_with('0') {
        (8, digits)
    } else {
        (10, digits)
    };
    let len = u64::from_str_radix(digits, radix)
        .ok()
        .filter(|_| digits.chars().all(|digit| digit.is_digit(radix)))
        .and_then(|n| n.checked_mul(multiplier))
        .filter(|n| (4..=i32::MAX as u64).contains(n))
        .ok_or_else(|| Error::Invalid(format!("invalid core.abbrev: {value}")))?;
    Ok(len.min(hex as u64) as usize)
}

impl Repo {
    /// The configured minimum; explicit log/show widths override it, while a bare flag retains it.
    pub fn abbreviation_len(&self, explicit: Option<usize>) -> Result<usize> {
        let hex = self.algorithm().digest_len() * 2;
        let entry = self.config().entries().iter().rev().find(|entry| {
            entry.section == "core" && entry.subsection.is_none() && entry.key == "abbrev"
        });
        let configured = match entry {
            None => None,
            Some(entry) => match entry.value.as_deref() {
                None => return Err(Error::Invalid("missing value for core.abbrev".into())),
                Some(value) if value.eq_ignore_ascii_case("auto") => None,
                Some(value)
                    if ["no", "off", "false", ""]
                        .iter()
                        .any(|v| value.eq_ignore_ascii_case(v)) =>
                {
                    Some(hex)
                }
                Some(value) => Some(configured_length(value, hex)?),
            },
        };
        if let Some(len) = explicit {
            return Ok(len.clamp(4, hex));
        }
        match configured {
            Some(len) => Ok(len),
            None => {
                let count = self.objects().approximate_object_count()?;
                let bits = usize::BITS - count.leading_zeros();
                Ok((bits as usize).div_ceil(2).max(7).min(hex))
            }
        }
    }

    /// A context for rendering many ids without rescanning the object store for each one.
    pub fn abbreviations(&self, explicit: Option<usize>) -> Result<Abbreviations> {
        Ok(Abbreviations {
            len: self.abbreviation_len(explicit)?,
            hex_len: self.algorithm().digest_len() * 2,
            ids: self
                .objects()
                .ids_with_prefix("")?
                .iter()
                .map(ObjectId::to_hex)
                .collect(),
        })
    }

    /// One id using the same policy, looking up only objects that share its initial prefix.
    pub fn abbreviate(&self, id: &ObjectId) -> Result<String> {
        let len = self.abbreviation_len(None)?;
        let ids = self.objects().ids_with_prefix(&id.to_hex()[..len])?;
        Ok(Abbreviations {
            len,
            hex_len: self.algorithm().digest_len() * 2,
            ids: ids.iter().map(ObjectId::to_hex).collect(),
        }
        .short(id))
    }
}
