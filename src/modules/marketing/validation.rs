// Marketing Automation — Input validation and pure value checks.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json as api_json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::*;
use super::types::*;
use super::store::*;
use super::helpers::*;
use super::service::*;
use super::service_sequences::*;
use super::handlers_campaigns::*;
use super::handlers_segments::*;
use super::handlers_sequences::*;
use super::handlers_scores::*;
use super::handlers_audience::*;
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

pub(crate) fn valid_email(email: &str) -> bool {
    let email = email.trim();
    if email.is_empty() || email.len() > 254 || email.contains(' ') {
        return false;
    }
    match email.split_once('@') {
        Some((local, domain)) => {
            !local.is_empty()
                && !domain.is_empty()
                && domain.contains('.')
                && email.matches('@').count() == 1
        }
        None => false,
    }
}

// ---------------------------------------------------------------------------
// Segment rule evaluation (rules are data, evaluated server-side).
// ---------------------------------------------------------------------------

/// Validate a segment rule tree against the configured field and
/// operator allowlists. Returns the number of leaf rules found.
pub(crate) fn validate_rule_tree(
    tree: &Value,
    settings: &MarketingSettings,
    depth: usize,
) -> UnifiedResult<usize> {
    if depth > MAX_RULE_DEPTH {
        return Err(UnifiedError::bad_request(format!(
            "Segment rule tree is nested too deep (maximum depth {MAX_RULE_DEPTH})"
        )));
    }
    let object = tree
        .as_object()
        .ok_or_else(|| UnifiedError::bad_request("Segment rule must be a JSON object"))?;
    if let Some(rules) = object.get("rules") {
        let operator = object
            .get("operator")
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                UnifiedError::bad_request("Segment rule group needs an \"operator\" of \"and\" or \"or\"")
            })?;
        if operator != "and" && operator != "or" {
            return Err(UnifiedError::bad_request(
                "Segment rule group operator must be \"and\" or \"or\"",
            ));
        }
        let rules = rules
            .as_array()
            .ok_or_else(|| UnifiedError::bad_request("Segment rule group \"rules\" must be an array"))?;
        if rules.is_empty() || rules.len() > MAX_RULE_LEAVES {
            return Err(UnifiedError::bad_request(format!(
                "Segment rule group must contain 1 to {MAX_RULE_LEAVES} rules"
            )));
        }
        let mut leaves = 0usize;
        for rule in rules {
            leaves += validate_rule_tree(rule, settings, depth + 1)?;
        }
        Ok(leaves)
    } else {
        let field = object
            .get("field")
            .and_then(|value| value.as_str())
            .ok_or_else(|| UnifiedError::bad_request("Segment rule leaf needs a \"field\""))?;
        if !settings.segment_fields.iter().any(|name| name == field) {
            return Err(UnifiedError::bad_request(format!(
                "Segment field \"{field}\" is not in the configured segment field list"
            )));
        }
        let op = object
            .get("op")
            .and_then(|value| value.as_str())
            .ok_or_else(|| UnifiedError::bad_request("Segment rule leaf needs an \"op\""))?;
        if !settings.segment_operators.iter().any(|name| name == op) {
            return Err(UnifiedError::bad_request(format!(
                "Segment operator \"{op}\" is not in the configured operator list"
            )));
        }
        if !object.contains_key("value") && op != "is_empty" && op != "is_not_empty" {
            return Err(UnifiedError::bad_request(
                "Segment rule leaf needs a \"value\" for this operator",
            ));
        }
        Ok(1)
    }
}

pub(crate) fn validate_campaign_input(
    state: &MarketingState,
    segment_id: &Option<String>,
    ab_winner_metric: &Option<String>,
) -> UnifiedResult<()> {
    if let Some(segment_id) = segment_id {
        let id = parse_uuid(segment_id, "segment")?;
        if !state.segments.contains_key(&id.to_string()) {
            return Err(UnifiedError::not_found("Segment not found"));
        }
    }
    if let Some(metric) = ab_winner_metric {
        if metric != "open_rate" && metric != "click_rate" {
            return Err(UnifiedError::bad_request(
                "ab_winner_metric must be \"open_rate\" or \"click_rate\"",
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_trigger_kind(kind: &str) -> UnifiedResult<()> {
    if matches!(kind, "manual" | "form_submitted" | "purchase" | "tag_added") {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(
            "trigger_kind must be one of: manual, form_submitted, purchase, tag_added",
        ))
    }
}

pub(crate) fn validate_step_condition(condition: &Value) -> UnifiedResult<()> {
    let condition_type = condition
        .get("type")
        .and_then(|value| value.as_str())
        .ok_or_else(|| UnifiedError::bad_request("Step condition needs a \"type\""))?;
    if !matches!(
        condition_type,
        "always" | "opened" | "clicked" | "did_not_open" | "did_not_click"
    ) {
        return Err(UnifiedError::bad_request(
            "Step condition type must be one of: always, opened, clicked, did_not_open, did_not_click",
        ));
    }
    if condition_type != "always" {
        let step_id = condition
            .get("step_id")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        if step_id.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "Step condition needs a \"step_id\" for branching conditions",
            ));
        }
        parse_uuid(step_id, "condition step")?;
        if let Some(window) = condition.get("window_hours") {
            let hours = window.as_u64().ok_or_else(|| {
                UnifiedError::bad_request("Step condition \"window_hours\" must be a positive integer")
            })?;
            if hours == 0 || hours > 24 * 90 {
                return Err(UnifiedError::bad_request(
                    "Step condition \"window_hours\" must be between 1 and 2160",
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn valid_email_accepts_well_formed_addresses() {
        assert!(valid_email("ana@example.com"));
        assert!(valid_email("  ana@example.com  "));
        assert!(valid_email("first.last+tag@mail.example.co.id"));
    }

    #[test]
    fn valid_email_rejects_malformed_addresses() {
        assert!(!valid_email(""));
        assert!(!valid_email("plainaddress"));
        assert!(!valid_email("a@@b.com"));
        assert!(!valid_email("a@b"));
        assert!(!valid_email("a b@c.com"));
        assert!(!valid_email("@example.com"));
    }

    #[test]
    fn require_text_trims_and_enforces_bounds() {
        assert_eq!(require_text("  hello  ", "title", 10).unwrap(), "hello");
        assert!(require_text("", "title", 10).is_err());
        assert!(require_text("   ", "title", 10).is_err());
        assert!(require_text("toolong", "title", 3).is_err());
    }

    #[test]
    fn parse_uuid_accepts_valid_ids_only() {
        assert!(parse_uuid("00000000-0000-0000-0000-000000000000", "segment").is_ok());
        assert!(parse_uuid("not-a-uuid", "segment").is_err());
    }

    #[test]
    fn validate_trigger_kind_accepts_known_kinds() {
        for kind in ["manual", "form_submitted", "purchase", "tag_added"] {
            assert!(validate_trigger_kind(kind).is_ok(), "{kind} should be valid");
        }
        assert!(validate_trigger_kind("email_opened").is_err());
    }

    #[test]
    fn validate_step_condition_requires_step_for_branching() {
        assert!(validate_step_condition(&json!({"type": "always"})).is_ok());
        assert!(validate_step_condition(&json!({"type": "opened"})).is_err());
        assert!(validate_step_condition(
            &json!({"type": "opened", "step_id": "00000000-0000-0000-0000-000000000000"})
        )
        .is_ok());
        assert!(validate_step_condition(&json!({"type": "bogus"})).is_err());
        assert!(validate_step_condition(
            &json!({"type": "clicked", "step_id": "00000000-0000-0000-0000-000000000000", "window_hours": 0})
        )
        .is_err());
    }

    #[test]
    fn validate_rule_tree_counts_leaf_rules() {
        let settings = MarketingSettings::default();
        let leaf = json!({"field": "email", "op": "eq", "value": "a@example.com"});
        assert_eq!(validate_rule_tree(&leaf, &settings, 0).unwrap(), 1);
        let group = json!({"operator": "and", "rules": [leaf.clone(), leaf]});
        assert_eq!(validate_rule_tree(&group, &settings, 0).unwrap(), 2);
    }

    #[test]
    fn validate_rule_tree_rejects_unknown_fields_and_operators() {
        let settings = MarketingSettings::default();
        assert!(validate_rule_tree(
            &json!({"field": "nope", "op": "eq", "value": "x"}),
            &settings, 0
        )
        .is_err());
        assert!(validate_rule_tree(
            &json!({"field": "email", "op": "regex", "value": "x"}),
            &settings, 0
        )
        .is_err());
        assert!(validate_rule_tree(
            &json!({"operator": "xor", "rules": [{"field": "email", "op": "eq", "value": "x"}]}),
            &settings, 0
        )
        .is_err());
    }

    #[test]
    fn validate_rule_tree_allows_empty_checks_without_a_value() {
        let settings = MarketingSettings::default();
        assert_eq!(
            validate_rule_tree(&json!({"field": "email", "op": "is_empty"}), &settings, 0).unwrap(),
            1
        );
        assert!(validate_rule_tree(&json!({"field": "email", "op": "eq"}), &settings, 0).is_err());
    }

    #[test]
    fn validate_rule_tree_rejects_excessive_depth() {
        let settings = MarketingSettings::default();
        assert!(validate_rule_tree(
            &json!({"field": "email", "op": "eq", "value": "x"}),
            &settings,
            MAX_RULE_DEPTH + 1
        )
        .is_err());
    }
}
