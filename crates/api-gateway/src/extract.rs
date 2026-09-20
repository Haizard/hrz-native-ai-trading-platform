//! Extractors that fail in this API's own envelope (`docs/12-API-GATEWAY.md`).
//!
//! ## The gap this closes
//!
//! `docs/12` requires a "consistent error envelope across REST and WS". Axum's
//! own rejections do not use it: a missing query parameter answers
//! `Failed to deserialize query string: missing field \`symbol\`` as plain text,
//! and a malformed body answers axum's own JSON shape. Both are 4xx and both are
//! *correct*, and a client that branches on `error.code` gets nothing to branch
//! on -- it has to parse prose, which is exactly what the envelope exists to
//! avoid.
//!
//! ## Why wrappers rather than `Result<Json<T>, _>` in every handler
//!
//! Accepting `Result<Json<T>, JsonRejection>` works, but it adds a line to every
//! handler body and the line is easy to forget when the next route is written.
//! These wrappers move the mapping into the extractor, so a handler declares
//! [`ApiJson`] instead of `Json` and gets the envelope for free -- and the
//! mistake becomes "used the wrong type in the signature", which the compiler
//! catches, rather than "forgot a line", which nothing does.

use axum::Json;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{FromRequest, FromRequestParts, Query, Request};
use axum::http::request::Parts;
use serde::de::DeserializeOwned;

use crate::error::ApiError;

/// `Json<T>`, rejecting with [`ApiError`].
#[derive(Debug, Clone, Copy)]
pub struct ApiJson<T>(pub T);

impl<T, S> FromRequest<S> for ApiJson<T>
where
    Json<T>: FromRequest<S, Rejection = JsonRejection>,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(request, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(ApiError::from)
    }
}

/// `Query<T>`, rejecting with [`ApiError`].
#[derive(Debug, Clone, Copy)]
pub struct ApiQuery<T>(pub T);

impl<T, S> FromRequestParts<S> for ApiQuery<T>
where
    Query<T>: FromRequestParts<S, Rejection = QueryRejection>,
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request_parts(parts, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(ApiError::from)
    }
}

// No unit tests here, on purpose.
//
// The obvious test is "construct a rejection, assert the code", and axum does
// not allow it: the variants wrap types with no public constructor, so the test
// would end up asserting something about a constructor rather than about the
// mapping.
//
// The mapping is covered end to end in `tests/market_flow.rs`, by sending
// requests that actually produce these rejections -- a missing query parameter
// and two kinds of malformed body. That is the stronger test anyway: it proves
// the envelope reaches the client, not merely that a `From` impl compiles.
