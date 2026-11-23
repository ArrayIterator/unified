//! HTTP handlers for product versions: revisions, stage
//! transitions through the data-driven stage graph, version
//! compare/diff, and stage-entry approvals.

use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::handlers_stages::resolve_initial_stage;
use super::helpers::{
    approved_version_approvals, caller, caller_company, created, err, load_stage_by_slug,
    load_version, ok, paginate, parse_uuid, require_permission, require_tenant, require_text,
    resolve_tenant, validate_effective_range, version_view,
};
use super::state::PlmState;
use super::types::{
    diff_versions, next_revision, stage_transition_allowed, ApprovalDecision,
    CreateApprovalRequest, CreateVersionRequest, PlmApproval, PlmVersion, UpdateVersionRequest,
    VersionTransitionRequest, EVENT_STAGE_CHANGED, EVENT_VERSION_CREATED, MAX_SUMMARY_LEN,
    PERM_PLM_MANAGE,
};

/// Path carrying a version id.
#[derive(Debug, Deserialize)]
struct VersionPath {
    version_id: String,
}

/// Query for the diff route.
#[derive(Debug, Deserialize)]
struct DiffQuery {
    against: String,
}

fn revision_in_use(
    state: &PlmState,
    tenant_id: &str,
    product_id: &str,
    major: i64,
    minor: i64,
    patch: i64,
) -> bool {
    state.versions.iter().any(|entry| {
        let version = entry.value();
        version.tenant_id == tenant_id
            && version.product_id == product_id
            && version.major == major
            && version.minor == minor
            && version.patch == patch
    })
}

fn clear_current_for_product(state: &PlmState, tenant_id: &str, product_id: &str, except: Uuid) {
    for mut entry in state.versions.iter_mut() {
        let version = entry.value_mut();
        if version.tenant_id == tenant_id
            && version.product_id == product_id
            && version.id != except
            && version.is_current
        {
            version.is_current = false;
            version.updated_at = Utc::now();
        }
    }
}

// ---------------------------------------------------------------------------
// Version handlers (authenticated)
// ---------------------------------------------------------------------------

pub(crate) async fn list_versions(
    state: web::Data<PlmState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let _caller = caller(&req)?;
        let params = query.into_inner();
        let product_filter = params.filters.get("product_id").cloned();
        let stage_filter = params.filters.get("stage").cloned();
        let current_filter = params.filters.get("is_current").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<PlmVersion> = state
            .versions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|version| {
                if version.tenant_id != tenant_id {
                    return false;
                }
                if let Some(product_id) = &product_filter {
                    if version.product_id != *product_id {
                        return false;
                    }
                }
                if let Some(stage) = &stage_filter {
                    if version.status != *stage {
                        return false;
                    }
                }
                if let Some(current) = &current_filter {
                    let want = current == "true";
                    if version.is_current != want {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !version.change_summary.to_lowercase().contains(&search)
                    && !version.revision().contains(&search)
                {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then(a.product_id.cmp(&b.product_id))
        });
        let views: Vec<Value> = items
            .iter()
            .map(|version| version_view(&state, version))
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_version(
    state: web::Data<PlmState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateVersionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PLM_MANAGE)?;
        let input = body.into_inner();
        let product_id = require_text(&input.product_id, "product_id", 200)?;
        let change_summary =
            require_text(&input.change_summary, "change_summary", MAX_SUMMARY_LEN)?;
        validate_effective_range(input.effective_from, input.effective_to)?;
        // Resolve the revision: explicit numbers win; otherwise a
        // bump off the superseded version; otherwise 1.0.0.
        let (major, minor, patch) = match (input.major, input.minor, input.patch) {
            (Some(major), Some(minor), Some(patch)) => {
                if major < 0 || minor < 0 || patch < 0 {
                    return Err(UnifiedError::bad_request(
                        "revision parts must not be negative",
                    ));
                }
                (major, minor, patch)
            }
            (None, None, None) => match &input.supersedes_version_id {
                Some(superseded_id) => {
                    let superseded = state
                        .versions
                        .get(superseded_id)
                        .map(|entry| entry.value().clone())
                        .ok_or_else(|| {
                            UnifiedError::not_found(format!("Version not found: {superseded_id}"))
                        })?;
                    if superseded.tenant_id != tenant_id {
                        return Err(UnifiedError::not_found(format!(
                            "Version not found: {superseded_id}"
                        )));
                    }
                    if superseded.product_id != product_id {
                        return Err(UnifiedError::bad_request(
                            "supersedes_version_id belongs to a different product",
                        ));
                    }
                    let bump = input.bump.as_deref().unwrap_or("minor");
                    next_revision(superseded.major, superseded.minor, superseded.patch, bump)
                        .ok_or_else(|| {
                            UnifiedError::bad_request(format!(
                                "bump must be major, minor, or patch, got \"{bump}\""
                            ))
                        })?
                }
                None => (1, 0, 0),
            },
            _ => {
                return Err(UnifiedError::bad_request(
                    "supply all of major, minor, and patch, or none of them",
                ));
            }
        };
        if revision_in_use(&state, &tenant_id, &product_id, major, minor, patch) {
            return Err(UnifiedError::conflict(format!(
                "revision {}.{}.{} already exists for this product",
                major, minor, patch
            )));
        }
        let stage = resolve_initial_stage(&state, &tenant_id, &input.status)?;
        let now = Utc::now();
        let version = PlmVersion {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            product_id: product_id.clone(),
            major,
            minor,
            patch,
            change_summary,
            status: stage.slug.clone(),
            effective_from: input.effective_from,
            effective_to: input.effective_to,
            supersedes_version_id: input.supersedes_version_id,
            is_current: input.is_current,
            product_deleted: false,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        if version.is_current {
            clear_current_for_product(&state, &tenant_id, &product_id, version.id);
        }
        state.versions.insert(version.id, version.clone());
        state.record_audit(
            &tenant_id,
            "version",
            version.id,
            "created",
            &caller.actor,
            Some(format!("revision {}", version.revision())),
            None,
            Some(version.status.clone()),
        );
        state.emit(
            EVENT_VERSION_CREATED,
            json!({
                "tenant_id": tenant_id,
                "version_id": version.id.to_string(),
                "product_id": version.product_id,
                "revision": version.revision(),
                "status": version.status,
                "created_by": caller.actor,
            }),
        );
        Ok(version_view(&state, &version))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_version(
    state: web::Data<PlmState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<VersionPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let _caller = caller(&req)?;
        let version = load_version(&state, &tenant_id, &path.version_id)?;
        Ok(version_view(&state, &version))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_version(
    state: web::Data<PlmState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<VersionPath>,
    body: web::Json<UpdateVersionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PLM_MANAGE)?;
        let mut version = load_version(&state, &tenant_id, &path.version_id)?;
        let input = body.into_inner();
        if let Some(raw) = &input.change_summary {
            version.change_summary = require_text(raw, "change_summary", MAX_SUMMARY_LEN)?;
        }
        if let Some(from) = &input.effective_from {
            version.effective_from = *from;
        }
        if let Some(to) = &input.effective_to {
            version.effective_to = *to;
        }
        validate_effective_range(version.effective_from, version.effective_to)?;
        if let Some(is_current) = input.is_current {
            version.is_current = is_current;
            if is_current {
                clear_current_for_product(&state, &tenant_id, &version.product_id, version.id);
            }
        }
        version.updated_at = Utc::now();
        state.versions.insert(version.id, version.clone());
        state.record_audit(
            &tenant_id,
            "version",
            version.id,
            "updated",
            &caller.actor,
            None,
            None,
            Some(version.revision()),
        );
        Ok(version_view(&state, &version))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_version(
    state: web::Data<PlmState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<VersionPath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PLM_MANAGE)?;
        let version = load_version(&state, &tenant_id, &path.version_id)?;
        if state.ecos.iter().any(|entry| {
            entry.value().tenant_id == tenant_id
                && entry.value().affected_version_ids.contains(&version.id)
        }) {
            return Err(UnifiedError::bad_request(
                "Version is referenced by an ECO; remove it from the ECO first",
            ));
        }
        if state.versions.iter().any(|entry| {
            entry.value().tenant_id == tenant_id
                && entry.value().supersedes_version_id == Some(version.id)
        }) {
            return Err(UnifiedError::bad_request(
                "Version is superseded by a newer revision; delete the newer revision first",
            ));
        }
        state.versions.remove(&version.id);
        state.record_audit(
            &tenant_id,
            "version",
            version.id,
            "deleted",
            &caller.actor,
            Some(format!("revision {}", version.revision())),
            Some(version.status.clone()),
            None,
        );
        Ok(json!({ "deleted": true, "version_id": version.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Move a version to another lifecycle stage. The move must be
/// allowed by the current stage's `allowed_transitions` (data,
/// not code), and when the target stage requires approvals the
/// version must already hold that many approved version
/// approvals (recorded through the approvals endpoint below).
pub(crate) async fn transition_version(
    state: web::Data<PlmState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<VersionPath>,
    body: web::Json<VersionTransitionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PLM_MANAGE)?;
        let mut version = load_version(&state, &tenant_id, &path.version_id)?;
        let input = body.into_inner();
        let target_slug = input.to_stage.trim().to_lowercase();
        let current_stage = load_stage_by_slug(&state, &tenant_id, &version.status)?;
        let target_stage = load_stage_by_slug(&state, &tenant_id, &target_slug)?;
        if !target_stage.active {
            return Err(UnifiedError::bad_request(format!(
                "stage is not active: {target_slug}"
            )));
        }
        if !stage_transition_allowed(&current_stage, &target_slug) {
            return Err(UnifiedError::bad_request(format!(
                "stage {} does not allow a transition to {}",
                current_stage.slug, target_slug
            )));
        }
        if target_stage.required_approvals > 0 {
            let approved = approved_version_approvals(&state, &tenant_id, version.id);
            if approved < target_stage.required_approvals {
                return Err(UnifiedError::bad_request(format!(
                    "entering stage {} requires {} approved version approval(s); {} recorded",
                    target_slug, target_stage.required_approvals, approved
                )));
            }
        }
        let from_stage = version.status.clone();
        version.status = target_slug.clone();
        version.updated_at = Utc::now();
        state.versions.insert(version.id, version.clone());
        state.record_audit(
            &tenant_id,
            "version",
            version.id,
            "stage_changed",
            &caller.actor,
            input.comment.clone(),
            Some(from_stage.clone()),
            Some(target_slug.clone()),
        );
        state.emit(
            EVENT_STAGE_CHANGED,
            json!({
                "tenant_id": tenant_id,
                "version_id": version.id.to_string(),
                "product_id": version.product_id,
                "revision": version.revision(),
                "from_stage": from_stage,
                "to_stage": target_slug,
                "actor_id": caller.actor,
            }),
        );
        Ok(version_view(&state, &version))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Compare two versions of the same product: field-by-field
/// diff metadata (revision, status, summary, effective dates,
/// current flag). The two versions must belong to the same
/// product and tenant.
pub(crate) async fn diff_version(
    state: web::Data<PlmState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<VersionPath>,
    query: web::Query<DiffQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let _caller = caller(&req)?;
        let version = load_version(&state, &tenant_id, &path.version_id)?;
        let other_id = parse_uuid(&query.against, "against version")?;
        let other = state
            .versions
            .get(&other_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Version not found: {other_id}")))?;
        if other.tenant_id != tenant_id {
            return Err(UnifiedError::not_found(format!(
                "Version not found: {other_id}"
            )));
        }
        if other.product_id != version.product_id {
            return Err(UnifiedError::bad_request(
                "both versions must belong to the same product",
            ));
        }
        Ok(json!({
            "version_id": version.id.to_string(),
            "against_id": other.id.to_string(),
            "product_id": version.product_id,
            "changes": diff_versions(&version, &other),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Record a stage-entry approval for a version: one approval
/// counts towards the target stage's `required_approvals`
/// gate. The decision is `approved` or `rejected`; the caller
/// must hold the manage capability.
pub(crate) async fn create_version_approval(
    state: web::Data<PlmState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<VersionPath>,
    body: web::Json<CreateApprovalRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PlmApproval> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PLM_MANAGE)?;
        let version = load_version(&state, &tenant_id, &path.version_id)?;
        let input = body.into_inner();
        let decision = ApprovalDecision::from_str(input.decision.trim().to_lowercase().as_str())
            .filter(|decision| *decision != ApprovalDecision::Pending)
            .ok_or_else(|| UnifiedError::bad_request("decision must be approved or rejected"))?;
        let approver_role =
            require_text(&input.approver_role, "approver_role", 128)?.to_lowercase();
        let comment = match &input.comment {
            Some(raw) => Some(require_text(raw, "comment", 2000)?),
            None => None,
        };
        let now = Utc::now();
        let approval = PlmApproval {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            entity_type: "version".to_string(),
            entity_id: version.id,
            step_index: 0,
            step_name: "stage_entry".to_string(),
            approver_role,
            approver_id: Some(caller.actor.clone()),
            decision,
            comment: comment.clone(),
            decided_at: Some(now),
            created_at: now,
            updated_at: now,
        };
        state.approvals.insert(approval.id, approval.clone());
        state.record_audit(
            &tenant_id,
            "version",
            version.id,
            "approval_decided",
            &caller.actor,
            comment,
            None,
            Some(decision.as_str().to_string()),
        );
        state.emit(
            "plm.approval.decided",
            json!({
                "tenant_id": tenant_id,
                "entity_type": "version",
                "entity_id": version.id.to_string(),
                "approval_id": approval.id.to_string(),
                "decision": decision.as_str(),
                "approver_id": caller.actor,
            }),
        );
        Ok(approval)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}
