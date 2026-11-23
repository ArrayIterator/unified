// supervision module -- HTTP handlers for daily logs and photos.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::*;

pub(crate) async fn list_daily_logs(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        let params = query.into_inner();
        let project_filter = params.filters.get("project_id").cloned();
        let supervisor_filter = params.filters.get("supervisor_id").cloned();
        let date_filter = params.filters.get("date").cloned();
        let signed_only = params
            .filters
            .get("signed")
            .map(|raw| raw == "true")
            .unwrap_or(false);
        let mut items: Vec<DailyLog> = state
            .daily_logs
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|log| {
                if let Some(project_id) = &project_filter {
                    if &log.project_id != project_id {
                        return false;
                    }
                }
                if let Some(supervisor_id) = &supervisor_filter {
                    if &log.supervisor_id != supervisor_id {
                        return false;
                    }
                }
                if let Some(date) = &date_filter {
                    if log.log_date.to_string() != *date {
                        return false;
                    }
                }
                if signed_only && log.signed_at.is_none() {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by_key(|log| log.log_date);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn create_daily_log(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    body: web::Json<CreateDailyLogRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DailyLog> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_CREATE)?;
        let input = body.into_inner();
        let project_id = require_id(&input.project_id, "project_id")?;
        query_project(&ctx, &project_id)?;
        if input.workforce_count < 0 || input.workforce_count > 100_000 {
            return Err(UnifiedError::bad_request(
                "workforce_count must be between 0 and 100000",
            ));
        }
        // One log per project per day: a second log for the same
        // date would split the site record.
        let duplicate = state.daily_logs.iter().any(|entry| {
            entry.value().project_id == project_id && entry.value().log_date == input.log_date
        });
        if duplicate {
            return Err(UnifiedError::conflict(format!(
                "a daily log already exists for project {project_id} on {}",
                input.log_date
            )));
        }
        let weather = match &input.weather {
            Some(raw) => {
                let text = raw.trim();
                if text.len() > MAX_WEATHER_LEN {
                    return Err(UnifiedError::bad_request(
                        "weather must be at most 64 characters",
                    ));
                }
                if text.is_empty() {
                    None
                } else {
                    Some(text.to_string())
                }
            }
            None => None,
        };
        let now = Utc::now();
        let log = DailyLog {
            id: Uuid::now_v7(),
            project_id: project_id.clone(),
            log_date: input.log_date,
            supervisor_id: require_id(&input.supervisor_id, "supervisor_id")?,
            weather,
            workforce_count: input.workforce_count,
            work_done: opt_text(&input.work_done, "work_done", MAX_TEXT_LEN)?,
            materials_received: opt_text(
                &input.materials_received,
                "materials_received",
                MAX_TEXT_LEN,
            )?,
            incidents_on_site: opt_text(
                &input.incidents_on_site,
                "incidents_on_site",
                MAX_TEXT_LEN,
            )?,
            signed_by: None,
            signed_at: None,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.daily_logs.insert(log.id, log.clone());
        state.emit(
            "supervision.daily_log.created",
            json!({
                "log_id": log.id.to_string(),
                "project_id": log.project_id,
                "log_date": log.log_date.to_string(),
            }),
        );
        Ok(log)
    })();
    match result {
        Ok(log) => created(log),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn get_daily_log(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DailyLog> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        load_daily_log(&state, &path.into_inner())
    })();
    match result {
        Ok(log) => ok(log),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn update_daily_log(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateDailyLogRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DailyLog> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_MANAGE)?;
        let mut log = load_daily_log(&state, &path.into_inner())?;
        if log.signed_at.is_some() {
            return Err(UnifiedError::conflict(
                "a signed-off daily log cannot be edited",
            ));
        }
        let input = body.into_inner();
        if let Some(weather) = input.weather {
            log.weather = match weather {
                Some(raw) => {
                    if raw.trim().len() > MAX_WEATHER_LEN {
                        return Err(UnifiedError::bad_request(
                            "weather must be at most 64 characters",
                        ));
                    }
                    if raw.trim().is_empty() {
                        None
                    } else {
                        Some(raw.trim().to_string())
                    }
                }
                None => None,
            };
        }
        if let Some(count) = input.workforce_count {
            if count < 0 || count > 100_000 {
                return Err(UnifiedError::bad_request(
                    "workforce_count must be between 0 and 100000",
                ));
            }
            log.workforce_count = count;
        }
        if let Some(work_done) = input.work_done {
            log.work_done = match work_done {
                Some(raw) => Some(require_text(&raw, "work_done", MAX_TEXT_LEN)?),
                None => None,
            };
        }
        if let Some(materials) = input.materials_received {
            log.materials_received = match materials {
                Some(raw) => Some(require_text(&raw, "materials_received", MAX_TEXT_LEN)?),
                None => None,
            };
        }
        if let Some(incidents) = input.incidents_on_site {
            log.incidents_on_site = match incidents {
                Some(raw) => Some(require_text(&raw, "incidents_on_site", MAX_TEXT_LEN)?),
                None => None,
            };
        }
        log.updated_at = Utc::now();
        state.daily_logs.insert(log.id, log.clone());
        state.emit(
            "supervision.daily_log.updated",
            json!({ "log_id": log.id.to_string() }),
        );
        Ok(log)
    })();
    match result {
        Ok(log) => ok(log),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn sign_off_daily_log(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DailyLog> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_CREATE)?;
        let mut log = load_daily_log(&state, &path.into_inner())?;
        if log.signed_at.is_some() {
            return Err(UnifiedError::conflict(
                "this daily log is already signed off",
            ));
        }
        // Only the log's supervisor signs it off.
        if log.supervisor_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "only the log's supervisor can sign it off",
            ));
        }
        let now = Utc::now();
        log.signed_by = Some(caller.actor.clone());
        log.signed_at = Some(now);
        log.updated_at = now;
        state.daily_logs.insert(log.id, log.clone());
        state.emit(
            "supervision.daily_log.signed_off",
            json!({
                "log_id": log.id.to_string(),
                "project_id": log.project_id,
                "signed_by": log.signed_by,
            }),
        );
        Ok(log)
    })();
    match result {
        Ok(log) => ok(log),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Photo handlers
// ---------------------------------------------------------------------------


pub(crate) async fn list_photos(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        let params = query.into_inner();
        let project_filter = params.filters.get("project_id").cloned();
        let subject_kind_filter = params.filters.get("subject_kind").cloned();
        let subject_id_filter = params.filters.get("subject_id").cloned();
        let mut items: Vec<Photo> = state
            .photos
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|photo| {
                if let Some(project_id) = &project_filter {
                    if &photo.project_id != project_id {
                        return false;
                    }
                }
                if let Some(kind) = &subject_kind_filter {
                    if photo.subject_kind.as_str() != kind {
                        return false;
                    }
                }
                if let Some(subject_id) = &subject_id_filter {
                    if photo.subject_id.to_string() != *subject_id {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|photo| photo.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) fn subject_exists(state: &SupervisionState, kind: PhotoSubjectKind, subject_id: Uuid) -> bool {
    match kind {
        PhotoSubjectKind::Inspection => state.inspections.contains_key(&subject_id),
        PhotoSubjectKind::Defect => state.defects.contains_key(&subject_id),
        PhotoSubjectKind::DailyLog => state.daily_logs.contains_key(&subject_id),
        PhotoSubjectKind::ProgressReport => state.progress_reports.contains_key(&subject_id),
    }
}

pub(crate) async fn create_photo(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    body: web::Json<CreatePhotoRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Photo> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_CREATE)?;
        let input = body.into_inner();
        let project_id = require_id(&input.project_id, "project_id")?;
        query_project(&ctx, &project_id)?;
        if !subject_exists(&state, input.subject_kind, input.subject_id) {
            return Err(UnifiedError::not_found(format!(
                "{} not found: {}",
                input.subject_kind.as_str(),
                input.subject_id
            )));
        }
        let storage_target = require_text(
            &input.storage_target,
            "storage_target",
            MAX_STORAGE_TARGET_LEN,
        )?;
        let storage_key =
            require_text(&input.storage_key, "storage_key", MAX_STORAGE_KEY_LEN)?;
        let content_sha256 = valid_sha256(&input.content_sha256)?;
        let content_type = valid_content_type(&input.content_type)?;
        if input.size_bytes <= 0 || input.size_bytes > MAX_PHOTO_BYTES {
            return Err(UnifiedError::bad_request(format!(
                "size_bytes must be between 1 and {MAX_PHOTO_BYTES}"
            )));
        }
        let caption = opt_text(&input.caption, "caption", MAX_CAPTION_LEN)?;
        let now = Utc::now();
        let photo = Photo {
            id: Uuid::now_v7(),
            project_id: project_id.clone(),
            subject_kind: input.subject_kind,
            subject_id: input.subject_id,
            storage_target,
            storage_key,
            content_sha256,
            content_type,
            size_bytes: input.size_bytes,
            caption,
            uploaded_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.photos.insert(photo.id, photo.clone());
        state.emit(
            "supervision.photo.attached",
            json!({
                "photo_id": photo.id.to_string(),
                "project_id": photo.project_id,
                "subject_kind": photo.subject_kind.as_str(),
                "subject_id": photo.subject_id.to_string(),
                "storage_target": photo.storage_target,
                "storage_key": photo.storage_key,
            }),
        );
        Ok(photo)
    })();
    match result {
        Ok(photo) => created(photo),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn get_photo(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Photo> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        load_photo(&state, &path.into_inner())
    })();
    match result {
        Ok(photo) => ok(photo),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn delete_photo(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_MANAGE)?;
        let photo = load_photo(&state, &path.into_inner())?;
        // Deleting here removes only the supervision reference;
        // the stored bytes are owned by the storage module and are
        // not deleted by this call.
        state.photos.remove(&photo.id);
        state.emit(
            "supervision.photo.deleted",
            json!({
                "photo_id": photo.id.to_string(),
                "project_id": photo.project_id,
                "storage_target": photo.storage_target,
                "storage_key": photo.storage_key,
            }),
        );
        Ok(json!({ "deleted": true, "photo_id": photo.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Dashboard
// ---------------------------------------------------------------------------
