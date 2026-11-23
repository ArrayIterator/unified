//! Recruitment - HTTP handlers for applicants and the hiring pipeline.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
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
use super::{constants::*, types::*, state::*, helpers::*, module::*, handlers_jobs::*, handlers_interviews::*, handlers_evaluations::*, handlers_offers::*};

/// Build an applicant from a request body, shared by the internal
/// create endpoint and the public apply endpoint. Consent is
/// required for a public application: without it the application
/// is refused, because an applicant who has not consented cannot
/// be contacted at all.
pub(crate) fn build_applicant(
    state: &RecruitmentState,
    job: &Job,
    input: CreateApplicantRequest,
    public_apply: bool,
) -> UnifiedResult<Applicant> {
    let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
    let email = input.email.trim().to_lowercase();
    if !valid_email(&email) {
        return Err(UnifiedError::bad_request("email must be a valid address"));
    }
    let phone = match &input.phone {
        Some(raw) => Some(require_text(raw, "phone", MAX_NAME_LEN)?),
        None => None,
    };
    let source = match &input.source {
        Some(raw) => Some(require_text(raw, "source", MAX_SOURCE_LEN)?),
        None => None,
    };
    let resume_storage_target = match &input.resume_storage_target {
        Some(raw) => Some(require_text(raw, "resume_storage_target", MAX_STORAGE_KEY_LEN)?),
        None => None,
    };
    let resume_storage_key = match &input.resume_storage_key {
        Some(raw) => Some(require_text(raw, "resume_storage_key", MAX_STORAGE_KEY_LEN)?),
        None => None,
    };
    if resume_storage_target.is_some() != resume_storage_key.is_some() {
        return Err(UnifiedError::bad_request(
            "resume_storage_target and resume_storage_key must be given together",
        ));
    }
    let consent_contact = input.consent_contact.unwrap_or(false);
    if public_apply && !consent_contact {
        return Err(UnifiedError::bad_request(
            "Applying requires consent_contact: true so the applicant can be contacted",
        ));
    }
    let notes = match &input.notes {
        Some(raw) => Some(require_text(raw, "notes", MAX_NOTES_LEN)?),
        None => None,
    };
    if state.applicants.iter().any(|entry| {
        entry.value().job_id == job.id && entry.value().email == email
    }) {
        return Err(UnifiedError::conflict(format!(
            "An application from \"{email}\" already exists for this job"
        )));
    }
    let now = Utc::now();
    let applicant = Applicant {
        id: Uuid::now_v7(),
        job_id: job.id,
        name,
        email,
        phone,
        source,
        resume_storage_target,
        resume_storage_key,
        consent_contact,
        consent_at: if consent_contact { Some(now) } else { None },
        stage: ApplicantStage::Applied,
        notes,
        created_at: now,
        updated_at: now,
    };
    record_stage_move(
        state,
        &applicant,
        None,
        ApplicantStage::Applied,
        "system",
        None,
    );
    Ok(applicant)
}

pub(crate) async fn apply_public(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    path: web::Path<String>,
    body: web::Json<CreateApplicantRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let job = load_job(&state, &path.into_inner())?;
        if job.status != JobStatus::Published {
            return Err(UnifiedError::not_found(format!("Job not found: {}", job.id)));
        }
        let applicant = build_applicant(&state, &job, body.into_inner(), true)?;
        state.applicants.insert(applicant.id, applicant.clone());
        state.emit(
            "recruitment.applicant.created",
            json!({
                "applicant_id": applicant.id.to_string(),
                "job_id": job.id.to_string(),
                "source": "public_apply",
            }),
        );
        Ok(json!({
            "applicant_id": applicant.id,
            "job_id": job.id,
            "stage": applicant.stage.as_str(),
            "message": "Application received",
        }))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Applicant handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_job_applicants(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_VIEW)?;
        let job = load_job(&state, &path.into_inner())?;
        let params = query.into_inner();
        let stage_filter = params.filters.get("stage").cloned();
        let source_filter = params.filters.get("source").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Value> = state
            .applicants
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|applicant| {
                if applicant.job_id != job.id {
                    return false;
                }
                if let Some(stage) = &stage_filter {
                    if applicant.stage.as_str() != stage.as_str() {
                        return false;
                    }
                }
                if let Some(source) = &source_filter {
                    if applicant.source.as_deref() != Some(source.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!("{} {}", applicant.name, applicant.email).to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .map(|applicant| applicant_view(&state, &applicant))
            .collect();
        let sort = params.allowed_sort(&["name", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "name" => a
                    .get("name")
                    .and_then(Value::as_str)
                    .cmp(&b.get("name").and_then(Value::as_str)),
                _ => a
                    .get("created_at")
                    .and_then(Value::as_str)
                    .cmp(&b.get("created_at").and_then(Value::as_str)),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_applicant(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateApplicantRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let job = load_job(&state, &path.into_inner())?;
        if job.status == JobStatus::Closed {
            return Err(UnifiedError::conflict("A closed job takes no new applicants"));
        }
        let applicant = build_applicant(&state, &job, body.into_inner(), false)?;
        state.applicants.insert(applicant.id, applicant.clone());
        state.emit(
            "recruitment.applicant.created",
            json!({
                "applicant_id": applicant.id.to_string(),
                "job_id": job.id.to_string(),
                "actor": caller.actor,
            }),
        );
        Ok(applicant_view(&state, &applicant))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_applicant(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_VIEW)?;
        let applicant = load_applicant(&state, &path.into_inner())?;
        Ok(applicant_view(&state, &applicant))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_applicant(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateApplicantRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let mut applicant = load_applicant(&state, &path.into_inner())?;
        if applicant.stage == ApplicantStage::Hired {
            return Err(UnifiedError::conflict("A hired applicant cannot be edited here"));
        }
        let input = body.into_inner();
        if let Some(raw) = &input.name {
            applicant.name = require_text(raw, "name", MAX_NAME_LEN)?;
        }
        if let Some(raw) = &input.email {
            let email = raw.trim().to_lowercase();
            if !valid_email(&email) {
                return Err(UnifiedError::bad_request("email must be a valid address"));
            }
            if state.applicants.iter().any(|entry| {
                entry.value().job_id == applicant.job_id
                    && entry.value().email == email
                    && entry.value().id != applicant.id
            }) {
                return Err(UnifiedError::conflict(format!(
                    "An application from \"{email}\" already exists for this job"
                )));
            }
            applicant.email = email;
        }
        if let Some(raw) = &input.phone {
            applicant.phone = match raw {
                Some(value) => Some(require_text(value, "phone", MAX_NAME_LEN)?),
                None => None,
            };
        }
        if let Some(raw) = &input.source {
            applicant.source = match raw {
                Some(value) => Some(require_text(value, "source", MAX_SOURCE_LEN)?),
                None => None,
            };
        }
        if let Some(raw) = &input.resume_storage_target {
            applicant.resume_storage_target = match raw {
                Some(value) => Some(require_text(value, "resume_storage_target", MAX_STORAGE_KEY_LEN)?),
                None => None,
            };
        }
        if let Some(raw) = &input.resume_storage_key {
            applicant.resume_storage_key = match raw {
                Some(value) => Some(require_text(value, "resume_storage_key", MAX_STORAGE_KEY_LEN)?),
                None => None,
            };
        }
        if applicant.resume_storage_target.is_some() != applicant.resume_storage_key.is_some() {
            return Err(UnifiedError::bad_request(
                "resume_storage_target and resume_storage_key must be given together",
            ));
        }
        if let Some(consent) = input.consent_contact {
            applicant.consent_contact = consent;
            applicant.consent_at = if consent { Some(Utc::now()) } else { None };
        }
        if let Some(raw) = &input.notes {
            applicant.notes = match raw {
                Some(value) => Some(require_text(value, "notes", MAX_NOTES_LEN)?),
                None => None,
            };
        }
        applicant.updated_at = Utc::now();
        state.applicants.insert(applicant.id, applicant.clone());
        state.emit(
            "recruitment.applicant.updated",
            json!({
                "applicant_id": applicant.id.to_string(),
                "actor": caller.actor,
            }),
        );
        Ok(applicant_view(&state, &applicant))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_applicant(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let applicant = load_applicant(&state, &path.into_inner())?;
        if applicant.stage == ApplicantStage::Hired {
            return Err(UnifiedError::conflict(
                "A hired applicant cannot be deleted; the HR module owns the hired record",
            ));
        }
        if state
            .interviews
            .iter()
            .any(|entry| entry.value().applicant_id == applicant.id)
            || state
                .evaluations
                .iter()
                .any(|entry| entry.value().applicant_id == applicant.id)
            || state
                .offers
                .iter()
                .any(|entry| entry.value().applicant_id == applicant.id)
        {
            return Err(UnifiedError::conflict(
                "Applicant has interviews, evaluations, or offers and cannot be deleted; reject them instead",
            ));
        }
        state.applicants.remove(&applicant.id);
        state.emit(
            "recruitment.applicant.deleted",
            json!({
                "applicant_id": applicant.id.to_string(),
                "job_id": applicant.job_id.to_string(),
                "actor": caller.actor,
            }),
        );
        Ok(applicant_view(&state, &applicant))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn move_applicant(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<MoveApplicantRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let mut applicant = load_applicant(&state, &path.into_inner())?;
        let input = body.into_inner();
        let reason = match &input.reason {
            Some(raw) => Some(require_text(raw, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        if !allowed_stage_transition(applicant.stage, input.to_stage) {
            return Err(UnifiedError::conflict(format!(
                "Stage cannot move from {} to {}",
                applicant.stage.as_str(),
                input.to_stage.as_str()
            )));
        }
        if input.to_stage == ApplicantStage::Hired {
            return Err(UnifiedError::conflict(
                "Hiring goes through POST /applicants/{id}/hire, which requires an accepted offer"
            ));
        }
        let from_stage = applicant.stage;
        applicant.stage = input.to_stage;
        applicant.updated_at = Utc::now();
        state.applicants.insert(applicant.id, applicant.clone());
        record_stage_move(
            &state,
            &applicant,
            Some(from_stage),
            input.to_stage,
            &caller.actor,
            reason,
        );
        state.emit(
            "recruitment.applicant.moved",
            json!({
                "applicant_id": applicant.id.to_string(),
                "job_id": applicant.job_id.to_string(),
                "from_stage": from_stage.as_str(),
                "to_stage": input.to_stage.as_str(),
                "actor": caller.actor,
            }),
        );
        Ok(applicant_view(&state, &applicant))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_stage_history(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_VIEW)?;
        let applicant = load_applicant(&state, &path.into_inner())?;
        let params = query.into_inner();
        let mut items: Vec<ApplicantStageHistory> = state
            .stage_history
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.applicant_id == applicant.id)
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
