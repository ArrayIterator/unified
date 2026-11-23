//! import export - handlers.

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

pub(crate) async fn list_entities(ctx: web::Data<Arc<AppContext>>, req: HttpRequest) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_READ)?;
        let config = settings();
        Ok(serde_json::json!({
            "entities": ENTITY_SPECS,
            "max_file_bytes": config.max_file_bytes,
            "max_rows_per_file": config.max_rows_per_file,
            "max_chunk_bytes": config.max_chunk_bytes,
            "max_rows_per_session": config.max_rows_per_session,
            "max_export_rows": config.max_export_rows,
            "formats": config.allowed_formats,
            "default_redact_pii": config.default_redact_pii,
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_settings(ctx: web::Data<Arc<AppContext>>, req: HttpRequest) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ImportExportSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_READ)?;
        Ok(settings())
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ImportExportSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE)?;
        let input = body.into_inner();
        let mut updated = settings();
        if let Some(value) = input.max_file_bytes {
            if !(1_024..=100 * 1024 * 1024).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "max_file_bytes must be between 1024 and 104857600",
                ));
            }
            updated.max_file_bytes = value;
        }
        if let Some(value) = input.max_rows_per_file {
            if !(1..=100_000).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "max_rows_per_file must be between 1 and 100000",
                ));
            }
            updated.max_rows_per_file = value;
        }
        if let Some(value) = input.max_chunk_bytes {
            if !(1_024..=10 * 1024 * 1024).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "max_chunk_bytes must be between 1024 and 10485760",
                ));
            }
            updated.max_chunk_bytes = value;
        }
        if let Some(value) = input.max_rows_per_session {
            if !(1..=500_000).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "max_rows_per_session must be between 1 and 500000",
                ));
            }
            updated.max_rows_per_session = value;
        }
        if let Some(value) = input.max_export_rows {
            if !(1..=100_000).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "max_export_rows must be between 1 and 100000",
                ));
            }
            updated.max_export_rows = value;
        }
        if let Some(value) = input.max_export_bytes {
            if !(1_024 * 1_024..=500 * 1024 * 1024).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "max_export_bytes must be between 1048576 and 524288000",
                ));
            }
            updated.max_export_bytes = value;
        }
        if let Some(value) = input.default_redact_pii {
            updated.default_redact_pii = value;
        }
        if let Some(formats) = input.allowed_formats {
            if formats.is_empty() {
                return Err(UnifiedError::bad_request(
                    "allowed_formats must not be empty",
                ));
            }
            for format in &formats {
                if !["csv", "json", "xml"].contains(&format.as_str()) {
                    return Err(UnifiedError::bad_request(format!(
                        "Unknown format \"{format}\"; allowed formats are csv, json, xml"
                    )));
                }
            }
            updated.allowed_formats = formats;
        }
        if let Some(mut guard) = settings_store().write().ok() {
            *guard = updated.clone();
        }
        emit_audit(
            import_export_events::SETTINGS_UPDATED,
            &caller.actor,
            None,
            "import_export:settings",
            serde_json::json!({
                "max_file_bytes": updated.max_file_bytes,
                "max_rows_per_file": updated.max_rows_per_file,
                "max_export_rows": updated.max_export_rows,
                "default_redact_pii": updated.default_redact_pii,
            }),
        );
        Ok(updated)
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_export(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateExportRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExportJob> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_EXPORT)?;
        let can_export_pii = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_EXPORT_PII);
        let company_id = require_caller_company(&ctx, &_tenant, &req, &caller)?;
        let input = body.into_inner();
        let spec = ExportSpec {
            entity: input.entity,
            format: input.format.unwrap_or_else(|| "json".to_string()),
            fields: input.fields,
            filters: input.filters.unwrap_or_default(),
            redact_pii: input.redact_pii,
            redact_names: input.redact_names,
            limit: input.limit,
            offset: input.offset,
        };
        run_export(&caller.actor, company_id, can_export_pii, &spec)
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_exports(
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
        let mut items: Vec<ExportJob> = export_jobs_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        items = scope_visible(items, |job| job.company_id, company_id, can_manage);
        if let Some(entity) = params.filters.get("entity") {
            items.retain(|job| job.entity == *entity);
        }
        if let Some(format) = params.filters.get("format") {
            items.retain(|job| job.format == *format);
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|job| job.status == *status);
        }
        if let Some(company) = params.filters.get("company_id") {
            if can_manage || company_id.map(|id| id.to_string()) == Some(company.clone()) {
                let wanted = Uuid::parse_str(company).ok();
                items.retain(|job| job.company_id == wanted);
            }
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_export(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExportJob> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_READ)?;
        let company_id = require_caller_company(&ctx, &_tenant, &req, &caller)?;
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let job = export_jobs_store()
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Export job not found"))?;
        if !can_view_company(company_id, job.company_id, can_manage) {
            return Err(UnifiedError::not_found("Export job not found"));
        }
        Ok(job)
    })();
    json::respond(result, debug)
}

pub(crate) async fn download_export(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_EXPORT) {
        return json::error_response(&error, debug);
    }
    let company_id = match require_caller_company(&ctx, &_tenant, &req, &caller) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, debug),
    };
    let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
    let Some(job) = export_jobs_store()
        .get(&id.into_inner())
        .map(|entry| entry.value().clone())
    else {
        return json::error_response(&UnifiedError::not_found("Export job not found"), debug);
    };
    if !can_view_company(company_id, job.company_id, can_manage) {
        return json::error_response(&UnifiedError::not_found("Export job not found"), debug);
    }
    if job.status != ExportStatus::Completed.as_str() {
        return json::error_response(
            &UnifiedError::conflict(format!(
                "Export job is in status \"{}\"; nothing to download",
                job.status
            )),
            debug,
        );
    }
    let (content_type, filename) = if job.format == "csv" {
        ("text/csv; charset=utf-8", format!("{}-export-{}.csv", job.entity, job.id))
    } else if job.format == "xml" {
        ("application/xml; charset=utf-8", format!("{}-export-{}.xml", job.entity, job.id))
    } else {
        (
            "application/json; charset=utf-8",
            format!("{}-export-{}.json", job.entity, job.id),
        )
    };
    HttpResponse::Ok()
        .content_type(content_type)
        .insert_header((
            "Content-Disposition",
            format!("attachment; filename=\"{filename}\""),
        ))
        .body(job.content)
}

pub(crate) async fn create_import(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateImportRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ImportJob> = (|| {
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
        let mapping = resolve_mapping(
            spec.entity,
            input.mapping,
            input.mapping_id,
            company_id,
            can_manage,
        )?;
        validate_mapping(spec, &mapping)?;
        let text = decode_content(&input.content, input.encoding.as_deref(), config.max_file_bytes)?;
        let raw_rows = content_to_raw_rows(spec, &format, &text, Some(&mapping))?;
        if raw_rows.len() > config.max_rows_per_file {
            return Err(UnifiedError::bad_request(format!(
                "File has {} rows; the maximum is {}",
                raw_rows.len(),
                config.max_rows_per_file
            )));
        }
        let (valid_rows, row_errors) = validate_rows(spec, raw_rows, company_id);
        // Row errors are counted per invalid row, not per problem, so
        // a row with three bad columns is still one invalid row.
        let mut invalid_row_numbers: Vec<usize> = row_errors.iter().map(|error| error.row).collect();
        invalid_row_numbers.sort_unstable();
        invalid_row_numbers.dedup();
        let invalid_rows = invalid_row_numbers.len();
        let total = valid_rows.len() + invalid_rows;
        let now = Utc::now();
        let id = Uuid::now_v7();
        let status = if invalid_rows == 0 {
            ImportStatus::Validated.as_str()
        } else {
            ImportStatus::Failed.as_str()
        };
        let job = ImportJob {
            id,
            entity: spec.entity.to_string(),
            format,
            status: status.to_string(),
            total_rows: total,
            valid_rows: valid_rows.len(),
            invalid_rows,
            row_errors,
            results: Vec::new(),
            actor: caller.actor.clone(),
            company_id,
            staged_rows: valid_rows,
            created_at: now,
            updated_at: now,
        };
        let view = job.clone();
        import_jobs_store().insert(id, job);
        let resource = format!("import:{id}");
        emit_audit(
            import_export_events::IMPORT_CREATED,
            &caller.actor,
            company_id,
            &resource,
            serde_json::json!({
                "entity": spec.entity,
                "format": view.format,
                "total_rows": total,
                "valid_rows": view.valid_rows,
                "invalid_rows": invalid_rows,
            }),
        );
        if invalid_rows > 0 {
            emit_audit(
                import_export_events::IMPORT_FAILED,
                &caller.actor,
                company_id,
                &resource,
                serde_json::json!({
                    "entity": spec.entity,
                    "invalid_rows": invalid_rows,
                    "total_rows": total,
                }),
            );
        }
        Ok(view)
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_imports(
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
        let mut items: Vec<ImportJob> = import_jobs_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        items = scope_visible(items, |job| job.company_id, company_id, can_manage);
        if let Some(entity) = params.filters.get("entity") {
            items.retain(|job| job.entity == *entity);
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|job| job.status == *status);
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_import(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ImportJob> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_READ)?;
        let company_id = require_caller_company(&ctx, &_tenant, &req, &caller)?;
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let job = import_jobs_store()
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Import job not found"))?;
        if !can_view_company(company_id, job.company_id, can_manage) {
            return Err(UnifiedError::not_found("Import job not found"));
        }
        Ok(job)
    })();
    json::respond(result, debug)
}

pub(crate) async fn download_import_errors(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_READ) {
        return json::error_response(&error, debug);
    }
    let company_id = match require_caller_company(&ctx, &_tenant, &req, &caller) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, debug),
    };
    let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
    let Some(job) = import_jobs_store()
        .get(&id.into_inner())
        .map(|entry| entry.value().clone())
    else {
        return json::error_response(&UnifiedError::not_found("Import job not found"), debug);
    };
    if !can_view_company(company_id, job.company_id, can_manage) {
        return json::error_response(&UnifiedError::not_found("Import job not found"), debug);
    }
    let mut text = String::from("row,column,message\n");
    for error in &job.row_errors {
        text.push_str(&format!(
            "{},{},{}\n",
            error.row,
            escape_csv_field(error.column.as_deref().unwrap_or("")),
            escape_csv_field(&error.message),
        ));
    }
    HttpResponse::Ok()
        .content_type("text/csv; charset=utf-8")
        .insert_header((
            "Content-Disposition",
            format!(
                "attachment; filename=\"{}-import-{}-errors.csv\"",
                job.entity, job.id
            ),
        ))
        .body(text)
}

pub(crate) async fn confirm_import(
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
        let existing = import_jobs_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Import job not found"))?;
        if !can_view_company(company_id, existing.company_id, can_manage) {
            return Err(UnifiedError::not_found("Import job not found"));
        }
        let mut job = import_jobs_store()
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Import job not found"))?;
        if job.status != ImportStatus::Validated.as_str() {
            return Err(UnifiedError::conflict(format!(
                "Cannot confirm an import in status \"{}\"; only a validated import can be confirmed",
                job.status
            )));
        }
        // Idempotency: within one job, a repeated external id stages
        // the first row and skips later duplicates.
        let mut seen_external_ids: Vec<String> = Vec::new();
        let mut results: Vec<RowResult> = Vec::new();
        for (index, row) in job.staged_rows.iter().enumerate() {
            let external_id = row
                .get("external_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            if let Some(external_id) = &external_id {
                if seen_external_ids.contains(external_id) {
                    results.push(RowResult {
                        row: index + 1,
                        external_id: Some(external_id.clone()),
                        status: "skipped".to_string(),
                        message: Some("Duplicate external_id inside this file".to_string()),
                    });
                    continue;
                }
                seen_external_ids.push(external_id.clone());
            }
            results.push(RowResult {
                row: index + 1,
                external_id,
                status: "staged".to_string(),
                message: None,
            });
        }
        let staged_count = results
            .iter()
            .filter(|result| result.status == "staged")
            .count();
        job.results = results;
        job.status = ImportStatus::Staged.as_str().to_string();
        job.updated_at = Utc::now();
        let staged_rows = job.staged_rows.clone();
        let entity = job.entity.clone();
        let job_company = job.company_id;
        let view = job.clone();
        drop(job);
        let resource = format!("import:{id}");
        emit_audit(
            import_export_events::IMPORT_CONFIRMED,
            &caller.actor,
            job_company,
            &resource,
            serde_json::json!({
                "entity": entity,
                "staged_rows": staged_count,
                "total_rows": view.total_rows,
            }),
        );
        // Hand the staged rows to the owning module through the hook
        // bus. No owning module consumes them yet; see the wiring
        // snippet in the coordinator report.
        emit(
            import_export_events::IMPORT_COMPLETED,
            serde_json::json!({
                "import_id": id,
                "entity": entity,
                "staged_rows": staged_count,
                "rows": staged_rows,
                "actor": caller.actor,
                "actor_id": caller.actor,
                "resource": resource,
                "company_id": job_company.map(|id| id.to_string()),
            }),
        );
        Ok(view)
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_mapping(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateMappingRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MappingTemplate> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE)?;
        let company_id = require_caller_company(&ctx, &_tenant, &req, &caller)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", 128)?;
        let spec = entity_spec(input.entity.trim()).ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Unknown entity \"{}\"; see GET /api/v1/import-export/entities",
                input.entity
            ))
        })?;
        if input.mapping.is_empty() {
            return Err(UnifiedError::bad_request("mapping must not be empty"));
        }
        validate_mapping(spec, &input.mapping)?;
        let now = Utc::now();
        let id = Uuid::now_v7();
        let template = MappingTemplate {
            id,
            name,
            entity: spec.entity.to_string(),
            mapping: input.mapping,
            actor: caller.actor.clone(),
            company_id,
            created_at: now,
            updated_at: now,
        };
        mappings_store().insert(id, template.clone());
        emit_audit(
            import_export_events::MAPPING_CREATED,
            &caller.actor,
            company_id,
            &format!("mapping:{id}"),
            serde_json::json!({ "entity": spec.entity, "name": template.name }),
        );
        Ok(template)
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_mappings(
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
        let mut items: Vec<MappingTemplate> = mappings_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        items = scope_visible(items, |item| item.company_id, company_id, can_manage);
        if let Some(entity) = params.filters.get("entity") {
            items.retain(|item| item.entity == *entity);
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_mapping(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MappingTemplate> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_IMPORT_EXPORT_READ)?;
        let company_id = require_caller_company(&ctx, &_tenant, &req, &caller)?;
        let can_manage = check_capability(&ctx, &caller.role, PERM_IMPORT_EXPORT_MANAGE);
        let template = mappings_store()
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Mapping template not found"))?;
        if !can_view_company(company_id, template.company_id, can_manage) {
            return Err(UnifiedError::not_found("Mapping template not found"));
        }
        Ok(template)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_mapping(
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
        let template = mappings_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Mapping template not found"))?;
        if !can_view_company(company_id, template.company_id, can_manage) {
            return Err(UnifiedError::not_found("Mapping template not found"));
        }
        mappings_store().remove(&id);
        emit_audit(
            import_export_events::MAPPING_DELETED,
            &caller.actor,
            template.company_id,
            &format!("mapping:{id}"),
            serde_json::json!({ "entity": template.entity, "name": template.name }),
        );
        Ok(serde_json::json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}
