// OKR / Goals - Input validation and pure value checks.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::*;
use super::types::*;
use super::store::*;
use super::helpers::*;
use super::service::*;
use super::handlers_cycles::*;
use super::handlers_objectives::*;
use super::handlers_key_results::*;
use super::hooks::*;
use super::module::*;

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

/// A number is usable as a key-result value when it is finite.
pub(crate) fn valid_number(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a finite number"
        )));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_number_accepts_finite_values_only() {
        assert_eq!(valid_number(1.5, "target").unwrap(), 1.5);
        assert_eq!(valid_number(0.0, "target").unwrap(), 0.0);
        assert!(valid_number(f64::NAN, "target").is_err());
        assert!(valid_number(f64::INFINITY, "target").is_err());
    }

    #[test]
    fn require_text_trims_and_enforces_bounds() {
        assert_eq!(require_text("  Q1 goals  ", "name", 20).unwrap(), "Q1 goals");
        assert!(require_text("", "name", 20).is_err());
        assert!(require_text("toolong", "name", 3).is_err());
    }
}
