//! The route table: every route with the credential reach it declares. `routes()` builds the router from it and
//! `openapi()` mirrors each reach as `x-pyn-reach`. See docs/concepts/access.md.

use std::collections::HashMap;
use std::convert::Infallible;

use axum::extract::{RawPathParams, Request, State};
use axum::handler::Handler;
use axum::http::Method;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodFilter, MethodRouter};
use axum::{Extension, Json, Router};
use pyn_core::{
    GrantLevel, GrantLevel::*, GrantNeed, Permission, ServiceScope, TokenResource, TokenResource::*,
};
use utoipa::OpenApi;
use utoipa::openapi::extensions::Extensions;

use crate::{
    AppState, access_api, access_session_api, admin_api, branch_api, caller, deletion_api,
    diff_api, freeze_api, lfs_api, limits_api, merge_api, oidc_api, org_api, org_settings_api,
    passkeys_api, profile_api, push_api, repo_api, repo_policy_api, requirements_api,
    self_service_api, server_invitation_api, session_api, setup_api, team_api, two_factor_api,
    webhook_api,
};

const fn need(resource: TokenResource, level: GrantLevel) -> GrantNeed {
    GrantNeed::new(resource, level)
}

/// Which credentials a route accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// Served in both access modes, signed out or not: no credential is read.
    Open,
    /// No credential is read; a private server refuses a signed-out caller.
    Public,
    /// Signed out or any user credential, a credential that is sent must be valid; a private server refuses a
    /// signed-out caller.
    OptionalSignIn,
    /// Like `OptionalSignIn`, but a signed-in caller needs full scope: a narrower token or a service credential is refused.
    OptionalFullScope,
    /// A web session cookie only (also while the account is scheduled for deletion).
    Session,
    /// Any user credential; the response is narrowed to what the credential's scope allows.
    AnyCredential,
    /// A session, a full-scope token or the development identity.
    FullScope,
    /// Full scope, and also an account scheduled for deletion (the routes that restore it).
    FullScopePending,
    /// Full scope, and also an account that must set up a second factor and has not (the enrolment routes).
    FullScopeEnrolment,
    /// Full scope, or an unscoped token holding `manage_roles`.
    Namespace,
    /// A server administrator on a full-scope credential; service credentials are refused.
    AdminOnly,
    /// A server administrator, or a service credential holding the scope.
    Service(ServiceScope),
    /// A full-scope user (the core then limits it to its own owner), or a service credential holding the scope.
    FullScopeOrService(ServiceScope),
    /// A full-scope credential, or a token owned by the organization in the path that holds the organization
    /// resource at the level. The creator's organization role still applies.
    OrgScoped(GrantNeed),
    /// A full-scope credential, or a token owned by the caller's own account that holds the account resource at
    /// the level.
    AccountScoped(GrantNeed),
    /// A signed-in credential holding the resource at the level in the named repository: a token through its grants, any
    /// other credential through the role permissions `permissions_of` derives for them.
    RepoScoped(GrantNeed),
    /// Signed out or signed in, holding the resource at the level (a public repository's reads need no credential). A
    /// private server answers a signed-out caller as it does for a private repository.
    PublicRepoRead(GrantNeed),
    /// `public_repo_read` for the Git LFS download routes: the handler grants a signed-out caller only a download, and a
    /// refusal is in the LFS error format (`401` on a private server, which the clients answer by asking for credentials).
    PublicLfsRead(GrantNeed),
}

impl Reach {
    /// Whether a private server refuses a signed-out caller here.
    fn closed_when_private(self) -> bool {
        matches!(
            self,
            Self::Public
                | Self::OptionalSignIn
                | Self::OptionalFullScope
                | Self::PublicRepoRead(_)
                | Self::PublicLfsRead(_)
        )
    }

    /// The stable name published as `x-pyn-reach`.
    pub fn label(&self) -> String {
        match self {
            Self::Open => "open".into(),
            Self::Public => "public".into(),
            Self::OptionalSignIn => "optional_sign_in".into(),
            Self::OptionalFullScope => "optional_full_scope".into(),
            Self::Session => "session".into(),
            Self::AnyCredential => "any_credential".into(),
            Self::FullScope => "full_scope".into(),
            Self::FullScopePending => "full_scope_pending".into(),
            Self::FullScopeEnrolment => "full_scope_enrolment".into(),
            Self::Namespace => "namespace".into(),
            Self::AdminOnly => "admin_only".into(),
            Self::Service(s) => format!("service:{}", s.as_str()),
            Self::FullScopeOrService(s) => format!("full_scope_or_service:{}", s.as_str()),
            Self::OrgScoped(n) => format!("org_scoped:{}", n.label()),
            Self::AccountScoped(n) => format!("account_scoped:{}", n.label()),
            Self::RepoScoped(n) => format!("repo_scoped:{}", n.label()),
            Self::PublicRepoRead(n) => format!("public_repo_read:{}", n.label()),
            Self::PublicLfsRead(n) => format!("public_lfs_read:{}", n.label()),
        }
    }
}

/// The reach and the template of the matched route, put in the request's extensions for the caller extractors.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Declared(pub(crate) Reach, pub(crate) &'static str);

/// One operation of the table.
pub struct Declaration {
    pub method: Method,
    pub path: &'static str,
    pub reach: Reach,
}

struct Entry {
    declaration: Declaration,
    route: MethodRouter<AppState, Infallible>,
}

fn on<H, T>(method: Method, path: &'static str, reach: Reach, handler: H) -> Entry
where
    H: Handler<T, AppState>,
    T: 'static,
{
    let filter = MethodFilter::try_from(method.clone()).expect("a routable method");
    Entry {
        declaration: Declaration {
            method,
            path,
            reach,
        },
        route: MethodRouter::new()
            .on(filter, handler)
            .layer(Extension(Declared(reach, path))),
    }
}

macro_rules! verbs {
    ($($name:ident => $method:ident),*) => {$(
        fn $name<H, T>(path: &'static str, reach: Reach, handler: H) -> Entry
        where
            H: Handler<T, AppState>,
            T: 'static,
        {
            on(Method::$method, path, reach, handler)
        }
    )*};
}

verbs!(get => GET, post => POST, put => PUT, patch => PATCH, delete => DELETE, head => HEAD, options => OPTIONS);

fn table() -> Vec<Entry> {
    vec![
        get("/healthz", Reach::Open, crate::health),
        get("/readyz", Reach::Open, crate::ready::readyz),
        get("/v1/capabilities", Reach::Open, crate::capabilities),
        get("/openapi.json", Reach::Open, || async { Json(openapi()) }),
        get("/v1/setup", Reach::Open, setup_api::setup_status),
        post("/v1/setup", Reach::Open, setup_api::complete_setup),
        get("/v1/registration", Reach::Open, access_api::registration),
        post("/v1/register", Reach::Open, access_api::register),
        post("/v1/register/verify", Reach::Open, access_api::verify_email),
        post(
            "/v1/register/resend",
            Reach::Open,
            access_api::resend_verification,
        ),
        get(
            "/v1/admin/users",
            Reach::Service(ServiceScope::ManageAccounts),
            admin_api::list_accounts,
        ),
        post(
            "/v1/admin/users",
            Reach::AdminOnly,
            server_invitation_api::add_account,
        ),
        post(
            "/v1/admin/users/{user}/resend-confirmation",
            Reach::AdminOnly,
            server_invitation_api::resend_confirmation,
        ),
        get(
            "/v1/admin/invitations",
            Reach::AdminOnly,
            server_invitation_api::list_invitations,
        ),
        post(
            "/v1/admin/invitations",
            Reach::AdminOnly,
            server_invitation_api::create_invitation,
        ),
        delete(
            "/v1/admin/invitations/{id}",
            Reach::AdminOnly,
            server_invitation_api::revoke_invitation,
        ),
        post(
            "/v1/admin/users/{user}/approve",
            Reach::Service(ServiceScope::ManageAccounts),
            admin_api::approve,
        ),
        post(
            "/v1/admin/users/{user}/disable",
            Reach::Service(ServiceScope::ManageAccounts),
            admin_api::disable,
        ),
        post(
            "/v1/admin/users/{user}/enable",
            Reach::Service(ServiceScope::ManageAccounts),
            admin_api::enable,
        ),
        post(
            "/v1/admin/users/{user}/delete",
            Reach::Service(ServiceScope::ManageAccounts),
            deletion_api::admin_delete_account,
        ),
        get(
            "/v1/admin/settings",
            Reach::Service(ServiceScope::ManageAccounts),
            deletion_api::admin_settings,
        ),
        get(
            "/v1/account/deletion",
            Reach::FullScopePending,
            deletion_api::account_deletion_status,
        ),
        post(
            "/v1/account/deletion",
            Reach::FullScope,
            deletion_api::schedule_account_deletion,
        ),
        post(
            "/v1/account/deletion/restore",
            Reach::FullScopePending,
            deletion_api::restore_account_deletion,
        ),
        put(
            "/v1/admin/users/{user}/admin",
            Reach::AdminOnly,
            admin_api::grant_admin,
        ),
        delete(
            "/v1/admin/users/{user}/admin",
            Reach::AdminOnly,
            admin_api::revoke_admin,
        ),
        get("/v1/admin/audit", Reach::AdminOnly, admin_api::audit),
        get(
            "/v1/admin/update",
            Reach::AdminOnly,
            admin_api::update_status,
        ),
        put(
            "/v1/admin/access",
            Reach::AdminOnly,
            admin_api::set_access_mode,
        ),
        get(
            "/v1/admin/repositories",
            Reach::AdminOnly,
            admin_api::list_repositories,
        ),
        get(
            "/v1/admin/organizations",
            Reach::AdminOnly,
            admin_api::list_organizations,
        ),
        post(
            "/v1/admin/orgs",
            Reach::Service(ServiceScope::ManageOrganizations),
            admin_api::create_org,
        ),
        delete(
            "/v1/admin/orgs/{org}",
            Reach::Service(ServiceScope::ManageOrganizations),
            admin_api::delete_org,
        ),
        get(
            "/v1/admin/service-credentials",
            Reach::AdminOnly,
            admin_api::list_service_credentials,
        ),
        post(
            "/v1/admin/service-credentials",
            Reach::AdminOnly,
            admin_api::create_service_credential,
        ),
        delete(
            "/v1/admin/service-credentials/{name}",
            Reach::AdminOnly,
            admin_api::revoke_service_credential,
        ),
        post(
            "/v1/admin/users/{user}/access-sessions",
            Reach::AdminOnly,
            access_session_api::start,
        ),
        get(
            "/v1/admin/access-sessions",
            Reach::AdminOnly,
            access_session_api::list,
        ),
        post(
            "/v1/admin/access-sessions/{id}/end",
            Reach::AdminOnly,
            access_session_api::end,
        ),
        get(
            "/v1/me/access-sessions",
            Reach::FullScope,
            access_session_api::mine,
        ),
        get(
            "/v1/admin/limits",
            Reach::Service(ServiceScope::ManageLimits),
            limits_api::list_limits,
        ),
        patch(
            "/v1/admin/owners/{owner}/limits",
            Reach::Service(ServiceScope::ManageLimits),
            limits_api::set_owner_limits,
        ),
        get(
            "/v1/admin/webhooks",
            Reach::Service(ServiceScope::ManageWebhooks),
            webhook_api::list_webhooks,
        ),
        get(
            "/v1/admin/webhooks/event-types",
            Reach::Service(ServiceScope::ManageWebhooks),
            webhook_api::list_webhook_event_types,
        ),
        post(
            "/v1/admin/webhooks",
            Reach::Service(ServiceScope::ManageWebhooks),
            webhook_api::create_webhook,
        ),
        get(
            "/v1/admin/webhooks/{id}",
            Reach::Service(ServiceScope::ManageWebhooks),
            webhook_api::get_webhook,
        ),
        patch(
            "/v1/admin/webhooks/{id}",
            Reach::Service(ServiceScope::ManageWebhooks),
            webhook_api::update_webhook,
        ),
        delete(
            "/v1/admin/webhooks/{id}",
            Reach::Service(ServiceScope::ManageWebhooks),
            webhook_api::delete_webhook,
        ),
        post(
            "/v1/admin/webhooks/{id}/secrets",
            Reach::Service(ServiceScope::ManageWebhooks),
            webhook_api::rotate_webhook_secret,
        ),
        delete(
            "/v1/admin/webhooks/{id}/secrets/{secret_id}",
            Reach::Service(ServiceScope::ManageWebhooks),
            webhook_api::retire_webhook_secret,
        ),
        get(
            "/v1/admin/webhooks/{id}/deliveries",
            Reach::Service(ServiceScope::ManageWebhooks),
            webhook_api::list_webhook_deliveries,
        ),
        post(
            "/v1/admin/webhooks/{id}/deliveries/{delivery_id}/redeliver",
            Reach::Service(ServiceScope::ManageWebhooks),
            webhook_api::redeliver_webhook,
        ),
        put(
            "/v1/admin/owners/{owner}/freeze",
            Reach::Service(ServiceScope::ManageFreeze),
            freeze_api::freeze_owner,
        ),
        delete(
            "/v1/admin/owners/{owner}/freeze",
            Reach::Service(ServiceScope::ManageFreeze),
            freeze_api::unfreeze_owner,
        ),
        get(
            "/v1/owners/{owner}/freeze",
            Reach::FullScopeOrService(ServiceScope::ManageFreeze),
            freeze_api::owner_freeze,
        ),
        get(
            "/v1/owners/{owner}/limits",
            Reach::FullScopeOrService(ServiceScope::ManageLimits),
            limits_api::owner_limits,
        ),
        get(
            "/v1/owners/{owner}/usage",
            Reach::FullScopeOrService(ServiceScope::ManageLimits),
            limits_api::owner_usage,
        ),
        get(
            "/v1/repos/{owner}/{name}/usage",
            Reach::PublicRepoRead(need(Contents, Read)),
            limits_api::repo_usage,
        ),
        get(
            "/v1/repos/{owner}/{name}/usage/breakdown",
            Reach::RepoScoped(need(Administration, Write)),
            limits_api::repo_usage_breakdown,
        ),
        put(
            "/v1/repos/{owner}/{name}/storage-cap",
            Reach::FullScopeOrService(ServiceScope::ManageLimits),
            limits_api::set_repo_storage_cap,
        ),
        post("/v1/login", Reach::Open, access_api::login),
        get("/v1/session", Reach::Session, session_api::current),
        post("/v1/session", Reach::Open, session_api::sign_in),
        delete("/v1/session", Reach::Open, session_api::sign_out),
        post(
            "/v1/session/passkey/begin",
            Reach::Open,
            passkeys_api::begin_sign_in,
        ),
        post(
            "/v1/session/passkey/finish",
            Reach::Open,
            passkeys_api::finish_sign_in,
        ),
        get(
            "/v1/sign-in-options",
            Reach::Open,
            oidc_api::sign_in_options,
        ),
        get("/v1/oidc/authorize", Reach::Open, oidc_api::authorize),
        post("/v1/oidc/link", Reach::FullScope, oidc_api::link),
        get("/v1/oidc/callback", Reach::Open, oidc_api::callback),
        get(
            "/v1/me/identities",
            Reach::FullScope,
            oidc_api::list_identities,
        ),
        delete(
            "/v1/me/identities/{id}",
            Reach::FullScope,
            oidc_api::unlink_identity,
        ),
        put(
            "/v1/me/password",
            Reach::FullScope,
            access_api::change_password,
        ),
        get(
            "/v1/me/two-factor",
            Reach::FullScopeEnrolment,
            two_factor_api::two_factor_status,
        ),
        post(
            "/v1/me/two-factor/totp",
            Reach::FullScopeEnrolment,
            two_factor_api::begin_totp,
        ),
        post(
            "/v1/me/two-factor/totp/{id}/confirm",
            Reach::FullScopeEnrolment,
            two_factor_api::confirm_totp,
        ),
        post(
            "/v1/me/two-factor/recovery-codes",
            Reach::FullScope,
            two_factor_api::recovery_codes,
        ),
        post(
            "/v1/me/two-factor/verify",
            Reach::FullScope,
            two_factor_api::verify_second_factor,
        ),
        delete(
            "/v1/me/two-factor/{id}",
            Reach::FullScopeEnrolment,
            two_factor_api::remove_factor,
        ),
        get(
            "/v1/me/passkeys",
            Reach::FullScopeEnrolment,
            passkeys_api::list_passkeys,
        ),
        post(
            "/v1/me/passkeys/register/begin",
            Reach::FullScopeEnrolment,
            passkeys_api::begin_registration,
        ),
        post(
            "/v1/me/passkeys/register/finish",
            Reach::FullScopeEnrolment,
            passkeys_api::finish_registration,
        ),
        post(
            "/v1/me/passkeys/verify/begin",
            Reach::FullScope,
            passkeys_api::begin_verify,
        ),
        post(
            "/v1/me/passkeys/verify/finish",
            Reach::FullScope,
            passkeys_api::finish_verify,
        ),
        patch(
            "/v1/me/passkeys/{id}",
            Reach::FullScopeEnrolment,
            passkeys_api::rename_passkey,
        ),
        delete(
            "/v1/me/passkeys/{id}",
            Reach::FullScopeEnrolment,
            passkeys_api::remove_passkey,
        ),
        get(
            "/v1/me/notices",
            Reach::FullScopeEnrolment,
            requirements_api::notices,
        ),
        delete(
            "/v1/me/notices/{id}",
            Reach::FullScopeEnrolment,
            requirements_api::dismiss_notice,
        ),
        get(
            "/v1/admin/two-factor",
            Reach::AdminOnly,
            requirements_api::get_server_requirement,
        ),
        put(
            "/v1/admin/two-factor",
            Reach::AdminOnly,
            requirements_api::set_server_requirement,
        ),
        post(
            "/v1/admin/users/{user}/two-factor/reset",
            Reach::AdminOnly,
            requirements_api::reset_two_factor,
        ),
        get(
            "/v1/orgs/{org}/two-factor",
            Reach::OrgScoped(need(OrgAdministration, Read)),
            requirements_api::get_org_requirement,
        ),
        put(
            "/v1/orgs/{org}/two-factor",
            Reach::OrgScoped(need(OrgAdministration, Write)),
            requirements_api::set_org_requirement,
        ),
        get(
            "/v1/me/emails",
            Reach::FullScope,
            self_service_api::list_emails,
        ),
        post(
            "/v1/me/emails",
            Reach::FullScope,
            self_service_api::add_email,
        ),
        delete(
            "/v1/me/emails/{address}",
            Reach::FullScope,
            self_service_api::delete_email,
        ),
        post(
            "/v1/me/emails/{address}/resend",
            Reach::FullScope,
            self_service_api::resend_email,
        ),
        put(
            "/v1/me/emails/{address}/primary",
            Reach::FullScope,
            self_service_api::set_primary_email,
        ),
        put(
            "/v1/me/username",
            Reach::FullScope,
            self_service_api::rename_account,
        ),
        get(
            "/v1/keys",
            Reach::AccountScoped(need(AccountSshKeys, Read)),
            access_api::list_keys,
        ),
        post(
            "/v1/keys",
            Reach::AccountScoped(need(AccountSshKeys, Write)),
            access_api::add_key,
        ),
        delete(
            "/v1/keys/{id}",
            Reach::AccountScoped(need(AccountSshKeys, Write)),
            access_api::delete_key,
        ),
        get("/v1/me", Reach::AnyCredential, access_api::me),
        put(
            "/v1/me/profile",
            Reach::AccountScoped(need(AccountProfile, Write)),
            profile_api::update_profile,
        ),
        put(
            "/v1/me/avatar",
            Reach::AccountScoped(need(AccountProfile, Write)),
            profile_api::put_avatar,
        ),
        delete(
            "/v1/me/avatar",
            Reach::AccountScoped(need(AccountProfile, Write)),
            profile_api::delete_avatar,
        ),
        get(
            "/v1/users",
            Reach::OptionalFullScope,
            profile_api::list_people,
        ),
        get(
            "/v1/users/{user}",
            Reach::OptionalSignIn,
            profile_api::get_profile,
        ),
        get(
            "/v1/users/{user}/starred",
            Reach::OptionalSignIn,
            profile_api::list_starred,
        ),
        get(
            "/v1/users/{user}/avatar",
            Reach::OptionalSignIn,
            profile_api::get_avatar,
        ),
        get(
            "/v1/orgs/{org}/profile",
            Reach::AnyCredential,
            profile_api::get_org_profile,
        ),
        put(
            "/v1/orgs/{org}/profile",
            Reach::OrgScoped(need(OrgAdministration, Write)),
            profile_api::update_org_profile,
        ),
        get(
            "/v1/orgs/{org}/avatar",
            Reach::Public,
            profile_api::get_org_avatar,
        ),
        put(
            "/v1/orgs/{org}/avatar",
            Reach::OrgScoped(need(OrgAdministration, Write)),
            profile_api::put_org_avatar,
        ),
        delete(
            "/v1/orgs/{org}/avatar",
            Reach::OrgScoped(need(OrgAdministration, Write)),
            profile_api::delete_org_avatar,
        ),
        get("/v1/me/locks", Reach::AnyCredential, repo_api::my_locks),
        get("/v1/me/watching", Reach::FullScope, repo_api::list_watching),
        get("/v1/tokens", Reach::FullScope, access_api::list_tokens),
        post("/v1/tokens", Reach::FullScope, access_api::create_token),
        delete(
            "/v1/tokens/{id}",
            Reach::FullScope,
            access_api::revoke_token,
        ),
        get("/v1/orgs", Reach::FullScope, org_api::list_orgs),
        post("/v1/orgs", Reach::Namespace, org_api::create_org),
        get("/v1/orgs/{org}", Reach::AnyCredential, org_api::get_org),
        delete(
            "/v1/orgs/{org}",
            Reach::OrgScoped(need(OrgAdministration, Write)),
            org_api::delete_org,
        ),
        get(
            "/v1/orgs/{org}/audit",
            Reach::OrgScoped(need(OrgAudit, Read)),
            org_api::org_audit,
        ),
        get(
            "/v1/orgs/{org}/members",
            Reach::OrgScoped(need(OrgMembers, Read)),
            org_api::list_members,
        ),
        post(
            "/v1/orgs/{org}/members",
            Reach::OrgScoped(need(OrgMembers, Write)),
            org_api::add_member,
        ),
        get(
            "/v1/orgs/{org}/invitations",
            Reach::OrgScoped(need(OrgMembers, Write)),
            org_api::list_invitations,
        ),
        delete(
            "/v1/orgs/{org}/invitations/{user}",
            Reach::OrgScoped(need(OrgMembers, Write)),
            org_api::cancel_invitation,
        ),
        get(
            "/v1/me/invitations",
            Reach::FullScope,
            org_api::my_invitations,
        ),
        post(
            "/v1/me/invitations/{org}/accept",
            Reach::Namespace,
            org_api::accept_invitation,
        ),
        post(
            "/v1/me/invitations/{org}/decline",
            Reach::Namespace,
            org_api::decline_invitation,
        ),
        patch(
            "/v1/orgs/{org}/members/{user}",
            Reach::OrgScoped(need(OrgMembers, Write)),
            org_api::set_member_role,
        ),
        delete(
            "/v1/orgs/{org}/members/{user}",
            Reach::OrgScoped(need(OrgMembers, Write)),
            org_api::remove_member,
        ),
        get(
            "/v1/orgs/{org}/settings",
            Reach::OrgScoped(need(OrgAdministration, Read)),
            org_settings_api::get_org_settings,
        ),
        put(
            "/v1/orgs/{org}/settings",
            Reach::OrgScoped(need(OrgAdministration, Write)),
            org_settings_api::set_org_settings,
        ),
        get(
            "/v1/orgs/{org}/repo-policy",
            Reach::OrgScoped(need(OrgAdministration, Read)),
            repo_policy_api::get_repo_policy,
        ),
        put(
            "/v1/orgs/{org}/repo-policy",
            Reach::OrgScoped(need(OrgAdministration, Write)),
            repo_policy_api::set_repo_policy,
        ),
        put(
            "/v1/orgs/{org}/repo-policy/rules/{effect}/{kind}/{subject}",
            Reach::OrgScoped(need(OrgAdministration, Write)),
            repo_policy_api::set_creation_rule,
        ),
        delete(
            "/v1/orgs/{org}/repo-policy/rules/{effect}/{kind}/{subject}",
            Reach::OrgScoped(need(OrgAdministration, Write)),
            repo_policy_api::remove_creation_rule,
        ),
        get(
            "/v1/orgs/{org}/teams",
            Reach::OrgScoped(need(OrgTeams, Read)),
            team_api::list_teams,
        ),
        post(
            "/v1/orgs/{org}/teams",
            Reach::OrgScoped(need(OrgTeams, Write)),
            team_api::create_team,
        ),
        get(
            "/v1/orgs/{org}/teams/{team}",
            Reach::OrgScoped(need(OrgTeams, Read)),
            team_api::get_team,
        ),
        patch(
            "/v1/orgs/{org}/teams/{team}",
            Reach::OrgScoped(need(OrgTeams, Write)),
            team_api::update_team,
        ),
        delete(
            "/v1/orgs/{org}/teams/{team}",
            Reach::OrgScoped(need(OrgTeams, Write)),
            team_api::delete_team,
        ),
        put(
            "/v1/orgs/{org}/teams/{team}/members/{user}",
            Reach::OrgScoped(need(OrgTeams, Write)),
            team_api::add_team_member,
        ),
        delete(
            "/v1/orgs/{org}/teams/{team}/members/{user}",
            Reach::OrgScoped(need(OrgTeams, Write)),
            team_api::remove_team_member,
        ),
        get("/v1/me/orgs", Reach::FullScope, org_api::my_orgs),
        get(
            "/v1/explore/repos",
            Reach::OptionalSignIn,
            repo_api::explore_repos,
        ),
        get("/v1/repos", Reach::AnyCredential, repo_api::list_repos),
        post("/v1/repos", Reach::Namespace, repo_api::create_repo),
        get(
            "/v1/repos/{owner}/{name}",
            Reach::PublicRepoRead(need(Metadata, Read)),
            repo_api::get_repo,
        ),
        patch(
            "/v1/repos/{owner}/{name}",
            Reach::RepoScoped(need(Administration, Write)),
            repo_api::update_repo,
        ),
        delete(
            "/v1/repos/{owner}/{name}",
            Reach::RepoScoped(need(Administration, Write)),
            repo_api::delete_repo,
        ),
        get(
            "/v1/repos/{owner}/{name}/star",
            Reach::FullScope,
            repo_api::get_star,
        ),
        put(
            "/v1/repos/{owner}/{name}/star",
            Reach::FullScope,
            repo_api::put_star,
        ),
        delete(
            "/v1/repos/{owner}/{name}/star",
            Reach::FullScope,
            repo_api::delete_star,
        ),
        get(
            "/v1/repos/{owner}/{name}/watch",
            Reach::FullScope,
            repo_api::get_watch,
        ),
        put(
            "/v1/repos/{owner}/{name}/watch",
            Reach::FullScope,
            repo_api::put_watch,
        ),
        delete(
            "/v1/repos/{owner}/{name}/watch",
            Reach::FullScope,
            repo_api::delete_watch,
        ),
        get(
            "/v1/repos/{owner}/{name}/me",
            Reach::RepoScoped(need(Metadata, Read)),
            repo_api::repo_me,
        ),
        post(
            "/v1/repos/{owner}/{name}/users",
            Reach::RepoScoped(need(Members, Write)),
            repo_api::add_user,
        ),
        get(
            "/v1/repos/{owner}/{name}/invites",
            Reach::RepoScoped(need(Members, Write)),
            repo_api::list_invites,
        ),
        post(
            "/v1/repos/{owner}/{name}/invites",
            Reach::RepoScoped(need(Members, Write)),
            repo_api::create_invite,
        ),
        delete(
            "/v1/repos/{owner}/{name}/invites/{id}",
            Reach::RepoScoped(need(Members, Write)),
            repo_api::revoke_invite,
        ),
        get(
            "/v1/repos/{owner}/{name}/roles",
            Reach::RepoScoped(need(Members, Read)),
            repo_api::list_roles,
        ),
        put(
            "/v1/repos/{owner}/{name}/roles/{role}",
            Reach::RepoScoped(need(Administration, Write)),
            repo_api::set_role,
        ),
        get(
            "/v1/repos/{owner}/{name}/members",
            Reach::RepoScoped(need(Members, Write)),
            repo_api::list_members,
        ),
        put(
            "/v1/repos/{owner}/{name}/members/{user}",
            Reach::RepoScoped(need(Members, Write)),
            repo_api::set_member,
        ),
        get(
            "/v1/repos/{owner}/{name}/teams",
            Reach::RepoScoped(need(Members, Write)),
            team_api::list_repo_teams,
        ),
        put(
            "/v1/repos/{owner}/{name}/teams/{team}",
            Reach::RepoScoped(need(Members, Write)),
            team_api::set_repo_team,
        ),
        delete(
            "/v1/repos/{owner}/{name}/teams/{team}",
            Reach::RepoScoped(need(Members, Write)),
            team_api::remove_repo_team,
        ),
        get(
            "/v1/repos/{owner}/{name}/locks",
            Reach::PublicRepoRead(need(Locks, Read)),
            crate::list_locks,
        ),
        get(
            "/v1/repos/{owner}/{name}/locks/events",
            Reach::PublicRepoRead(need(Locks, Read)),
            crate::lock_stream_api::lock_events,
        ),
        get(
            "/v1/repos/{owner}/{name}/info/lfs/locks",
            Reach::RepoScoped(need(Locks, Read)),
            lfs_api::list_locks,
        ),
        post(
            "/v1/repos/{owner}/{name}/info/lfs/locks",
            Reach::RepoScoped(need(Locks, Write)),
            lfs_api::create_lock,
        ),
        post(
            "/v1/repos/{owner}/{name}/info/lfs/locks/verify",
            Reach::RepoScoped(need(Locks, Read)),
            lfs_api::verify_locks,
        ),
        post(
            "/v1/repos/{owner}/{name}/info/lfs/locks/{id}/unlock",
            Reach::RepoScoped(need(Locks, Write)),
            lfs_api::unlock,
        ),
        post(
            "/v1/repos/{owner}/{name}/info/lfs/objects/batch",
            Reach::PublicLfsRead(need(Contents, Read)),
            lfs_api::batch,
        ),
        post(
            "/v1/repos/{owner}/{name}/info/lfs/objects/verify",
            Reach::RepoScoped(need(Contents, Write)),
            lfs_api::verify_object,
        ),
        get(
            "/v1/repos/{owner}/{name}/info/lfs/objects/{oid}",
            Reach::PublicLfsRead(need(Contents, Read)),
            lfs_api::download,
        ),
        put(
            "/v1/repos/{owner}/{name}/info/lfs/objects/{oid}",
            Reach::RepoScoped(need(Contents, Write)),
            lfs_api::upload_object,
        ),
        head(
            "/v1/repos/{owner}/{name}/info/lfs/objects/{oid}/tus",
            Reach::RepoScoped(need(Contents, Write)),
            lfs_api::tus::head,
        ),
        post(
            "/v1/repos/{owner}/{name}/info/lfs/objects/{oid}/tus",
            Reach::RepoScoped(need(Contents, Write)),
            lfs_api::tus::create,
        ),
        patch(
            "/v1/repos/{owner}/{name}/info/lfs/objects/{oid}/tus",
            Reach::RepoScoped(need(Contents, Write)),
            lfs_api::tus::patch,
        ),
        delete(
            "/v1/repos/{owner}/{name}/info/lfs/objects/{oid}/tus",
            Reach::RepoScoped(need(Contents, Write)),
            lfs_api::tus::terminate,
        ),
        options(
            "/v1/repos/{owner}/{name}/info/lfs/objects/{oid}/tus",
            Reach::Open,
            lfs_api::tus::options,
        ),
        get(
            "/v1/repos/{owner}/{name}/files",
            Reach::PublicRepoRead(need(Contents, Read)),
            crate::list_files,
        ),
        get(
            "/v1/repos/{owner}/{name}/tree",
            Reach::PublicRepoRead(need(Contents, Read)),
            crate::tree,
        ),
        get(
            "/v1/repos/{owner}/{name}/summary",
            Reach::PublicRepoRead(need(Contents, Read)),
            crate::summary,
        ),
        get(
            "/v1/repos/{owner}/{name}/content",
            Reach::PublicRepoRead(need(Contents, Read)),
            crate::get_content,
        ),
        post(
            "/v1/repos/{owner}/{name}/checkout",
            Reach::RepoScoped(need(Locks, Write)),
            crate::checkout,
        ),
        post(
            "/v1/repos/{owner}/{name}/release",
            Reach::RepoScoped(need(Locks, Write)),
            crate::release,
        ),
        post(
            "/v1/repos/{owner}/{name}/checkin",
            Reach::RepoScoped(need(Contents, Write)),
            crate::checkin,
        ),
        post(
            "/v1/repos/{owner}/{name}/changesets",
            Reach::RepoScoped(need(Contents, Write)),
            crate::changeset,
        ),
        post(
            "/v1/repos/{owner}/{name}/restore",
            Reach::RepoScoped(need(Contents, Write).via(Permission::Restore)),
            crate::restore,
        ),
        post(
            "/v1/repos/{owner}/{name}/force-unlock",
            Reach::RepoScoped(need(Locks, Admin)),
            crate::force_unlock,
        ),
        get(
            "/v1/repos/{owner}/{name}/policy",
            Reach::PublicRepoRead(need(Contents, Read)),
            crate::get_policy,
        ),
        post(
            "/v1/repos/{owner}/{name}/policy/preview",
            Reach::RepoScoped(need(Policy, Write)),
            crate::preview_policy,
        ),
        get(
            "/v1/repos/{owner}/{name}/audit",
            Reach::RepoScoped(need(Audit, Read)),
            crate::audit,
        ),
        put(
            "/v1/repos/{owner}/{name}/objects",
            Reach::RepoScoped(need(Contents, Write)),
            crate::put_object,
        ),
        put(
            "/v1/repos/{owner}/{name}/objects/chunk",
            Reach::RepoScoped(need(Contents, Write)),
            crate::put_chunk,
        ),
        post(
            "/v1/repos/{owner}/{name}/objects/missing",
            Reach::RepoScoped(need(Contents, Write)),
            crate::missing_objects,
        ),
        post(
            "/v1/repos/{owner}/{name}/objects/assemble",
            Reach::RepoScoped(need(Contents, Write)),
            crate::assemble_object,
        ),
        get(
            "/v1/repos/{owner}/{name}/history",
            Reach::PublicRepoRead(need(Contents, Read)),
            crate::history,
        ),
        get(
            "/v1/repos/{owner}/{name}/history/authors",
            Reach::PublicRepoRead(need(Contents, Read)),
            crate::history_authors,
        ),
        get(
            "/v1/repos/{owner}/{name}/verify",
            Reach::PublicRepoRead(need(Contents, Read)),
            crate::verify,
        ),
        get(
            "/v1/repos/{owner}/{name}/commits",
            Reach::PublicRepoRead(need(Contents, Read)),
            crate::list_commits,
        ),
        get(
            "/v1/repos/{owner}/{name}/commits/{commit}",
            Reach::PublicRepoRead(need(Contents, Read)),
            crate::get_commit,
        ),
        get(
            "/v1/repos/{owner}/{name}/snapshot",
            Reach::PublicRepoRead(need(Contents, Read)),
            crate::snapshot,
        ),
        get(
            "/v1/repos/{owner}/{name}/branches",
            Reach::PublicRepoRead(need(Contents, Read)),
            branch_api::list_branches,
        ),
        get(
            "/v1/repos/{owner}/{name}/compare",
            Reach::PublicRepoRead(need(Contents, Read)),
            branch_api::compare,
        ),
        get(
            "/v1/repos/{owner}/{name}/diff",
            Reach::PublicRepoRead(need(Contents, Read)),
            diff_api::diff,
        ),
        get(
            "/v1/repos/{owner}/{name}/lines",
            Reach::PublicRepoRead(need(Contents, Read)),
            diff_api::file_lines,
        ),
        post(
            "/v1/repos/{owner}/{name}/branches",
            Reach::RepoScoped(need(Contents, Write)),
            branch_api::create_branch,
        ),
        post(
            "/v1/repos/{owner}/{name}/push",
            Reach::RepoScoped(need(Contents, Write)),
            push_api::push,
        ),
        post(
            "/v1/repos/{owner}/{name}/fetch",
            Reach::PublicRepoRead(need(Contents, Read)),
            push_api::fetch,
        ),
        get(
            "/v1/repos/{owner}/{name}/objects/{id}",
            Reach::PublicRepoRead(need(Contents, Read)),
            push_api::get_object,
        ),
        post(
            "/v1/repos/{owner}/{name}/merge",
            Reach::RepoScoped(need(Contents, Write)),
            merge_api::merge,
        ),
        post(
            "/v1/repos/{owner}/{name}/merge/preview",
            Reach::RepoScoped(need(Contents, Read)),
            merge_api::preview_merge,
        ),
        get(
            "/v1/repos/{owner}/{name}/branches/{*branch}",
            Reach::PublicRepoRead(need(Contents, Read)),
            branch_api::get_branch,
        ),
        delete(
            "/v1/repos/{owner}/{name}/branches/{*branch}",
            Reach::RepoScoped(need(Contents, Write)),
            branch_api::delete_branch,
        ),
        patch(
            "/v1/repos/{owner}/{name}/branches/{*branch}",
            Reach::RepoScoped(need(Contents, Write)),
            branch_api::rename_branch,
        ),
    ]
}

/// Every operation with its declared reach.
pub fn declarations() -> Vec<Declaration> {
    table().into_iter().map(|e| e.declaration).collect()
}

/// Applies the server's access mode to a route declared `reach`: the one place a private server shuts the signed-out
/// routes.
async fn enforce_access_mode(
    State((state, reach)): State<(AppState, Reach)>,
    params: RawPathParams,
    request: Request,
    next: Next,
) -> Response {
    let repo = params
        .iter()
        .find(|(k, _)| *k == "owner")
        .zip(params.iter().find(|(k, _)| *k == "name"))
        .map(|((_, owner), (_, name))| format!("{owner}/{name}"));
    let req = caller::Req {
        headers: request.headers(),
        source: crate::client::source_ip(
            request.headers(),
            request.extensions(),
            state.trust_forwarded,
            state.trusted_proxies,
        ),
        call: None,
    };
    match caller::admit(&state, req, reach, repo).await {
        Ok(()) => next.run(request).await,
        Err(e) if matches!(reach, Reach::PublicLfsRead(_)) => {
            crate::lfs_api::LfsError::from(e).into_response()
        }
        Err(e) => e.into_response(),
    }
}

pub(crate) fn routes(state: &AppState) -> Router<AppState> {
    let mut paths: Vec<&'static str> = Vec::new();
    let mut merged: HashMap<&'static str, MethodRouter<AppState, Infallible>> = HashMap::new();
    for entry in table() {
        let path = entry.declaration.path;
        let reach = entry.declaration.reach;
        let route = if reach.closed_when_private() {
            entry.route.layer(middleware::from_fn_with_state(
                (state.clone(), reach),
                enforce_access_mode,
            ))
        } else {
            entry.route
        };
        match merged.remove(path) {
            Some(existing) => merged.insert(path, existing.merge(route)),
            None => {
                paths.push(path);
                merged.insert(path, route)
            }
        };
    }
    paths.into_iter().fold(Router::new(), |router, path| {
        router.route(path, merged.remove(path).expect("merged above"))
    })
}

/// The OpenAPI document with each operation's reach as `x-pyn-reach`.
pub fn openapi() -> utoipa::openapi::OpenApi {
    let mut doc = crate::ApiDoc::openapi();
    for d in declarations() {
        let path = openapi_path(d.path);
        let Some(item) = doc.paths.paths.get_mut(&path) else {
            continue;
        };
        let op = match d.method {
            Method::GET => &mut item.get,
            Method::POST => &mut item.post,
            Method::PUT => &mut item.put,
            Method::PATCH => &mut item.patch,
            Method::DELETE => &mut item.delete,
            Method::HEAD => &mut item.head,
            Method::OPTIONS => &mut item.options,
            _ => continue,
        };
        if let Some(op) = op {
            let mut ext = op.extensions.take().unwrap_or_default();
            ext.merge(
                Extensions::builder()
                    .add("x-pyn-reach", d.reach.label())
                    .build(),
            );
            op.extensions = Some(ext);
        }
    }
    doc
}

/// The OpenAPI spelling of a route path: a catch-all `{*name}` is `{name}`.
pub fn openapi_path(path: &str) -> String {
    path.replace("{*", "{")
}
