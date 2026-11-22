// Unified error type.
//
// Every error in the application is a `UnifiedError`. It carries the message
// shown to the caller, the HTTP status code, a metadata object, and an
// optional trace that is only ever rendered in debug / development mode.
// The JSON rendering lives in `crate::core::json`.

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
    /// The object has exactly three keys — `message`, `code`, and
    /// `metadata` — plus an optional `trace` that is only included when
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
        Self::bad_request(format!("JSON error: {error}")).with_trace(format!("{error:?}"))
    }
}

/// A result type used across the core engine.
pub type UnifiedResult<T> = Result<T, UnifiedError>;
