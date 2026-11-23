// Generic Approvals — Approval definition handlers.

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
use super::handlers_requests::*;
use super::handlers_dashboards::*;
use super::hooks::*;
use super::module::*;

// ---------------------------------------------------------------------------
// Definitions
// ---------------------------------------------------------------------------

pub(crate) async fn list_definitions(
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
        let module_filter = params.filters.get("module").cloned();
        let record_type_filter = params.filters.get("record_type").cloned();
        let active_filter = params.filters.get("is_active").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<ApprovalDefinition> = state
            .definitions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|definition| {
                if let Some(module) = &module_filter {
                    if definition.module != *module {
                        return false;
                    }
                }
                if let Some(record_type) = &record_type_filter {
                    if definition.record_type != *record_type {
                        return false;
                    }
                }
                if let Some(active) = &active_filter {
                    let expected = match active.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return false,
                    };
                    if definition.is_active != expected {
                        return false;
                    }
                }
                if let Some(company_id) = &company_filter {
                    if definition.company_id.as_deref() != Some(company_id.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {} {}",
                        definition.name,
                        definition.module,
                        definition.record_type,
                        definition.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|definition| definition.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_definition(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    body: web::Json<CreateDefinitionInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApprovalDefinition> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_MANAGE)?;
        let input = body.into_inner();
        let settings = state.settings();
        let module = require_text(&input.module, "module", MAX_MODULE_LEN)?;
        if !valid_record_key(&module, MAX_MODULE_LEN) {
            return Err(UnifiedError::bad_request(
                "module must be lowercase letters, digits, dots, underscores, or hyphens",
            ));
        }
        let record_type = require_text(&input.record_type, "record_type", MAX_RECORD_TYPE_LEN)?;
        if !valid_record_key(&record_type, MAX_RECORD_TYPE_LEN) {
            return Err(UnifiedError::bad_request(
                "record_type must be lowercase letters, digits, dots, underscores, or hyphens",
            ));
        }
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let timeout_hours = validate_timeout(input.timeout_hours, "timeout_hours")?;
        let escalation_extension_hours =
            validate_extension(input.escalation_extension_hours, "escalation_extension_hours")?;
        if !ESCALATION_ACTIONS.contains(&input.escalation_action.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown escalation action: {} (expected one of {})",
                input.escalation_action,
                ESCALATION_ACTIONS.join(", ")
            )));
        }
        if let Some(target) = &input.escalate_to {
            validate_approver(target)?;
        }
        let company_id = optional_text(&input.company_id, "company_id", MAX_COMPANY_ID_LEN)?;
        if input.steps.len() > settings.max_steps_per_definition {
            return Err(UnifiedError::bad_request(format!(
                "A definition holds at most {} steps",
                settings.max_steps_per_definition
            )));
        }
        for step in &input.steps {
            require_text(&step.name, "step name", MAX_NAME_LEN)?;
            if !STEP_MODES.contains(&step.mode.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown step mode: {} (expected one of {})",
                    step.mode,
                    STEP_MODES.join(", ")
                )));
            }
            validate_approvers(&step.approvers, settings.max_approvers_per_step)?;
        }
        if input.is_active
            && active_definition_for(&state, &module, &record_type).is_some()
        {
            return Err(UnifiedError::conflict(format!(
                "An active approval definition already exists for {module} / {record_type}"
            )));
        }
        let now = Utc::now();
        let definition = ApprovalDefinition {
            id: Uuid::new_v4(),
            module,
            record_type,
            name,
            description,
            is_active: input.is_active,
            timeout_hours,
            escalation_action: input.escalation_action,
            escalate_to: input.escalate_to,
            escalation_extension_hours,
            company_id,
            created_by: caller.actor,
            created_at: now,
            updated_at: now,
        };
        state.definitions.insert(definition.id, definition.clone());
        let mut position = 0;
        for step in input.steps {
            position += 1;
            let stored = ApprovalStep {
                id: Uuid::new_v4(),
                definition_id: definition.id,
                position,
                name: step.name.trim().to_string(),
                mode: step.mode,
                approvers: step.approvers,
                is_active: step.is_active,
                created_at: now,
            };
            state.steps.insert(stored.id, stored);
        }
        Ok(definition)
    })();
    match result {
        Ok(definition) => created(definition),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_definition(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_VIEW)?;
        let definition = load_definition(&state, &path.into_inner())?;
        let steps = steps_for_definition(&state, definition.id);
        Ok(json!({ "definition": definition, "steps": steps }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_definition(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateDefinitionInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApprovalDefinition> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_MANAGE)?;
        let input = body.into_inner();
        let mut definition = load_definition(&state, &path.into_inner())?;
        if let Some(name) = &input.name {
            definition.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if input.description.is_some() {
            definition.description =
                optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        }
        if let Some(active) = input.is_active {
            if active
                && !definition.is_active
                && active_definition_for(&state, &definition.module, &definition.record_type)
                    .is_some()
            {
                return Err(UnifiedError::conflict(format!(
                    "An active approval definition already exists for {} / {}",
                    definition.module, definition.record_type
                )));
            }
            definition.is_active = active;
        }
        if input.timeout_hours.is_some() {
            definition.timeout_hours = validate_timeout(input.timeout_hours, "timeout_hours")?;
        }
        if let Some(action) = &input.escalation_action {
            if !ESCALATION_ACTIONS.contains(&action.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown escalation action: {action} (expected one of {})",
                    ESCALATION_ACTIONS.join(", ")
                )));
            }
            definition.escalation_action = action.clone();
        }
        if let Some(target) = &input.escalate_to {
            validate_approver(target)?;
            definition.escalate_to = Some(target.clone());
        }
        if input.escalation_extension_hours.is_some() {
            definition.escalation_extension_hours = validate_extension(
                input.escalation_extension_hours,
                "escalation_extension_hours",
            )?;
        }
        if input.company_id.is_some() {
            definition.company_id = optional_text(&input.company_id, "company_id", MAX_COMPANY_ID_LEN)?;
        }
        definition.updated_at = Utc::now();
        state.definitions.insert(definition.id, definition.clone());
        Ok(definition)
    })();
    match result {
        Ok(definition) => ok(definition),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_definition(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_MANAGE)?;
        let definition = load_definition(&state, &path.into_inner())?;
        let open = state.requests.iter().any(|entry| {
            entry.value().definition_id == definition.id
                && OPEN_STATUSES.contains(&entry.value().status.as_str())
        });
        if open {
            return Err(UnifiedError::conflict(
                "Cannot delete a definition with open approval requests; disable it instead",
            ));
        }
        let step_ids: Vec<Uuid> = state
            .steps
            .iter()
            .filter(|entry| entry.value().definition_id == definition.id)
            .map(|entry| *entry.key())
            .collect();
        for step_id in step_ids {
            state.steps.remove(&step_id);
        }
        state.definitions.remove(&definition.id);
        Ok(json!({ "deleted": true, "id": definition.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn add_definition_step(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateStepInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApprovalStep> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_MANAGE)?;
        let input = body.into_inner();
        let settings = state.settings();
        let definition = load_definition(&state, &path.into_inner())?;
        let name = require_text(&input.name, "step name", MAX_NAME_LEN)?;
        if !STEP_MODES.contains(&input.mode.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown step mode: {} (expected one of {})",
                input.mode,
                STEP_MODES.join(", ")
            )));
        }
        validate_approvers(&input.approvers, settings.max_approvers_per_step)?;
        let existing = steps_for_definition(&state, definition.id);
        if existing.len() >= settings.max_steps_per_definition {
            return Err(UnifiedError::bad_request(format!(
                "A definition holds at most {} steps",
                settings.max_steps_per_definition
            )));
        }
        let position = existing
            .iter()
            .map(|step| step.position)
            .max()
            .unwrap_or(0)
            + 1;
        let step = ApprovalStep {
            id: Uuid::new_v4(),
            definition_id: definition.id,
            position,
            name,
            mode: input.mode,
            approvers: input.approvers,
            is_active: input.is_active,
            created_at: Utc::now(),
        };
        state.steps.insert(step.id, step.clone());
        Ok(step)
    })();
    match result {
        Ok(step) => created(step),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_definition_step(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_MANAGE)?;
        let (definition_id, step_id) = path.into_inner();
        let definition = load_definition(&state, &definition_id)?;
        let step_uuid = parse_uuid(&step_id, "step")?;
        let step = state
            .steps
            .get(&step_uuid)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Approval step not found: {step_id}")))?;
        if step.definition_id != definition.id {
            return Err(UnifiedError::not_found(format!(
                "Approval step not found: {step_id}"
            )));
        }
        let in_use = state.requests.iter().any(|entry| {
            entry.value().definition_id == definition.id
                && OPEN_STATUSES.contains(&entry.value().status.as_str())
                && entry.value().current_step_index
                    >= steps_for_definition(&state, definition.id)
                        .iter()
                        .position(|candidate| candidate.id == step.id)
                        .map(|index| index as i32)
                        .unwrap_or(i32::MAX)
        });
        if in_use {
            return Err(UnifiedError::conflict(
                "Cannot remove a step that open requests have reached or passed; disable the definition instead",
            ));
        }
        state.steps.remove(&step.id);
        Ok(json!({ "deleted": true, "id": step.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
