//! import export - handlers schedules.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;
use super::types::*;
use super::events::*;
use super::auth::*;
use super::entities::*;
use super::store::*;
use super::jobs::*;
use super::formats::*;
use super::validation::*;
use super::export::*;

pub(crate) async fn create_schedule(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateScheduleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExportSchedule> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE)?;
        let company_id = require_caller_company(&ctx, &_tenant, &req, &caller)?;
        let can_export_pii = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_EXPORT_PII);
        let config = settings();
        let input = body.into_inner();
        let name = require_text(&input.name, "name", 128)?;
        let spec = entity_spec(input.entity.trim()).ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Unknown entity \"{}\"; see GET /api/v1/import-export/entities",
                input.entity
            ))
        })?;
        let format = input.format.unwrap_or_else(|| "json".to_string());
        if !valid_format(&format, &config.allowed_formats) {
            return Err(UnifiedError::bad_request(format!(
                "format must be one of {}, got \"{format}\"",
                config.allowed_formats.join(", ")
            )));
        }
        let fields: Vec<String> = match &input.fields {
            Some(fields) => {
                validate_fields(spec, fields)?;
                fields.clone()
            }
            None => spec
                .columns
                .iter()
                .map(|column| column.name.to_string())
                .collect(),
        };
        let filters = input.filters.unwrap_or_default();
        for key in filters.keys() {
            if !spec.columns.iter().any(|column| column.name == key) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown filter \"{key}\" for entity \"{}\"",
                    spec.entity
                )));
            }
        }
        let schedule_text = input.schedule.trim().to_string();
        if !valid_schedule(&schedule_text) {
            return Err(UnifiedError::bad_request(
                "schedule must be an interval in seconds or five cron-like fields",
            ));
        }
        let redact_pii = input.redact_pii.unwrap_or(config.default_redact_pii);
        let redact_names = input.redact_names.unwrap_or(false);
        let enabled = input.enabled.unwrap_or(true);
        let now = Utc::now();
        let id = Uuid::now_v7();
        // A schedule that exports personal data unredacted is only
        // created by a caller who holds the PII capability now; the
        // flag is captured here and never widened by a run.
        let pii_allowed = can_export_pii && !redact_pii;
        let scheduling = if enabled {
            notify_scheduler(&ctx.hooks, id)
        } else {
            SchedulingOutcome {
                enqueued: false,
                message: "Manual-only schedule: the schedule is disabled.".to_string(),
            }
        };
        let schedule = ExportSchedule {
            id,
            name,
            entity: spec.entity.to_string(),
            format,
            fields,
            filters,
            redact_pii,
            redact_names,
            pii_allowed,
            schedule: schedule_text,
            enabled,
            company_id,
            actor: caller.actor.clone(),
            last_run_at: None,
            last_export_id: None,
            last_error: None,
            run_count: 0,
            scheduling,
            created_at: now,
            updated_at: now,
        };
        export_schedules_store().insert(id, schedule.clone());
        emit_audit(
            import_export_events::EXPORT_SCHEDULE_CREATED,
            &caller.actor,
            company_id,
            &format!("export_schedule:{id}"),
            serde_json::json!({
                "entity": spec.entity,
                "schedule": schedule.schedule,
                "enabled": enabled,
                "pii_redacted": redact_pii,
            }),
        );
        Ok(schedule)
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_schedules(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_READ)?;
        let company_id = require_caller_company(&ctx, &_tenant, &req, &caller)?;
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let mut items: Vec<ExportSchedule> = export_schedules_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        items = scope_visible(items, |item| item.company_id, company_id, can_manage);
        if let Some(entity) = params.filters.get("entity") {
            items.retain(|item| item.entity == *entity);
        }
        if let Some(enabled) = params.filters.get("enabled") {
            let wanted = enabled == "true";
            items.retain(|item| item.enabled == wanted);
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_schedule(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExportSchedule> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_READ)?;
        let company_id = require_caller_company(&ctx, &_tenant, &req, &caller)?;
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let schedule = export_schedules_store()
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Export schedule not found"))?;
        if !can_view_company(company_id, schedule.company_id, can_manage) {
            return Err(UnifiedError::not_found("Export schedule not found"));
        }
        Ok(schedule)
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_schedule(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<UpdateScheduleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let id = id.into_inner();
    let result: UnifiedResult<ExportSchedule> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE)?;
        let company_id = require_caller_company(&ctx, &_tenant, &req, &caller)?;
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let can_export_pii = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_EXPORT_PII);
        let config = settings();
        let input = body.into_inner();
        let stored = export_schedules_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Export schedule not found"))?;
        if !can_view_company(company_id, stored.company_id, can_manage) {
            return Err(UnifiedError::not_found("Export schedule not found"));
        }
        let spec = entity_spec(&stored.entity).ok_or_else(|| {
            UnifiedError::internal(format!(
                "Stored schedule references unknown entity \"{}\"",
                stored.entity
            ))
        })?;
        let mut updated = stored.clone();
        if let Some(name) = input.name {
            updated.name = require_text(&name, "name", 128)?;
        }
        if let Some(format) = input.format {
            if !valid_format(&format, &config.allowed_formats) {
                return Err(UnifiedError::bad_request(format!(
                    "format must be one of {}, got \"{format}\"",
                    config.allowed_formats.join(", ")
                )));
            }
            updated.format = format;
        }
        if let Some(fields) = input.fields {
            validate_fields(spec, &fields)?;
            updated.fields = fields;
        }
        if let Some(filters) = input.filters {
            for key in filters.keys() {
                if !spec.columns.iter().any(|column| column.name == key) {
                    return Err(UnifiedError::bad_request(format!(
                        "Unknown filter \"{key}\" for entity \"{}\"",
                        spec.entity
                    )));
                }
            }
            updated.filters = filters;
        }
        if let Some(redact_pii) = input.redact_pii {
            updated.redact_pii = redact_pii;
            // The PII flag is only widened by a caller who holds the
            // capability at this moment; turning redaction back on
            // always narrows it.
            if !redact_pii && can_export_pii {
                updated.pii_allowed = true;
            }
            if redact_pii {
                updated.pii_allowed = false;
            }
        }
        if let Some(redact_names) = input.redact_names {
            updated.redact_names = redact_names;
        }
        let mut reschedule = false;
        if let Some(schedule_text) = input.schedule {
            let schedule_text = schedule_text.trim().to_string();
            if !valid_schedule(&schedule_text) {
                return Err(UnifiedError::bad_request(
                    "schedule must be an interval in seconds or five cron-like fields",
                ));
            }
            if schedule_text != updated.schedule {
                reschedule = true;
            }
            updated.schedule = schedule_text;
        }
        if let Some(enabled) = input.enabled {
            if enabled != updated.enabled {
                reschedule = true;
            }
            updated.enabled = enabled;
        }
        if reschedule {
            updated.scheduling = if updated.enabled {
                notify_scheduler(&ctx.hooks, id)
            } else {
                SchedulingOutcome {
                    enqueued: false,
                    message: "Manual-only schedule: the schedule is disabled.".to_string(),
                }
            };
        }
        updated.updated_at = Utc::now();
        export_schedules_store().insert(id, updated.clone());
        emit_audit(
            import_export_events::EXPORT_SCHEDULE_UPDATED,
            &caller.actor,
            updated.company_id,
            &format!("export_schedule:{id}"),
            serde_json::json!({
                "entity": updated.entity,
                "schedule": updated.schedule,
                "enabled": updated.enabled,
            }),
        );
        Ok(updated)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_schedule(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let id = id.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE)?;
        let company_id = require_caller_company(&ctx, &_tenant, &req, &caller)?;
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let schedule = export_schedules_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Export schedule not found"))?;
        if !can_view_company(company_id, schedule.company_id, can_manage) {
            return Err(UnifiedError::not_found("Export schedule not found"));
        }
        export_schedules_store().remove(&id);
        emit_audit(
            import_export_events::EXPORT_SCHEDULE_DELETED,
            &caller.actor,
            schedule.company_id,
            &format!("export_schedule:{id}"),
            serde_json::json!({ "entity": schedule.entity, "name": schedule.name }),
        );
        Ok(serde_json::json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn run_schedule_now(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExportJob> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_EXPORT)?;
        let company_id = require_caller_company(&ctx, &_tenant, &req, &caller)?;
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let can_export_pii = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_EXPORT_PII);
        let schedule = export_schedules_store()
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Export schedule not found"))?;
        if !can_view_company(company_id, schedule.company_id, can_manage) {
            return Err(UnifiedError::not_found("Export schedule not found"));
        }
        // A run-now uses the caller's live PII capability when it is
        // wider than what was captured at creation time.
        let mut effective = schedule.clone();
        if can_export_pii && !schedule.redact_pii {
            effective.pii_allowed = true;
        }
        let spec = ExportSpec {
            entity: effective.entity.clone(),
            format: effective.format.clone(),
            fields: Some(effective.fields.clone()),
            filters: effective.filters.clone(),
            redact_pii: Some(effective.redact_pii),
            redact_names: Some(effective.redact_names),
            limit: None,
            offset: None,
        };
        let job = run_export(
            &caller.actor,
            schedule.company_id,
            effective.pii_allowed,
            &spec,
        )?;
        let now = Utc::now();
        if let Some(mut stored) = export_schedules_store().get_mut(&schedule.id) {
            stored.last_run_at = Some(now);
            stored.last_export_id = Some(job.id);
            stored.last_error = None;
            stored.run_count += 1;
            stored.updated_at = now;
        }
        emit_audit(
            import_export_events::EXPORT_SCHEDULE_RUN,
            &caller.actor,
            schedule.company_id,
            &format!("export_schedule:{}", schedule.id),
            serde_json::json!({
                "entity": schedule.entity,
                "export_id": job.id,
                "row_count": job.row_count,
                "via": "run-now",
            }),
        );
        Ok(job)
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn create_session(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateSessionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ImportSession> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_IMPORT)?;
        let company_id = require_caller_company(&ctx, &_tenant, &req, &caller)?;
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let config = settings();
        let input = body.into_inner();
        let spec = entity_spec(input.entity.trim()).ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Unknown entity \"{}\"; see GET /api/v1/import-export/entities",
                input.entity
            ))
        })?;
        let format = input.format.trim().to_lowercase();
        if !valid_format(&format, &config.allowed_formats) {
            return Err(UnifiedError::bad_request(format!(
                "format must be one of {}, got \"{format}\"",
                config.allowed_formats.join(", ")
            )));
        }
        if let Some(total_chunks) = input.total_chunks {
            if total_chunks == 0 {
                return Err(UnifiedError::bad_request(
                    "total_chunks must be at least 1 when provided",
                ));
            }
        }
        let mapping = resolve_mapping(
            spec.entity,
            input.mapping,
            input.mapping_id,
            company_id,
            can_manage,
        )?;
        validate_mapping(spec, &mapping)?;
        let now = Utc::now();
        let id = Uuid::now_v7();
        let session = ImportSession {
            id,
            entity: spec.entity.to_string(),
            format,
            status: "open".to_string(),
            total_chunks: input.total_chunks,
            received_chunks: Vec::new(),
            total_rows: 0,
            valid_rows: 0,
            invalid_rows: 0,
            row_errors: Vec::new(),
            mapping,
            actor: caller.actor.clone(),
            company_id,
            staged_rows: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        import_sessions_store().insert(id, session.clone());
        Ok(session)
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_sessions(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_READ)?;
        let company_id = require_caller_company(&ctx, &_tenant, &req, &caller)?;
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let mut items: Vec<ImportSession> = import_sessions_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        items = scope_visible(items, |item| item.company_id, company_id, can_manage);
        if let Some(entity) = params.filters.get("entity") {
            items.retain(|item| item.entity == *entity);
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|item| item.status == *status);
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_session(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ImportSession> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_READ)?;
        let company_id = require_caller_company(&ctx, &_tenant, &req, &caller)?;
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let session = import_sessions_store()
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Import session not found"))?;
        if !can_view_company(company_id, session.company_id, can_manage) {
            return Err(UnifiedError::not_found("Import session not found"));
        }
        Ok(session)
    })();
    json::respond(result, debug)
}

pub(crate) async fn append_chunk(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<AppendChunkRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let id = id.into_inner();
    let result: UnifiedResult<ImportSession> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_IMPORT)?;
        let company_id = require_caller_company(&ctx, &_tenant, &req, &caller)?;
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let config = settings();
        let input = body.into_inner();
        let mut session = import_sessions_store()
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Import session not found"))?;
        if !can_view_company(company_id, session.company_id, can_manage) {
            return Err(UnifiedError::not_found("Import session not found"));
        }
        if session.status != "open" {
            return Err(UnifiedError::conflict(format!(
                "Cannot append to a session in status \"{}\"",
                session.status
            )));
        }
        if let Some(total_chunks) = session.total_chunks {
            if input.chunk_index >= total_chunks {
                return Err(UnifiedError::bad_request(format!(
                    "chunk_index {} is out of range for total_chunks {total_chunks}",
                    input.chunk_index
                )));
            }
        }
        if session.received_chunks.contains(&input.chunk_index) {
            return Err(UnifiedError::conflict(format!(
                "Chunk {} was already received for this session",
                input.chunk_index
            )));
        }
        let spec = entity_spec(&session.entity).ok_or_else(|| {
            UnifiedError::internal(format!(
                "Stored session references unknown entity \"{}\"",
                session.entity
            ))
        })?;
        let text = decode_content(
            &input.content,
            input.encoding.as_deref(),
            config.max_chunk_bytes,
        )?;
        let raw_rows = content_to_raw_rows(spec, &session.format, &text, Some(&session.mapping))?;
        if raw_rows.len() > config.max_rows_per_file {
            return Err(UnifiedError::bad_request(format!(
                "Chunk has {} rows; the maximum per chunk is {}",
                raw_rows.len(),
                config.max_rows_per_file
            )));
        }
        if session.total_rows + raw_rows.len() > config.max_rows_per_session {
            return Err(UnifiedError::bad_request(format!(
                "Session would exceed the maximum of {} rows",
                config.max_rows_per_session
            )));
        }
        // Validate this chunk before accepting anything from it:
        // a chunk with invalid rows contributes its errors and its
        // valid rows, but nothing is written anywhere yet.
        let (valid_rows, mut chunk_errors) = validate_rows(spec, raw_rows, session.company_id);
        let chunk_total = valid_rows.len()
            + {
                let mut numbers: Vec<usize> =
                    chunk_errors.iter().map(|error| error.row).collect();
                numbers.sort_unstable();
                numbers.dedup();
                numbers.len()
            };
        // Row numbers in the error report are session-wide, not
        // chunk-local, so the report stays readable after completion.
        let row_offset = session.total_rows;
        for error in &mut chunk_errors {
            error.row += row_offset;
        }
        let mut invalid_row_numbers: Vec<usize> =
            chunk_errors.iter().map(|error| error.row).collect();
        invalid_row_numbers.sort_unstable();
        invalid_row_numbers.dedup();
        session.total_rows += chunk_total;
        session.valid_rows += valid_rows.len();
        session.invalid_rows += invalid_row_numbers.len();
        session.row_errors.append(&mut chunk_errors);
        session.staged_rows.extend(valid_rows);
        session.received_chunks.push(input.chunk_index);
        session.received_chunks.sort_unstable();
        session.updated_at = Utc::now();
        Ok(session.clone())
    })();
    json::respond(result, debug)
}

pub(crate) async fn complete_session(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let id = id.into_inner();
    let result: UnifiedResult<ImportJob> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_IMPORT)?;
        let company_id = require_caller_company(&ctx, &_tenant, &req, &caller)?;
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let mut session = import_sessions_store()
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Import session not found"))?;
        if !can_view_company(company_id, session.company_id, can_manage) {
            return Err(UnifiedError::not_found("Import session not found"));
        }
        if session.status != "open" {
            return Err(UnifiedError::conflict(format!(
                "Cannot complete a session in status \"{}\"",
                session.status
            )));
        }
        if session.received_chunks.is_empty() {
            return Err(UnifiedError::bad_request(
                "Session has no chunks yet; append at least one chunk first",
            ));
        }
        if let Some(total_chunks) = session.total_chunks {
            if session.received_chunks.len() != total_chunks {
                return Err(UnifiedError::bad_request(format!(
                    "Session declared {total_chunks} chunks but only {} were received",
                    session.received_chunks.len()
                )));
            }
        }
        let status = if session.invalid_rows == 0 {
            ImportStatus::Validated.as_str()
        } else {
            ImportStatus::Failed.as_str()
        };
        let now = Utc::now();
        let job_id = Uuid::now_v7();
        let job = ImportJob {
            id: job_id,
            entity: session.entity.clone(),
            format: session.format.clone(),
            status: status.to_string(),
            total_rows: session.total_rows,
            valid_rows: session.valid_rows,
            invalid_rows: session.invalid_rows,
            row_errors: session.row_errors.clone(),
            results: Vec::new(),
            actor: session.actor.clone(),
            company_id: session.company_id,
            staged_rows: session.staged_rows.clone(),
            created_at: now,
            updated_at: now,
        };
        session.status = status.to_string();
        session.updated_at = now;
        let session_status = session.status.clone();
        let session_entity = session.entity.clone();
        drop(session);
        let view = job.clone();
        import_jobs_store().insert(job_id, job);
        let resource = format!("import:{job_id}");
        emit_audit(
            import_export_events::IMPORT_SESSION_COMPLETED,
            &caller.actor,
            company_id,
            &resource,
            serde_json::json!({
                "entity": session_entity,
                "session_id": id,
                "session_status": session_status,
                "total_rows": view.total_rows,
                "valid_rows": view.valid_rows,
                "invalid_rows": view.invalid_rows,
            }),
        );
        emit_audit(
            import_export_events::IMPORT_CREATED,
            &caller.actor,
            company_id,
            &resource,
            serde_json::json!({
                "entity": session_entity,
                "format": view.format,
                "total_rows": view.total_rows,
                "valid_rows": view.valid_rows,
                "invalid_rows": view.invalid_rows,
            }),
        );
        Ok(view)
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}
