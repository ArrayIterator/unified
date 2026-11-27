// Tenant Destroy module - destroy HTTP handlers.
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

// HTTP handlers. Every handler checks a capability first.

pub(crate) async fn request_destroy(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<RequestDestroyBody>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DestroyRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_DESTROY)?;
        let input = body.into_inner();
        let reason = validate_reason(&input.reason)?;
        // Fail closed: the tenant record must resolve before anything
        // is created, and a destroyed tenant can never be destroyed
        // again.
        let tenant = resolve_tenant(&ctx, &input.tenant_id)?;
        if tenant_already_destroyed(&tenant) {
            return Err(UnifiedError::conflict(
                "Tenant is already destroyed",
            ));
        }
        let active = destroy_requests_store()
            .iter()
            .map(|entry| entry.value().clone())
            .find(|existing| {
                existing.tenant_id == input.tenant_id && !existing.is_terminal()
            });
        if let Some(existing) = active {
            return Err(UnifiedError::conflict(format!(
                "Tenant already has an active destroy request {} in status \"{}\"",
                existing.id, existing.status
            )));
        }
        let settings = destroy_settings();
        let created_at = now();
        let mut request = DestroyRequest {
            id: Uuid::now_v7(),
            tenant_id: input.tenant_id,
            tenant_slug: tenant.slug.clone(),
            status: STATUS_REQUESTED.to_string(),
            requested_by: caller.actor.clone(),
            requested_at: created_at,
            reason: reason.clone(),
            export_job_id: None,
            export_requested_at: None,
            export_completed_at: None,
            export_download_url: None,
            export_verified_via: None,
            export_request_claimed: false,
            confirmation_attempts: 0,
            failed_confirmation_attempts: 0,
            confirmation_locked_until: None,
            confirmed_at: None,
            confirmed_by: None,
            second_approval_required: false,
            second_approval_reason: None,
            second_approver: None,
            second_approved_at: None,
            grace_period_days: settings.grace_period_days,
            grace_expires_at: None,
            suspended_at: None,
            destroyed_at: None,
            schema_name_snapshot: None,
            subdomains_revoke_requested: false,
            external_cleanup: Vec::new(),
            audit_trail: Vec::new(),
            cancel_reason: None,
            cancelled_by: None,
            cancelled_at: None,
            last_error: None,
            created_at,
            updated_at: created_at,
        };
        append_audit(
            &mut request,
            &caller.actor,
            "requested",
            &format!(
                "Destroy requested for tenant \"{}\" ({}).",
                tenant.slug, tenant.id
            ),
        );
        emit_audit(
            &ctx,
            EVENT_TENANT_DESTROY_REQUESTED,
            &caller.actor,
            input.tenant_id,
            json!({
                "destroy_request_id": request.id.to_string(),
                "tenant_slug": tenant.slug,
                "reason": reason,
            }),
        );
        // Mandatory export first: ask the import/export module for a
        // full tenant export. A handler that answers with an export
        // job id claims the request; otherwise the export is created
        // through the import/export endpoint and attached later.
        let export_context = ctx.hooks.emit(
            import_export_events::EXPORT_REQUESTED,
            json!({
                "entity": settings.export_entity,
                "tenant_id": input.tenant_id.to_string(),
                "scope": "full_tenant_export",
                "destroy_request_id": request.id.to_string(),
                "format": "json",
                "actor": caller.actor,
            }),
        );
        let mut claimed_job: Option<Uuid> = None;
        for result in &export_context.results {
            // A claim answer that names a tenant must name this
            // request's tenant: an export job created for another
            // tenant can never be the proof for this destroy.
            // Answers without a tenant binding are claimed as
            // before - the job is created in direct response to
            // this request's tenant-scoped emit - and the binding
            // is re-checked when the proof is attached or the
            // `export.completed` event arrives.
            if let Some(bound) = export_tenant_binding(result) {
                if bound != input.tenant_id {
                    continue;
                }
            }
            let candidate = result
                .get("export_id")
                .or_else(|| result.get("export_job_id"))
                .or_else(|| result.get("job_id"));
            if let Some(raw) = candidate.and_then(Value::as_str) {
                if let Ok(job_id) = Uuid::parse_str(raw) {
                    claimed_job = Some(job_id);
                    break;
                }
            }
        }
        request.export_requested_at = Some(now());
        request.export_job_id = claimed_job;
        request.export_request_claimed = claimed_job.is_some();
        request.status = STATUS_EXPORT_PENDING.to_string();
        append_audit(
            &mut request,
            &caller.actor,
            "export_requested",
            if claimed_job.is_some() {
                "Full tenant export requested; the export module claimed the request."
            } else {
                "Full tenant export requested; no export handler answered, so the export job must be created through the import/export endpoint and attached as proof."
            },
        );
        save_request(&request);
        Ok(request)
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_destroy_requests(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_DESTROY_READ)?;
        let settings = destroy_settings();
        let stale_after =
            now() - chrono::Duration::days(settings.export_pending_stale_days);
        let mut items: Vec<Value> = destroy_requests_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect::<Vec<DestroyRequest>>()
            .into_iter()
            .map(|request| {
                let mut view = serde_json::to_value(&request).unwrap_or(Value::Null);
                if request.status == STATUS_EXPORT_PENDING && request.created_at < stale_after {
                    view["export_stale"] = Value::Bool(true);
                } else {
                    view["export_stale"] = Value::Bool(false);
                }
                view
            })
            .collect();
        if let Some(status) = params.filters.get("status") {
            items.retain(|item| item.get("status").and_then(Value::as_str) == Some(status.as_str()));
        }
        if let Some(tenant_id) = params.filters.get("tenant_id") {
            items.retain(|item| {
                item.get("tenant_id").and_then(Value::as_str) == Some(tenant_id.as_str())
            });
        }
        if let Some(requested_by) = params.filters.get("requested_by") {
            items.retain(|item| {
                item.get("requested_by").and_then(Value::as_str) == Some(requested_by.as_str())
            });
        }
        items.sort_by(|a, b| {
            let a_at = a.get("created_at").and_then(Value::as_str).unwrap_or("");
            let b_at = b.get("created_at").and_then(Value::as_str).unwrap_or("");
            b_at.cmp(a_at)
        });
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_destroy_request(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DestroyRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_DESTROY_READ)?;
        get_request(&id.into_inner())
    })();
    json::respond(result, debug)
}

pub(crate) async fn attach_export_proof(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<AttachExportProofBody>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DestroyRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_DESTROY)?;
        let input = body.into_inner();
        let mut request = get_request(&id.into_inner())?;
        if request.status != STATUS_REQUESTED && request.status != STATUS_EXPORT_PENDING {
            return Err(UnifiedError::conflict(format!(
                "Export proof can only be attached while the request is \"{}\" or \"{}\" (current status: \"{}\")",
                STATUS_REQUESTED, STATUS_EXPORT_PENDING, request.status
            )));
        }
        let download_url = match input.download_url {
            Some(url) => {
                if url.trim().is_empty() || url.chars().count() > MAX_NOTE_LEN {
                    return Err(UnifiedError::bad_request(
                        "download_url must be a non-empty URL of at most 2000 characters",
                    ));
                }
                Some(url.trim().to_string())
            }
            None => None,
        };
        // Tenant binding is verified BEFORE the job is attached as
        // proof: an export job for another tenant can never prove
        // this tenant's export, and a completed job whose lookup
        // answer carries no tenant binding cannot be verified at
        // all, so it is not accepted as ready (fail closed). A job
        // the lookup cannot answer yet stays pending proof and is
        // completed later by the `export.completed` event, which
        // carries the tenant binding and is checked there.
        let lookup = lookup_export(&ctx, &input.export_job_id);
        if let Some((_, _, Some(bound))) = &lookup {
            if !export_binding_matches(Some(*bound), &request.tenant_id) {
                return Err(UnifiedError::conflict(format!(
                    "Export job {} belongs to a different tenant and cannot be used as proof for this destroy request",
                    input.export_job_id
                )));
            }
        }
        if let Some((status, _, None)) = &lookup {
            if status == "completed" {
                return Err(UnifiedError::new(
                    "Export job carries no tenant binding, so the proof cannot be verified for this tenant; attach an export whose lookup answer names its tenant",
                    424,
                ));
            }
        }
        request.export_job_id = Some(input.export_job_id);
        request.export_request_claimed = true;
        if let Some(url) = download_url {
            request.export_download_url = Some(url);
        }
        append_audit(
            &mut request,
            &caller.actor,
            "export_proof_attached",
            &format!("Export job {} attached as proof.", input.export_job_id),
        );
        // Verify the job through the import/export module when it
        // answers; otherwise the `export.completed` event completes
        // the proof later.
        match lookup {
            Some((status, download_url, _)) if status == "completed" => {
                mark_export_ready(&mut request, download_url, "export_lookup");
                save_request(&request);
                emit_audit(
                    &ctx,
                    EVENT_EXPORT_READY,
                    &caller.actor,
                    request.tenant_id,
                    json!({
                        "destroy_request_id": request.id.to_string(),
                        "export_id": input.export_job_id.to_string(),
                        "verified_via": "export_lookup",
                    }),
                );
            }
            Some((status, _, _)) => {
                append_audit(
                    &mut request,
                    &caller.actor,
                    "export_proof_pending",
                    &format!("Export job is in status \"{status}\"; waiting for completion."),
                );
                save_request(&request);
            }
            None => {
                save_request(&request);
            }
        }
        Ok(request)
    })();
    json::respond(result, debug)
}

pub(crate) async fn confirm_destroy(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<TypedConfirmationBody>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DestroyRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_DESTROY)?;
        let input = body.into_inner();
        let mut request = get_request(&id.into_inner())?;
        if request.status != STATUS_EXPORT_READY && request.status != STATUS_AWAITING_CONFIRMATION
        {
            return Err(UnifiedError::conflict(format!(
                "Typed confirmation requires status \"{}\" or \"{}\" (current status: \"{}\")",
                STATUS_EXPORT_READY, STATUS_AWAITING_CONFIRMATION, request.status
            )));
        }
        if caller.actor != request.requested_by {
            return Err(UnifiedError::forbidden(
                "Only the admin who requested the destroy may type the confirmation",
            ));
        }
        let settings = destroy_settings();
        if let Some(locked_until) = request.confirmation_locked_until {
            if now() < locked_until {
                return Err(UnifiedError::new(
                    "Confirmation is locked after too many failed attempts; try again later",
                    429,
                )
                .with_metadata("locked_until", Value::String(locked_until.to_rfc3339())));
            }
        }
        if input.typed_slug.is_empty() || input.typed_slug.chars().count() > MAX_SLUG_LEN {
            return Err(UnifiedError::bad_request(
                "typed_slug must be a non-empty slug of at most 100 characters",
            ));
        }
        // Case-sensitive exact match against the stored slug. The
        // error is generic on purpose: it never reveals the slug.
        if input.typed_slug != request.tenant_slug {
            request.confirmation_attempts += 1;
            request.failed_confirmation_attempts += 1;
            let remaining = settings
                .max_failed_confirmations
                .saturating_sub(request.failed_confirmation_attempts);
            if request.failed_confirmation_attempts >= settings.max_failed_confirmations {
                let locked_until =
                    now() + chrono::Duration::minutes(settings.confirmation_lockout_minutes);
                request.confirmation_locked_until = Some(locked_until);
                request.status = STATUS_AWAITING_CONFIRMATION.to_string();
                append_audit(
                    &mut request,
                    &caller.actor,
                    "confirmation_failed",
                    &format!(
                        "Typed confirmation did not match; confirmation locked until {locked_until}."
                    ),
                );
                save_request(&request);
                emit_audit(
                    &ctx,
                    EVENT_CONFIRMATION_FAILED,
                    &caller.actor,
                    request.tenant_id,
                    json!({
                        "destroy_request_id": request.id.to_string(),
                        "failed_attempts": request.failed_confirmation_attempts,
                        "locked_until": locked_until.to_rfc3339(),
                    }),
                );
                return Err(UnifiedError::new(
                    "Confirmation does not match; confirmation is now locked after too many failed attempts",
                    429,
                )
                .with_metadata("locked_until", Value::String(locked_until.to_rfc3339())));
            }
            request.status = STATUS_AWAITING_CONFIRMATION.to_string();
            append_audit(
                &mut request,
                &caller.actor,
                "confirmation_failed",
                "Typed confirmation did not match.",
            );
            save_request(&request);
            emit_audit(
                &ctx,
                EVENT_CONFIRMATION_FAILED,
                &caller.actor,
                request.tenant_id,
                json!({
                    "destroy_request_id": request.id.to_string(),
                    "failed_attempts": request.failed_confirmation_attempts,
                }),
            );
            return Err(UnifiedError::bad_request(
                "Confirmation does not match the tenant slug",
            )
            .with_metadata("attempts_remaining", Value::from(remaining)));
        }
        // Match. Re-check the live slug: the tenant must not have
        // been renamed since the request was opened.
        let tenant = resolve_tenant(&ctx, &request.tenant_id)?;
        if tenant.slug != request.tenant_slug {
            return Err(UnifiedError::internal(
                "Tenant slug changed since the destroy was requested; destroy refuses to proceed",
            ));
        }
        request.confirmation_attempts += 1;
        request.failed_confirmation_attempts = 0;
        request.confirmation_locked_until = None;
        request.confirmed_at = Some(now());
        request.confirmed_by = Some(caller.actor.clone());
        append_audit(
            &mut request,
            &caller.actor,
            "confirmed",
            "Typed confirmation matched the tenant slug.",
        );
        let requirement = second_approval_requirement(&settings, &tenant)?;
        match requirement {
            Some(reason) => {
                request.second_approval_required = true;
                request.second_approval_reason = Some(reason.clone());
                request.status = STATUS_AWAITING_SECOND_APPROVAL.to_string();
                append_audit(
                    &mut request,
                    &caller.actor,
                    "second_approval_required",
                    &format!("Second approval required: {reason}."),
                );
                save_request(&request);
                emit_audit(
                    &ctx,
                    EVENT_SECOND_APPROVAL_REQUIRED,
                    &caller.actor,
                    request.tenant_id,
                    json!({
                        "destroy_request_id": request.id.to_string(),
                        "reason": reason,
                    }),
                );
            }
            None => {
                request.second_approval_required = false;
                save_request(&request);
                enter_suspension_and_grace(&ctx, &mut request, &caller.actor)?;
            }
        }
        Ok(request)
    })();
    json::respond(result, debug)
}

pub(crate) async fn approve_second(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<SecondApprovalBody>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DestroyRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_DESTROY)?;
        let note = validate_reason(&body.into_inner().note)?;
        let mut request = get_request(&id.into_inner())?;
        if request.status != STATUS_AWAITING_SECOND_APPROVAL {
            return Err(UnifiedError::conflict(format!(
                "Second approval requires status \"{}\" (current status: \"{}\")",
                STATUS_AWAITING_SECOND_APPROVAL, request.status
            )));
        }
        // The requester's own approval never counts: the second
        // approver must be a different user.
        if caller.actor == request.requested_by {
            return Err(UnifiedError::forbidden(
                "The second approver must be a different admin than the requester",
            ));
        }
        request.second_approver = Some(caller.actor.clone());
        request.second_approved_at = Some(now());
        append_audit(
            &mut request,
            &caller.actor,
            "second_approved",
            &format!(
                "Second approval recorded.{}",
                note.as_deref()
                    .map(|text| format!(" Note: {text}"))
                    .unwrap_or_default()
            ),
        );
        save_request(&request);
        enter_suspension_and_grace(&ctx, &mut request, &caller.actor)?;
        Ok(request)
    })();
    json::respond(result, debug)
}

pub(crate) async fn cancel_destroy(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<CancelDestroyBody>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DestroyRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_DESTROY)?;
        let reason = validate_reason(&body.into_inner().reason)?;
        let mut request = get_request(&id.into_inner())?;
        if !request.is_cancellable() {
            return Err(UnifiedError::conflict(format!(
                "Destroy request in status \"{}\" can no longer be cancelled",
                request.status
            )));
        }
        let was_in_grace = request.status == STATUS_SUSPENDED_GRACE;
        request.status = STATUS_CANCELLED.to_string();
        request.cancel_reason = reason.clone();
        request.cancelled_by = Some(caller.actor.clone());
        request.cancelled_at = Some(now());
        append_audit(
            &mut request,
            &caller.actor,
            "cancelled",
            &format!(
                "Destroy cancelled.{}",
                reason
                    .as_deref()
                    .map(|text| format!(" Reason: {text}"))
                    .unwrap_or_default()
            ),
        );
        save_request(&request);
        // Cancelling during grace resumes the tenant: serving
        // restarts and the grace clock is abandoned.
        if was_in_grace {
            ctx.hooks.emit(
                EVENT_TENANT_RESUMED,
                json!({
                    "tenant_id": request.tenant_id.to_string(),
                    "destroy_request_id": request.id.to_string(),
                    "reason": "destroy_cancelled_during_grace",
                    "actor": caller.actor,
                }),
            );
        }
        emit_audit(
            &ctx,
            EVENT_DESTROY_CANCELLED,
            &caller.actor,
            request.tenant_id,
            json!({
                "destroy_request_id": request.id.to_string(),
                "reason": reason,
                "resumed": was_in_grace,
            }),
        );
        Ok(request)
    })();
    json::respond(result, debug)
}

pub(crate) async fn execute_destroy(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DestroyRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_DESTROY)?;
        let request_id = id.into_inner();
        run_final_destroy(&ctx, &request_id, &caller.actor)
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_grace_info(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_DESTROY_READ)?;
        let request = get_request(&id.into_inner())?;
        Ok(json!({
            "destroy_request_id": request.id,
            "tenant_id": request.tenant_id,
            "tenant_slug": request.tenant_slug,
            "status": request.status,
            "suspended_at": request.suspended_at,
            "grace_period_days": request.grace_period_days,
            "grace_expires_at": request.grace_expires_at,
            "grace_expired": request.grace_expired(),
            "grace_remaining_seconds": request.grace_remaining_seconds(),
            "can_cancel": request.is_cancellable(),
            "can_execute": request.status == STATUS_SUSPENDED_GRACE && request.grace_expired(),
            "export_job_id": request.export_job_id,
            "export_completed_at": request.export_completed_at,
            "second_approval_required": request.second_approval_required,
            "second_approved_at": request.second_approved_at,
            "external_cleanup": request.external_cleanup,
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn confirm_external_cleanup(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<ConfirmExternalCleanupBody>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DestroyRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_DESTROY)?;
        let input = body.into_inner();
        let note = validate_reason(&input.note)?;
        if !EXTERNAL_CLEANUP_KINDS.contains(&input.item.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "item must be one of {}",
                EXTERNAL_CLEANUP_KINDS.join(", ")
            )));
        }
        let request_id = id.into_inner();
        let current = get_request(&request_id)?;
        if current.status != STATUS_DESTROYED_PENDING_EXTERNAL_CLEANUP {
            return Err(UnifiedError::conflict(format!(
                "External cleanup can only be confirmed while the request is \"{}\" (current status: \"{}\")",
                STATUS_DESTROYED_PENDING_EXTERNAL_CLEANUP, current.status
            )));
        }
        let updated = confirm_external_cleanup_item(
            &request_id,
            &input.item,
            &caller.actor,
            note,
        )
        .ok_or_else(|| UnifiedError::not_found("Destroy request not found"))?;
        if updated.status == STATUS_DESTROYED {
            emit_audit(
                &ctx,
                EVENT_EXTERNAL_CLEANUP_COMPLETED,
                &caller.actor,
                updated.tenant_id,
                json!({
                    "destroy_request_id": updated.id.to_string(),
                    "item": input.item,
                }),
            );
        }
        Ok(updated)
    })();
    json::respond(result, debug)
}

pub(crate) async fn process_due(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_DESTROY)?;
        let destroyed = process_due_destroys(&ctx)?;
        Ok(json!({
            "destroyed_request_ids": destroyed,
            "destroyed_count": destroyed.len(),
            "due_remaining": due_destroy_requests().len(),
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TenantDestroySettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_DESTROY_READ)?;
        Ok(destroy_settings())
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsBody>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TenantDestroySettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_TENANTS_DESTROY)?;
        let input = body.into_inner();
        let mut settings = destroy_settings();
        if let Some(days) = input.grace_period_days {
            validate_grace_period_days(days)?;
            settings.grace_period_days = days;
        }
        if let Some(age) = input.second_approval_min_tenant_age_days {
            if let Some(days) = age {
                if days < 0 {
                    return Err(UnifiedError::bad_request(
                        "second_approval_min_tenant_age_days must be zero or more",
                    ));
                }
            }
            settings.second_approval_min_tenant_age_days = age;
        }
        if let Some(size) = input.second_approval_min_data_size_bytes {
            settings.second_approval_min_data_size_bytes = size;
        }
        if let Some(max) = input.max_failed_confirmations {
            if !(1..=100).contains(&max) {
                return Err(UnifiedError::bad_request(
                    "max_failed_confirmations must be between 1 and 100",
                ));
            }
            settings.max_failed_confirmations = max;
        }
        if let Some(minutes) = input.confirmation_lockout_minutes {
            if !(0..=1440).contains(&minutes) {
                return Err(UnifiedError::bad_request(
                    "confirmation_lockout_minutes must be between 0 and 1440",
                ));
            }
            settings.confirmation_lockout_minutes = minutes;
        }
        if let Some(entity) = input.export_entity {
            let trimmed = entity.trim().to_string();
            if trimmed.is_empty() || trimmed.chars().count() > MAX_SLUG_LEN {
                return Err(UnifiedError::bad_request(
                    "export_entity must be a non-empty name of at most 100 characters",
                ));
            }
            settings.export_entity = trimmed;
        }
        if let Some(days) = input.export_pending_stale_days {
            if !(0..=365).contains(&days) {
                return Err(UnifiedError::bad_request(
                    "export_pending_stale_days must be between 0 and 365",
                ));
            }
            settings.export_pending_stale_days = days;
        }
        write_destroy_settings(settings_store(), &settings)?;
        ctx.hooks.emit(
            "tenant.destroy.settings.updated",
            json!({
                "actor": caller.actor,
                "grace_period_days": settings.grace_period_days,
            }),
        );
        Ok(settings)
    })();
    json::respond(result, debug)
}
