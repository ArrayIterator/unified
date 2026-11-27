//! Curriculum module - competency-based school curricula. - Assessment domain handlers.
//!
//! Split from `src/modules/curriculum.rs`; behaviour unchanged.

use super::ai_capabilities::*;
use super::basic_competencies::*;
use super::constants::*;
use super::core_competencies::*;
use super::frameworks::*;
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

pub(super) async fn list_assessment_domains(
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
        let items: Vec<AssessmentDomain> = state
            .assessment_domains
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        Ok(list_response(items, &query.into_inner()))
    })();
    json::respond(result, debug)
}

pub(super) async fn create_assessment_domain(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    body: web::Json<AssessmentDomainInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let input = body.into_inner();
        let code = require_code(input.code.as_deref().unwrap_or(""), "code")?;
        if state.assessment_domains.iter().any(|entry| entry.value().code == code) {
            return Err(UnifiedError::conflict(format!(
                "An assessment domain with code \"{code}\" already exists"
            )));
        }
        let parent_domain_id = optional_uuid(&input.parent_domain_id, "assessment domain")?;
        if let Some(parent_id) = parent_domain_id {
            let parent = get_record(&state.assessment_domains, &parent_id, "Assessment domain")?;
            if parent.parent_domain_id.is_some() {
                return Err(UnifiedError::bad_request(
                    "A sub-domain cannot have a parent that is itself a sub-domain (one nesting level only)",
                ));
            }
        }
        let domain = AssessmentDomain {
            id: Uuid::new_v4(),
            code,
            name: require_text(input.name.as_deref().unwrap_or(""), "name", MAX_NAME_LEN)?,
            parent_domain_id,
            description: optional_text(&input.description, "description", MAX_TEXT_LEN)?
                .unwrap_or_default(),
            sort_order: input.sort_order.unwrap_or(0),
            is_active: input.is_active.unwrap_or(true),
            created_at: now(),
            updated_at: now(),
        };
        state.assessment_domains.insert(domain.id, domain.clone());
        state.emit(
            "curriculum.assessment_domain.created",
            json!({ "assessment_domain": domain }),
        );
        Ok(json!(domain))
    })();
    json::respond(result, debug)
}

pub(super) async fn get_assessment_domain(
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
        let id = parse_uuid(&path.into_inner(), "assessment domain")?;
        Ok(json!(get_record(&state.assessment_domains, &id, "Assessment domain")?))
    })();
    json::respond(result, debug)
}

pub(super) async fn update_assessment_domain(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CurriculumState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AssessmentDomainInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CURRICULUM_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "assessment domain")?;
        let input = body.into_inner();
        let mut domain = get_record(&state.assessment_domains, &id, "Assessment domain")?;
        if let Some(raw) = &input.code {
            let code = require_code(raw, "code")?;
            if state
                .assessment_domains
                .iter()
                .any(|entry| entry.value().code == code && entry.value().id != id)
            {
                return Err(UnifiedError::conflict(format!(
                    "An assessment domain with code \"{code}\" already exists"
                )));
            }
            domain.code = code;
        }
        if let Some(raw) = &input.name {
            domain.name = require_text(raw, "name", MAX_NAME_LEN)?;
        }
        if let Some(raw) = &input.parent_domain_id {
            let parent_id = if raw.trim().is_empty() {
                None
            } else {
                Some(parse_uuid(raw.trim(), "assessment domain")?)
            };
            if let Some(parent_id) = parent_id {
                if parent_id == id {
                    return Err(UnifiedError::bad_request(
                        "An assessment domain cannot be its own parent",
                    ));
                }
                let parent =
                    get_record(&state.assessment_domains, &parent_id, "Assessment domain")?;
                if parent.parent_domain_id.is_some() {
                    return Err(UnifiedError::bad_request(
                        "A sub-domain cannot have a parent that is itself a sub-domain (one nesting level only)",
                    ));
                }
            }
            domain.parent_domain_id = parent_id;
        }
        if let Some(raw) = &input.description {
            domain.description = require_text(raw, "description", MAX_TEXT_LEN)?;
        }
        if let Some(order) = input.sort_order {
            domain.sort_order = order;
        }
        if let Some(active) = input.is_active {
            domain.is_active = active;
        }
        domain.updated_at = now();
        state.assessment_domains.insert(domain.id, domain.clone());
        state.emit(
            "curriculum.assessment_domain.updated",
            json!({ "assessment_domain": domain }),
        );
        Ok(json!(domain))
    })();
    json::respond(result, debug)
}

pub(super) async fn delete_assessment_domain(
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
        let id = parse_uuid(&path.into_inner(), "assessment domain")?;
        get_record(&state.assessment_domains, &id, "Assessment domain")?;
        let referenced = state
            .assessment_domains
            .iter()
            .any(|entry| entry.value().parent_domain_id == Some(id))
            || state
                .grade_descriptors
                .iter()
                .any(|entry| entry.value().domain_id == Some(id))
            || state
                .report_card_entries
                .iter()
                .any(|entry| entry.value().domain_id == id);
        if referenced {
            return Err(UnifiedError::conflict(
                "Assessment domain is still referenced by sub-domains, grade descriptors, or report-card entries",
            ));
        }
        state.assessment_domains.remove(&id);
        state.emit(
            "curriculum.assessment_domain.deleted",
            json!({ "assessment_domain_id": id }),
        );
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}
