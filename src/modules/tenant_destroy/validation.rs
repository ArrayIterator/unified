// Tenant Destroy module — caller, input, and audit helpers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::tenancy::{
    drop_tenant_schema, is_valid_schema_name, schema_exists, schema_name,
    EVENT_TENANT_DESTROY_REQUESTED, EVENT_TENANT_DESTROYED, EVENT_TENANT_SUSPENDED,
};
use unified_core::db::DbPool;
use crate::modules::import_export::import_export_events;
use crate::modules::tenants::{
    begin_deprovision, finish_deprovision_as_deleted, set_export_reference, TenantsState,
    STATUS_DEPROVISIONING,
};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock, RwLock};
use uuid::Uuid;
use super::*;

// ---------------------------------------------------------------------------
// Caller identity and capability checks (same trusted-parameter
// pattern as the import/export module: X-User-Id plus X-Role
// headers, `role.check` hook, fail closed).
// ---------------------------------------------------------------------------

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks).
pub struct Caller {
    pub actor: String,
    pub role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
pub(crate) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// Grace period in days: at least 1, so the safety grace can never
/// be configured away to zero.
pub(crate) fn validate_grace_period_days(days: i64) -> UnifiedResult<()> {
    if !(1..=3650).contains(&days) {
        return Err(UnifiedError::bad_request(
            "grace_period_days must be between 1 and 3650",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

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

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw.trim()).map_err(|_| UnifiedError::bad_request(format!("Invalid {what}: expected a UUID")))
}

pub(crate) fn validate_reason(raw: &Option<String>) -> UnifiedResult<Option<String>> {
    match raw {
        None => Ok(None),
        Some(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            if trimmed.chars().count() > MAX_REASON_LEN {
                return Err(UnifiedError::bad_request(format!(
                    "reason must be at most {MAX_REASON_LEN} characters"
                )));
            }
            Ok(Some(trimmed.to_string()))
        }
    }
}

pub(crate) fn append_audit(request: &mut DestroyRequest, actor: &str, action: &str, detail: &str) {
    request.audit_trail.push(DestroyAuditEntry {
        at: now(),
        actor: actor.to_string(),
        action: action.to_string(),
        detail: detail.to_string(),
    });
    request.updated_at = now();
}

/// Emit a lifecycle event in the audit shape the audit module
/// records (`actor` / `actor_id`, `resource`, plus the payload).
/// The audit module only persists events listed in its
/// `AUDITED_EVENTS` table (see the OPEN notes).
pub(crate) fn emit_audit(ctx: &AppContext, event: &str, actor: &str, tenant_id: Uuid, extra: Value) {
    let mut payload = json!({
        "actor": actor,
        "actor_id": actor,
        "resource": format!("tenant:{tenant_id}"),
        "tenant_id": tenant_id.to_string(),
    });
    if let (Some(target), Some(source)) = (payload.as_object_mut(), extra.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
    ctx.hooks.emit(event, payload);
}

/// Emit an event from a hook handler (no `AppContext` there).
pub(crate) fn emit_from_hook(event: &str, payload: Value) {
    if let Some(bus) = HOOK_BUS.get() {
        bus.emit(event, payload);
    }
}

pub(crate) fn get_request(id: &Uuid) -> UnifiedResult<DestroyRequest> {
    destroy_requests_store()
        .get(id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Destroy request not found"))
}

pub(crate) fn save_request(request: &DestroyRequest) {
    destroy_requests_store().insert(request.id, request.clone());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_reason_trims_and_caps_length() {
        assert_eq!(validate_reason(&None).unwrap(), None);
        assert_eq!(validate_reason(&Some("   ".to_string())).unwrap(), None);
        assert_eq!(
            validate_reason(&Some("  cleanup  ".to_string())).unwrap(),
            Some("cleanup".to_string())
        );
        assert!(validate_reason(&Some("x".repeat(2001))).is_err());
        assert!(validate_reason(&Some("x".repeat(2000))).is_ok());
    }

    #[test]
    fn parse_uuid_accepts_uuids_only() {
        let id = Uuid::new_v4();
        assert_eq!(parse_uuid(&id.to_string(), "tenant id").unwrap(), id);
        assert!(parse_uuid("not-a-uuid", "tenant id").is_err());
        assert!(parse_uuid("  ", "tenant id").is_err());
    }

    #[test]
    fn grace_period_days_must_be_at_least_one() {
        assert!(validate_grace_period_days(0).is_err());
        assert!(validate_grace_period_days(-1).is_err());
        assert!(validate_grace_period_days(1).is_ok());
        assert!(validate_grace_period_days(3650).is_ok());
        assert!(validate_grace_period_days(3651).is_err());
    }
}
