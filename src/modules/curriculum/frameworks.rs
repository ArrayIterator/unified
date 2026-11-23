//! Curriculum module — competency-based school curricula. — Framework handlers.
//!
//! Split from `src/modules/curriculum.rs`; behaviour unchanged.

use super::ai_capabilities::*;
use super::assessment_domains::*;
use super::basic_competencies::*;
use super::constants::*;
use super::core_competencies::*;
use super::grade_component_mappings::*;
use super::grade_descriptors::*;
use super::helpers::*;
use super::hooks::*;
use super::inputs::*;
use super::interventions::*;
use super::lesson_plans::*;
use super::levels::*;
use super::module::*;
use super::report_card_entries::*;
use super::report_cards::*;
use super::settings::*;
use super::state::*;
use super::structures::*;
use super::subjects::*;
use super::types::*;

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

pub(super) async fn list_frameworks(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let items: Vec<CurriculumFramework> =
            state.frameworks.iter().map(|entry| entry.value().clone()).collect();
        Ok(list_response(items, &query.into_inner()))
    })();
    json::respond(result, debug)
}

pub(super) async fn create_framework(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    body: web::Json<FrameworkInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let input = body.into_inner();
        let code = require_code(
            input.code.as_deref().unwrap_or(""),
            "code",
        )?;
        if state.frameworks.iter().any(|entry| entry.value().code == code) {
            return Err(UnifiedError::conflict(format!(
                "A framework with code \"{code}\" already exists"
            )));
        }
        let name = require_text(input.name.as_deref().unwrap_or(""), "name", MAX_NAME_LEN)?;
        let description = optional_text(&input.description, "description", MAX_TEXT_LEN)?
            .unwrap_or_default();
        let country =
            optional_text(&input.country, "country", MAX_NAME_LEN)?.unwrap_or_default();
        let framework = CurriculumFramework {
            id: Uuid::new_v4(),
            code: code.clone(),
            name,
            description,
            country,
            is_active: input.is_active.unwrap_or(true),
            is_built_in: false,
            created_at: now(),
            updated_at: now(),
        };
        state.frameworks.insert(framework.id, framework.clone());
        state.emit("curriculum.framework.created", json!({ "framework": framework }));
        Ok(json!(framework))
    })();
    json::respond(result, debug)
}

pub(super) async fn get_framework(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_VIEW)?;
        let id = parse_uuid(&path.into_inner(), "framework")?;
        Ok(json!(get_record(&state.frameworks, &id, "Framework")?))
    })();
    json::respond(result, debug)
}

pub(super) async fn update_framework(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<FrameworkInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "framework")?;
        let input = body.into_inner();
        let mut framework = get_record(&state.frameworks, &id, "Framework")?;
        if let Some(raw) = &input.code {
            let code = require_code(raw, "code")?;
            if state
                .frameworks
                .iter()
                .any(|entry| entry.value().code == code && entry.value().id != id)
            {
                return Err(UnifiedError::conflict(format!(
                    "A framework with code \"{code}\" already exists"
                )));
            }
            framework.code = code;
        }
        if let Some(raw) = &input.name {
            framework.name = require_text(raw, "name", MAX_NAME_LEN)?;
        }
        if let Some(raw) = &input.description {
            framework.description = require_text(raw, "description", MAX_TEXT_LEN)?;
        }
        if let Some(raw) = &input.country {
            framework.country = require_text(raw, "country", MAX_NAME_LEN)?;
        }
        if let Some(active) = input.is_active {
            framework.is_active = active;
        }
        framework.updated_at = now();
        state.frameworks.insert(framework.id, framework.clone());
        state.emit("curriculum.framework.updated", json!({ "framework": framework }));
        Ok(json!(framework))
    })();
    json::respond(result, debug)
}

pub(super) async fn delete_framework(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "framework")?;
        let framework = get_record(&state.frameworks, &id, "Framework")?;
        if framework.is_built_in {
            return Err(UnifiedError::conflict(
                "A built-in framework cannot be deleted",
            ));
        }
        let referenced = state.levels.iter().any(|entry| entry.value().framework_id == id)
            || state.subjects.iter().any(|entry| entry.value().framework_id == id)
            || state.core_competencies.iter().any(|entry| entry.value().framework_id == id)
            || state.basic_competencies.iter().any(|entry| entry.value().framework_id == id)
            || state.structures.iter().any(|entry| entry.value().framework_id == id)
            || state.grade_descriptors.iter().any(|entry| entry.value().framework_id == id)
            || state.lesson_plans.iter().any(|entry| entry.value().framework_id == id)
            || state.report_cards.iter().any(|entry| entry.value().framework_id == id);
        if referenced {
            return Err(UnifiedError::conflict(
                "Framework is still referenced by levels, subjects, competencies, structures, descriptors, lesson plans, or report cards",
            ));
        }
        state.frameworks.remove(&id);
        state.emit("curriculum.framework.deleted", json!({ "framework_id": id }));
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

pub(super) async fn set_default_framework(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "framework")?;
        let framework = get_record(&state.frameworks, &id, "Framework")?;
        if !framework.is_active {
            return Err(UnifiedError::conflict(
                "Only an active framework can be set as the default",
            ));
        }
        {
            let mut settings = state
                .settings
                .write()
                .expect("settings lock poisoned");
            settings.default_framework_code = framework.code.clone();
        }
        state.emit(
            "curriculum.framework.default_changed",
            json!({ "framework": framework }),
        );
        Ok(json!(framework))
    })();
    json::respond(result, debug)
}
