//! An organization's repository defaults, applied where a repository's own policy file says nothing.

/// A field that is `None` is not set by the organization, so the built-in default applies.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OrgSettings {
    /// Whether exclusive files are read-only in a work tree until their lock is held.
    pub exclusive_read_only: Option<bool>,
}

/// Where an effective read-only default came from, most specific first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOnlySource {
    Repository,
    Organization,
    Default,
}

impl ReadOnlySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Repository => "repository",
            Self::Organization => "organization",
            Self::Default => "default",
        }
    }
}

/// The read-only default in force for a repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadOnlyDefault {
    pub enabled: bool,
    pub source: ReadOnlySource,
}

impl ReadOnlyDefault {
    /// The repository's policy file, then the organization's setting, then on.
    pub fn resolve(file: Option<bool>, org: Option<bool>) -> Self {
        match (file, org) {
            (Some(enabled), _) => Self {
                enabled,
                source: ReadOnlySource::Repository,
            },
            (None, Some(enabled)) => Self {
                enabled,
                source: ReadOnlySource::Organization,
            },
            (None, None) => Self {
                enabled: true,
                source: ReadOnlySource::Default,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_repository_beats_the_organization_beats_the_built_in_default() {
        use ReadOnlySource::{Default, Organization, Repository};
        for (file, org, enabled, source) in [
            (None, None, true, Default),
            (None, Some(false), false, Organization),
            (None, Some(true), true, Organization),
            (Some(false), Some(true), false, Repository),
            (Some(true), Some(false), true, Repository),
            (Some(false), None, false, Repository),
        ] {
            let got = ReadOnlyDefault::resolve(file, org);
            assert_eq!(
                (got.enabled, got.source),
                (enabled, source),
                "{file:?} {org:?}"
            );
        }
    }
}
