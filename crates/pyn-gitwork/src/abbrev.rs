//! Git's abbreviation length and uniqueness policy, shared by display and revision lookup.

use pyn_object::ObjectId;

use crate::{Error, Repo, Result};

/// A display's minimum width and the object ids that can make its prefixes ambiguous.
pub struct Abbreviations {
    len: usize,
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

impl Repo {
    /// The configured minimum; explicit log/show widths override it, while a bare flag retains it.
    pub fn abbreviation_len(&self, explicit: Option<usize>) -> Result<usize> {
        let hex = self.algorithm().digest_len() * 2;
        let configured = match self.config().get("core.abbrev") {
            None => None,
            Some(value) if value.eq_ignore_ascii_case("auto") => None,
            Some(value)
                if ["no", "off", "false", ""]
                    .iter()
                    .any(|v| value.eq_ignore_ascii_case(v)) =>
            {
                Some(hex)
            }
            Some(value) => Some(
                value
                    .parse::<usize>()
                    .ok()
                    .filter(|n| (4..=hex).contains(n))
                    .ok_or_else(|| Error::Invalid(format!("invalid core.abbrev: {value}")))?,
            ),
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
            ids: ids.iter().map(ObjectId::to_hex).collect(),
        }
        .short(id))
    }
}
