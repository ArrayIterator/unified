// JSON API envelope.
//
// The API standard is fixed for the whole application:
//
//   - Success: exactly one top-level key, `data`.
//   - Error: exactly one top-level key, `error`, whose object has exactly
//     three keys — `message`, `code`, `metadata` — plus an optional
//     `trace` that is only rendered in debug / development mode.
//
// No handler may return any other envelope.

use crate::core::error::UnifiedError;
use actix_web::HttpResponse;
use serde::Serialize;
use serde_json::{json, Value};

/// Build a success envelope: `{"data": ...}`.
pub fn success<T: Serialize>(data: T) -> Value {
    json!({ "data": data })
}

/// Build an error envelope from a `UnifiedError`.
pub fn error_json(error: &UnifiedError, debug: bool) -> Value {
    error.to_json(debug)
}

/// Build an `HttpResponse` carrying a success envelope.
pub fn success_response<T: Serialize>(data: T, status: u16) -> HttpResponse {
    HttpResponse::build(
        actix_web::http::StatusCode::from_u16(status).unwrap_or(actix_web::http::StatusCode::OK),
    )
    .json(success(data))
}

/// Build an `HttpResponse` carrying an error envelope.
pub fn error_response(error: &UnifiedError, debug: bool) -> HttpResponse {
    HttpResponse::build(
        actix_web::http::StatusCode::from_u16(error.code)
            .unwrap_or(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR),
    )
    .json(error_json(error, debug))
}

/// A small helper for handlers: turn a `Result<T, UnifiedError>` into a
/// success or error `HttpResponse` using the standard envelope.
pub fn respond<T: Serialize>(result: Result<T, UnifiedError>, debug: bool) -> HttpResponse {
    match result {
        Ok(data) => success_response(data, 200),
        Err(error) => error_response(&error, debug),
    }
}
