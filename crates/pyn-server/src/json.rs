//! JSON bodies in and out. A body that does not parse is answered with the usual error body, naming what was wrong.
#![allow(clippy::disallowed_types)]

use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, Request};
use axum::response::{IntoResponse, Response};
use pyn_core::PynError;
use serde::Serialize;

use crate::ApiError;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Json<T>(pub(crate) T);

impl<T, S> FromRequest<S> for Json<T>
where
    axum::Json<T>: FromRequest<S, Rejection = JsonRejection>,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(rejected(&rejection)),
        }
    }
}

/// Keeps axum's status (400 syntax, 415 content type, 422 data) with an `invalid_request` body.
fn rejected(rejection: &JsonRejection) -> Response {
    let mut res = ApiError(PynError::InvalidRequest(rejection.body_text())).into_response();
    *res.status_mut() = rejection.status();
    res
}

impl<T: Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}
