// Generic Approvals — Input validation and pure value checks.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::tenancy::TenantContext as TenancyContext;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::*;
use super::types::*;
use super::store::*;
use super::helpers::*;
use super::service::*;
use super::handlers_definitions::*;
use super::handlers_requests::*;
use super::handlers_dashboards::*;
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

pub(crate) fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else if trimmed.len() > max {
                Err(UnifiedError::bad_request(format!(
                    "{field} must be at most {max} characters"
                )))
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
    }
}

/// A module id / record type: lowercase letters, digits, dots,
/// underscores, and hyphens, never leading / trailing with a dot, no
/// empty segments (for example `invoices`, `purchase.order`).
pub(crate) fn valid_record_key(value: &str, max: usize) -> bool {
    if value.is_empty() || value.len() > max {
        return false;
    }
    if value.starts_with('.') || value.ends_with('.') || value.contains("..") {
        return false;
    }
    value.chars().all(|c| {
        c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_' || c == '-'
    })
}

pub(crate) fn validate_approver(slot: &StepApprover) -> UnifiedResult<()> {
    if !APPROVER_KINDS.contains(&slot.kind.as_str()) {
        return Err(UnifiedError::bad_request(format!(
            "Unknown approver kind: {} (expected one of {})",
            slot.kind,
            APPROVER_KINDS.join(", ")
        )));
    }
    match slot.kind.as_str() {
        APPROVER_KIND_USER => {
            require_text(&slot.ref_value, "approver ref (user id)", MAX_USER_ID_LEN)?;
        }
        APPROVER_KIND_ROLE => {
            require_text(&slot.ref_value, "approver ref (role slug)", MAX_ROLE_LEN)?;
        }
        APPROVER_KIND_MANAGER => {
            // The ref is ignored for a manager slot; anything stored
            // there is a caller mistake, not data.
            if !slot.ref_value.trim().is_empty() && slot.ref_value.trim() != "manager" {
                return Err(UnifiedError::bad_request(
                    "A manager approver slot takes no ref value",
                ));
            }
        }
        _ => {
            // The kind was validated against APPROVER_KINDS above,
            // so this arm is unreachable in practice — but a panic
            // here would take down the request if the list and this
            // match ever drift apart, so fail with a proper error
            // instead of `unreachable!()`.
            return Err(UnifiedError::bad_request(format!(
                "Unknown approver kind: {} (expected one of {})",
                slot.kind,
                APPROVER_KINDS.join(", ")
            )));
        }
    }
    Ok(())
}

pub(crate) fn validate_approvers(slots: &[StepApprover], max: usize) -> UnifiedResult<()> {
    if slots.is_empty() {
        return Err(UnifiedError::bad_request(
            "A step needs at least one approver",
        ));
    }
    if slots.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "A step holds at most {max} approvers"
        )));
    }
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for slot in slots {
        validate_approver(slot)?;
        if !seen.insert((slot.kind.clone(), slot.ref_value.clone())) {
            return Err(UnifiedError::bad_request(format!(
                "Duplicate approver slot: {}:{}",
                slot.kind, slot.ref_value
            )));
        }
    }
    Ok(())
}

pub(crate) fn validate_timeout(value: Option<i64>, field: &str) -> UnifiedResult<Option<i64>> {
    match value {
        None => Ok(None),
        Some(hours) => {
            if hours < 1 || hours > MAX_TIMEOUT_HOURS {
                return Err(UnifiedError::bad_request(format!(
                    "{field} must be between 1 and {MAX_TIMEOUT_HOURS} hours"
                )));
            }
            Ok(Some(hours))
        }
    }
}

pub(crate) fn validate_extension(value: Option<i64>, field: &str) -> UnifiedResult<Option<i64>> {
    match value {
        None => Ok(None),
        Some(hours) => {
            if hours < 1 || hours > MAX_ESCALATION_EXTENSION_HOURS {
                return Err(UnifiedError::bad_request(format!(
                    "{field} must be between 1 and {MAX_ESCALATION_EXTENSION_HOURS} hours"
                )));
            }
            Ok(Some(hours))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_record_key_accepts_dotted_lowercase_keys() {
        assert!(valid_record_key("invoices", 128));
        assert!(valid_record_key("purchase.order", 128));
        assert!(valid_record_key("a_b-c.d", 128));
        assert!(!valid_record_key("Invoices", 128));
        assert!(!valid_record_key(".leading", 128));
        assert!(!valid_record_key("trailing.", 128));
        assert!(!valid_record_key("a..b", 128));
        assert!(!valid_record_key("", 128));
        assert!(!valid_record_key("a b", 128));
        assert!(!valid_record_key(&"a".repeat(129), 128));
    }

    #[test]
    fn parse_uuid_accepts_valid_ids_only() {
        assert!(parse_uuid("00000000-0000-0000-0000-000000000000", "request").is_ok());
        assert!(parse_uuid("nope", "request").is_err());
    }

    #[test]
    fn validate_timeout_enforces_the_hour_bounds() {
        assert_eq!(validate_timeout(None, "timeout_hours").unwrap(), None);
        assert_eq!(validate_timeout(Some(1), "timeout_hours").unwrap(), Some(1));
        assert_eq!(validate_timeout(Some(720), "timeout_hours").unwrap(), Some(720));
        assert!(validate_timeout(Some(0), "timeout_hours").is_err());
        assert!(validate_timeout(Some(721), "timeout_hours").is_err());
        assert!(validate_timeout(Some(-5), "timeout_hours").is_err());
    }

    #[test]
    fn validate_extension_enforces_the_hour_bounds() {
        assert_eq!(validate_extension(None, "extension").unwrap(), None);
        assert_eq!(validate_extension(Some(24), "extension").unwrap(), Some(24));
        assert!(validate_extension(Some(0), "extension").is_err());
        assert!(validate_extension(Some(721), "extension").is_err());
    }

    #[test]
    fn validate_approver_checks_kind_and_ref() {
        let user = StepApprover { kind: "user".to_string(), ref_value: "user-7".to_string() };
        assert!(validate_approver(&user).is_ok());
        let empty_user = StepApprover { kind: "user".to_string(), ref_value: "  ".to_string() };
        assert!(validate_approver(&empty_user).is_err());
        let role = StepApprover { kind: "role".to_string(), ref_value: "finance".to_string() };
        assert!(validate_approver(&role).is_ok());
        let manager = StepApprover { kind: "manager".to_string(), ref_value: String::new() };
        assert!(validate_approver(&manager).is_ok());
        let manager_word = StepApprover { kind: "manager".to_string(), ref_value: "manager".to_string() };
        assert!(validate_approver(&manager_word).is_ok());
        let manager_ref = StepApprover { kind: "manager".to_string(), ref_value: "someone".to_string() };
        assert!(validate_approver(&manager_ref).is_err());
        let unknown = StepApprover { kind: "group".to_string(), ref_value: "x".to_string() };
        assert!(validate_approver(&unknown).is_err());
    }

    #[test]
    fn validate_approvers_rejects_empty_and_duplicate_slots() {
        assert!(validate_approvers(&[], 10).is_err());
        let slot = StepApprover { kind: "user".to_string(), ref_value: "user-7".to_string() };
        assert!(validate_approvers(std::slice::from_ref(&slot), 10).is_ok());
        assert!(validate_approvers(&[slot.clone(), slot], 10).is_err());
    }

    #[test]
    fn validate_approvers_enforces_the_maximum() {
        let first = StepApprover { kind: "user".to_string(), ref_value: "user-1".to_string() };
        let second = StepApprover { kind: "user".to_string(), ref_value: "user-2".to_string() };
        assert!(validate_approvers(&[first, second], 1).is_err());
    }

    #[test]
    fn optional_text_normalises_blank_to_none() {
        assert_eq!(optional_text(&None, "note", 10).unwrap(), None);
        assert_eq!(optional_text(&Some("  ".to_string()), "note", 10).unwrap(), None);
        assert_eq!(
            optional_text(&Some("  hi  ".to_string()), "note", 10).unwrap(),
            Some("hi".to_string())
        );
        assert!(optional_text(&Some("toolong".to_string()), "note", 3).is_err());
    }
}
