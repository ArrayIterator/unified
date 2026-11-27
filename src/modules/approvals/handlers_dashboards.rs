// Generic Approvals - Dashboard, delegation, and settings handlers.

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
use super::validation::*;
use super::service::*;
use super::handlers_definitions::*;
use super::handlers_requests::*;
use super::hooks::*;
use super::module::*;

// Dashboards

pub(crate) async fn my_pending(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_VIEW)?;
        let params = query.into_inner();
        let now = Utc::now();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<ApprovalRequest> = state
            .requests
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|request| {
                if let Some(status) = &status_filter {
                    if request.status != *status {
                        return false;
                    }
                }
                caller_is_pending_approver(&state, request, &caller.actor, &caller.role, now)
            })
            .collect();
        items.sort_by_key(|request| request.expires_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn by_record(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_VIEW)?;
        let params = query.into_inner();
        let module = params
            .filters
            .get("module")
            .cloned()
            .ok_or_else(|| UnifiedError::bad_request("Query parameter module is required"))?;
        let record_type = params
            .filters
            .get("record_type")
            .cloned()
            .ok_or_else(|| UnifiedError::bad_request("Query parameter record_type is required"))?;
        let record_id = params
            .filters
            .get("record_id")
            .cloned()
            .ok_or_else(|| UnifiedError::bad_request("Query parameter record_id is required"))?;
        let mut items: Vec<ApprovalRequestDetail> = state
            .requests
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|request| {
                request.module == module
                    && request.record_type == record_type
                    && request.record_id == record_id
            })
            .map(|request| request_detail(&state, &request))
            .collect();
        items.sort_by_key(|detail| detail.request.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Delegations

pub(crate) async fn list_delegations(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_VIEW)?;
        let params = query.into_inner();
        let delegator_filter = params.filters.get("delegator_id").cloned();
        let delegate_filter = params.filters.get("delegate_id").cloned();
        let active_filter = params.filters.get("is_active").cloned();
        let mut items: Vec<ApprovalDelegation> = state
            .delegations
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|delegation| {
                if let Some(delegator) = &delegator_filter {
                    if delegation.delegator_id != *delegator {
                        return false;
                    }
                }
                if let Some(delegate) = &delegate_filter {
                    if delegation.delegate_id != *delegate {
                        return false;
                    }
                }
                if let Some(active) = &active_filter {
                    let expected = match active.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return false,
                    };
                    if delegation.is_active != expected {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|delegation| delegation.starts_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_delegation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    body: web::Json<CreateDelegationInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApprovalDelegation> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_MANAGE)?;
        let input = body.into_inner();
        let delegator_id = require_text(&input.delegator_id, "delegator_id", MAX_USER_ID_LEN)?;
        let delegate_id = require_text(&input.delegate_id, "delegate_id", MAX_USER_ID_LEN)?;
        if delegator_id == delegate_id {
            return Err(UnifiedError::bad_request(
                "An approver cannot delegate to themselves",
            ));
        }
        if input.ends_at <= input.starts_at {
            return Err(UnifiedError::bad_request(
                "ends_at must be after starts_at",
            ));
        }
        if input.ends_at <= Utc::now() {
            return Err(UnifiedError::bad_request(
                "ends_at must be in the future",
            ));
        }
        let delegation = ApprovalDelegation {
            id: Uuid::new_v4(),
            delegator_id,
            delegate_id,
            starts_at: input.starts_at,
            ends_at: input.ends_at,
            is_active: true,
            created_by: caller.actor,
            created_at: Utc::now(),
        };
        state.delegations.insert(delegation.id, delegation.clone());
        Ok(delegation)
    })();
    match result {
        Ok(delegation) => created(delegation),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_delegation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "delegation")?;
        let delegation = state
            .delegations
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Approval delegation not found: {id}")))?;
        // Revoking is a soft delete: the row stays so past decisions
        // that recorded `acting_for` keep their meaning.
        let mut revoked = delegation.clone();
        revoked.is_active = false;
        state.delegations.insert(revoked.id, revoked);
        Ok(json!({ "revoked": true, "id": delegation.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Settings

pub(crate) async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApprovalSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_VIEW)?;
        Ok(state.settings())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApprovalSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_MANAGE)?;
        let input = body.into_inner();
        let mut settings = state.settings();
        if let Some(hours) = input.default_timeout_hours {
            if hours < 1 || hours > MAX_TIMEOUT_HOURS {
                return Err(UnifiedError::bad_request(format!(
                    "default_timeout_hours must be between 1 and {MAX_TIMEOUT_HOURS}"
                )));
            }
            settings.default_timeout_hours = hours;
        }
        if let Some(hours) = input.default_escalation_extension_hours {
            if hours < 1 || hours > MAX_ESCALATION_EXTENSION_HOURS {
                return Err(UnifiedError::bad_request(format!(
                    "default_escalation_extension_hours must be between 1 and {MAX_ESCALATION_EXTENSION_HOURS}"
                )));
            }
            settings.default_escalation_extension_hours = hours;
        }
        if let Some(max) = input.max_escalations {
            if max < 1 || max > 100 {
                return Err(UnifiedError::bad_request(
                    "max_escalations must be between 1 and 100",
                ));
            }
            settings.max_escalations = max;
        }
        if let Some(max) = input.max_steps_per_definition {
            if max < 1 || max > 100 {
                return Err(UnifiedError::bad_request(
                    "max_steps_per_definition must be between 1 and 100",
                ));
            }
            settings.max_steps_per_definition = max;
        }
        if let Some(max) = input.max_approvers_per_step {
            if max < 1 || max > 50 {
                return Err(UnifiedError::bad_request(
                    "max_approvers_per_step must be between 1 and 50",
                ));
            }
            settings.max_approvers_per_step = max;
        }
        if let Some(allow) = input.allow_self_approval {
            settings.allow_self_approval = allow;
        }
        if let Some(require) = input.require_comment_on_approve {
            settings.require_comment_on_approve = require;
        }
        *state
            .settings
            .write()
            .expect("settings lock poisoned") = settings.clone();
        Ok(settings)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
