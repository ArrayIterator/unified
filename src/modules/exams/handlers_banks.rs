//! exams - handlers banks.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::Arc;
use super::{constants::*, settings::*, types::*, module::*, helpers::*, inputs::*, handlers_questions::*, handlers_exams::*, handlers_attempts::*, handlers_grading::*, handlers_results::*, handlers_ai::*, handlers_settings::*, hooks::*};



// ---------------------------------------------------------------------------
// Handlers: question banks
// ---------------------------------------------------------------------------
pub(crate) async fn list_banks(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<QuestionBank>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<QuestionBank> = state
            .banks
            .iter()
            .filter(|entry| {
                let bank = entry.value();
                in_company_scope(&bank.company_id, &_scope)
                    && (params
                    .filters
                    .get("category")
                    .map(|v| &bank.category == v)
                    .unwrap_or(true))
                    && (params
                        .filters
                        .get("owner_id")
                        .map(|v| &bank.owner_id == v)
                        .unwrap_or(true))
                    && (params
                        .filters
                        .get("company_id")
                        .map(|v| bank.company_id.as_deref() == Some(v.as_str()))
                        .unwrap_or(true))
                    && matches_search(&format!("{} {}", bank.title, bank.description), &params.search)
            })
            .map(|entry| entry.value().clone())
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.order.as_deref() == Some("asc") {
            items.reverse();
        }
        Ok(page_of(items, &params))
    })();
    json::respond(result, debug)
}


pub(crate) async fn create_bank(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    body: web::Json<CreateBankInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<QuestionBank> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let settings = state.settings();
        let title = require_text(&body.title, "title", MAX_TITLE_LEN)?;
        let description = body
            .description
            .as_deref()
            .unwrap_or("")
            .trim()
            .to_string();
        if description.len() > MAX_DESCRIPTION_LEN {
            return Err(UnifiedError::bad_request(
                "description must be at most 5000 characters",
            ));
        }
        let category = require_in_list(&body.category, "category", &settings.categories)?;
        let company_id = require_company_match(&_tenant, &req, &optional_text(&body.company_id, "company_id", MAX_SLUG_LEN)?)?;
        let bank = QuestionBank {
            id: new_id(),
            title,
            description,
            category,
            owner_id: caller.actor,
            company_id,
            created_at: now(),
            updated_at: now(),
        };
        state.banks.insert(bank.id.clone(), bank.clone());
        Ok(bank)
    })();
    json::respond(result, debug)
}


pub(crate) async fn get_bank(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<QuestionBank> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        require_bank_by_id_in_scope(&state, path.as_str(), &_scope)
    })();
    json::respond(result, debug)
}


pub(crate) async fn update_bank(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateBankInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<QuestionBank> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let settings = state.settings();
        let mut bank = require_bank_by_id_in_scope(&state, path.as_str(), &_scope)?;
        if let Some(title) = &body.title {
            bank.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = &body.description {
            if description.len() > MAX_DESCRIPTION_LEN {
                return Err(UnifiedError::bad_request(
                    "description must be at most 5000 characters",
                ));
            }
            bank.description = description.trim().to_string();
        }
        if let Some(category) = &body.category {
            bank.category = require_in_list(category, "category", &settings.categories)?;
        }
        bank.updated_at = now();
        state.banks.insert(bank.id.clone(), bank.clone());
        Ok(bank)
    })();
    json::respond(result, debug)
}


pub(crate) async fn delete_bank(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let bank = require_bank_by_id_in_scope(&state, path.as_str(), &_scope)?;
        if state
            .questions
            .iter()
            .any(|entry| entry.value().bank_id == bank.id)
        {
            return Err(UnifiedError::conflict(
                "Question bank still holds questions; delete or move them first",
            ));
        }
        state.banks.remove(&bank.id);
        Ok(json!({ "deleted": true, "id": bank.id }))
    })();
    json::respond(result, debug)
}
