//! Account profiles: the optional, user-edited fields and the avatar record, with their validation.
//! Avatar bytes are checked by magic number and size only; nothing here decodes an image.

use pyn_object::ObjectId;

use crate::error::{PynError, Result};

pub const MAX_DISPLAY_NAME_CHARS: usize = 64;
pub const MAX_BIO_CHARS: usize = 160;
pub const MAX_LOCATION_CHARS: usize = 64;
pub const MAX_COMPANY_CHARS: usize = 64;
pub const MAX_LINKS: usize = 5;
pub const MAX_LINK_BYTES: usize = 200;
pub const MAX_ORG_DESCRIPTION_CHARS: usize = 256;
pub const MAX_ORG_DISPLAY_NAME_CHARS: usize = 80;
/// The largest avatar upload.
pub const MAX_AVATAR_BYTES: u64 = 1 << 20;

/// The profile text a user edits. Every field is optional; `links` may be empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileFields {
    pub display_name: Option<String>,
    pub bio: Option<String>,
    pub location: Option<String>,
    pub company: Option<String>,
    pub links: Vec<String>,
}

/// The image formats an avatar may be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Png,
    Jpeg,
    Webp,
}

impl ImageKind {
    pub fn media_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
        }
    }

    pub fn from_media_type(s: &str) -> Option<Self> {
        [Self::Png, Self::Jpeg, Self::Webp]
            .into_iter()
            .find(|k| k.media_type() == s)
    }

    /// The format the bytes start as, judged from their magic numbers alone.
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        if bytes.len() >= 33 && bytes.starts_with(b"\x89PNG\r\n\x1a\n") && &bytes[12..16] == b"IHDR"
        {
            return Some(Self::Png);
        }
        if bytes.len() >= 4 && bytes.starts_with(&[0xFF, 0xD8, 0xFF]) && bytes[3] >= 0xC0 {
            return Some(Self::Jpeg);
        }
        if bytes.len() >= 20
            && bytes.starts_with(b"RIFF")
            && &bytes[8..12] == b"WEBP"
            && matches!(&bytes[12..16], b"VP8 " | b"VP8L" | b"VP8X")
        {
            return Some(Self::Webp);
        }
        None
    }
}

/// A stored avatar: where its bytes are in the object store and what they are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Avatar {
    /// The SHA-256 of the bytes, which is also their key in the object store.
    pub content: ObjectId,
    pub kind: ImageKind,
    pub size: u64,
}

/// Which profile fields other people may see. Every field is visible by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileVisibility {
    pub display_name: bool,
    pub bio: bool,
    pub location: bool,
    pub company: bool,
    pub links: bool,
    pub avatar: bool,
    pub starred: bool,
}

impl Default for ProfileVisibility {
    fn default() -> Self {
        Self {
            display_name: true,
            bio: true,
            location: true,
            company: true,
            links: true,
            avatar: true,
            starred: true,
        }
    }
}

/// Everything stored about an account's profile.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Profile {
    pub fields: ProfileFields,
    pub avatar: Option<Avatar>,
    pub visibility: ProfileVisibility,
}

impl Profile {
    /// The profile as anyone but its owner sees it: hidden fields and avatar are absent.
    pub fn public_view(&self) -> Self {
        let v = self.visibility;
        let f = &self.fields;
        Self {
            fields: ProfileFields {
                display_name: f.display_name.clone().filter(|_| v.display_name),
                bio: f.bio.clone().filter(|_| v.bio),
                location: f.location.clone().filter(|_| v.location),
                company: f.company.clone().filter(|_| v.company),
                links: if v.links { f.links.clone() } else { Vec::new() },
            },
            avatar: self.avatar.clone().filter(|_| v.avatar),
            visibility: v,
        }
    }
}

/// The profile of a repository owner: a user's or an organization's, by the account's kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerProfile {
    User(Profile),
    Org(OrgProfile),
}

/// What an organization shows about itself. Nothing in it is hidden from viewers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrgProfile {
    pub description: Option<String>,
    pub display_name: Option<String>,
    pub url: Option<String>,
    pub avatar: Option<Avatar>,
}

/// The organization profile text an owner edits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrgProfileFields {
    pub description: Option<String>,
    pub display_name: Option<String>,
    pub url: Option<String>,
}

impl OrgProfileFields {
    /// Trims and checks each field; blank becomes `None`. The description may span lines, the display name may not,
    /// and the url must be an http or https address without credentials.
    pub fn validated(self) -> Result<Self> {
        Ok(Self {
            description: text(
                "description",
                self.description,
                MAX_ORG_DESCRIPTION_CHARS,
                true,
            )?,
            display_name: text(
                "display name",
                self.display_name,
                MAX_ORG_DISPLAY_NAME_CHARS,
                false,
            )?,
            url: match self.url.as_deref().map(str::trim) {
                None | Some("") => None,
                Some(url) => Some(link("url", url)?),
            },
        })
    }
}

/// Checks avatar bytes (size, then magic numbers) and returns their format.
pub fn check_avatar(bytes: &[u8]) -> Result<ImageKind> {
    check_image(bytes, "avatar")
}

/// The avatar checks for any uploaded image; `what` names it in the messages.
pub fn check_image(bytes: &[u8], what: &str) -> Result<ImageKind> {
    if bytes.is_empty() {
        return Err(PynError::InvalidRequest(format!("the {what} is empty")));
    }
    if bytes.len() as u64 > MAX_AVATAR_BYTES {
        return Err(PynError::ObjectTooLarge {
            limit: MAX_AVATAR_BYTES,
        });
    }
    ImageKind::sniff(bytes).ok_or_else(|| {
        PynError::UnsupportedImage(format!("an {what} must be a png, jpeg or webp image"))
    })
}

fn bad(field: &str, why: &str) -> PynError {
    PynError::InvalidRequest(format!("invalid {field}: {why}"))
}

/// Trims `value`; blank becomes `None`. Longer than `max` characters, or holding a control character
/// (a newline too unless `multiline`), is refused.
pub(crate) fn text(
    field: &str,
    value: Option<String>,
    max: usize,
    multiline: bool,
) -> Result<Option<String>> {
    let Some(value) = value else { return Ok(None) };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.chars().count() > max {
        return Err(bad(field, &format!("at most {max} characters")));
    }
    if value
        .chars()
        .any(|c| c.is_control() && !(multiline && c == '\n'))
    {
        return Err(bad(field, "no control characters"));
    }
    Ok(Some(value.to_string()))
}

pub(crate) fn link(field: &str, raw: &str) -> Result<String> {
    let url = raw.trim();
    if url.len() > MAX_LINK_BYTES {
        return Err(bad(field, &format!("at most {MAX_LINK_BYTES} bytes")));
    }
    if !url.is_ascii()
        || url
            .bytes()
            .any(|b| b.is_ascii_control() || b == b' ' || b == b'\\')
    {
        return Err(bad(
            "link",
            "ASCII only, with no spaces, control characters or backslashes",
        ));
    }
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| bad(field, "expected http:// or https://"))?;
    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") {
        return Err(bad(field, "the scheme must be http or https"));
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') {
        return Err(bad(field, "no user name or password in the url"));
    }
    let host = match authority.rsplit_once(':') {
        Some((host, port)) if port.bytes().all(|b| b.is_ascii_digit()) && !port.is_empty() => host,
        Some(_) if !authority.ends_with(']') => return Err(bad(field, "the port is not valid")),
        _ => authority,
    };
    let host_ok = match host.strip_prefix('[') {
        Some(inner) => inner.strip_suffix(']').is_some_and(|a| {
            !a.is_empty()
                && a.chars()
                    .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.')
        }),
        None => {
            host.len() <= 253
                && host.split('.').all(|l| {
                    !l.is_empty()
                        && l.len() <= 63
                        && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                })
        }
    };
    if !host_ok {
        return Err(bad(field, "the host is not valid"));
    }
    Ok(url.to_string())
}

impl ProfileFields {
    /// Trims and checks every field; blank text becomes `None` and blank or repeated links are dropped.
    pub fn validated(self) -> Result<Self> {
        let mut links: Vec<String> = Vec::new();
        for raw in &self.links {
            if raw.trim().is_empty() {
                continue;
            }
            let url = link("link", raw)?;
            if !links.contains(&url) {
                links.push(url);
            }
        }
        if links.len() > MAX_LINKS {
            return Err(bad("links", &format!("at most {MAX_LINKS}")));
        }
        Ok(Self {
            display_name: text(
                "display name",
                self.display_name,
                MAX_DISPLAY_NAME_CHARS,
                false,
            )?,
            bio: text("bio", self.bio, MAX_BIO_CHARS, true)?,
            location: text("location", self.location, MAX_LOCATION_CHARS, false)?,
            company: text("company", self.company, MAX_COMPANY_CHARS, false)?,
            links,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(f: impl FnOnce(&mut ProfileFields)) -> Result<ProfileFields> {
        let mut p = ProfileFields::default();
        f(&mut p);
        p.validated()
    }

    #[test]
    fn text_is_trimmed_and_blank_is_cleared() {
        let p = fields(|p| {
            p.display_name = Some("  Ada Lovelace \t".into());
            p.bio = Some("   ".into());
        })
        .unwrap();
        assert_eq!(p.display_name.as_deref(), Some("Ada Lovelace"));
        assert_eq!(p.bio, None);
    }

    #[test]
    fn length_limits_count_characters_not_bytes() {
        assert!(fields(|p| p.display_name = Some("é".repeat(64))).is_ok());
        assert!(fields(|p| p.display_name = Some("é".repeat(65))).is_err());
        assert!(fields(|p| p.bio = Some("b".repeat(160))).is_ok());
        assert!(fields(|p| p.bio = Some("b".repeat(161))).is_err());
        assert!(fields(|p| p.location = Some("l".repeat(65))).is_err());
        assert!(fields(|p| p.company = Some("c".repeat(65))).is_err());
    }

    #[test]
    fn control_characters_are_refused_and_only_a_bio_may_hold_newlines() {
        assert!(fields(|p| p.display_name = Some("a\u{1b}[2J".into())).is_err());
        assert!(fields(|p| p.display_name = Some("a\nb".into())).is_err());
        assert!(fields(|p| p.location = Some("a\u{7}".into())).is_err());
        assert!(fields(|p| p.bio = Some("line one\nline two".into())).is_ok());
        assert!(fields(|p| p.bio = Some("a\rb".into())).is_err());
    }

    #[test]
    fn links_must_be_http_urls_without_credentials() {
        let ok = |s: &str| fields(|p| p.links = vec![s.into()]);
        assert!(ok("https://example.com").is_ok());
        assert!(ok("http://example.com:8080/a?b=c#d").is_ok());
        assert!(ok("https://[::1]:8443/x").is_ok());
        for bad in [
            "ftp://example.com",
            "javascript:alert(1)",
            "example.com",
            "https://user:pw@example.com",
            "https://exa mple.com",
            "https://",
            "https://example.com:port",
            "https://exämple.com",
            "https://a..b",
        ] {
            assert!(ok(bad).is_err(), "{bad}");
        }
        assert!(ok(&format!("https://example.com/{}", "a".repeat(200))).is_err());
    }

    #[test]
    fn at_most_five_distinct_links() {
        let many = |n: usize| (0..n).map(|i| format!("https://e{i}.example")).collect();
        assert!(fields(|p| p.links = many(5)).is_ok());
        assert!(fields(|p| p.links = many(6)).is_err());
        let p = fields(|p| {
            p.links = vec![
                "https://a.example".into(),
                " ".into(),
                "https://a.example".into(),
            ]
        })
        .unwrap();
        assert_eq!(p.links, ["https://a.example"]);
    }

    #[test]
    fn an_org_description_is_trimmed_bounded_and_may_span_lines() {
        let d = |s: &str| {
            OrgProfileFields {
                description: Some(s.into()),
                ..Default::default()
            }
            .validated()
            .map(|f| f.description)
        };
        assert_eq!(d("  two\nlines ").unwrap().as_deref(), Some("two\nlines"));
        assert_eq!(d("   ").unwrap(), None);
        assert!(d(&"d".repeat(256)).is_ok());
        assert!(d(&"d".repeat(257)).is_err());
        assert!(d("a\u{1b}b").is_err());
        assert_eq!(
            OrgProfileFields::default().validated().unwrap(),
            Default::default()
        );
    }

    #[test]
    fn an_org_display_name_and_url_are_bounded_and_the_url_is_plain_http() {
        let f = |name: Option<&str>, url: Option<&str>| {
            OrgProfileFields {
                display_name: name.map(Into::into),
                url: url.map(Into::into),
                ..Default::default()
            }
            .validated()
        };
        let ok = f(Some("  Acme Inc "), Some(" https://acme.example/x ")).unwrap();
        assert_eq!(ok.display_name.as_deref(), Some("Acme Inc"));
        assert_eq!(ok.url.as_deref(), Some("https://acme.example/x"));
        assert_eq!(f(Some("  "), Some("")).unwrap(), Default::default());
        assert!(f(Some(&"é".repeat(80)), None).is_ok());
        assert!(f(Some(&"é".repeat(81)), None).is_err());
        assert!(f(Some("a\nb"), None).is_err());
        for bad in [
            "ftp://a.example",
            "javascript:alert(1)",
            "acme.example",
            "https://u:p@a.example",
        ] {
            assert!(f(None, Some(bad)).is_err(), "{bad}");
        }
        assert!(
            f(
                None,
                Some(&format!("https://a.example/{}", "a".repeat(200)))
            )
            .is_err()
        );
    }

    fn png() -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        b.resize(40, 0);
        b
    }

    #[test]
    fn avatars_are_sniffed_from_their_magic_numbers() {
        assert_eq!(check_avatar(&png()).unwrap(), ImageKind::Png);
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0];
        jpeg.resize(30, 0);
        assert_eq!(check_avatar(&jpeg).unwrap(), ImageKind::Jpeg);
        let mut webp = b"RIFF\x20\0\0\0WEBPVP8 ".to_vec();
        webp.resize(40, 0);
        assert_eq!(check_avatar(&webp).unwrap(), ImageKind::Webp);
        for junk in [
            &b"GIF89a......................"[..],
            b"<svg xmlns=''/>",
            b"%PDF-1.7",
        ] {
            assert!(matches!(
                check_avatar(junk),
                Err(PynError::UnsupportedImage(_))
            ));
        }
        assert!(check_avatar(b"").is_err());
    }

    #[test]
    fn an_avatar_over_the_limit_is_too_large_even_if_it_is_an_image() {
        let mut big = png();
        big.resize(MAX_AVATAR_BYTES as usize + 1, 0);
        assert!(matches!(
            check_avatar(&big),
            Err(PynError::ObjectTooLarge { .. })
        ));
        big.truncate(MAX_AVATAR_BYTES as usize);
        assert!(check_avatar(&big).is_ok());
    }
}
