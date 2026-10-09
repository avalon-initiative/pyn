//! An organization's repository-creation policy: who besides its owners may create repositories, and the one
//! function that decides.

use std::fmt;
use std::str::FromStr;

use crate::access::OrgRole;
use crate::error::{PynError, Result};
use crate::repo::Visibility;
use crate::types::UserId;

macro_rules! word_enum {
    ($(#[$meta:meta])* $name:ident, $what:literal, { $($variant:ident => $word:literal),+ }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $word),+
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = PynError;
            fn from_str(s: &str) -> Result<Self> {
                match s {
                    $($word => Ok(Self::$variant),)+
                    other => Err(PynError::InvalidRequest(format!(
                        "unknown {} {other:?}; use {}",
                        $what,
                        [$($word),+].join(" or ")
                    ))),
                }
            }
        }
    };
}

word_enum!(
    /// What members get with no rule: nothing (owners only), private repositories, or both kinds.
    MemberCreation, "member creation setting", { None => "none", Private => "private", Both => "both" }
);

word_enum!(
    /// The visibilities a rule covers.
    CreationScope, "creation scope", { Public => "public", Private => "private", Both => "both" }
);

word_enum!(
    CreationEffect, "creation effect", { Allow => "allow", Deny => "deny" }
);

word_enum!(
    /// The kind of person or group a rule names.
    SubjectKind, "subject kind", { Team => "team", User => "user", Role => "role" }
);

impl MemberCreation {
    pub fn covers(self, visibility: Visibility) -> bool {
        match self {
            Self::None => false,
            Self::Private => visibility == Visibility::Private,
            Self::Both => true,
        }
    }
}

impl CreationScope {
    pub fn covers(self, visibility: Visibility) -> bool {
        match self {
            Self::Public => visibility == Visibility::Public,
            Self::Private => visibility == Visibility::Private,
            Self::Both => true,
        }
    }
}

/// Whom a rule applies to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum CreationSubject {
    Team(String),
    User(UserId),
    Role(OrgRole),
}

impl CreationSubject {
    pub fn kind(&self) -> SubjectKind {
        match self {
            Self::Team(_) => SubjectKind::Team,
            Self::User(_) => SubjectKind::User,
            Self::Role(_) => SubjectKind::Role,
        }
    }

    /// The team slug, user name or role name.
    pub fn name(&self) -> String {
        match self {
            Self::Team(slug) => slug.clone(),
            Self::User(user) => user.to_string(),
            Self::Role(role) => role.to_string(),
        }
    }

    pub fn parse(kind: SubjectKind, name: &str) -> Result<Self> {
        Ok(match kind {
            SubjectKind::Team => Self::Team(name.to_string()),
            SubjectKind::User => Self::User(UserId::new(name)),
            SubjectKind::Role => Self::Role(name.parse().map_err(|_| {
                PynError::InvalidRequest(format!(
                    "unknown organization role {name:?}; use owner or member"
                ))
            })?),
        })
    }

    fn matches(&self, who: &Standing) -> bool {
        match self {
            Self::Team(slug) => who.teams.contains(slug),
            Self::User(user) => *user == who.user,
            Self::Role(role) => *role == who.role,
        }
    }
}

impl fmt::Display for CreationSubject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.kind(), self.name())
    }
}

/// An allow or deny for a subject over a scope. A subject holds at most one rule per effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreationRule {
    pub subject: CreationSubject,
    pub effect: CreationEffect,
    pub scope: CreationScope,
}

/// A member as the policy sees them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Standing {
    pub user: UserId,
    pub role: OrgRole,
    /// Slugs of the organization's teams the person is in.
    pub teams: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoPolicy {
    pub base: MemberCreation,
    pub rules: Vec<CreationRule>,
}

impl Default for RepoPolicy {
    fn default() -> Self {
        Self {
            base: MemberCreation::None,
            rules: Vec::new(),
        }
    }
}

impl RepoPolicy {
    /// Whether the member may create a repository of `visibility`: owners always; otherwise a matching deny
    /// refuses, else a matching allow or the base setting grants.
    pub fn permits(&self, who: &Standing, visibility: Visibility) -> bool {
        if who.role == OrgRole::Owner {
            return true;
        }
        let matching = self
            .rules
            .iter()
            .filter(|r| r.scope.covers(visibility) && r.subject.matches(who));
        let (mut denied, mut allowed) = (false, false);
        for rule in matching {
            match rule.effect {
                CreationEffect::Deny => denied = true,
                CreationEffect::Allow => allowed = true,
            }
        }
        !denied && (allowed || self.base.covers(visibility))
    }
}
