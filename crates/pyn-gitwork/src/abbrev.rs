//! Git's abbreviation length and uniqueness policy, shared by display and revision lookup.

use pyn_object::ObjectId;

use crate::{Error, Repo, Result};

/// A display's minimum width and a handle to its object database.
pub struct Abbreviations {
    len: usize,
    objects: std::sync::Arc<pyn_git::ObjectDb>,
}

impl Abbreviations {
    /// The shortest unique prefix at least as long as the selected minimum.
    pub fn short(&self, id: &ObjectId) -> String {
        let hex = id.to_hex();
        hex[..self.objects.abbreviation_len(id, self.len)].to_string()
    }

    /// The null sentinel is abbreviated against the same object ids as real objects.
    pub fn short_null(&self) -> String {
        let algorithm = self.objects.algorithm();
        let id = ObjectId::from_bytes(algorithm, &[0; 32][..algorithm.digest_len()])
            .expect("the repository's digest width");
        self.short(&id)
    }

    /// Abbreviates a normalized lowercase ASCII hex id at the repository's full hash width (40 or 64 digits).
    pub fn short_hex(&self, hex: &str) -> String {
        self.short(&ObjectId::from_hex(self.objects.algorithm(), hex).expect("a full object id"))
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
        let default = self
            .abbrev
            .get_or_init(|| self.default_abbreviation_len().map_err(|e| e.to_string()));
        let len = *default.as_ref().map_err(|e| Error::Invalid(e.clone()))?;
        Ok(explicit.map_or(len, |n| n.clamp(4, self.algorithm().digest_len() * 2)))
    }

    fn default_abbreviation_len(&self) -> Result<usize> {
        let hex = self.algorithm().digest_len() * 2;
        let mut configured = None;
        for entry in self.config().entries().iter().filter(|entry| {
            entry.section == "core" && entry.subsection.is_none() && entry.key == "abbrev"
        }) {
            configured = match entry.value.as_deref() {
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
            };
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

    /// A context that looks up each rendered id lazily in the object store.
    pub fn abbreviations(&self, explicit: Option<usize>) -> Result<Abbreviations> {
        Ok(Abbreviations {
            len: self.abbreviation_len(explicit)?,
            objects: self.objects.clone(),
        })
    }

    /// One id using the same lazy lookup as a multi-id renderer.
    pub fn abbreviate(&self, id: &ObjectId) -> Result<String> {
        Ok(self.abbreviations(None)?.short(id))
    }
}
