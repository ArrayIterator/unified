//! HTTP handlers for lifecycle stages, including seeding the
//! default stage set.

use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    caller, caller_company, created, err, load_stage, load_stage_by_slug, ok, paginate,
    require_permission, require_slug, require_tenant, require_text, resolve_tenant,
    validate_description, validate_name,
};
use super::state::PlmState;
use super::types::{
    default_stage_definitions, CreateStageRequest, PlmStage, UpdateStageRequest, PERM_PLM_MANAGE,
};

/// Path carrying a stage id.
#[derive(Debug, Deserialize)]
struct StagePath {
    stage_id: String,
}

/// Validate the allowed-transitions list: at most 20 targets,
/// each a valid slug.
fn validate_transitions(raw: &[String]) -> UnifiedResult<Vec<String>> {
    if raw.len() > 20 {
        return Err(UnifiedError::bad_request(
            "allowed_transitions must have at most 20 entries",
        ));
    }
    let mut out = Vec::with_capacity(raw.len());
    for target in raw {
        out.push(require_slug(target, "allowed_transitions entry")?);
    }
    Ok(out)
}

fn stage_exists(state: &PlmState, tenant_id: &str, slug: &str) -> bool {
    state
        .stages
        .iter()
        .any(|entry| entry.value().tenant_id == tenant_id && entry.value().slug == slug)
}

// ---------------------------------------------------------------------------
// Stage handlers (authenticated)
// ---------------------------------------------------------------------------

pub(crate) async fn list_stages(
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
        let active_filter = params.filters.get("active").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<PlmStage> = state
            .stages
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|stage| {
                if stage.tenant_id != tenant_id {
                    return false;
                }
                if let Some(active) = &active_filter {
                    let want = active == "true";
                    if stage.active != want {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {}",
                        stage.name,
                        stage.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|stage| (stage.sort_order, stage.name.clone()));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_stage(
    state: web::Data<PlmState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateStageRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PlmStage> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PLM_MANAGE)?;
        let input = body.into_inner();
        let slug = require_slug(&input.slug, "slug")?;
        let name = validate_name(&input.name)?;
        let description = validate_description(&input.description)?;
        let allowed_transitions = validate_transitions(&input.allowed_transitions)?;
        if input.required_approvals < 0 {
            return Err(UnifiedError::bad_request(
                "required_approvals must not be negative",
            ));
        }
        if stage_exists(&state, &tenant_id, &slug) {
            return Err(UnifiedError::conflict(format!(
                "slug is already in use: {slug}"
            )));
        }
        if input.is_initial
            && state
                .stages
                .iter()
                .any(|entry| entry.value().tenant_id == tenant_id && entry.value().is_initial)
        {
            return Err(UnifiedError::conflict(
                "an initial stage already exists for this tenant",
            ));
        }
        let now = Utc::now();
        let stage = PlmStage {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.clone(),
            slug: slug.clone(),
            name,
            description,
            sort_order: input.sort_order,
            is_initial: input.is_initial,
            is_terminal: input.is_terminal,
            allowed_transitions,
            required_approvals: input.required_approvals,
            active: input.active,
            created_at: now,
            updated_at: now,
        };
        state.stages.insert(stage.id, stage.clone());
        state.emit(
            "plm.stage.created",
            json!({
                "tenant_id": tenant_id,
                "stage_id": stage.id.to_string(),
                "slug": stage.slug,
                "name": stage.name,
            }),
        );
        Ok(stage)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_stage(
    state: web::Data<PlmState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<StagePath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PlmStage> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let _caller = caller(&req)?;
        load_stage(&state, &tenant_id, &path.stage_id)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_stage(
    state: web::Data<PlmState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<StagePath>,
    body: web::Json<UpdateStageRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PlmStage> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PLM_MANAGE)?;
        let mut stage = load_stage(&state, &tenant_id, &path.stage_id)?;
        let input = body.into_inner();
        if let Some(raw) = &input.name {
            stage.name = validate_name(raw)?;
        }
        if let Some(description) = &input.description {
            stage.description = match description {
                Some(raw) => Some(require_text(raw, "description", 5000)?),
                None => None,
            };
        }
        if let Some(sort_order) = input.sort_order {
            stage.sort_order = sort_order;
        }
        if let Some(is_initial) = input.is_initial {
            if is_initial
                && !stage.is_initial
                && state
                    .stages
                    .iter()
                    .any(|entry| entry.value().tenant_id == tenant_id && entry.value().is_initial)
            {
                return Err(UnifiedError::conflict(
                    "an initial stage already exists for this tenant",
                ));
            }
            stage.is_initial = is_initial;
        }
        if let Some(is_terminal) = input.is_terminal {
            stage.is_terminal = is_terminal;
        }
        if let Some(transitions) = &input.allowed_transitions {
            stage.allowed_transitions = validate_transitions(transitions)?;
        }
        if let Some(required) = input.required_approvals {
            if required < 0 {
                return Err(UnifiedError::bad_request(
                    "required_approvals must not be negative",
                ));
            }
            stage.required_approvals = required;
        }
        if let Some(active) = input.active {
            stage.active = active;
        }
        stage.updated_at = Utc::now();
        state.stages.insert(stage.id, stage.clone());
        state.emit(
            "plm.stage.updated",
            json!({
                "tenant_id": tenant_id,
                "stage_id": stage.id.to_string(),
                "slug": stage.slug,
                "name": stage.name,
            }),
        );
        Ok(stage)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_stage(
    state: web::Data<PlmState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<StagePath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> =
        (|| {
            let _tenant = require_tenant(&req)?;
            let _scope = caller_company(&_tenant, &req)?;
            let tenant_id = resolve_tenant(&req)?;
            let caller = caller(&req)?;
            require_permission(&ctx, &caller.role, PERM_PLM_MANAGE)?;
            let stage = load_stage(&state, &tenant_id, &path.stage_id)?;
            if state.versions.iter().any(|entry| {
                entry.value().tenant_id == tenant_id && entry.value().status == stage.slug
            }) {
                return Err(UnifiedError::bad_request(
                    "Stage is in use by product versions; move them to another stage first",
                ));
            }
            state.stages.remove(&stage.id);
            state.emit(
                "plm.stage.deleted",
                json!({
                    "tenant_id": tenant_id,
                    "stage_id": stage.id.to_string(),
                    "slug": stage.slug,
                    "name": stage.name,
                }),
            );
            Ok(json!({ "deleted": true, "stage_id": stage.id.to_string() }))
        })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Seed the default stage set for the tenant. Stages whose slug
/// already exists are skipped, so seeding is idempotent and
/// never overwrites an admin's edits. The default set is seed
/// data, not an enum: every seeded stage can be renamed,
/// rewired, or deleted afterwards.
pub(crate) async fn seed_stages(
    state: web::Data<PlmState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PLM_MANAGE)?;
        let now = Utc::now();
        let mut created: Vec<PlmStage> = Vec::new();
        let mut skipped: Vec<String> = Vec::new();
        for definition in default_stage_definitions() {
            if stage_exists(&state, &tenant_id, definition.slug) {
                skipped.push(definition.slug.to_string());
                continue;
            }
            // Only one initial stage per tenant: if the tenant
            // already has one, the seeded initial stage keeps
            // its place in the graph but is not initial.
            let has_initial = state
                .stages
                .iter()
                .any(|entry| entry.value().tenant_id == tenant_id && entry.value().is_initial);
            let stage = PlmStage {
                id: Uuid::now_v7(),
                tenant_id: tenant_id.clone(),
                slug: definition.slug.to_string(),
                name: definition.name.to_string(),
                description: Some(definition.description.to_string()),
                sort_order: definition.sort_order,
                is_initial: definition.is_initial && !has_initial,
                is_terminal: definition.is_terminal,
                allowed_transitions: definition
                    .allowed_transitions
                    .iter()
                    .map(|item| item.to_string())
                    .collect(),
                required_approvals: definition.required_approvals,
                active: true,
                created_at: now,
                updated_at: now,
            };
            state.stages.insert(stage.id, stage.clone());
            created.push(stage);
        }
        state.emit(
            "plm.stage.seeded",
            json!({
                "tenant_id": tenant_id,
                "created": created.len(),
                "skipped": skipped.len(),
            }),
        );
        Ok(json!({ "created": created, "skipped": skipped }))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

/// Resolve the stage a new version starts in: the named slug
/// when given (it must exist and be active), otherwise the
/// tenant's initial stage. Errors when the tenant has no
/// stages at all, so the caller seeds or creates stages first.
pub(crate) fn resolve_initial_stage(
    state: &PlmState,
    tenant_id: &str,
    requested: &Option<String>,
) -> UnifiedResult<PlmStage> {
    match requested {
        Some(slug) => {
            let stage = load_stage_by_slug(state, tenant_id, slug)?;
            if !stage.active {
                return Err(UnifiedError::bad_request(format!(
                    "stage is not active: {slug}"
                )));
            }
            Ok(stage)
        }
        None => state
            .stages
            .iter()
            .map(|entry| entry.value().clone())
            .find(|stage| stage.tenant_id == tenant_id && stage.is_initial && stage.active)
            .ok_or_else(|| {
                UnifiedError::bad_request(
                    "no initial stage exists for this tenant; seed or create stages first",
                )
            }),
    }
}
