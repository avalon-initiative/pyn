//! The second factor at sign-in and the fresh check before sensitive actions.

use anyhow::Result;
use pyn_proto as api;

use crate::client::{Api, ApiError};
use crate::credentials;
use crate::transport::Method;

/// Codes asked for before giving up.
const MAX_CODES: usize = 3;

fn code_of(err: &anyhow::Error) -> Option<&str> {
    err.downcast_ref::<ApiError>().map(|e| e.code.as_str())
}

pub fn is_fresh_required(err: &anyhow::Error) -> bool {
    code_of(err) == Some("fresh_check_required")
}

fn ask(retry: bool) -> Result<String> {
    if retry {
        eprintln!("that code was not accepted");
    }
    credentials::read_code("Authenticator code or recovery code: ")
}

/// Signs in, asking for the second-factor code when the account has one.
pub fn login(api: &Api, username: &str, password: &str) -> Result<api::CreatedToken> {
    let mut code = None;
    for attempt in 0..=MAX_CODES {
        let body = api::LoginRequest {
            username: username.to_string(),
            password: password.to_string(),
            code: code.take(),
            remember: false,
        };
        let sent = api.send(api.anonymous(Method::POST, "/v1/login").json(&body));
        match sent {
            Ok(reply) => return reply.json(),
            Err(e) if code_of(&e) == Some("second_factor_required") && attempt == 0 => {
                code = Some(ask(false)?);
            }
            Err(e) if code_of(&e) == Some("second_factor_invalid") && attempt + 1 < MAX_CODES => {
                code = Some(ask(true)?);
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!("the last attempt returns")
}

/// Passes the second factor again so the credential may do sensitive actions for a while.
pub fn fresh_check(api: &Api) -> Result<()> {
    eprintln!("this action needs a second-factor check");
    let mut retry = false;
    for attempt in 0..MAX_CODES {
        let body = api::CodeRequest { code: ask(retry)? };
        match api.send(
            api.request(Method::POST, "/v1/me/two-factor/verify")
                .json(&body),
        ) {
            Ok(_) => return Ok(()),
            Err(e) if code_of(&e) == Some("second_factor_invalid") && attempt + 1 < MAX_CODES => {
                retry = true;
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!("the last attempt returns")
}
