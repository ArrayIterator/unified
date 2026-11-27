//! HTTP handlers for pipeline stages: the data-driven
//! referral pipeline (stages, order, terminal / hired /
//! rejected flags, allowed transitions).

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    caller, caller_company, created, err, load_stage, load_stage_by_slug, ok, paginate,
    record_audit, require_manage, require_tenant, require_text, require_view, resolve_tenant,
    validate_stage_input,
};
use super::state::ReferralState;
use super::types::{
    CreateStageRequest, MAX_NAME_LEN, MAX_SLUG_LEN, PipelineStage, UpdateStageRequest,
};

/// List pipeline stages (paginated, in pipeline order;
/// filter by active, terminal, hired, rejected; search by
/// name/slug).
pub(crate) async fn list_stages(
    state: web::Data<ReferralState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let params = query.into_inner();
        let bool_filter = |key: &str| {
            params.filters.get(key).and_then(|raw| match raw.as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            })
        };
        let active_filter = bool_filter("active");
        let terminal_filter = bool_filter("is_terminal");
        let hired_filter = bool_filter("is_hired_stage");
        let rejected_filter = bool_filter("is_rejected_stage");
        let search = params.search.clone().map(|needle| needle.to_lowercase());
        let mut items = state.stages_of(&tenant_id);
        items.retain(|stage| {
            if let Some(active) = active_filter {
                if stage.active != active {
                    return false;
                }
            }
            if let Some(terminal) = terminal_filter {
                if stage.is_terminal != terminal {
                    return false;
                }
            }
            if let Some(hired) = hired_filter {
                if stage.is_hired_stage != hired {
                    return false;
                }
            }
            if let Some(rejected) = rejected_filter {
                if stage.is_rejected_stage != rejected {
                    return false;
                }
            }
            if let Some(needle) = &search {
                if !stage.name.to_lowercase().contains(needle)
                    && !stage.slug.to_lowercase().contains(needle)
                {
                    return false;
                }
            }
            true
        });
        let views: Vec<Value> = items
            .iter()
            .map(|item| serde_json::to_value(item).unwrap_or(Value::Null))
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Create a pipeline stage (manager). The slug is unique
/// per tenant; a hired stage and a rejected stage are both
/// allowed to exist, but a stage that is both hired and
/// rejected at once is rejected as contradictory.
pub(crate) async fn create_stage(
    state: web::Data<ReferralState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateStageRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        if input.is_hired_stage && input.is_rejected_stage {
            return Err(UnifiedError::bad_request(
                "A stage cannot be both the hired stage and the rejected stage".to_string(),
            ));
        }
        let (name, slug, next_ids) = validate_stage_input(&state, &tenant_id, &input)?;
        if state
            .stages_of(&tenant_id)
            .iter()
            .any(|stage| stage.slug == slug)
        {
            return Err(UnifiedError::conflict(format!(
                "A pipeline stage with slug \"{slug}\" already exists"
            )));
        }
        let now = Utc::now();
        let stage = PipelineStage {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.clone(),
            name,
            slug,
            sort_order: input.sort_order,
            is_terminal: input.is_terminal,
            is_hired_stage: input.is_hired_stage,
            is_rejected_stage: input.is_rejected_stage,
            allowed_next_stage_ids: next_ids,
            active: true,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.stages.insert(stage.id, stage.clone());
        record_audit(
            &state,
            &tenant_id,
            "pipeline_stage",
            &stage.id.to_string(),
            "created",
            &caller.actor,
            None,
            serde_json::to_value(&stage).ok(),
        );
        Ok(serde_json::to_value(&stage).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

/// Get one pipeline stage.
pub(crate) async fn get_stage(
    state: web::Data<ReferralState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let stage = load_stage(&state, &tenant_id, &path.into_inner())?;
        Ok(serde_json::to_value(&stage).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Update a pipeline stage (manager). Renaming a slug
/// rewrites the slug snapshot on referrals currently in
/// that stage, so list filters keep matching; the history
/// rows keep the slug as it was at the time of the move.
pub(crate) async fn update_stage(
    state: web::Data<ReferralState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<UpdateStageRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut stage = load_stage(&state, &tenant_id, &path.into_inner())?;
        let before = serde_json::to_value(&stage).ok();
        if let Some(name) = &input.name {
            stage.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(slug) = &input.slug {
            let slug = slug.trim().to_lowercase();
            if !super::types::valid_slug(&slug) {
                return Err(UnifiedError::bad_request(
                    "slug must be lowercase letters, digits, and dashes, starting with a letter or digit".to_string(),
                ));
            }
            if slug.len() > MAX_SLUG_LEN {
                return Err(UnifiedError::bad_request(
                    "slug must be at most 64 characters".to_string(),
                ));
            }
            if slug != stage.slug
                && state
                    .stages_of(&tenant_id)
                    .iter()
                    .any(|other| other.slug == slug)
            {
                return Err(UnifiedError::conflict(format!(
                    "A pipeline stage with slug \"{slug}\" already exists"
                )));
            }
            stage.slug = slug;
        }
        if let Some(sort_order) = input.sort_order {
            stage.sort_order = sort_order;
        }
        if let Some(is_terminal) = input.is_terminal {
            stage.is_terminal = is_terminal;
        }
        if let Some(is_hired_stage) = input.is_hired_stage {
            stage.is_hired_stage = is_hired_stage;
        }
        if let Some(is_rejected_stage) = input.is_rejected_stage {
            stage.is_rejected_stage = is_rejected_stage;
        }
        if stage.is_hired_stage && stage.is_rejected_stage {
            return Err(UnifiedError::bad_request(
                "A stage cannot be both the hired stage and the rejected stage".to_string(),
            ));
        }
        if let Some(next_ids) = &input.allowed_next_stage_ids {
            if next_ids.len() > super::types::MAX_NEXT_STAGES {
                return Err(UnifiedError::bad_request(format!(
                    "At most {} allowed next stages per stage",
                    super::types::MAX_NEXT_STAGES
                )));
            }
            let mut seen = std::collections::HashSet::new();
            let mut clean = Vec::new();
            for raw in next_ids {
                let next = load_stage(&state, &tenant_id, raw)?;
                if next.id == stage.id {
                    return Err(UnifiedError::bad_request(
                        "A stage cannot transition to itself".to_string(),
                    ));
                }
                if !seen.insert(next.id) {
                    return Err(UnifiedError::bad_request(format!(
                        "allowed_next_stage_ids must not repeat \"{raw}\""
                    )));
                }
                clean.push(next.id);
            }
            stage.allowed_next_stage_ids = clean;
        }
        if let Some(active) = input.active {
            stage.active = active;
        }
        stage.updated_at = Utc::now();
        let old_slug = match &before {
            Some(value) => value
                .get("slug")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string(),
            None => String::new(),
        };
        state.stages.insert(stage.id, stage.clone());
        if old_slug != stage.slug {
            for mut entry in state.referrals.iter_mut() {
                if entry.value().tenant_id == tenant_id && entry.value().stage_id == stage.id {
                    entry.value_mut().stage_slug = stage.slug.clone();
                    entry.value_mut().updated_at = Utc::now();
                }
            }
        }
        record_audit(
            &state,
            &tenant_id,
            "pipeline_stage",
            &stage.id.to_string(),
            "updated",
            &caller.actor,
            before,
            serde_json::to_value(&stage).ok(),
        );
        Ok(serde_json::to_value(&stage).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Delete a pipeline stage (manager). A stage that still
/// holds referrals cannot be deleted - move the referrals
/// first. References to the deleted stage are pruned from
/// other stages' allowed-next lists, and reward rules that
/// triggered on it lose their trigger stage.
pub(crate) async fn delete_stage(
    state: web::Data<ReferralState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let stage = load_stage(&state, &tenant_id, &path.into_inner())?;
        let in_use = state
            .referrals_of(&tenant_id)
            .iter()
            .any(|referral| referral.stage_id == stage.id);
        if in_use {
            return Err(UnifiedError::conflict(
                "Pipeline stage still holds referrals; move them first".to_string(),
            ));
        }
        let before = serde_json::to_value(&stage).ok();
        state.stages.remove(&stage.id);
        for mut entry in state.stages.iter_mut() {
            if entry.value().tenant_id == tenant_id {
                entry
                    .value_mut()
                    .allowed_next_stage_ids
                    .retain(|id| *id != stage.id);
            }
        }
        for mut entry in state.reward_rules.iter_mut() {
            if entry.value().tenant_id == tenant_id
                && entry.value().trigger_stage_id == Some(stage.id)
            {
                entry.value_mut().trigger_stage_id = None;
                entry.value_mut().updated_at = Utc::now();
            }
        }
        record_audit(
            &state,
            &tenant_id,
            "pipeline_stage",
            &stage.id.to_string(),
            "deleted",
            &caller.actor,
            before,
            None,
        );
        Ok(json!({ "deleted": true, "pipeline_stage_id": stage.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Resolve a stage by slug (used by the AI grounding
/// handler and the recruitment hooks).
pub(crate) fn stage_by_slug(
    state: &ReferralState,
    tenant_id: &str,
    slug: &str,
) -> UnifiedResult<PipelineStage> {
    load_stage_by_slug(state, tenant_id, slug)
}
