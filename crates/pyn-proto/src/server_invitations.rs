//! Server invitations and administrator-made accounts (`/v1/admin/invitations`, `/v1/admin/users`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Which mailer the server sends with. Never carries a host or a credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MailerKind {
    /// No mail is sent.
    None,
    /// Messages are written to the server log, for testing.
    Log,
    /// Messages go out over SMTP.
    Smtp,
}

/// Whether the server sends email.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EmailSettings {
    pub mailer: MailerKind,
    /// Whether sign-up must confirm the address (`PYN_EMAIL_VERIFICATION`, on by default when a mailer is set).
    pub verification: bool,
    /// The newest send the mailer refused since the server started; null when none failed.
    #[serde(default)]
    pub last_failure: Option<EmailFailure>,
    /// When a message last went out since the server started; null when none has.
    #[serde(default)]
    pub last_sent_at: Option<DateTime<Utc>>,
}

/// A refused send. The error never carries a recipient address or a credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EmailFailure {
    pub at: DateTime<Utc>,
    pub error: String,
}

/// The role a server invitation gives in one repository or one organization; exactly one of the two targets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ServerInvitationGrant {
    /// `owner/name` of a personal repository.
    #[serde(default)]
    pub repository: Option<String>,
    /// An organization's name.
    #[serde(default)]
    pub organization: Option<String>,
    /// A `Role` when `repository` is set, an `OrgRole` when `organization` is.
    #[schema(schema_with = role_schema)]
    pub role: String,
}

fn role_schema() -> utoipa::openapi::schema::Schema {
    use utoipa::openapi::{Ref, schema::OneOfBuilder};
    OneOfBuilder::new()
        .item(Ref::from_schema_name("Role"))
        .item(Ref::from_schema_name("OrgRole"))
        .into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct CreateServerInvitationRequest {
    /// Where to send the code; used only when a mailer is configured, and stored only to tell invitations apart.
    #[serde(default)]
    pub email: Option<String>,
    /// Days the code works, 1 to 90; 7 when left out.
    #[serde(default)]
    pub expires_in_days: Option<u32>,
    #[serde(default)]
    pub grant: Option<ServerInvitationGrant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ServerInvitationStatus {
    Pending,
    Used,
    Revoked,
    Expired,
}

/// A server invitation as an administrator sees it; the code is never part of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ServerInvitationInfo {
    pub id: String,
    pub email: Option<String>,
    pub grant: Option<ServerInvitationGrant>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub status: ServerInvitationStatus,
    pub used_at: Option<DateTime<Utc>>,
    pub used_by: Option<String>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct CreatedServerInvitation {
    /// Give this to the person being invited, as `pyn register <name> --invite <code>` or in the sign-up form. Shown once.
    pub code: String,
    /// The message was handed to a configured mailer; false when no address was given or no mailer is on.
    pub emailed: bool,
    pub invitation: ServerInvitationInfo,
}

impl std::fmt::Debug for CreatedServerInvitation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreatedServerInvitation")
            .field("invitation", &self.invitation)
            .finish_non_exhaustive()
    }
}

/// An account an administrator creates with a one-time password.
#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct AddAccountRequest {
    pub username: String,
    /// The one-time password: the person must replace it at their first sign-in.
    pub password: String,
}

impl std::fmt::Debug for AddAccountRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AddAccountRequest")
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}
