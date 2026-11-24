//! Owner and repository names in request paths are URL forms: any casing and `%20` for a space reach the canonical name.

use axum::extract::Request;
use axum::http::Uri;
use pyn_core::UserId;
use pyn_core::account::normalize_name;

/// Name positions as path segments after `/v1/`: `*` is any segment, `@` an owner name, `#` a repository name.
const SHAPES: [&str; 13] = [
    "repos/@",
    "repos/@/#",
    "repos/*/*/members/@",
    "orgs/@",
    "orgs/*/members/@",
    "orgs/*/invitations/@",
    "orgs/*/teams/*/members/@",
    "users/@",
    "owners/@",
    "admin/users/@",
    "admin/owners/@",
    "admin/orgs/@",
    "me/invitations/@",
];

/// The account a typed name in a body or query refers to.
pub(crate) fn typed_owner(name: impl AsRef<str>) -> UserId {
    UserId::new(normalize_name(name.as_ref()))
}

/// The canonical name for one raw path segment, or `None` when it is already canonical or not a name form.
fn canonical_segment(raw: &str, dots: bool) -> Option<String> {
    let spaced = raw.replace("%20", " ");
    let name = normalize_name(&spaced);
    let plain = !name.is_empty()
        && name.bytes().all(|b| {
            b.is_ascii_lowercase()
                || b.is_ascii_digit()
                || b == b'-'
                || b == b'_'
                || (dots && b == b'.')
        });
    (plain && name != raw).then_some(name)
}

pub(crate) fn canonical_path(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = path.split('/').collect();
    if parts.get(1) != Some(&"v1") {
        return None;
    }
    let mut names = Vec::new();
    for shape in SHAPES {
        let want: Vec<&str> = shape.split('/').collect();
        let rest = &parts[2..];
        if rest.len() < want.len() {
            continue;
        }
        let fits = want
            .iter()
            .zip(rest)
            .all(|(w, got)| matches!(*w, "@" | "#" | "*") || w == got);
        if fits {
            names.extend(
                want.iter()
                    .enumerate()
                    .filter(|(_, w)| matches!(**w, "@" | "#"))
                    .map(|(i, w)| (2 + i, *w == "#")),
            );
        }
    }
    let renamed: Vec<(usize, String)> = names
        .into_iter()
        .filter_map(|(i, dots)| canonical_segment(parts[i], dots).map(|n| (i, n)))
        .collect();
    if renamed.is_empty() {
        return None;
    }
    for (i, n) in &renamed {
        parts[*i] = n;
    }
    Some(parts.join("/"))
}

pub(crate) fn canonicalize(mut req: Request) -> Request {
    let Some(path) = canonical_path(req.uri().path()) else {
        return req;
    };
    let query = req
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    if let Ok(uri) = format!("{path}{query}").parse::<Uri>() {
        *req.uri_mut() = uri;
    }
    req
}

#[cfg(test)]
mod tests {
    use super::canonical_path;

    #[test]
    fn owner_segments_are_canonicalised_and_other_paths_are_left_alone() {
        let c = |p| canonical_path(p);
        assert_eq!(
            c("/v1/repos/Test%20Studio/Game/files").as_deref(),
            Some("/v1/repos/test-studio/game/files")
        );
        assert_eq!(
            c("/v1/orgs/TeSt-StuDiO").as_deref(),
            Some("/v1/orgs/test-studio")
        );
        assert_eq!(
            c("/v1/admin/users/Alice/approve").as_deref(),
            Some("/v1/admin/users/alice/approve")
        );
        assert_eq!(
            c("/v1/me/invitations/ACME/accept").as_deref(),
            Some("/v1/me/invitations/acme/accept")
        );
        assert_eq!(
            c("/v1/repos/Alice/Game/members/BOB").as_deref(),
            Some("/v1/repos/alice/game/members/bob")
        );
        assert_eq!(
            c("/v1/orgs/Acme/teams/Core/members/BOB").as_deref(),
            Some("/v1/orgs/acme/teams/Core/members/bob")
        );
        assert_eq!(
            c("/v1/repos/Alice/My%20Game/files").as_deref(),
            Some("/v1/repos/alice/my-game/files")
        );
        assert_eq!(
            c("/v1/repos/alice/Game.V2").as_deref(),
            Some("/v1/repos/alice/game.v2")
        );
        assert_eq!(c("/v1/repos/alice/game"), None);
        assert_eq!(c("/v1/repos/alice/g%C3%A9"), None);
        assert_eq!(c("/v1/repos"), None);
        assert_eq!(c("/v1/admin/users"), None);
        assert_eq!(c("/v1/repos/a%2Fb/x"), None);
        assert_eq!(c("/v1/repos/%C3%A9/x"), None);
        assert_eq!(c("/v1/session/Alice"), None);
        assert_eq!(c("/healthz"), None);
    }
}
