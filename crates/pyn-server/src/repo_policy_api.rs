//! An organization's repository-creation policy.

use std::str::FromStr;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use pyn_core::{
    CreationEffect, CreationRule, CreationScope, CreationSubject, MemberCreation, RepoPolicy,
    SubjectKind, UserId,
};
use pyn_proto as api;

use crate::{ApiResult, AppState, identify};

fn rule_info(rule: CreationRule) -> api::CreationRuleInfo {
    api::CreationRuleInfo {
        effect: rule.effect.to_string(),
        kind: rule.subject.kind().to_string(),
        subject: rule.subject.name(),
        scope: rule.scope.to_string(),
    }
}

fn policy_info(policy: RepoPolicy) -> api::RepoPolicyInfo {
    api::RepoPolicyInfo {
        member_creation: policy.base.to_string(),
        rules: policy.rules.into_iter().map(rule_info).collect(),
    }
}

fn subject(kind: &str, name: &str) -> pyn_core::Result<CreationSubject> {
    CreationSubject::parse(SubjectKind::from_str(kind)?, name)
}

#[utoipa::path(get, path = "/v1/orgs/{org}/repo-policy",
    params(("org" = String, Path, description = "the organization's name")),
    responses((status = 200, body = api::RepoPolicyInfo),
              (status = 403, body = api::ErrorBody, description = "not_org_owner"),
              (status = 404, body = api::ErrorBody, description = "org_not_found")))]
pub(crate) async fn get_repo_policy(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(org): Path<String>,
) -> ApiResult<Json<api::RepoPolicyInfo>> {
    let who = identify(&s, &headers).await?;
    let policy = s.access.repo_policy(&who, &UserId::new(org)).await?;
    Ok(Json(policy_info(policy)))
}

#[utoipa::path(put, path = "/v1/orgs/{org}/repo-policy",
    params(("org" = String, Path, description = "the organization's name")),
    request_body = api::SetRepoPolicyRequest,
    responses((status = 200, body = api::RepoPolicyInfo, description = "the policy after the change; rules are untouched"),
              (status = 400, body = api::ErrorBody, description = "invalid_request: member_creation is not none, private or both"),
              (status = 403, body = api::ErrorBody, description = "not_org_owner, or a token without manage_roles or limited to repositories"),
              (status = 404, body = api::ErrorBody, description = "org_not_found")))]
pub(crate) async fn set_repo_policy(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(org): Path<String>,
    Json(req): Json<api::SetRepoPolicyRequest>,
) -> ApiResult<Json<api::RepoPolicyInfo>> {
    let who = identify(&s, &headers).await?;
    let base = MemberCreation::from_str(&req.member_creation)?;
    let policy = s
        .access
        .set_member_creation(&who, &UserId::new(org), base)
        .await?;
    Ok(Json(policy_info(policy)))
}

#[utoipa::path(put, path = "/v1/orgs/{org}/repo-policy/rules/{effect}/{kind}/{subject}",
    params(("org" = String, Path, description = "the organization's name"),
           ("effect" = api::CreationEffect, Path),
           ("kind" = api::CreationSubjectKind, Path),
           ("subject" = String, Path, description = "a team slug, an organization member's user name, or owner or member")),
    request_body = api::SetCreationRuleRequest,
    responses((status = 200, body = api::CreationRuleInfo, description = "adds the rule or replaces its scope"),
              (status = 400, body = api::ErrorBody, description = "invalid_request: bad effect, kind, subject or scope, or a deny for the owner role"),
              (status = 403, body = api::ErrorBody, description = "not_org_owner, or a token without manage_roles or limited to repositories"),
              (status = 404, body = api::ErrorBody, description = "org_not_found or team_not_found"),
              (status = 409, body = api::ErrorBody, description = "user_not_org_member")))]
pub(crate) async fn set_creation_rule(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((org, effect, kind, name)): Path<(String, String, String, String)>,
    Json(req): Json<api::SetCreationRuleRequest>,
) -> ApiResult<Json<api::CreationRuleInfo>> {
    let who = identify(&s, &headers).await?;
    let rule = CreationRule {
        effect: CreationEffect::from_str(&effect)?,
        subject: subject(&kind, &name)?,
        scope: CreationScope::from_str(&req.scope)?,
    };
    let rule = s
        .access
        .set_creation_rule(&who, &UserId::new(org), rule)
        .await?;
    Ok(Json(rule_info(rule)))
}

#[utoipa::path(delete, path = "/v1/orgs/{org}/repo-policy/rules/{effect}/{kind}/{subject}",
    params(("org" = String, Path, description = "the organization's name"),
           ("effect" = api::CreationEffect, Path),
           ("kind" = api::CreationSubjectKind, Path),
           ("subject" = String, Path, description = "a team slug, a user name, or owner or member")),
    responses((status = 204, description = "rule removed"),
              (status = 400, body = api::ErrorBody, description = "invalid_request: bad effect, kind or subject"),
              (status = 403, body = api::ErrorBody, description = "not_org_owner, or a token without manage_roles or limited to repositories"),
              (status = 404, body = api::ErrorBody, description = "org_not_found or creation_rule_not_found")))]
pub(crate) async fn remove_creation_rule(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((org, effect, kind, name)): Path<(String, String, String, String)>,
) -> ApiResult<StatusCode> {
    let who = identify(&s, &headers).await?;
    s.access
        .remove_creation_rule(
            &who,
            &UserId::new(org),
            &subject(&kind, &name)?,
            CreationEffect::from_str(&effect)?,
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
