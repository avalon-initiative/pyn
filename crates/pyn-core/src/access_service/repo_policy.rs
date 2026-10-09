//! Who may create repositories in an organization: owners always, others by the organization's policy.

use super::*;
use crate::repo::Visibility;
use crate::repo_policy::{
    CreationEffect, CreationRule, CreationSubject, MemberCreation, RepoPolicy, Standing,
};

impl AccessService {
    /// The member's role if they may create a `visibility` repository in the organization.
    /// A non-member gets `NotOrgMember`; a member the policy refuses gets `RepoCreateForbidden`.
    pub async fn require_repo_creator(
        &self,
        org: &UserId,
        user: &UserId,
        visibility: Visibility,
    ) -> Result<OrgRole> {
        let Some(role) = self.store.org_role(org, user).await? else {
            return Err(PynError::NotOrgMember(org.to_string()));
        };
        if role != OrgRole::Owner {
            let standing = Standing {
                user: user.clone(),
                role,
                teams: self.store.teams_of(org, user).await?,
            };
            if !self
                .store
                .repo_policy(org)
                .await?
                .permits(&standing, visibility)
            {
                return Err(PynError::RepoCreateForbidden {
                    org: org.to_string(),
                    scope: visibility.to_string(),
                });
            }
        }
        Ok(role)
    }

    async fn require_policy_admin(&self, actor: &Identity, org: &UserId) -> Result<()> {
        actor.require_namespace_management()?;
        self.org(org).await?;
        self.require_org_owner(org, &actor.user).await
    }

    /// The organization's repository-creation policy. Owners only.
    pub async fn repo_policy(&self, actor: &Identity, org: &UserId) -> Result<RepoPolicy> {
        self.org(org).await?;
        self.require_org_owner(org, &actor.user).await?;
        self.store.repo_policy(org).await
    }

    /// Sets what members may create when no rule applies. Owners only.
    pub async fn set_member_creation(
        &self,
        actor: &Identity,
        org: &UserId,
        base: MemberCreation,
    ) -> Result<RepoPolicy> {
        self.require_policy_admin(actor, org).await?;
        let old = self.store.set_member_creation(org, base).await?;
        if old != base {
            let detail = format!("members may create: {old} -> {base}");
            self.record(
                AuditScope::Org(org.clone()),
                &actor.user,
                AuditAction::RepoCreationPolicyChanged,
                detail,
            )
            .await?;
        }
        self.store.repo_policy(org).await
    }

    /// Adds or changes the rule for a subject and effect. A team must exist and a user belong to the
    /// organization; owners cannot be denied.
    pub async fn set_creation_rule(
        &self,
        actor: &Identity,
        org: &UserId,
        rule: CreationRule,
    ) -> Result<CreationRule> {
        self.require_policy_admin(actor, org).await?;
        if rule.effect == CreationEffect::Deny
            && rule.subject == CreationSubject::Role(OrgRole::Owner)
        {
            return Err(PynError::InvalidRequest(
                "owners can always create repositories; they cannot be denied".into(),
            ));
        }
        match &rule.subject {
            CreationSubject::Team(slug) => {
                self.require_team(org, slug).await?;
            }
            CreationSubject::User(user) if self.store.org_role(org, user).await?.is_none() => {
                return Err(PynError::UserNotOrgMember {
                    org: org.to_string(),
                    user: user.to_string(),
                });
            }
            _ => {}
        }
        if !self.store.set_creation_rule(org, &rule).await? {
            return Err(match &rule.subject {
                CreationSubject::Team(slug) => PynError::TeamNotFound {
                    org: org.to_string(),
                    team: slug.clone(),
                },
                subject => PynError::UserNotOrgMember {
                    org: org.to_string(),
                    user: subject.name(),
                },
            });
        }
        let detail = format!("{} {} for {}", rule.effect, rule.scope, rule.subject);
        self.record(
            AuditScope::Org(org.clone()),
            &actor.user,
            AuditAction::RepoCreationRuleSet,
            detail,
        )
        .await?;
        Ok(rule)
    }

    /// Removes the rule for a subject and effect; `CreationRuleNotFound` if there is none.
    pub async fn remove_creation_rule(
        &self,
        actor: &Identity,
        org: &UserId,
        subject: &CreationSubject,
        effect: CreationEffect,
    ) -> Result<()> {
        self.require_policy_admin(actor, org).await?;
        let Some(scope) = self
            .store
            .remove_creation_rule(org, subject, effect)
            .await?
        else {
            return Err(PynError::CreationRuleNotFound {
                org: org.to_string(),
                rule: format!("{effect} {subject}"),
            });
        };
        let detail = format!("{effect} {scope} for {subject} removed");
        self.record(
            AuditScope::Org(org.clone()),
            &actor.user,
            AuditAction::RepoCreationRuleRemoved,
            detail,
        )
        .await
    }

    /// The rules naming `subject`, as a note for the audit entry of the removal that drops them.
    pub(super) async fn rules_note(
        &self,
        org: &UserId,
        subject: &CreationSubject,
    ) -> Result<String> {
        let rules: Vec<_> = self
            .store
            .repo_policy(org)
            .await?
            .rules
            .into_iter()
            .filter(|r| &r.subject == subject)
            .map(|r| format!("{} {}", r.effect, r.scope))
            .collect();
        Ok(if rules.is_empty() {
            String::new()
        } else {
            format!("; creation rules dropped: {}", join(rules.iter()))
        })
    }
}
