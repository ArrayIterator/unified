//! tenants - helpers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::tenancy::{schema_name};
use actix_web::{HttpResponse};
use chrono::{DateTime, Utc};
use serde::{Serialize};
use serde_json::{json, Value};
use uuid::Uuid;
use super::{constants::*, vocabulary::*, events::*, settings::*, records::*, state::*, auth::*, provisioning::*, destroy::*, handlers::*, module::*, tenancy::*};



// Helpers.
pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
}


pub(crate) fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}


pub(crate) fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}


pub(crate) fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
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


/// Database pool attached to the context, if one is configured.
/// Provisioning steps that need the database fail closed when
/// there is no pool instead of pretending to succeed.
pub(crate) fn db_pool(ctx: &AppContext) -> Option<unified_core::db::DbPool> {
    ctx.db.read().expect("db lock poisoned").clone()
}


/// Emit one audit-shaped event on the hook bus: the audit module
/// records events by name, so the payload carries the actor, the
/// action, and the target in a fixed shape.
pub(crate) fn emit_audit(
    ctx: &AppContext,
    event: &str,
    actor_id: &str,
    action: &str,
    target_type: &str,
    target_id: &str,
    company_id: Option<Uuid>,
    details: Value,
) {
    let mut payload = serde_json::json!({
        "actor_id": actor_id,
        "action": action,
        "target_type": target_type,
        "target_id": target_id,
        "details": details,
        "recorded_at": Utc::now(),
    });
    if let Some(company_id) = company_id {
        payload["company_id"] = serde_json::json!(company_id);
    }
    ctx.hooks.emit(event, payload);
}


/// Validate and normalize a tenant slug against the live settings:
/// lowercase, within the configured length range, DNS-label
/// characters only, never starting or ending with a hyphen, and
/// never on the reserved-label deny-list. Returns the normalized
/// slug or a 400/409 error; the caller checks uniqueness.
pub(crate) fn validate_slug(raw: &str, settings: &TenantsSettings) -> UnifiedResult<String> {
    let slug = raw.trim().to_ascii_lowercase();
    if slug.is_empty() {
        return Err(UnifiedError::bad_request("slug must not be empty"));
    }
    let len = slug.chars().count() as u32;
    if len < settings.slug_min_length {
        return Err(UnifiedError::bad_request(format!(
            "slug must be at least {} characters",
            settings.slug_min_length
        )));
    }
    if len > settings.slug_max_length {
        return Err(UnifiedError::bad_request(format!(
            "slug must be at most {} characters",
            settings.slug_max_length
        )));
    }
    if slug.starts_with('-') || slug.ends_with('-') {
        return Err(UnifiedError::bad_request(
            "slug must not start or end with a hyphen",
        ));
    }
    if !slug
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        return Err(UnifiedError::bad_request(
            "slug may only contain lowercase letters, digits, and hyphens",
        ));
    }
    if settings
        .reserved_slugs
        .iter()
        .any(|reserved| reserved == &slug)
    {
        return Err(UnifiedError::bad_request(format!(
            "slug \"{slug}\" is reserved and cannot be claimed"
        )));
    }
    Ok(slug)
}


/// Load a tenant by id or slug, or 404. Slugs are unique, so a
/// slug lookup can never return another tenant's record.
pub fn get_tenant(state: &TenantsState, raw_id: &str) -> UnifiedResult<Tenant> {
    let trimmed = raw_id.trim();
    if let Ok(id) = Uuid::parse_str(trimmed) {
        if let Some(tenant) = state.tenants.get(&id) {
            return Ok(tenant.value().clone());
        }
    }
    let slug = trimmed.to_ascii_lowercase();
    for entry in state.tenants.iter() {
        if entry.value().slug == slug {
            return Ok(entry.value().clone());
        }
    }
    Err(UnifiedError::not_found(format!(
        "Tenant \"{trimmed}\" not found"
    )))
}


/// Move a tenant to a new status through the state machine. Any
/// transition not listed in `TENANT_TRANSITIONS` is rejected with
/// 409, so the lifecycle graph is enforced in exactly one place.
/// Same-status moves are rejected too: the graph has no self-edges
/// and `deleted` is terminal, so re-suspending an already-suspended
/// tenant (or "transitioning" out of a terminal state) must fail
/// instead of reporting success.
pub(crate) fn transition_tenant(
    state: &TenantsState,
    tenant: &mut Tenant,
    to: &str,
) -> UnifiedResult<()> {
    if !transition_allowed(&tenant.status, to) {
        return Err(UnifiedError::conflict(format!(
            "Tenant cannot move from \"{}\" to \"{to}\"",
            tenant.status
        ))
        .with_metadata("from", json!(tenant.status))
        .with_metadata("to", json!(to)));
    }
    tenant.status = to.to_string();
    tenant.updated_at = now();
    state.tenants.insert(tenant.id, tenant.clone());
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    fn tenant_with_status(status: &str) -> Tenant {
        Tenant {
            id: Uuid::now_v7(),
            slug: "acme".to_string(),
            display_name: "Acme".to_string(),
            status: status.to_string(),
            owner_user_id: Uuid::now_v7(),
            package_id: None,
            preset_id: None,
            plan_id: None,
            subdomain: None,
            subdomain_pending: false,
            schema_name: "tenant_acme".to_string(),
            suspended_at: None,
            suspended_reason: None,
            destroyed_at: None,
            export_reference: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn validate_slug_accepts_clean_slugs() {
        let settings = TenantsSettings::default();
        assert_eq!(validate_slug("acme", &settings).unwrap().as_str(), "acme");
        assert_eq!(validate_slug("ACME", &settings).unwrap().as_str(), "acme");
        assert_eq!(
            validate_slug("  acme-corp  ", &settings).unwrap().as_str(),
            "acme-corp"
        );
    }

    #[test]
    fn validate_slug_rejects_empty_short_long_and_reserved() {
        let settings = TenantsSettings::default();
        assert!(validate_slug("", &settings).is_err());
        assert!(validate_slug("ab", &settings).is_err());
        assert!(validate_slug(&"a".repeat(64), &settings).is_err());
        assert!(validate_slug("admin", &settings).is_err());
    }

    #[test]
    fn validate_slug_rejects_leading_trailing_hyphens_and_bad_chars() {
        let settings = TenantsSettings::default();
        assert!(validate_slug("-acme", &settings).is_err());
        assert!(validate_slug("acme-", &settings).is_err());
        assert!(validate_slug("acme corp", &settings).is_err());
        assert!(validate_slug("acme_corp", &settings).is_err());
    }

    #[test]
    fn transition_tenant_moves_through_allowed_edges() {
        let state = TenantsState::default();
        let mut tenant = tenant_with_status("active");
        transition_tenant(&state, &mut tenant, "suspended").unwrap();
        assert_eq!(tenant.status.as_str(), "suspended");
        assert_eq!(
            state.tenants.get(&tenant.id).unwrap().status.as_str(),
            "suspended"
        );
    }

    #[test]
    fn transition_tenant_rejects_unlisted_edges_and_keeps_status() {
        let state = TenantsState::default();
        let mut tenant = tenant_with_status("active");
        assert!(transition_tenant(&state, &mut tenant, "deleted").is_err());
        assert_eq!(tenant.status.as_str(), "active");
        // Same-status moves are not in the graph either: the
        // lifecycle has no self-edges, so they are 409s too.
        assert!(transition_tenant(&state, &mut tenant, "active").is_err());
        assert_eq!(tenant.status.as_str(), "active");
    }

    #[test]
    fn transition_tenant_rejects_same_status_from_terminal_and_suspended() {
        let state = TenantsState::default();
        let mut deleted = tenant_with_status("deleted");
        assert!(transition_tenant(&state, &mut deleted, "deleted").is_err());
        assert_eq!(deleted.status.as_str(), "deleted");
        let mut suspended = tenant_with_status("suspended");
        assert!(transition_tenant(&state, &mut suspended, "suspended").is_err());
        assert_eq!(suspended.status.as_str(), "suspended");
    }

    #[test]
    fn get_tenant_finds_by_slug_and_id() {
        let state = TenantsState::default();
        let tenant = tenant_with_status("active");
        let id = tenant.id;
        state.tenants.insert(id, tenant);
        assert_eq!(get_tenant(&state, "acme").unwrap().id, id);
        assert_eq!(
            get_tenant(&state, &id.to_string()).unwrap().slug.as_str(),
            "acme"
        );
        assert!(get_tenant(&state, "missing").is_err());
    }
}
