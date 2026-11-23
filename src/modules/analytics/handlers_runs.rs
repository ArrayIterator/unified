//! analytics - handlers runs.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{http::header, web, HttpRequest, HttpResponse};
use serde_json::{ json, Value };
use std::sync::Arc;
use super::{events::*, helpers::*, types::*, views::*, inputs::*, state::*, hooks::*, reports::*, handlers_dashboard::*, handlers_definitions::*, handlers_scheduled::*, module::*};



// ---------------------------------------------------------------------------
// Report runs and export.
// ---------------------------------------------------------------------------
pub(crate) async fn run_report_now(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnalyticsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<ReportRun> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        analytics_authorize(&ctx, &req, PERM_ANALYTICS_MANAGE)?;
        let definition = state.definition(path.as_str())?;
        let run = run_report(&state, &definition.id, &_scope)?;
        ctx.hooks.emit(
            ANALYTICS_REPORT_GENERATED,
            serde_json::to_value(&run).unwrap_or(Value::Null),
        );
        Ok(run)
    })();
    respond_created(result, false)
}


pub(crate) async fn list_runs(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnalyticsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let result: UnifiedResult<PageResult<ReportRun>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        analytics_authorize(&ctx, &req, PERM_ANALYTICS_VIEW)?;
        let definition = state.definition(path.as_str())?;
        let mut items: Vec<ReportRun> = state
            .runs
            .iter()
            .filter(|entry| entry.value().definition_id == definition.id)
            .map(|entry| entry.value().clone())
            .collect();
        items.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}


pub(crate) async fn get_run(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnalyticsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        analytics_authorize(&ctx, &req, PERM_ANALYTICS_VIEW)?;
        let run = state
            .runs
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Report run \"{}\" not found", path.as_str())))?;
        let page = paginate(run.rows.clone(), &params);
        Ok(json!({
            "id": run.id,
            "definition_id": run.definition_id,
            "status": run.status,
            "started_at": run.started_at,
            "finished_at": run.finished_at,
            "row_count": run.row_count,
            "error": run.error,
            "rows": page,
        }))
    })();
    json::respond(result, false)
}


pub(crate) fn escape_csv_cell(cell: &str) -> String {
    if cell.contains(',') || cell.contains('"') || cell.contains('\n') || cell.contains('\r') {
        format!("\"{}\"", cell.replace('"', "\"\""))
    } else {
        cell.to_string()
    }
}


pub(crate) fn rows_to_csv(rows: &[Value]) -> String {
    let mut headers: Vec<String> = Vec::new();
    for row in rows {
        if let Value::Object(map) = row {
            for key in map.keys() {
                if !headers.contains(key) {
                    headers.push(key.clone());
                }
            }
        }
    }
    headers.sort();
    let mut output = String::new();
    output.push_str(
        &headers
            .iter()
            .map(|header| escape_csv_cell(header))
            .collect::<Vec<String>>()
            .join(","),
    );
    output.push('\n');
    for row in rows {
        let cells: Vec<String> = headers
            .iter()
            .map(|header| {
                let cell = row.get(header).map(|value| match value {
                    Value::Null => String::new(),
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                });
                escape_csv_cell(&cell.unwrap_or_default())
            })
            .collect();
        output.push_str(&cells.join(","));
        output.push('\n');
    }
    output
}


pub(crate) async fn export_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AnalyticsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ExportQuery>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = analytics_authorize(&ctx, &req, PERM_ANALYTICS_VIEW) {
        return json::error_response(&error, ctx.is_debug());
    }
    let debug = ctx.is_debug();
    let query = query.into_inner();
    let format = query.format.unwrap_or_else(|| "json".to_string()).to_lowercase();
    if format != "json" && format != "csv" {
        return json::error_response(
            &UnifiedError::bad_request("format must be json or csv"),
            debug,
        );
    }
    let run = match query.run_id {
        Some(run_id) => state
            .runs
            .get(run_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Report run \"{run_id}\" not found"))),
        None => {
            let definition = match state.definition(path.as_str()) {
                Ok(definition) => definition,
                Err(error) => return json::error_response(&error, debug),
            };
            state
                .runs
                .iter()
                .filter(|entry| entry.value().definition_id == definition.id)
                .map(|entry| entry.value().clone())
                .max_by_key(|run| run.started_at)
                .ok_or_else(|| {
                    UnifiedError::not_found(format!(
                        "No runs exist for report \"{}\"; run it first",
                        definition.id
                    ))
                })
        }
    };
    let run = match run {
        Ok(run) => run,
        Err(error) => return json::error_response(&error, debug),
    };
    if run.definition_id != path.as_str() {
        return json::error_response(
            &UnifiedError::not_found(format!(
                "Report run \"{}\" does not belong to report \"{}\"",
                run.id,
                path.as_str()
            )),
            debug,
        );
    }
    if run.status != "completed" {
        return json::error_response(
            &UnifiedError::conflict(format!("Report run \"{}\" did not complete", run.id)),
            debug,
        );
    }
    // SANCTIONED CARVE-OUT from the standard API envelope (see
    // `crates/unified-core/src/json.rs`): a download, not an API
    // payload, so the body is the raw export rather than the
    // {"data": ...} envelope (same pattern as the logs export).
    // Errors before the download starts still use the envelope.
    let date = now().format("%Y%m%d");
    let (body, content_type, filename) = if format == "csv" {
        (
            rows_to_csv(&run.rows),
            "text/csv",
            format!("analytics-report-{}-{date}.csv", run.definition_id),
        )
    } else {
        (
            serde_json::to_string_pretty(&run.rows).unwrap_or_else(|_| "[]".to_string()),
            "application/json",
            format!("analytics-report-{}-{date}.json", run.definition_id),
        )
    };
    ctx.hooks.emit(
        ANALYTICS_REPORT_EXPORTED,
        json!({
            "definition_id": run.definition_id,
            "run_id": run.id,
            "format": format,
            "row_count": run.row_count,
            "filename": filename,
        }),
    );
    HttpResponse::Ok()
        .content_type(content_type)
        .insert_header((
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        ))
        .body(body)
}
