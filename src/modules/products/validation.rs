//! Validation, slug and taxonomy-tree helpers for products.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpResponse};
use chrono::{DateTime, Utc};
use serde::{Serialize};
use serde_json::{json};
use uuid::Uuid;
use super::types::{BILLING_PERIODS, BUNDLE_RULES, LicenseTemplate, MAX_DUNNING_RETRIES, MAX_DUNNING_RETRY_INTERVAL_DAYS, MAX_GRACE_PERIOD_DAYS, PRICING_MODELS, PRODUCT_KINDS, PRORATION_POLICIES, TaxonomyNode, TaxonomyTreeNode, default_dunning_max_retries, default_dunning_retry_interval_days, default_proration_policy};
use super::state::ProductsState;

// Helpers

pub(crate) fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
}

pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

pub(crate) fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

pub(crate) fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true;
    for ch in name.trim().to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    slug.trim_matches('-').to_string()
}

pub(crate) fn validate_kind(kind: &str) -> UnifiedResult<()> {
    if PRODUCT_KINDS.contains(&kind) {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(format!(
            "Kind must be one of: {}",
            PRODUCT_KINDS.join(", ")
        )))
    }
}

pub(crate) fn validate_pricing_model(model: &str) -> UnifiedResult<()> {
    if PRICING_MODELS.contains(&model) {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(format!(
            "Pricing model must be one of: {}",
            PRICING_MODELS.join(", ")
        )))
    }
}

pub(crate) fn validate_billing_period(period: &str) -> UnifiedResult<()> {
    if BILLING_PERIODS.contains(&period) {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(format!(
            "Billing period must be one of: {}",
            BILLING_PERIODS.join(", ")
        )))
    }
}

pub(crate) fn validate_proration_policy(policy: &str) -> UnifiedResult<()> {
    if PRORATION_POLICIES.contains(&policy) {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(format!(
            "Proration policy must be one of: {}",
            PRORATION_POLICIES.join(", ")
        )))
    }
}

/// Validate the subscription plan fields and resolve them to
/// concrete values. Range checks always apply; a non-default value
/// on a product whose pricing model is not `subscription` is
/// rejected, following the bundle-rule precedent (plan fields only
/// take effect for subscription products). Returns
/// `(proration_policy, grace_period_days, dunning_max_retries,
/// dunning_retry_interval_days)`.
pub(crate) fn resolve_subscription_settings(
    pricing_model: &str,
    proration_policy: Option<String>,
    grace_period_days: Option<u32>,
    dunning_max_retries: Option<u32>,
    dunning_retry_interval_days: Option<u32>,
) -> UnifiedResult<(String, u32, u32, u32)> {
    let policy = proration_policy.unwrap_or_else(default_proration_policy);
    validate_proration_policy(&policy)?;
    let grace = grace_period_days.unwrap_or(0);
    if grace > MAX_GRACE_PERIOD_DAYS {
        return Err(UnifiedError::bad_request(format!(
            "Grace period must be at most {MAX_GRACE_PERIOD_DAYS} days"
        )));
    }
    let max_retries = dunning_max_retries.unwrap_or_else(default_dunning_max_retries);
    if max_retries > MAX_DUNNING_RETRIES {
        return Err(UnifiedError::bad_request(format!(
            "Dunning max retries must be at most {MAX_DUNNING_RETRIES}"
        )));
    }
    let interval =
        dunning_retry_interval_days.unwrap_or_else(default_dunning_retry_interval_days);
    if interval == 0 || interval > MAX_DUNNING_RETRY_INTERVAL_DAYS {
        return Err(UnifiedError::bad_request(format!(
            "Dunning retry interval must be between 1 and {MAX_DUNNING_RETRY_INTERVAL_DAYS} days"
        )));
    }
    if pricing_model != "subscription"
        && (policy != default_proration_policy()
            || grace != 0
            || max_retries != default_dunning_max_retries()
            || interval != default_dunning_retry_interval_days())
    {
        return Err(UnifiedError::bad_request(
            "Subscription plan fields (proration policy, grace period, dunning settings) only apply to subscription products",
        ));
    }
    Ok((policy, grace, max_retries, interval))
}

pub(crate) fn validate_bundle_rule(rule: &str) -> UnifiedResult<()> {
    if BUNDLE_RULES.contains(&rule) {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(format!(
            "Bundle rule must be one of: {}",
            BUNDLE_RULES.join(", ")
        )))
    }
}

pub(crate) fn validate_bundle_discount_bps(value: i64) -> UnifiedResult<()> {
    if (0..=10_000).contains(&value) {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(
            "Bundle discount must be between 0 and 10000 basis points",
        ))
    }
}

pub(crate) fn validate_tax_rate_bps(value: i64) -> UnifiedResult<()> {
    if (0..=10_000).contains(&value) {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(
            "Tax rate must be between 0 and 10000 basis points",
        ))
    }
}

pub(crate) fn validate_region(region: &str) -> UnifiedResult<()> {
    if region == "GLOBAL"
        || (region.len() == 2 && region.chars().all(|c| c.is_ascii_alphabetic()))
    {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(
            "Region must be a 2-letter country code or GLOBAL",
        ))
    }
}

pub(crate) fn validate_license_template(template: &LicenseTemplate) -> UnifiedResult<()> {
    if template.seats < 1 {
        return Err(UnifiedError::bad_request("License seats must be at least 1"));
    }
    if template.download_limit < 1 {
        return Err(UnifiedError::bad_request(
            "License download limit must be at least 1",
        ));
    }
    if let Some(days) = template.duration_days {
        if days < 1 {
            return Err(UnifiedError::bad_request(
                "License duration must be at least 1 day",
            ));
        }
    }
    Ok(())
}

/// Validated lifecycle transitions. `retired` is terminal; every
/// other move must be listed here.
pub(crate) fn validate_status_transition(from: &str, to: &str) -> UnifiedResult<()> {
    if from == to {
        return Err(UnifiedError::conflict(format!(
            "Product is already {from}"
        )));
    }
    let allowed = match from {
        "draft" => ["active", "retired"].as_slice(),
        "active" => ["deprecated", "retired"].as_slice(),
        "deprecated" => ["active", "retired"].as_slice(),
        "retired" => &[],
        _ => &[],
    };
    if allowed.contains(&to) {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(format!(
            "Cannot move product from {from} to {to}"
        )))
    }
}

pub(crate) fn validate_hex_checksum(value: &str, expected_len: usize, field: &str) -> UnifiedResult<()> {
    let valid =
        value.len() == expected_len && value.chars().all(|ch| ch.is_ascii_hexdigit());
    if valid {
        Ok(())
    } else {
        Err(UnifiedError::bad_request(format!(
            "{field} must be {expected_len} hex characters"
        )))
    }
}

pub(crate) fn taxonomy_path(state: &ProductsState, parent_id: Option<&str>, slug: &str) -> (i64, String) {
    match parent_id.and_then(|id| state.taxonomy.get(id)) {
        Some(parent) => (parent.depth + 1, format!("{}/{}", parent.path, slug)),
        None => (0, format!("/{slug}")),
    }
}

/// Recompute depth and path for a node and every descendant after a move.
pub(crate) fn recompute_subtree(state: &ProductsState, root_id: &str) {
    let mut queue: Vec<String> = vec![root_id.to_string()];
    while let Some(id) = queue.pop() {
        let (parent_id, slug) = match state.taxonomy.get(&id) {
            Some(node) => (node.parent_id.clone(), node.slug.clone()),
            None => continue,
        };
        let (depth, path) = taxonomy_path(state, parent_id.as_deref(), &slug);
        if let Some(mut node) = state.taxonomy.get_mut(&id) {
            node.depth = depth;
            node.path = path;
            node.updated_at = now();
        }
        let children: Vec<String> = state
            .taxonomy
            .iter()
            .filter(|entry| entry.value().parent_id.as_deref() == Some(id.as_str()))
            .map(|entry| entry.key().clone())
            .collect();
        queue.extend(children);
    }
}

pub(crate) fn build_tree(
    state: &ProductsState,
    parent_id: Option<&str>,
    kind: Option<&str>,
) -> Vec<TaxonomyTreeNode> {
    let mut nodes: Vec<TaxonomyNode> = state
        .taxonomy
        .iter()
        .filter(|entry| {
            entry.value().parent_id.as_deref() == parent_id
                && kind.is_none_or(|kind| entry.value().kind == kind)
        })
        .map(|entry| entry.value().clone())
        .collect();
    nodes.sort_by(|a, b| {
        a.sort_order
            .cmp(&b.sort_order)
            .then_with(|| a.name.cmp(&b.name))
    });
    nodes
        .into_iter()
        .map(|node| TaxonomyTreeNode {
            children: build_tree(state, Some(node.id.as_str()), kind),
            node,
        })
        .collect()
}

pub(crate) fn is_descendant_of(state: &ProductsState, node_id: &str, ancestor_id: &str) -> bool {
    let mut current = state
        .taxonomy
        .get(node_id)
        .and_then(|node| node.parent_id.clone());
    while let Some(id) = current {
        if id == ancestor_id {
            return true;
        }
        current = state
            .taxonomy
            .get(&id)
            .and_then(|node| node.parent_id.clone());
    }
    false
}

pub(crate) fn respond_created<T: Serialize>(result: UnifiedResult<T>, debug: bool) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) fn validate_product_refs(
    state: &ProductsState,
    vendor_company_id: Option<&str>,
    group_id: Option<&str>,
    category_id: Option<&str>,
) -> UnifiedResult<()> {
    if let Some(vendor) = vendor_company_id {
        if vendor.trim().is_empty() {
            return Err(UnifiedError::bad_request("Vendor company id must not be empty"));
        }
        if vendor.parse::<Uuid>().is_err() {
            return Err(UnifiedError::bad_request(
                "Vendor company id must be a UUID",
            ));
        }
        if !state.vendor_exists(vendor) {
            return Err(UnifiedError::not_found("Vendor company not found"));
        }
    }
    if let Some(group_id) = group_id {
        match state.taxonomy.get(group_id) {
            Some(node) if node.kind == "group" => {}
            _ => return Err(UnifiedError::not_found("Group not found")),
        }
    }
    if let Some(category_id) = category_id {
        match state.taxonomy.get(category_id) {
            Some(node) if node.kind == "category" => {}
            _ => return Err(UnifiedError::not_found("Category not found")),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_collapses_separators() {
        assert_eq!(slugify("Hello World!"), "hello-world");
        assert_eq!(slugify("  spaced  out  "), "spaced-out");
        assert_eq!(slugify("already-slug"), "already-slug");
        assert_eq!(slugify(""), "");
    }

    #[test]
    fn validate_kind_accepts_known_kinds() {
        for kind in PRODUCT_KINDS {
            assert!(validate_kind(kind).is_ok(), "kind {kind}");
        }
        assert!(validate_kind("app").is_err());
        assert!(validate_kind("").is_err());
    }

    #[test]
    fn validate_pricing_model_billing_period_and_proration() {
        assert!(validate_pricing_model("subscription").is_ok());
        assert!(validate_pricing_model("free").is_err());
        assert!(validate_billing_period("monthly").is_ok());
        assert!(validate_billing_period("daily").is_err());
        assert!(validate_proration_policy("prorate").is_ok());
        assert!(validate_proration_policy("never").is_err());
    }

    #[test]
    fn validate_status_transition_rules() {
        assert!(validate_status_transition("draft", "active").is_ok());
        assert!(validate_status_transition("active", "deprecated").is_ok());
        assert!(validate_status_transition("deprecated", "active").is_ok());
        assert!(validate_status_transition("active", "retired").is_ok());
        assert!(validate_status_transition("draft", "draft").is_err());
        assert!(validate_status_transition("retired", "active").is_err());
        assert!(validate_status_transition("draft", "deprecated").is_err());
    }

    #[test]
    fn validate_bundle_discount_and_tax_rate_bounds() {
        assert!(validate_bundle_discount_bps(0).is_ok());
        assert!(validate_bundle_discount_bps(10_000).is_ok());
        assert!(validate_bundle_discount_bps(10_001).is_err());
        assert!(validate_bundle_discount_bps(-1).is_err());
        assert!(validate_tax_rate_bps(0).is_ok());
        assert!(validate_tax_rate_bps(10_000).is_ok());
        assert!(validate_tax_rate_bps(10_001).is_err());
    }

    #[test]
    fn validate_region_accepts_country_codes_and_global() {
        assert!(validate_region("US").is_ok());
        assert!(validate_region("GLOBAL").is_ok());
        assert!(validate_region("us").is_ok());
        assert!(validate_region("USA").is_err());
        assert!(validate_region("U1").is_err());
        assert!(validate_region("").is_err());
    }

    #[test]
    fn validate_hex_checksum_checks_length_and_digits() {
        assert!(validate_hex_checksum("abcdef0123456789", 16, "sha256").is_ok());
        assert!(validate_hex_checksum("abc", 16, "sha256").is_err());
        assert!(validate_hex_checksum("zzzzzzzzzzzzzzzz", 16, "sha256").is_err());
    }

    #[test]
    fn validate_hex_checksum_accepts_uppercase_and_names_hex_not_lowercase() {
        // The check accepts any ASCII hex digit, so the message must
        // say "hex characters", not "lowercase hex characters".
        assert!(validate_hex_checksum("ABCDEF0123456789", 16, "sha256").is_ok());
        let error = validate_hex_checksum("zz", 16, "sha256").unwrap_err();
        assert!(error.message.contains("16 hex characters"));
        assert!(!error.message.contains("lowercase"));
    }

    #[test]
    fn resolve_subscription_settings_defaults() {
        let settings =
            resolve_subscription_settings("subscription", None, None, None, None)
                .unwrap();
        assert_eq!(settings, ("prorate".to_string(), 0, 3, 3));
    }

    #[test]
    fn resolve_subscription_settings_rejects_out_of_range() {
        assert!(
            resolve_subscription_settings("subscription", None, Some(91), None, None)
                .is_err()
        );
        assert!(
            resolve_subscription_settings("subscription", None, None, Some(11), None)
                .is_err()
        );
        assert!(
            resolve_subscription_settings("subscription", None, None, None, Some(0))
                .is_err()
        );
    }

    #[test]
    fn resolve_subscription_settings_rejects_plan_fields_for_one_time() {
        assert!(resolve_subscription_settings(
            "one_time",
            Some("credit_only".to_string()),
            None,
            None,
            None,
        )
        .is_err());
        assert!(
            resolve_subscription_settings("one_time", None, None, None, None).is_ok()
        );
    }
}
