// Unified error type.
//
// Every error in the application is a `UnifiedError`. It carries the message
// shown to the caller, the HTTP status code, a metadata object, and an
// optional trace that is only ever rendered in debug / development mode.
// The JSON rendering lives in `crate::json`.

use serde_json::{Map, Value};
use std::fmt;

#[derive(Debug, Clone)]
pub struct UnifiedError {
    pub message: String,
    pub code: u16,
    pub metadata: Map<String, Value>,
    pub trace: Option<String>,
}

impl UnifiedError {
    pub fn new(message: impl Into<String>, code: u16) -> Self {
        Self {
            message: message.into(),
            code,
            metadata: Map::new(),
            trace: None,
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(message, 400)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(message, 401)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(message, 403)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(message, 404)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(message, 409)
    }

    pub fn unprocessable(message: impl Into<String>) -> Self {
        Self::new(message, 422)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(message, 500)
    }

    pub fn with_metadata(mut self, key: &str, value: Value) -> Self {
        self.metadata.insert(key.to_string(), value);
        self
    }

    pub fn with_trace(mut self, trace: impl Into<String>) -> Self {
        self.trace = Some(trace.into());
        self
    }

    /// Render the error as the standard error envelope.
    ///
    /// The object has exactly three keys - `message`, `code`, and
    /// `metadata` - plus an optional `trace` that is only included when
    /// `debug` is true.
    pub fn to_json(&self, debug: bool) -> Value {
        let mut error = Map::new();
        error.insert("message".to_string(), Value::String(self.message.clone()));
        error.insert("code".to_string(), Value::from(self.code));
        error.insert("metadata".to_string(), Value::Object(self.metadata.clone()));
        if debug {
            if let Some(trace) = &self.trace {
                error.insert("trace".to_string(), Value::String(trace.clone()));
            }
        }
        let mut root = Map::new();
        root.insert("error".to_string(), Value::Object(error));
        Value::Object(root)
    }
}

impl fmt::Display for UnifiedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (HTTP {})", self.message, self.code)
    }
}

impl std::error::Error for UnifiedError {}

impl actix_web::ResponseError for UnifiedError {
    fn status_code(&self) -> actix_web::http::StatusCode {
        actix_web::http::StatusCode::from_u16(self.code)
            .unwrap_or(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR)
    }

    fn error_response(&self) -> actix_web::HttpResponse {
        // Deliberately `debug: false`: the `ResponseError` trait
        // hands this impl no request context, so the mode is
        // unknowable here and the trace must never render. Handlers
        // that need the debug render call `json::error_response`
        // with `ctx.is_debug()` directly instead of relying on this
        // automatic conversion.
        actix_web::HttpResponse::build(self.status_code()).json(self.to_json(false))
    }
}

impl From<std::io::Error> for UnifiedError {
    fn from(error: std::io::Error) -> Self {
        Self::internal(format!("I/O error: {error}")).with_trace(format!("{error:?}"))
    }
}

impl From<serde_yaml::Error> for UnifiedError {
    fn from(error: serde_yaml::Error) -> Self {
        Self::bad_request(format!("Configuration error: {error}"))
            .with_trace(format!("{error:?}"))
    }
}

impl From<serde_json::Error> for UnifiedError {
    fn from(error: serde_json::Error) -> Self {
        // Classify the failure instead of mapping every JSON error to 400:
        // a truncated or malformed document is a client error, while an
        // I/O failure while reading JSON is a server error. The category
        // is also recorded in `metadata.kind` so callers can distinguish
        // the cases without parsing the message.
        use serde_json::error::Category;
        let kind = match error.classify() {
            Category::Io => "io",
            Category::Syntax => "syntax",
            Category::Data => "data",
            Category::Eof => "eof",
        };
        let base = match error.classify() {
            Category::Io => Self::internal(format!("JSON error: {error}")),
            _ => Self::bad_request(format!("JSON error: {error}")),
        };
        base.with_metadata("kind", Value::String(kind.to_string()))
            .with_trace(format!("{error:?}"))
    }
}

impl From<diesel::result::Error> for UnifiedError {
    fn from(error: diesel::result::Error) -> Self {
        use diesel::result::{DatabaseErrorKind, Error as DieselError};
        // Map Diesel failures to HTTP statuses without leaking database
        // internals (constraint and table names) into the client-visible
        // message; the full error stays in `trace`, rendered in debug
        // mode only. The machine-readable kind is in `metadata.kind`.
        let trace = format!("{error:?}");
        let (message, code, kind): (&str, u16, &str) = match &error {
            DieselError::NotFound => ("Record not found", 404, "not_found"),
            DieselError::DatabaseError(DatabaseErrorKind::UniqueViolation, _) => (
                "A record with the same unique value already exists",
                409,
                "unique_violation",
            ),
            DieselError::DatabaseError(DatabaseErrorKind::ForeignKeyViolation, _) => (
                "The referenced record does not exist or is still in use",
                422,
                "foreign_key_violation",
            ),
            DieselError::DatabaseError(DatabaseErrorKind::CheckViolation, _) => (
                "The record violates a database check constraint",
                422,
                "check_violation",
            ),
            DieselError::DatabaseError(DatabaseErrorKind::NotNullViolation, _) => {
                ("A required field is missing", 422, "not_null_violation")
            }
            DieselError::DatabaseError(_, _) => ("Database error", 500, "database_error"),
            _ => ("Database error", 500, "database_error"),
        };
        Self::new(message, code)
            .with_metadata("kind", Value::String(kind.to_string()))
            .with_trace(trace)
    }
}

/// A result type used across the core engine.
pub type UnifiedResult<T> = Result<T, UnifiedError>;
