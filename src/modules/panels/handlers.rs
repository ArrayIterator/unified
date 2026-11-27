//! panels module - Request shapes, record lookup and listing helpers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::HttpRequest;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::adapter::unsupported;
use super::constants::PERM_PANELS_MANAGE;
use super::constants::PERM_PANELS_VIEW;
use super::registry::panel_registry;
use super::store::SyncedRecord;
use super::store::connections_store;
use super::store::records_store;
use super::store::sync_state_store;
use super::types::GenericPanelConfig;
use super::types::PanelCapabilities;
use super::types::PanelConnection;
use super::types::PanelCredentials;

// Request types.

#[derive(Debug, Clone, Deserialize)]
pub struct CreateConnectionRequest {
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub credentials: PanelCredentials,
    #[serde(default)]
    pub generic_config: Option<GenericPanelConfig>,
    /// Explicit opt-in to a private / loopback panel endpoint for
    /// this connection. Default `false`: private endpoints are
    /// refused unless this is set (see `validate_panel_endpoint`).
    #[serde(default)]
    pub allow_private_endpoint: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

pub(crate) fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UpdateConnectionRequest {
    pub name: Option<String>,
    pub endpoint: Option<String>,
    pub credentials: Option<PanelCredentials>,
    pub generic_config: Option<GenericPanelConfig>,
    pub allow_private_endpoint: Option<bool>,
    pub enabled: Option<bool>,
}

/// Flag a stored record as locally changed (optionally preferring
/// the local version on the next sync).
///
/// Known gap, documented not hidden: this is flag-only. There is no
/// API to supply a new local payload, so the "local version" a
/// local-wins resolution keeps is the payload stored from the last
/// sync, not an edited copy. Editing the local payload needs a
/// separate update-record endpoint (not built in this round).
#[derive(Debug, Clone, Deserialize)]
pub struct MarkLocalChangedRequest {
    #[serde(default)]
    pub prefer_local: bool,
}

// Caller authentication (same pattern as calculator / bi /
// supervision: `X-User-Id` + `X-Role` headers, capability checked
// through the `role.check` hook, fail closed).
//
// Every panels handler authenticates first: panel handlers trigger
// outbound requests carrying stored credentials, so an
// unauthenticated caller must never reach them. Reads need
// `panels.view`; anything that writes state or talks to a panel
// needs `panels.manage`.

/// The caller, from the `X-User-Id` and `X-Role` headers.
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
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

pub(crate) fn require_permission(
    ctx: &AppContext,
    role: &str,
    permission: &str,
) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// Authenticate the caller and require `permission` in one step.
/// Returns the caller on success so handlers can record the actor.
pub(crate) fn authorize(
    ctx: &AppContext,
    req: &HttpRequest,
    permission: &str,
) -> UnifiedResult<Caller> {
    let caller = caller(req)?;
    require_permission(ctx, &caller.role, permission)?;
    Ok(caller)
}

/// Read permission for this module (`panels.view`).
pub(crate) fn authorize_view(ctx: &AppContext, req: &HttpRequest) -> UnifiedResult<Caller> {
    authorize(ctx, req, PERM_PANELS_VIEW)
}

/// Manage permission for this module (`panels.manage`).
pub(crate) fn authorize_manage(ctx: &AppContext, req: &HttpRequest) -> UnifiedResult<Caller> {
    authorize(ctx, req, PERM_PANELS_MANAGE)
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResolveConflictRequest {
    pub resolution: String,
}

// Views.

#[derive(Debug, Serialize)]
pub(crate) struct PanelKindView {
    pub(crate) kind: String,
    pub(crate) display_name: String,
    pub(crate) capabilities: PanelCapabilities,
    pub(crate) required_credentials: Vec<String>,
    pub(crate) endpoint_required: bool,
}

// Shared handler helpers.

pub(crate) fn paginate<T: Serialize + Clone>(
    items: Vec<T>,
    params: &PaginationParams,
) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start >= items.len() {
        Vec::new()
    } else {
        items[start..end].to_vec()
    };
    PageResult::new(page_items, total, &pagination)
}

pub(crate) fn find_connection(id: Uuid) -> UnifiedResult<PanelConnection> {
    connections_store()
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Panel connection \"{id}\" not found")))
}

pub(crate) fn require_enabled(connection: &PanelConnection) -> UnifiedResult<()> {
    if connection.enabled {
        Ok(())
    } else {
        Err(UnifiedError::conflict(format!(
            "Panel connection \"{}\" is disabled",
            connection.name
        )))
    }
}

pub(crate) fn filter_records(
    mut records: Vec<SyncedRecord>,
    params: &PaginationParams,
) -> Vec<SyncedRecord> {
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        records.retain(|record| {
            record.name.to_lowercase().contains(&needle)
                || record.remote_id.to_lowercase().contains(&needle)
                || record.status.to_lowercase().contains(&needle)
        });
    }
    if let Some(status) = params.filters.get("status") {
        records.retain(|record| record.status == *status);
    }
    if let Some(direction) = params.filters.get("direction") {
        records.retain(|record| record.direction == *direction);
    }
    if let Some(conflict) = params.filters.get("conflict") {
        let wanted = conflict == "true";
        records.retain(|record| record.conflict == wanted);
    }
    let sort = params.allowed_sort(&["name", "status", "last_synced_at", "remote_id"], "name");
    match sort.as_str() {
        "status" => {
            records.sort_by(|a, b| a.status.cmp(&b.status).then_with(|| a.name.cmp(&b.name)))
        }
        "last_synced_at" => records.sort_by(|a, b| {
            a.last_synced_at
                .cmp(&b.last_synced_at)
                .then_with(|| a.name.cmp(&b.name))
        }),
        "remote_id" => records.sort_by(|a, b| a.remote_id.cmp(&b.remote_id)),
        _ => records.sort_by(|a, b| a.name.cmp(&b.name)),
    }
    if params.normalized().order == "desc" {
        records.reverse();
    }
    records
}

pub(crate) fn stored_records(connection_id: Uuid, collection: &str) -> Vec<SyncedRecord> {
    records_store()
        .iter()
        .filter(|entry| entry.key().0 == connection_id && entry.key().1 == collection)
        .map(|entry| entry.value().clone())
        .collect()
}

pub(crate) fn list_collection_records(
    connection_id: Uuid,
    collection: &str,
    params: &PaginationParams,
) -> UnifiedResult<Value> {
    let connection = find_connection(connection_id)?;
    let adapter = panel_registry().build(&connection)?;
    if !adapter.capabilities().supports(collection) {
        return Err(unsupported(connection.kind, collection));
    }
    let records = filter_records(stored_records(connection_id, collection), params);
    let mut page = paginate(records, params).to_json();
    if let Value::Object(map) = &mut page {
        map.insert(
            "connection_id".to_string(),
            Value::String(connection_id.to_string()),
        );
        map.insert(
            "collection".to_string(),
            Value::String(collection.to_string()),
        );
        let state = sync_state_store().get(&connection_id);
        map.insert(
            "last_synced_at".to_string(),
            state
                .as_ref()
                .and_then(|state| state.last_synced_at)
                .map(|time| Value::String(time.to_rfc3339()))
                .unwrap_or(Value::Null),
        );
        map.insert(
            "sync_status".to_string(),
            state
                .as_ref()
                .and_then(|state| state.last_status.clone())
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
    }
    Ok(page)
}

#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    use super::authorize_manage;
    use super::authorize_view;
    use super::caller;
    use unified_core::config::AppConfig;
    use unified_core::context::AppContext;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;
    use actix_web::test::TestRequest;
    use std::sync::Arc;

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    #[test]
    fn caller_requires_a_verified_caller() {
        let req = TestRequest::default().to_http_request();
        assert!(caller(&req).is_err());
        let req = TestRequest::default().to_http_request();
        assert!(caller(&req).is_err());
        let req = verified_request("user-1", "admin");
        let caller = caller(&req).unwrap();
        assert_eq!(caller.actor, test_user_uuid("user-1").to_string());
        assert_eq!(caller.role, "admin");
    }

    #[test]
    fn authorize_fails_closed_without_a_role_check_answer() {
        // No `role.check` subscriber is registered on this bus, so
        // the capability check must deny rather than allow.
        let ctx = test_context();
        let req = verified_request("user-1", "admin");
        assert!(authorize_view(&ctx, &req).is_err());
        assert!(authorize_manage(&ctx, &req).is_err());
        // Missing headers fail even before the permission check.
        let anon = TestRequest::default().to_http_request();
        assert!(authorize_view(&ctx, &anon).is_err());
    }
}

// Tenant edge guard (C3 pilot 4, group C).

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
///
/// Panel (connections, synced records) records carry no company field, so the guard is a
/// fail-closed gate only: real per-company partitioning needs a
/// `tenant_id` on the records (C11 follow-up), and no per-record
/// company check is applied here.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

#[cfg(test)]
mod tenant_guard_tests {
    use super::*;
    use actix_web::HttpMessage;
    use actix_web::test::TestRequest;

    /// Request carrying the middleware-resolved tenant context the
    /// tenant-resolution middleware would attach.
    fn tenant_request() -> HttpRequest {
        let req = TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "acme".to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let req = tenant_request();
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
    }
}
