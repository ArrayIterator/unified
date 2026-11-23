//! Recruitment - HTTP handlers for jobs, including the public job board.

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
use super::{constants::*, types::*, state::*, helpers::*, module::*, handlers_applicants::*, handlers_interviews::*, handlers_evaluations::*, handlers_offers::*};

// ---------------------------------------------------------------------------
// Job handlers
// ---------------------------------------------------------------------------

pub(crate) fn public_job_view(job: &Job) -> Value {
    json!({
        "id": job.id,
        "title": job.title,
        "description": job.description,
        "department_id": job.department_id,
        "location": job.location,
        "job_type": job.job_type.as_str(),
        "salary_min": job.salary_min,
        "salary_max": job.salary_max,
        "currency": job.currency,
        "status": job.status.as_str(),
        "created_at": job.created_at,
        "updated_at": job.updated_at,
    })
}

pub(crate) async fn list_jobs(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let type_filter = params.filters.get("type").cloned();
        let department_filter = params.filters.get("department_id").cloned();
        let location_filter = params.filters.get("location").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Job> = state
            .jobs
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|job| {
                if let Some(status) = &status_filter {
                    if job.status.as_str() != status.as_str() {
                        return false;
                    }
                }
                if let Some(job_type) = &type_filter {
                    if job.job_type.as_str() != job_type.as_str() {
                        return false;
                    }
                }
                if let Some(department) = &department_filter {
                    if job.department_id.as_deref() != Some(department.as_str()) {
                        return false;
                    }
                }
                if let Some(location) = &location_filter {
                    if job.location.as_deref() != Some(location.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {}",
                        job.title,
                        job.description.clone().unwrap_or_default(),
                        job.location.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["title", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "title" => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
                _ => a.created_at.cmp(&b.created_at),
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

pub(crate) async fn create_job(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    body: web::Json<CreateJobRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Job> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let department_id = match &input.department_id {
            Some(raw) => Some(require_text(raw, "department_id", MAX_NAME_LEN)?),
            None => None,
        };
        let location = match &input.location {
            Some(raw) => Some(require_text(raw, "location", MAX_LOCATION_LEN)?),
            None => None,
        };
        let (salary_min, salary_max) =
            validate_salary_range(input.salary_min, input.salary_max)?;
        let currency = match &input.currency {
            Some(raw) => valid_currency(raw)?,
            None => "USD".to_string(),
        };
        let now = Utc::now();
        let job = Job {
            id: Uuid::now_v7(),
            title,
            description,
            department_id,
            location,
            job_type: input.job_type.unwrap_or_default(),
            salary_min,
            salary_max,
            currency,
            status: JobStatus::Draft,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.jobs.insert(job.id, job.clone());
        state.emit(
            "recruitment.job.created",
            json!({
                "job_id": job.id.to_string(),
                "title": job.title,
                "actor": caller.actor,
            }),
        );
        Ok(job)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_job(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Job> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_VIEW)?;
        load_job(&state, &path.into_inner())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_job(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateJobRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Job> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let mut job = load_job(&state, &path.into_inner())?;
        if job.status == JobStatus::Closed {
            return Err(UnifiedError::conflict("A closed job cannot be edited"));
        }
        let input = body.into_inner();
        if let Some(raw) = &input.title {
            job.title = require_text(raw, "title", MAX_TITLE_LEN)?;
        }
        if let Some(raw) = &input.description {
            job.description = match raw {
                Some(value) => Some(require_text(value, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(raw) = &input.department_id {
            job.department_id = match raw {
                Some(value) => Some(require_text(value, "department_id", MAX_NAME_LEN)?),
                None => None,
            };
        }
        if let Some(raw) = &input.location {
            job.location = match raw {
                Some(value) => Some(require_text(value, "location", MAX_LOCATION_LEN)?),
                None => None,
            };
        }
        if let Some(job_type) = input.job_type {
            job.job_type = job_type;
        }
        if input.salary_min.is_some() || input.salary_max.is_some() {
            let next_min = input.salary_min.unwrap_or(job.salary_min);
            let next_max = input.salary_max.unwrap_or(job.salary_max);
            let (salary_min, salary_max) = validate_salary_range(next_min, next_max)?;
            job.salary_min = salary_min;
            job.salary_max = salary_max;
        }
        if let Some(raw) = &input.currency {
            job.currency = valid_currency(raw)?;
        }
        job.updated_at = Utc::now();
        state.jobs.insert(job.id, job.clone());
        state.emit(
            "recruitment.job.updated",
            json!({
                "job_id": job.id.to_string(),
                "actor": caller.actor,
            }),
        );
        Ok(job)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_job(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Job> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let job = load_job(&state, &path.into_inner())?;
        if job.status != JobStatus::Draft {
            return Err(UnifiedError::conflict(
                "Only a draft job can be deleted; close a published job instead",
            ));
        }
        if state
            .applicants
            .iter()
            .any(|entry| entry.value().job_id == job.id)
        {
            return Err(UnifiedError::conflict(
                "Job has applicants and cannot be deleted",
            ));
        }
        state.jobs.remove(&job.id);
        state.emit(
            "recruitment.job.deleted",
            json!({
                "job_id": job.id.to_string(),
                "actor": caller.actor,
            }),
        );
        Ok(job)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn publish_job(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Job> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let mut job = load_job(&state, &path.into_inner())?;
        if job.status != JobStatus::Draft {
            return Err(UnifiedError::conflict(format!(
                "Only a draft job can be published (job is {})",
                job.status.as_str()
            )));
        }
        job.status = JobStatus::Published;
        job.updated_at = Utc::now();
        state.jobs.insert(job.id, job.clone());
        state.emit(
            "recruitment.job.published",
            json!({
                "job_id": job.id.to_string(),
                "title": job.title,
                "actor": caller.actor,
            }),
        );
        Ok(job)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn close_job(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Job> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let mut job = load_job(&state, &path.into_inner())?;
        if job.status != JobStatus::Published {
            return Err(UnifiedError::conflict(format!(
                "Only a published job can be closed (job is {})",
                job.status.as_str()
            )));
        }
        job.status = JobStatus::Closed;
        job.updated_at = Utc::now();
        state.jobs.insert(job.id, job.clone());
        state.emit(
            "recruitment.job.closed",
            json!({
                "job_id": job.id.to_string(),
                "title": job.title,
                "actor": caller.actor,
            }),
        );
        Ok(job)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Public job handlers (no auth; gated by the API Manager
// ApiAccessRule for these paths -- see the module header).
// ---------------------------------------------------------------------------

pub(crate) async fn list_public_jobs(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let params = query.into_inner();
        let type_filter = params.filters.get("type").cloned();
        let department_filter = params.filters.get("department_id").cloned();
        let location_filter = params.filters.get("location").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Value> = state
            .jobs
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|job| job.status == JobStatus::Published)
            .filter(|job| {
                if let Some(job_type) = &type_filter {
                    if job.job_type.as_str() != job_type.as_str() {
                        return false;
                    }
                }
                if let Some(department) = &department_filter {
                    if job.department_id.as_deref() != Some(department.as_str()) {
                        return false;
                    }
                }
                if let Some(location) = &location_filter {
                    if job.location.as_deref() != Some(location.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {}",
                        job.title,
                        job.description.clone().unwrap_or_default(),
                        job.location.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .map(|job| public_job_view(&job))
            .collect();
        items.sort_by(|a, b| {
            b.get("created_at")
                .and_then(Value::as_str)
                .cmp(&a.get("created_at").and_then(Value::as_str))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_public_job(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let job = load_job(&state, &path.into_inner())?;
        if job.status != JobStatus::Published {
            return Err(UnifiedError::not_found(format!("Job not found: {}", job.id)));
        }
        Ok(public_job_view(&job))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
