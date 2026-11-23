// Tenant Destroy module — the irreversible end of a tenant lifecycle.
//
// Destroy is NEVER one click. A tenant is destroyed only through the
// full flow below, and every step is reversible until the final schema
// drop. The state machine is:
//
//   requested
//     -> export_pending            (full export requested, proof pending)
//     -> export_ready              (export completed and downloadable)
//     -> awaiting_confirmation     (typed confirmation attempted / pending)
//     -> awaiting_second_approval  (only when the settings rule requires it)
//     -> suspended_grace           (tenant suspended, grace clock running)
//     -> destroyed_pending_external_cleanup
//     -> destroyed                 (terminal)
//   Any non-terminal state except `destroyed_pending_external_cleanup`
//   can move to `cancelled` (terminal). Cancellation during
//   `suspended_grace` resumes the tenant.
//
// Step-by-step:
//   1. REQUEST: an admin/owner with `tenants:destroy` opens a destroy
//      request for one tenant. The tenant record is resolved through
//      the tenants module (`tenant.lookup` hook query, see below) and
//      the flow refuses to proceed when the record cannot be resolved.
//      A full export is requested through the import/export module
//      (`export.requested`) and the request waits in `export_pending`.
//   2. EXPORT FIRST: the flow cannot advance past `export_pending`
//      until a completed, downloadable export is proven: either the
//      `export.completed` event is observed for the attached export
//      job, or the `export.lookup` hook query answers `completed`.
//      The export job id is stored on the request as the proof.
//   3. TYPED CONFIRMATION: the requester types the tenant slug
//      exactly (case-sensitive). A mismatch is rejected with a generic
//      error, counted, and after a configured number of failures the
//      confirmation locks for a configured lockout period.
//   4. SECOND APPROVER: when the settings rule says the tenant is
//      above the size/age threshold, a second admin — a different
//      user than the requester, enforced — must approve. The
//      requester's own approval never counts.
//   5. SUSPENSION + GRACE: on final confirmation the tenant is
//      suspended first (`tenant.suspended` is emitted, serving
//      stops) and a grace period starts (configurable setting,
//      default 30 days, stored as an explicit expiry timestamp).
//      During grace the destroy can still be cancelled, which
//      resumes the tenant (`tenant.resumed`).
//   6. FINAL DESTROY: only after grace expiry, only from
//      `suspended_grace`: the tenant moves to `deprovisioning`
//      through the tenants module's `begin_deprovision` helper
//      (the single source of truth for the tenant record — this
//      module keeps NO parallel destroyed-tenants store), the
//      tenant schema is dropped through
//      `unified_core::tenancy::drop_tenant_schema` (never by building
//      DDL in this file), the drop is verified with `schema_exists`
//      (must be false, else fail closed and alert), the export
//      reference is recorded with `set_export_reference`, the
//      tenant row moves to `deleted` with `destroyed_at` through
//      `finish_deprovision_as_deleted`, subdomains are revoked
//      through saas_domains events, and `tenant.destroyed` is
//      emitted with a full audit trail. A destroyed tenant uuidv7 is
//      NEVER reused: the tenants row itself (status `deleted`,
//      `destroyed_at` set) is the guard record, consulted by
//      provisioning through the `tenant.destroyed.check` hook query
//      and `is_destroyed_tenant()`.
//   7. EXTERNAL CLEANUP: dropping the schema removes the tenant's
//      database rows only. Stored files, backups, and search indexes
//      live outside the schema, so the flow emits
//      `tenant.storage_cleanup_requested`,
//      `tenant.backup_cleanup_requested`, and
//      `tenant.search_index_cleanup_requested` (tenant id plus schema
//      name) and stays in `destroyed_pending_external_cleanup` until
//      every item is confirmed — by the owning module's
//      `tenant.<area>_cleanup.completed` event, or by an admin through
//      the confirm endpoint. Only then is the request `destroyed`.
//
// TENANT-SCOPED DATA ACCESS (standing rule). Every destroy step
// resolves its data through tenant context and through the owning
// module's own events — never by querying another module's tables
// directly, and never against the main/control-plane database for
// tenant business data:
//   - The destroy request and the control-plane tenant row are
//     control-plane data owned by the tenants module: this module
//     reads the tenant record only through the `tenant.lookup` hook
//     query (answered by the tenants module) and moves the record
//     through the tenants module's public helpers
//     (`begin_deprovision` / `set_export_reference` /
//     `finish_deprovision_as_deleted`) over the ONE shared
//     `TenantsState` (see `set_shared_tenants_state`). It never
//     reads or writes the tenants tables itself, and it keeps no
//     parallel destroyed-tenants store: the tenants row with
//     status `deleted` is the only destroy record.
//   - The export goes through the import/export module's events.
//   - Subdomain revocation goes through saas_domains events.
//   - The schema drop goes through the tenancy helper.
//   - No tenant id, schema name, or package id is ever hard-coded:
//     every destroy step resolves the schema name from the tenant's
//     uuidv7 through `schema_name()` at execution time.
//   - Fail closed: when the tenant record or the tenancy plumbing
//     cannot resolve the tenant, the flow refuses to proceed instead
//     of falling back to any default or to the `public` schema.
//
// Routes live under `/api/v1/tenants` (same prefix as the tenants
// module, so the integrator merges the two route sets):
//
//   POST   /destroy-requests — request a destroy (destroy).
//   GET    /destroy-requests — list destroy requests (destroy_read).
//   GET    /destroy-requests/{id} — one destroy request (destroy_read).
//   POST   /destroy-requests/{id}/export-proof — attach an export
//          job id as proof (destroy).
//   POST   /destroy-requests/{id}/confirm — typed slug
//          confirmation (destroy, requester only).
//   POST   /destroy-requests/{id}/approve-second — second approver
//          (destroy, must differ from the requester).
//   POST   /destroy-requests/{id}/cancel — cancel (destroy).
//   POST   /destroy-requests/{id}/execute — final destroy, only
//          after grace expiry (destroy).
//   GET    /destroy-requests/{id}/grace — grace/expiry info
//          (destroy_read).
//   POST   /destroy-requests/{id}/external-cleanup/confirm —
//          confirm one external cleanup item (destroy).
//   POST   /destroy-requests/process-due — run the due-destroy
//          sweep for grace-expired requests (destroy).
//   GET    /destroy-settings — configurable defaults (destroy_read).
//   PUT    /destroy-settings — update the defaults (destroy).
//
// Events emitted on the hook bus:
//   - `tenant.destroy_requested` (tenancy constant
//     `EVENT_TENANT_DESTROY_REQUESTED`)
//   - `export.requested` (import/export module; full tenant export)
//   - `tenant.destroy.export_ready`
//   - `tenant.destroy.confirmation_failed`
//   - `tenant.destroy.second_approval_required`
//   - `tenant.suspended` (tenancy constant `EVENT_TENANT_SUSPENDED`)
//   - `tenant.resumed` (cancel during grace)
//   - `tenant.destroy.cancelled`
//   - `tenant.destroy_due` (scheduler kind; see below)
//   - `tenant.schema_dropped` (tenancy constant
//     `EVENT_TENANT_SCHEMA_DROPPED`)
//   - `saas_domains.subdomain.release_requested` (subdomain revoke)
//   - `tenant.soft_delete_requested` (control-plane soft-delete)
//   - `tenant.storage_cleanup_requested`,
//     `tenant.backup_cleanup_requested`,
//     `tenant.search_index_cleanup_requested`
//   - `tenant.destroyed` (tenancy constant `EVENT_TENANT_DESTROYED`)
//   - `tenant.destroy.external_cleanup_completed`
//   - `tenant.destroy.failed` (fail-closed alert: drop failed, or
//     the schema still exists after the drop)
//
// Events this module listens to (all read defensively — a missing
// field or an unknown request is skipped, never an error):
//   - `export.completed` — marks the matching request `export_ready`.
//   - `tenant.storage_cleanup.completed`,
//     `tenant.backup_cleanup.completed`,
//     `tenant.search_index_cleanup.completed` — confirm one external
//     cleanup item.
//   - `tenant.destroyed.check` — hook query answered with
//     `{"tenant_id", "destroyed"}` for the provisioning guard.
//   - `tenant.destroy_due` — scheduler dispatch signal; the handler
//     only lists due requests, it never drops anything (hook
//     handlers have no `AppContext`). The scheduler arm runs the
//     sweep itself: it rebuilds the `AppContext` from the handles
//     the scheduler stored at `init` and calls
//     `process_due_destroys(ctx)`.
//
// Scheduler integration: the kind offered to `scheduler.enqueue`
// is `tenant.destroy_due` (`DESTROY_DUE_KIND`). The scheduler
// dispatches it through its `tenant.destroy_due` arm (which calls
// `process_due_destroys(ctx)`), seeded as an hourly interval job
// by the scheduler's `Module::init`; an admin can still trigger
// the same sweep through `POST /destroy-requests/process-due`.
//
// Hook queries this module sends (fail closed when unanswered):
//   - `tenant.lookup` — `{"tenant_id", "tenant": null}`; the tenants
//     module answers with the tenant record (or a `tenant` object in
//     the results). No answer means the tenant cannot be resolved
//     and the flow refuses to proceed.
//   - `export.lookup` — `{"export_id", "export": null}`; the
//     import/export module answers with the export job status.
//   - `role.check` — capability checks, fail closed.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory destroy-request / destroyed-tenants management with the
// full state machine above; typed confirmation with attempt counting
// and lockout; settings-driven second-approver rule and grace
// period; suspension-first flow with an explicit grace expiry;
// final destroy through the tenancy helper with post-drop
// verification that fails closed; the destroyed-uuidv7 guard; and
// external-cleanup tracking that never claims the destroy is
// complete for files, backups, or search indexes until each item is
// confirmed. What is NOT built here: durable storage (the DashMap
// stores are authoritative; the migration snippet for the
// `tenant_destroy_requests` and `destroyed_tenants` tables is in the
// builder report for the integrator to add to the tenants
// migration — this module creates no second migration); the tenant
// record itself and its lifecycle statuses (the tenants module owns
// them); the export job itself (the import/export module owns it);
// subdomain storage and release (the saas_domains module owns it);
// the actual deletion of stored files, backups, and search indexes
// (the owning modules own that — this module only requests and
// tracks it). The scheduler dispatch arm for `tenant.destroy_due`
// exists (the scheduler module owns it): it rebuilds the
// `AppContext` from the handles stored at scheduler `init` and
// calls `process_due_destroys` here; this module declares the kind
// and offers the sweep.
//
// CONFIG-DRIVEN — READ THIS FIRST. Nothing behavioural is
// hard-coded: the grace period, the second-approver age/size
// thresholds, the failed-confirmation limit and lockout, and the
// export entity name all live in `TenantDestroySettings`, editable
// by an administrator through `PUT /destroy-settings`. The values
// below are CONFIGURABLE DEFAULTS. The only hard-coded vocabulary
// is disclosed in the OPEN notes at the end of this header: the
// closed destroy-request statuses, the three external-cleanup kinds,
// the event names, and the capability names.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs against,
// exactly like the import/export module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
//
// OPEN — known gaps, recorded honestly instead of hidden:
//   - The tenants module (`src/modules/tenants.rs`) does not exist
//     yet (parallel builder B2 owns it), so `tenant.lookup` has no
//     answerer today and every destroy step fails closed with a
//     clear "tenant record unavailable" error until B2 lands the
//     listener. B2 must answer `tenant.lookup` with the tenant
//     record: `id`, `slug`, `status`, `created_at` (required — the
//     second-approver age rule needs it), `data_size_bytes`
//     (optional — the size rule is skipped when absent),
//     `destroyed_at` and `export_reference` when present. B2 must
//     also listen to `tenant.suspended` (flip the tenant to
//     `suspended`), `tenant.resumed` (resume serving),
//     `tenant.soft_delete_requested` (keep the row with
//     `destroyed_at` plus the export reference, status `deleted`),
//     and `tenant.destroyed.check` is answered here, not by B2.
//   - The import/export module answers `export.requested` with
//     scope `full_tenant_export` by running the export job, and
//     answers `export.lookup` with the job view, so the destroy
//     flow's export-first gate works end to end without a manual
//     export. The manual path still exists: an admin can create
//     the full export through `POST /api/v1/import-export/exports`
//     and attach the job id through
//     `POST /destroy-requests/{id}/export-proof`. OPEN: no module
//     answers the data-provider query for entity `tenant` yet, so
//     the listener refuses a zero-row export as an error until the
//     tenants module adds the provider.
//   - The saas_domains module answers
//     `saas_domains.subdomain.release_requested`: the destroy flow
//     emits it after the verified schema drop, and the listener
//     releases the tenant's allocations immediately and its
//     product subdomains with the configured grace. The revoke
//     request is also recorded on the destroy request; the
//     destroy is not blocked on the release, because serving
//     already stopped at suspension.
//   - The storage, backup, and search modules have no listeners for
//     the three `tenant.<area>_cleanup_requested` events and no
//     emitters for the matching `.completed` events today, so a
//     destroyed tenant normally waits in
//     `destroyed_pending_external_cleanup` until an admin confirms
//     each item (or the owning modules add listeners). The destroy
//     is never reported as complete for those areas before that.
//   - The audit module records only events listed in its
//     `AUDITED_EVENTS` table; the integrator must add the
//     `tenant.destroy.*`, `tenant.suspended`, `tenant.resumed`, and
//     `tenant.destroyed` events there for the emitted audit trail to
//     be persisted.
//   - The final-drop core (`run_final_destroy`) is shared by the
//     scheduler sweep (`process_due_destroys`) and by the manual
//     `POST /destroy-requests/{id}/execute` endpoint, with identical
//     gates. The tenancy helper documents the drop as "never called
//     from a request handler"; the manual endpoint exists so an
//     operator can execute a due destroy on demand, and it runs the
//     same audited core as the scheduler path. If the integrator
//     wants strict compliance with that note, the manual endpoint
//     should only enqueue `tenant.destroy_due` and the scheduler
//     sweep should be the sole drop caller.
//   - `cargo check` has never run in this environment (no Rust
//     toolchain, crates.io blocked), so compilation is NOT
//     verified; see the builder report for the verification that
//     was done instead. No migration has ever run against live
//     PostgreSQL, so the destroy flow is NOT proven live.

// Tenant Destroy module — destroy lifecycle: export, confirmation, suspension, final destroy.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::tenancy::{
    EVENT_TENANT_DESTROY_REQUESTED, EVENT_TENANT_DESTROYED, EVENT_TENANT_SUSPENDED,
    drop_tenant_schema, is_valid_schema_name, schema_exists, schema_name,
};
use unified_core::db::DbPool;
use crate::modules::import_export::import_export_events;
use crate::modules::tenants::{
    STATUS_DEPROVISIONING, TenantsState, begin_deprovision, finish_deprovision_as_deleted,
    set_export_reference,
};
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::{Arc, OnceLock, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Tenant resolution seam — the ONLY way this module reads a tenant
// record. The tenants module answers the `tenant.lookup` hook query;
// this module never queries the tenants tables (or any other
// module's tables) directly. Fail closed: no answer, or an answer
// that does not match the requested id, refuses to proceed.
// ---------------------------------------------------------------------------

/// Parse one hook result value into a `TenantRef`. Accepts either a
/// bare tenant object (`{"id", "slug", ...}`) or an object carrying
/// a nested `tenant` object.
pub(crate) fn parse_tenant_ref(value: &Value) -> Option<TenantRef> {
    let object = value
        .get("tenant")
        .filter(|nested| nested.is_object())
        .unwrap_or(value);
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .and_then(|raw| Uuid::parse_str(raw).ok())?;
    let slug = object.get("slug").and_then(Value::as_str)?.to_string();
    if slug.is_empty() || slug.chars().count() > MAX_SLUG_LEN {
        return None;
    }
    let status = object
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let created_at = object
        .get("created_at")
        .and_then(Value::as_str)
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|parsed| parsed.with_timezone(&Utc));
    let data_size_bytes = object.get("data_size_bytes").and_then(Value::as_u64);
    let destroyed_at = object
        .get("destroyed_at")
        .and_then(Value::as_str)
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|parsed| parsed.with_timezone(&Utc));
    let export_reference = object
        .get("export_reference")
        .and_then(Value::as_str)
        .map(str::to_string);
    Some(TenantRef {
        id,
        slug,
        status,
        created_at,
        data_size_bytes,
        destroyed_at,
        export_reference,
    })
}

/// First parseable `TenantRef` in a `tenant.lookup` answer: the
/// `tenant` payload key first, then any result. Shared by
/// `resolve_tenant` (AppContext path) and `is_destroyed_tenant`
/// (stored-bus path) so both read the same answer shape.
pub(crate) fn tenant_ref_from_context(context: &HookContext) -> Option<TenantRef> {
    if let Some(found) = parse_tenant_ref(context.get("tenant").unwrap_or(&Value::Null)) {
        return Some(found);
    }
    context.results.iter().find_map(parse_tenant_ref)
}

/// Resolve the tenant record through the tenants module. Fails
/// closed when nothing answers the query, when the answer does not
/// carry the requested id, or when the record has no usable slug.
pub(crate) fn resolve_tenant(ctx: &AppContext, tenant_id: &Uuid) -> UnifiedResult<TenantRef> {
    let context = ctx.hooks.emit(
        QUERY_TENANT_LOOKUP,
        json!({ "tenant_id": tenant_id.to_string(), "tenant": null }),
    );
    let tenant = tenant_ref_from_context(&context).ok_or_else(|| {
        UnifiedError::internal(
            "Tenant record unavailable: the tenants module did not answer the tenant lookup; destroy refuses to proceed",
        )
    })?;
    if &tenant.id != tenant_id {
        return Err(UnifiedError::internal(
            "Tenant lookup answered with a different tenant id; destroy refuses to proceed",
        ));
    }
    Ok(tenant)
}

/// True when the tenant record says the tenant is already gone
/// (`deleted` status or a `destroyed_at` timestamp).
pub(crate) fn tenant_already_destroyed(tenant: &TenantRef) -> bool {
    tenant.status == STATUS_DESTROYED || tenant.status == "deleted" || tenant.destroyed_at.is_some()
}

// ---------------------------------------------------------------------------
// Export verification seam.
// ---------------------------------------------------------------------------

/// Read the tenant an export job belongs to from one hook answer
/// value. The import/export module scopes a full-tenant export to
/// the tenant by passing the tenant id as the export's `company_id`
/// (see its `export.requested` handler), and its `export.completed`
/// event carries that same `company_id`; a future `export.lookup`
/// answer may carry `tenant_id` instead. Accepts either key, on the
/// answer object itself or on its nested `export` object. Returns
/// `None` when the answer carries no tenant linkage at all — the
/// caller must then treat the binding as unverified, never as
/// belonging to the requesting tenant.
pub(crate) fn export_tenant_binding(value: &Value) -> Option<Uuid> {
    let object = value
        .get("export")
        .filter(|nested| nested.is_object())
        .unwrap_or(value);
    object
        .get("tenant_id")
        .or_else(|| object.get("company_id"))
        .and_then(Value::as_str)
        .and_then(|raw| Uuid::parse_str(raw).ok())
}

/// True when an export job's tenant binding (when the answer carries
/// one) names the tenant the destroy request is for. A missing
/// binding is NOT a match: the caller must fail closed on an
/// unverified binding instead of treating "no binding" as "any
/// tenant".
pub(crate) fn export_binding_matches(binding: Option<Uuid>, tenant_id: &Uuid) -> bool {
    binding.is_some_and(|bound| &bound == tenant_id)
}

/// Ask the import/export module for one export job's status through
/// the `export.lookup` hook query. Returns
/// `Some((status, download_url, tenant_binding))` when a handler
/// answers, `None` when nothing answers (the caller then relies on
/// the `export.completed` event). The tenant binding is `Some` only
/// when the answer carries a `tenant_id` / `company_id` for the job;
/// the stock `export.lookup` answer (whose export view has no tenant
/// field) returns `None` here, so a caller that marks a proof ready
/// on a lookup answer alone must refuse when the binding is missing.
pub(crate) fn lookup_export(
    ctx: &AppContext,
    export_id: &Uuid,
) -> Option<(String, Option<String>, Option<Uuid>)> {
    let context = ctx.hooks.emit(
        QUERY_EXPORT_LOOKUP,
        json!({ "export_id": export_id.to_string(), "export": null }),
    );
    let mut candidates: Vec<&Value> = Vec::new();
    if let Some(export) = context.get("export") {
        if export.is_object() {
            candidates.push(export);
        }
    }
    for result in &context.results {
        candidates.push(result);
    }
    for candidate in candidates {
        let object = candidate
            .get("export")
            .filter(|nested| nested.is_object())
            .unwrap_or(candidate);
        if let Some(status) = object.get("status").and_then(Value::as_str) {
            let download_url = object
                .get("download_url")
                .and_then(Value::as_str)
                .map(str::to_string);
            return Some((
                status.to_string(),
                download_url,
                export_tenant_binding(candidate),
            ));
        }
    }
    None
}

/// Mark the export on a request as completed and downloadable, and
/// move the request to `export_ready`. Called from the
/// `export.completed` hook handler and from the attach-proof route
/// when the lookup query answers `completed`.
pub(crate) fn mark_export_ready(
    request: &mut DestroyRequest,
    download_url: Option<String>,
    via: &str,
) {
    let completed_at = now();
    request.export_completed_at = Some(completed_at);
    if download_url.is_some() {
        request.export_download_url = download_url;
    }
    request.export_verified_via = Some(via.to_string());
    request.status = STATUS_EXPORT_READY.to_string();
    append_audit(
        request,
        "system",
        "export_completed",
        &format!("Full tenant export completed and downloadable (verified via {via})."),
    );
}

// ---------------------------------------------------------------------------
// Second-approver rule (settings-driven, never hard-coded).
// ---------------------------------------------------------------------------

/// Evaluate the second-approver rule for a tenant. Returns
/// `Some(reason)` when a second approver is required, `None` when
/// the requester alone may proceed. The age rule needs the tenant
/// `created_at`; when it is missing the flow fails closed instead of
/// guessing. The size rule is skipped when the tenants module does
/// not report a data size.
pub(crate) fn second_approval_requirement(
    settings: &TenantDestroySettings,
    tenant: &TenantRef,
) -> UnifiedResult<Option<String>> {
    let mut reasons: Vec<String> = Vec::new();
    if let Some(min_age_days) = settings.second_approval_min_tenant_age_days {
        let created_at = tenant.created_at.ok_or_else(|| {
            UnifiedError::internal(
                "Tenant age unavailable: the tenants module did not report created_at; second-approver rule cannot be evaluated, destroy refuses to proceed",
            )
        })?;
        let age_days = (now() - created_at).num_days();
        if age_days >= min_age_days {
            reasons.push(format!(
                "tenant age {age_days} days meets the threshold of {min_age_days} days"
            ));
        }
    }
    if let Some(min_size) = settings.second_approval_min_data_size_bytes {
        if let Some(size) = tenant.data_size_bytes {
            if size >= min_size {
                reasons.push(format!(
                    "tenant data size {size} bytes meets the threshold of {min_size} bytes"
                ));
            }
        }
    }
    if reasons.is_empty() {
        Ok(None)
    } else {
        Ok(Some(reasons.join("; ")))
    }
}

// ---------------------------------------------------------------------------
// Suspension + grace (shared core: typed confirmation and second
// approval both end here).
// ---------------------------------------------------------------------------

/// Suspend the tenant and start the grace period. Only ever called
/// after export proof, typed confirmation, and (when required) the
/// second approval are all on the record. Emits `tenant.suspended`
/// so the tenants module stops serving the tenant; the grace expiry
/// is stored as an explicit timestamp on the request.
pub(crate) fn enter_suspension_and_grace(
    ctx: &AppContext,
    request: &mut DestroyRequest,
    actor: &str,
) -> UnifiedResult<()> {
    let tenant = resolve_tenant(ctx, &request.tenant_id)?;
    if tenant_already_destroyed(&tenant) {
        return Err(UnifiedError::conflict(
            "Tenant is already destroyed; the destroy request cannot proceed",
        ));
    }
    if tenant.slug != request.tenant_slug {
        return Err(UnifiedError::internal(
            "Tenant slug changed since the destroy was requested; destroy refuses to proceed",
        ));
    }
    let settings = destroy_settings();
    let schema = schema_name(request.tenant_id);
    if !is_valid_schema_name(&schema) {
        return Err(UnifiedError::internal(
            "Built tenant schema name failed validation; destroy refuses to proceed",
        ));
    }
    let grace_days = settings.grace_period_days;
    let suspended_at = now();
    let grace_expires_at = suspended_at + chrono::Duration::days(grace_days);
    request.suspended_at = Some(suspended_at);
    request.grace_period_days = grace_days;
    request.grace_expires_at = Some(grace_expires_at);
    request.schema_name_snapshot = Some(schema.clone());
    request.status = STATUS_SUSPENDED_GRACE.to_string();
    append_audit(
        request,
        actor,
        "suspended_grace_started",
        &format!(
            "Tenant suspended; grace period of {grace_days} days started, expiring at {grace_expires_at}."
        ),
    );
    save_request(request);
    emit_audit(
        ctx,
        EVENT_TENANT_SUSPENDED,
        actor,
        request.tenant_id,
        json!({
            "destroy_request_id": request.id.to_string(),
            "schema": schema,
            "reason": "destroy_confirmed",
            "grace_expires_at": grace_expires_at.to_rfc3339(),
        }),
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Final destroy (shared core: manual execute endpoint and scheduler
// sweep both run this, with identical gates).
// ---------------------------------------------------------------------------

/// Record a failure that happened after the schema drop on the
/// request (last error + audit trail + `tenant.destroy.failed`
/// alert) and return the error. The schema is already gone at
/// this point, so the request must show exactly where the flow
/// stopped instead of looking retryable-clean.
pub(crate) fn fail_after_drop(
    ctx: &AppContext,
    request: &mut DestroyRequest,
    actor: &str,
    kind: &str,
    error: UnifiedError,
) -> UnifiedError {
    request.last_error = Some(error.message.clone());
    append_audit(
        request,
        actor,
        "destroy_failed",
        &format!("{kind}: {}.", error.message),
    );
    save_request(request);
    emit_audit(
        ctx,
        EVENT_DESTROY_FAILED,
        actor,
        request.tenant_id,
        json!({
            "destroy_request_id": request.id.to_string(),
            "kind": kind,
            "error": error.message,
        }),
    );
    error
}

/// Run the final destroy for one request. Preconditions, all
/// enforced here so no caller can reach the schema drop without
/// them: state `suspended_grace`, grace expired, export proof on
/// record, typed confirmation on record, second approval on record
/// when required. The tenant record moves through the tenants
/// module's helpers only (`begin_deprovision` before the drop,
/// `set_export_reference` + `finish_deprovision_as_deleted` after
/// the verified drop) over the one shared `TenantsState`. The schema name is resolved from the tenant
/// uuidv7 through `schema_name()` at execution time; the drop goes
/// through the tenancy helper only; the drop is verified with
/// `schema_exists` and any failure fails closed with an alert
/// event, leaving the request in `suspended_grace`.
pub(crate) fn run_final_destroy(
    ctx: &AppContext,
    request_id: &Uuid,
    actor: &str,
) -> UnifiedResult<DestroyRequest> {
    let mut request = get_request(request_id)?;
    if request.status != STATUS_SUSPENDED_GRACE {
        return Err(UnifiedError::conflict(format!(
            "Only a destroy request in status \"{}\" can be executed (current status: \"{}\")",
            STATUS_SUSPENDED_GRACE, request.status
        )));
    }
    if !request.grace_expired() {
        return Err(UnifiedError::conflict(
            "Grace period has not expired; the tenant cannot be destroyed yet",
        ));
    }
    if request.export_job_id.is_none() || request.export_completed_at.is_none() {
        return Err(UnifiedError::conflict(
            "No completed export proof is on record; the tenant cannot be destroyed",
        ));
    }
    if request.confirmed_at.is_none() {
        return Err(UnifiedError::conflict(
            "No typed confirmation is on record; the tenant cannot be destroyed",
        ));
    }
    if request.second_approval_required && request.second_approved_at.is_none() {
        return Err(UnifiedError::conflict(
            "Second approval is required and has not been recorded; the tenant cannot be destroyed",
        ));
    }
    let tenant = resolve_tenant(ctx, &request.tenant_id)?;
    if tenant_already_destroyed(&tenant) {
        return Err(UnifiedError::conflict(
            "Tenant is already destroyed; the destroy request cannot be executed again",
        ));
    }
    // Move the tenant record to `deprovisioning` through the
    // tenants module — the single source of truth for destroy
    // state. Skipped when a previous attempt already moved it
    // there, so a retry after a failed drop resumes instead of
    // failing the transition (`deprovisioning` ->
    // `deprovisioning` is not a legal transition).
    let state = tenants_state()?;
    if tenant.status != STATUS_DEPROVISIONING {
        begin_deprovision(
            ctx,
            &state,
            request.tenant_id,
            actor,
            request
                .reason
                .as_deref()
                .unwrap_or("tenant destroy executed"),
        )?;
    }
    let pool: DbPool = ctx
        .db
        .read()
        .map_err(|_| UnifiedError::internal("Database lock poisoned"))?
        .clone()
        .ok_or_else(|| {
            UnifiedError::internal("Database pool unavailable; destroy refuses to proceed")
        })?;
    let schema = schema_name(request.tenant_id);
    if !is_valid_schema_name(&schema) {
        return Err(UnifiedError::internal(
            "Built tenant schema name failed validation; destroy refuses to proceed",
        ));
    }
    // The irreversible step. On any failure the request stays in
    // `suspended_grace` with the error recorded, and an alert event
    // is emitted; nothing after the drop runs.
    if let Err(error) = drop_tenant_schema(&pool, request.tenant_id) {
        request.last_error = Some(error.message.clone());
        append_audit(
            &mut request,
            actor,
            "destroy_failed",
            &format!("Schema drop failed: {}.", error.message),
        );
        save_request(&request);
        emit_audit(
            ctx,
            EVENT_DESTROY_FAILED,
            actor,
            request.tenant_id,
            json!({
                "destroy_request_id": request.id.to_string(),
                "kind": "drop_failed",
                "error": error.message,
            }),
        );
        return Err(error);
    }
    // Verify the schema is gone. A surviving schema means the drop
    // did not take effect: fail closed, alert, never proceed.
    match schema_exists(&pool, request.tenant_id) {
        Ok(true) => {
            let message = "Schema still exists after the drop; destroy refuses to proceed";
            request.last_error = Some(message.to_string());
            append_audit(&mut request, actor, "destroy_failed", message);
            save_request(&request);
            emit_audit(
                ctx,
                EVENT_DESTROY_FAILED,
                actor,
                request.tenant_id,
                json!({
                    "destroy_request_id": request.id.to_string(),
                    "kind": "drop_verification_failed",
                    "error": message,
                }),
            );
            return Err(UnifiedError::internal(message));
        }
        Ok(false) => {}
        Err(error) => {
            request.last_error = Some(error.message.clone());
            append_audit(
                &mut request,
                actor,
                "destroy_failed",
                &format!("Post-drop verification failed: {}.", error.message),
            );
            save_request(&request);
            emit_audit(
                ctx,
                EVENT_DESTROY_FAILED,
                actor,
                request.tenant_id,
                json!({
                    "destroy_request_id": request.id.to_string(),
                    "kind": "drop_verification_error",
                    "error": error.message,
                }),
            );
            return Err(error);
        }
    }
    let export_reference = request.export_job_id.map(|id| id.to_string());
    // The tenant record moves through the tenants module only:
    // record the export reference, then finish deprovisioning
    // (status `deleted`, `destroyed_at` stamped, `tenant.deleted`
    // emitted). The row IS the guard record now — no parallel
    // store is written. A failure here is recorded on the request
    // like a drop failure: the schema is already gone, so the
    // operator must see exactly where the flow stopped.
    if let Some(reference) = export_reference.as_deref() {
        if let Err(error) = set_export_reference(&state, request.tenant_id, reference) {
            return Err(fail_after_drop(
                ctx,
                &mut request,
                actor,
                "export_reference_failed",
                error,
            ));
        }
    }
    let deleted = match finish_deprovision_as_deleted(ctx, &state, request.tenant_id, actor) {
        Ok(tenant) => tenant,
        Err(error) => {
            return Err(fail_after_drop(
                ctx,
                &mut request,
                actor,
                "finish_deprovision_failed",
                error,
            ));
        }
    };
    let destroyed_at = deleted.destroyed_at.unwrap_or_else(now);
    request.destroyed_at = Some(destroyed_at);
    request.schema_name_snapshot = Some(schema.clone());
    request.status = STATUS_DESTROYED_PENDING_EXTERNAL_CLEANUP.to_string();
    request.external_cleanup = EXTERNAL_CLEANUP_KINDS
        .iter()
        .map(|kind| ExternalCleanupItem {
            kind: kind.to_string(),
            status: "pending".to_string(),
            confirmed_at: None,
            confirmed_by: None,
            note: None,
        })
        .collect();
    append_audit(
        &mut request,
        actor,
        "schema_dropped",
        &format!("Tenant schema {schema} dropped and verified gone."),
    );
    save_request(&request);
    // No guard record is written here: the tenants row moved to
    // `deleted` by `finish_deprovision_as_deleted` above IS the
    // guard, so the uuidv7 can never be provisioned or re-created
    // again.
    ctx.hooks.emit(
        unified_core::tenancy::EVENT_TENANT_SCHEMA_DROPPED,
        json!({
            "tenant_id": request.tenant_id.to_string(),
            "schema": schema,
            "destroy_request_id": request.id.to_string(),
            "actor": actor,
        }),
    );
    // Subdomain revocation is the saas_domains module's job; this
    // module only requests it. The revoke is not a blocker for the
    // destroy: serving already stopped at suspension.
    request.subdomains_revoke_requested = true;
    save_request(&request);
    ctx.hooks.emit(
        EVENT_SUBDOMAIN_RELEASE_REQUESTED,
        json!({
            "tenant_id": request.tenant_id.to_string(),
            "schema": schema,
            "destroy_request_id": request.id.to_string(),
            "actor": actor,
        }),
    );
    // External cleanup: the schema drop removed database rows only.
    // Stored files, backups, and search indexes are requested here
    // and tracked until each owning module confirms.
    for (event, kind) in [
        (EVENT_STORAGE_CLEANUP_REQUESTED, CLEANUP_STORAGE),
        (EVENT_BACKUP_CLEANUP_REQUESTED, CLEANUP_BACKUPS),
        (EVENT_SEARCH_INDEX_CLEANUP_REQUESTED, CLEANUP_SEARCH_INDEX),
    ] {
        ctx.hooks.emit(
            event,
            json!({
                "tenant_id": request.tenant_id.to_string(),
                "schema": schema,
                "destroy_request_id": request.id.to_string(),
                "cleanup_kind": kind,
                "export_reference": export_reference,
                "actor": actor,
            }),
        );
    }
    append_audit(
        &mut request,
        actor,
        "destroyed_pending_external_cleanup",
        "Schema dropped and verified; subdomain revoke, control-plane soft-delete, and external cleanup requested.",
    );
    save_request(&request);
    emit_audit(
        ctx,
        EVENT_TENANT_DESTROYED,
        actor,
        request.tenant_id,
        json!({
            "destroy_request_id": request.id.to_string(),
            "schema": schema,
            "export_id": export_reference,
            "destroyed_at": destroyed_at.to_rfc3339(),
            "external_cleanup_pending": EXTERNAL_CLEANUP_KINDS,
        }),
    );
    Ok(request)
}

/// Run the final destroy for every grace-expired request. Called by
/// the scheduler arm for `tenant.destroy_due` and by the manual
/// sweep endpoint. One failure never blocks the others: failures
/// stay recorded on their requests with a `tenant.destroy.failed`
/// event. Returns the ids that were destroyed in this run.
pub fn process_due_destroys(ctx: &AppContext) -> UnifiedResult<Vec<Uuid>> {
    let mut destroyed: Vec<Uuid> = Vec::new();
    for due in due_destroy_requests() {
        if run_final_destroy(ctx, &due.id, "system").is_ok() {
            destroyed.push(due.id);
        }
    }
    Ok(destroyed)
}

/// Confirm one external-cleanup item on a request. When every item
/// is confirmed the request reaches `destroyed`. Shared by the HTTP
/// confirm endpoint and the `tenant.<area>_cleanup.completed` hook
/// handlers. Idempotent: confirming an already confirmed item is a
/// no-op that returns the current request.
pub(crate) fn confirm_external_cleanup_item(
    request_id: &Uuid,
    kind: &str,
    confirmed_by: &str,
    note: Option<String>,
) -> Option<DestroyRequest> {
    let mut request = destroy_requests_store().get_mut(request_id)?;
    if request.status != STATUS_DESTROYED_PENDING_EXTERNAL_CLEANUP {
        return None;
    }
    let mut changed = false;
    for item in request.external_cleanup.iter_mut() {
        if item.kind == kind && item.status != "confirmed" {
            item.status = "confirmed".to_string();
            item.confirmed_at = Some(now());
            item.confirmed_by = Some(confirmed_by.to_string());
            item.note = note.clone();
            changed = true;
        }
    }
    if !changed {
        return Some(request.clone());
    }
    append_audit(
        &mut request,
        confirmed_by,
        "external_cleanup_confirmed",
        &format!("External cleanup confirmed for area \"{kind}\"."),
    );
    if request.external_cleanup_complete() {
        request.status = STATUS_DESTROYED.to_string();
        append_audit(
            &mut request,
            confirmed_by,
            "destroyed",
            "All external cleanup confirmed; tenant destroy is complete.",
        );
    }
    let completed = request.status == STATUS_DESTROYED;
    let finished = request.clone();
    drop(request);
    if completed {
        emit_from_hook(
            EVENT_EXTERNAL_CLEANUP_COMPLETED,
            json!({
                "tenant_id": finished.tenant_id.to_string(),
                "destroy_request_id": finished.id.to_string(),
                "actor": confirmed_by,
            }),
        );
    }
    Some(finished)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tenant(age_days: i64, size: Option<u64>) -> TenantRef {
        TenantRef {
            id: Uuid::new_v4(),
            slug: "acme".to_string(),
            status: "active".to_string(),
            created_at: Some(Utc::now() - chrono::Duration::days(age_days)),
            data_size_bytes: size,
            destroyed_at: None,
            export_reference: None,
        }
    }

    fn settings(age: Option<i64>, size: Option<u64>) -> TenantDestroySettings {
        TenantDestroySettings {
            second_approval_min_tenant_age_days: age,
            second_approval_min_data_size_bytes: size,
            ..Default::default()
        }
    }

    #[test]
    fn no_rules_means_no_second_approval() {
        let tenant = tenant(900, Some(u64::MAX));
        assert_eq!(
            second_approval_requirement(&settings(None, None), &tenant).unwrap(),
            None
        );
    }

    #[test]
    fn old_tenant_triggers_the_age_rule() {
        let tenant = tenant(400, None);
        let reason = second_approval_requirement(&settings(Some(365), None), &tenant).unwrap();
        assert!(reason.is_some());
        assert!(reason.unwrap().contains("365 days"));
    }

    #[test]
    fn young_tenant_skips_the_age_rule() {
        let tenant = tenant(10, None);
        assert_eq!(
            second_approval_requirement(&settings(Some(365), None), &tenant).unwrap(),
            None
        );
    }

    #[test]
    fn missing_created_at_fails_closed() {
        let mut tenant = tenant(400, None);
        tenant.created_at = None;
        assert!(second_approval_requirement(&settings(Some(365), None), &tenant).is_err());
    }

    #[test]
    fn large_tenant_triggers_the_size_rule() {
        let tenant = tenant(1, Some(20_000));
        let reason = second_approval_requirement(&settings(None, Some(10_000)), &tenant).unwrap();
        assert!(reason.is_some());
        let small = tenant(1, Some(100));
        assert_eq!(
            second_approval_requirement(&settings(None, Some(10_000)), &small).unwrap(),
            None
        );
    }

    #[test]
    fn export_tenant_binding_reads_tenant_or_company_id() {
        let tenant_id = Uuid::new_v4();
        let other = Uuid::new_v4();
        assert_eq!(
            export_tenant_binding(&json!({ "tenant_id": tenant_id.to_string() })),
            Some(tenant_id)
        );
        assert_eq!(
            export_tenant_binding(&json!({ "company_id": tenant_id.to_string() })),
            Some(tenant_id)
        );
        assert_eq!(
            export_tenant_binding(
                &json!({ "export": { "status": "completed", "company_id": tenant_id.to_string() } })
            ),
            Some(tenant_id)
        );
        assert_eq!(
            export_tenant_binding(&json!({ "status": "completed" })),
            None
        );
        assert_eq!(
            export_tenant_binding(&json!({ "company_id": "not-a-uuid" })),
            None
        );
        assert_eq!(
            export_tenant_binding(&json!({ "tenant_id": other.to_string(), "company_id": tenant_id.to_string() })),
            Some(other),
            "tenant_id wins over company_id when both are present"
        );
    }

    #[test]
    fn export_binding_matches_only_the_same_tenant() {
        let tenant_id = Uuid::new_v4();
        assert!(export_binding_matches(Some(tenant_id), &tenant_id));
        assert!(!export_binding_matches(Some(Uuid::new_v4()), &tenant_id));
        assert!(
            !export_binding_matches(None, &tenant_id),
            "a missing binding must never count as a match"
        );
    }

    #[test]
    fn lookup_export_returns_the_tenant_binding_when_the_answer_carries_one() {
        use unified_core::config::AppConfig;
        use unified_core::modules::ModuleRegistry;
        let tenant_id = Uuid::new_v4();
        let job_id = Uuid::new_v4();
        let ctx = AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        );
        let bound = tenant_id.to_string();
        ctx.hooks.register(
            QUERY_EXPORT_LOOKUP,
            "test",
            Arc::new(move |hook: &mut HookContext| {
                hook.add_result(json!({
                    "exists": true,
                    "export": {
                        "id": job_id.to_string(),
                        "status": "completed",
                        "download_url": "https://example.invalid/export.json",
                        "company_id": bound,
                    },
                }));
            }),
        );
        let found = lookup_export(&ctx, &job_id).expect("lookup must answer");
        assert_eq!(found.0, "completed");
        assert_eq!(found.1.as_deref(), Some("https://example.invalid/export.json"));
        assert_eq!(found.2, Some(tenant_id));
    }

    #[test]
    fn lookup_export_binding_is_none_when_the_answer_carries_no_tenant_linkage() {
        use unified_core::config::AppConfig;
        use unified_core::modules::ModuleRegistry;
        let job_id = Uuid::new_v4();
        let ctx = AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        );
        ctx.hooks.register(
            QUERY_EXPORT_LOOKUP,
            "test",
            Arc::new(move |hook: &mut HookContext| {
                hook.add_result(json!({
                    "exists": true,
                    "export": { "id": job_id.to_string(), "status": "completed" },
                }));
            }),
        );
        let found = lookup_export(&ctx, &job_id).expect("lookup must answer");
        assert_eq!(found.0, "completed");
        assert_eq!(found.2, None);
    }
}
