// admissions module — Application handlers.
//
// Split from the former single-file module `src/modules/admissions.rs`.
// The module root (`admissions.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::{MAX_SHORT, MAX_TEXT, PERM_ADMISSIONS_APPLY, PERM_ADMISSIONS_MANAGE, PERM_ADMISSIONS_READ, PERM_ADMISSIONS_READ_ALL};
use super::types::{Application, CreateApplication, UpdateApplication};
use super::state::AdmissionsState;
use super::helpers::{
    caller, caller_company, check_capability, load_application_for_read, matches_search, new_id,
    now, optional_text, paginate, require_id, require_permission, require_tenant, require_text,
    valid_email,
};

// ---------------------------------------------------------------------------
// Handlers: applications
// ---------------------------------------------------------------------------

pub(crate) async fn list_applications(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Application>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let params = query.into_inner();
        let read_all = check_capability(&ctx, &caller.role, PERM_ADMISSIONS_READ_ALL);
        let mut items: Vec<Application> = state
            .applications
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|application| {
                (read_all || application.applicant_user_id == caller.actor)
                    && matches_search(
                        &format!("{} {} {}", application.full_name, application.email, application.program_id),
                        &params,
                    )
                    && params
                        .filters
                        .get("period_id")
                        .is_none_or(|period| application.period_id == *period)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| application.status == *status)
                    && params
                        .filters
                        .get("program_id")
                        .is_none_or(|program| application.program_id == *program)
                    && params
                        .filters
                        .get("level")
                        .is_none_or(|level| application.level == *level)
                    && params
                        .filters
                        .get("applicant_user_id")
                        .is_none_or(|user| application.applicant_user_id == *user)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn create_application(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    body: web::Json<CreateApplication>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Application> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        let input = body.into_inner();
        let applicant_user_id = match input.applicant_user_id {
            Some(user_id) if !user_id.trim().is_empty() => {
                let user_id = require_id(&user_id, "applicant_user_id")?;
                if user_id != caller.actor
                    && !check_capability(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)
                {
                    return Err(UnifiedError::forbidden(
                        "Only admissions managers may create an application for another user",
                    ));
                }
                user_id
            }
            _ => {
                require_permission(&ctx, &caller.role, PERM_ADMISSIONS_APPLY)?;
                caller.actor.clone()
            }
        };
        let period = state.period(&input.period_id)?;
        if period.status == "archived" {
            return Err(UnifiedError::conflict(
                "Applications cannot be created for an archived period",
            ));
        }
        let full_name = require_text(&input.full_name, "full_name", MAX_TEXT)?;
        let email = valid_email(&input.email)?;
        let phone = optional_text(&input.phone, "phone", MAX_SHORT)?;
        let program_id = require_id(&input.program_id, "program_id")?;
        let level = require_text(&input.level, "level", MAX_SHORT)?;
        let form_submission_id = match input.form_submission_id {
            Some(value) if !value.trim().is_empty() => {
                Some(require_id(&value, "form_submission_id")?)
            }
            _ => None,
        };
        if state.applications.iter().any(|entry| {
            let application = entry.value();
            application.period_id == period.id
                && application.applicant_user_id == applicant_user_id
                && application.status != "withdrawn"
        }) {
            return Err(UnifiedError::conflict(
                "An active application already exists for this applicant in this period",
            ));
        }
        if let Some(submission_id) = &form_submission_id {
            if state.applications.iter().any(|entry| {
                entry
                    .value()
                    .form_submission_id
                    .as_deref()
                    == Some(submission_id.as_str())
            }) {
                return Err(UnifiedError::conflict(
                    "An application already exists for this form submission",
                ));
            }
        }
        let application = Application {
            id: new_id(),
            period_id: period.id.clone(),
            applicant_user_id,
            full_name,
            email,
            phone,
            program_id,
            level,
            form_submission_id,
            status: "draft".to_string(),
            total_score: None,
            fee_amount: period.registration_fee_amount,
            fee_currency: period.fee_currency.clone(),
            fee_invoice_status: "none".to_string(),
            fee_invoice_id: None,
            student_id: None,
            source: "manual".to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state.ensure_checklist(&application);
        state.applications.insert(application.id.clone(), application.clone());
        state.emit(
            "admissions.application_created",
            json!({
                "application_id": application.id,
                "period_id": application.period_id,
                "applicant_user_id": application.applicant_user_id,
                "source": "manual",
            }),
        );
        Ok(application)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_application(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Application> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        load_application_for_read(&state, &ctx, &caller, path.as_str())
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_application(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateApplication>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Application> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let input = body.into_inner();
        let application_id = path.as_str().to_string();
        let mut application = load_application_for_read(&state, &ctx, &caller, &application_id)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE);
        let is_owner = application.applicant_user_id == caller.actor
            && check_capability(&ctx, &caller.role, PERM_ADMISSIONS_APPLY);
        if !is_manager && !is_owner {
            return Err(UnifiedError::forbidden(
                "Only the applicant or an admissions manager may update this application",
            ));
        }
        if application.status != "draft" {
            return Err(UnifiedError::conflict(
                "Only a draft application can be updated",
            ));
        }
        if let Some(full_name) = input.full_name {
            application.full_name = require_text(&full_name, "full_name", MAX_TEXT)?;
        }
        if let Some(email) = input.email {
            application.email = valid_email(&email)?;
        }
        if let Some(phone) = input.phone {
            application.phone = match phone {
                Some(value) => Some(require_text(&value, "phone", MAX_SHORT)?),
                None => None,
            };
        }
        if let Some(program_id) = input.program_id {
            application.program_id = require_id(&program_id, "program_id")?;
        }
        if let Some(level) = input.level {
            application.level = require_text(&level, "level", MAX_SHORT)?;
        }
        application.updated_at = now();
        state
            .applications
            .insert(application.id.clone(), application.clone());
        Ok(application)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_application(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let application_id = path.as_str().to_string();
        let application = state.application(&application_id)?;
        if !matches!(application.status.as_str(), "draft" | "withdrawn" | "rejected") {
            return Err(UnifiedError::conflict(
                "Only a draft, withdrawn, or rejected application can be deleted",
            ));
        }
        state
            .scores
            .retain(|_, score| score.application_id != application_id);
        state
            .checklist
            .retain(|_, item| item.application_id != application_id);
        state
            .waitlist
            .retain(|_, entry| entry.application_id != application_id);
        state
            .results
            .retain(|_, result| result.application_id != application_id);
        state
            .applications
            .remove(application_id.as_str())
            .map(|_| json!({ "deleted": true, "id": application_id }))
            .ok_or_else(|| UnifiedError::not_found("Application not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn submit_application(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Application> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let application_id = path.as_str().to_string();
        let mut application = load_application_for_read(&state, &ctx, &caller, &application_id)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE);
        let is_owner = application.applicant_user_id == caller.actor
            && check_capability(&ctx, &caller.role, PERM_ADMISSIONS_APPLY);
        if !is_manager && !is_owner {
            return Err(UnifiedError::forbidden(
                "Only the applicant or an admissions manager may submit this application",
            ));
        }
        if application.status != "draft" {
            return Err(UnifiedError::conflict(
                "Only a draft application can be submitted",
            ));
        }
        if application.full_name.trim().is_empty()
            || application.email.trim().is_empty()
            || application.program_id.trim().is_empty()
            || application.level.trim().is_empty()
            || application.applicant_user_id.trim().is_empty()
        {
            return Err(UnifiedError::bad_request(
                "full_name, email, program_id, level, and applicant_user_id must be filled in before submitting",
            ));
        }
        let period = state.period(&application.period_id)?;
        if !state.period_accepts_submissions(&period) {
            return Err(UnifiedError::conflict(
                "Admission period is not open for submissions",
            ));
        }
        application.status = "submitted".to_string();
        application.updated_at = now();
        state
            .applications
            .insert(application.id.clone(), application.clone());
        state.emit(
            "admissions.application_submitted",
            json!({
                "application_id": application.id,
                "period_id": application.period_id,
            }),
        );
        Ok(application)
    })();
    json::respond(result, debug)
}

pub(crate) async fn withdraw_application(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let application_id = path.as_str().to_string();
        let mut application = load_application_for_read(&state, &ctx, &caller, &application_id)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE);
        let is_owner = application.applicant_user_id == caller.actor
            && check_capability(&ctx, &caller.role, PERM_ADMISSIONS_APPLY);
        if !is_manager && !is_owner {
            return Err(UnifiedError::forbidden(
                "Only the applicant or an admissions manager may withdraw this application",
            ));
        }
        if !matches!(
            application.status.as_str(),
            "submitted" | "under_review" | "waitlisted" | "accepted"
        ) {
            return Err(UnifiedError::conflict(format!(
                "An application in status \"{}\" cannot be withdrawn",
                application.status
            )));
        }
        let freed_seat = application.status == "accepted";
        let period_id = application.period_id.clone();
        application.status = "withdrawn".to_string();
        application.updated_at = now();
        state
            .applications
            .insert(application.id.clone(), application.clone());
        for mut entry in state.waitlist.iter_mut() {
            if entry.value().application_id == application_id
                && entry.value().status == "waiting"
            {
                entry.status = "expired".to_string();
                entry.updated_at = now();
            }
        }
        state.emit(
            "admissions.application_withdrawn",
            json!({
                "application_id": application.id,
                "period_id": application.period_id,
            }),
        );
        let mut promoted: Option<Application> = None;
        if freed_seat {
            promoted = state.promote_top_waitlisted(&period_id)?;
        }
        Ok(json!({
            "application": application,
            "waitlist_promoted": promoted,
        }))
    })();
    json::respond(result, debug)
}
