//! Settings - Shared types: request and response bodies, views, and state shapes.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json::{error_response, success_response};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use super::{handlers::*, module::*};

pub type SettingsStore = Arc<DashMap<(String, String), SettingRecord>>;

/// One stored setting: its typed value plus the last update time.
#[derive(Debug, Clone, Serialize)]
pub struct SettingRecord {
    pub namespace: String,
    pub key: String,
    #[serde(rename = "type")]
    pub value_type: String,
    pub value: Value,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PutSettingInput {
    pub value: Value,
    /// Explicit type: `string`, `number`, `bool`, or `json`. When omitted
    /// the type is inferred from the JSON value.
    #[serde(default, rename = "type")]
    pub value_type: Option<String>,
}

pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// Infer the stored type from a JSON value.
pub(crate) fn infer_type(value: &Value) -> &'static str {
    match value {
        Value::String(_) => "string",
        Value::Number(_) => "number",
        Value::Bool(_) => "bool",
        _ => "json",
    }
}

/// Check that a value matches its declared type.
pub(crate) fn type_matches(value_type: &str, value: &Value) -> bool {
    match value_type {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "bool" => value.is_boolean(),
        "json" => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_name_cases() {
        assert!(valid_name("a-b_c.d"));
        assert!(valid_name("vcs.github"));
        assert!(!valid_name(""));
        assert!(!valid_name("a b"));
        assert!(!valid_name("a/b"));
        assert!(!valid_name("a:b"));
    }

    #[test]
    fn infer_type_cases() {
        assert_eq!(infer_type(&serde_json::json!("x")), "string");
        assert_eq!(infer_type(&serde_json::json!(1.5)), "number");
        assert_eq!(infer_type(&serde_json::json!(true)), "bool");
        assert_eq!(infer_type(&serde_json::json!(null)), "json");
        assert_eq!(infer_type(&serde_json::json!([1, 2])), "json");
        assert_eq!(infer_type(&serde_json::json!({"a": 1})), "json");
    }

    #[test]
    fn type_matches_cases() {
        assert!(type_matches("string", &serde_json::json!("x")));
        assert!(!type_matches("string", &serde_json::json!(1)));
        assert!(type_matches("number", &serde_json::json!(1.5)));
        assert!(!type_matches("number", &serde_json::json!("1")));
        assert!(type_matches("bool", &serde_json::json!(false)));
        assert!(!type_matches("bool", &serde_json::json!(0)));
        assert!(type_matches("json", &serde_json::json!({"a": 1})));
        assert!(type_matches("json", &serde_json::json!(null)));
        assert!(!type_matches("weird", &serde_json::json!("x")));
    }
}
