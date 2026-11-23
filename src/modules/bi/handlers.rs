// bi module -- HTTP handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Report handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_reports(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_VIEW)?;
        let caller_company = _scope.clone();
        let params = query.into_inner();
        let source_filter = params.filters.get("source").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let visibility_filter = params.filters.get("visibility").cloned();
        let owner_filter = params.filters.get("owner_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<BiReport> = state
            .reports
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|report| {
                // Tenant scope: a company caller only ever sees
                // their own company's reports, an unresolved caller
                // only global (empty-company) reports. The
                // caller-supplied `company_id` filter below can only
                // narrow further, never widen past this scope.
                if !in_company_scope_str(&report.company_id, &caller_company) {
                    return false;
                }
                if let Some(source) = &source_filter {
                    if &report.source != source {
                        return false;
                    }
                }
                if let Some(company) = &company_filter {
                    if &report.company_id != company {
                        return false;
                    }
                }
                if let Some(visibility) = &visibility_filter {
                    if &report.visibility != visibility {
                        return false;
                    }
                }
                if let Some(owner) = &owner_filter {
                    if &report.owner_id != owner {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {}",
                        report.name,
                        report.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["name", "source", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "name" => a.name.cmp(&b.name),
                "source" => a.source.cmp(&b.source),
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


pub(crate) async fn create_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    body: web::Json<CreateReportRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BiReport> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_MANAGE)?;
        let name = require_text(&body.name, "name", MAX_NAME_LEN)?;
        let company_id = require_text(&body.company_id, "company_id", MAX_COMPANY_LEN)?;
        // Tenant scope: a company caller may only create reports
        // for their own company, never for a company id supplied
        // in the request body. An unresolved caller (no company)
        // may not claim a company at all (the shared
        // `CallerScope::Unresolved` rule).
        let company_id =
            require_company_match(&_tenant, &req, &Some(company_id))?.unwrap_or_default();
        let source = validate_source(&body.source)?;
        let fields = validate_fields(&source, &body.fields)?;
        let filters = validate_filters(&body.filters)?;
        let group_by = match &body.group_by {
            Some(group) => {
                let group = group.trim().to_lowercase();
                if group.len() > MAX_GROUP_LEN {
                    return Err(UnifiedError::bad_request(
                        "group_by must be at most 128 characters",
                    ));
                }
                if !allowed_fields(&source).contains(&group.as_str()) {
                    return Err(UnifiedError::bad_request(format!(
                        "group_by \"{group}\" is not available for source \"{source}\""
                    )));
                }
                Some(group)
            }
            None => None,
        };
        let chart_type = validate_chart_type(body.chart_type.as_deref().unwrap_or("table"))?;
        let visibility = match body.visibility.as_deref().unwrap_or("private") {
            "shared" => "shared".to_string(),
            "private" => "private".to_string(),
            other => {
                return Err(UnifiedError::bad_request(format!(
                    "visibility must be \"shared\" or \"private\", got \"{other}\""
                )))
            }
        };
        let description = match &body.description {
            Some(text) if !text.trim().is_empty() => Some(require_text(
                text,
                "description",
                MAX_DESCRIPTION_LEN,
            )?),
            _ => None,
        };
        let now = Utc::now();
        let report = BiReport {
            id: Uuid::new_v4(),
            company_id: company_id.clone(),
            owner_id: caller.actor.clone(),
            name,
            description,
            source: source.clone(),
            fields,
            filters,
            group_by,
            chart_type,
            visibility,
            created_at: now,
            updated_at: now,
        };
        state.reports.insert(report.id, report.clone());
        state.emit(
            "bi.report.created",
            json!({
                "report_id": report.id.to_string(),
                "company_id": company_id,
                "source": source,
                "owner_id": caller.actor,
            }),
        );
        Ok(report)
    })();
    match result {
        Ok(report) => created(report),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn get_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BiReport> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_VIEW)?;
        let caller_company = _scope.clone();
        let report = load_report(&state, &path.into_inner())?;
        if !can_read_report(&report, &caller.actor, caller_company.as_deref())
            && !check_capability(&ctx, &caller.role, PERM_BI_MANAGE)
        {
            return Err(UnifiedError::forbidden(
                "Report is private to its owner",
            ));
        }
        Ok(report)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn update_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateReportRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BiReport> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_MANAGE)?;
        let mut report = load_report(&state, &path.into_inner())?;
        // Tenant scope: only the report's owner, or a manage-capable
        // caller inside the report's company, may change it.
        let can_manage = check_capability(&ctx, &caller.role, PERM_BI_MANAGE);
        if report.owner_id != caller.actor
            && !can_access_company(_scope.as_deref(), &report.company_id, can_manage)
        {
            return Err(UnifiedError::forbidden(
                "Report belongs to another company",
            ));
        }
        if let Some(name) = &body.name {
            report.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = &body.description {
            report.description = match description {
                Some(text) if !text.trim().is_empty() => Some(require_text(
                    text,
                    "description",
                    MAX_DESCRIPTION_LEN,
                )?),
                _ => None,
            };
        }
        if let Some(source) = &body.source {
            let source = validate_source(source)?;
            if let Some(fields) = &body.fields {
                report.fields = validate_fields(&source, fields)?;
            } else {
                report.fields = validate_fields(&source, &report.fields)?;
            }
            if let Some(group_by) = &report.group_by {
                if !allowed_fields(&source).contains(&group_by.as_str()) {
                    return Err(UnifiedError::bad_request(format!(
                        "group_by \"{group_by}\" is not available for source \"{source}\""
                    )));
                }
            }
            report.source = source;
        } else if let Some(fields) = &body.fields {
            report.fields = validate_fields(&report.source, fields)?;
        }
        if let Some(filters) = &body.filters {
            report.filters = validate_filters(filters)?;
        }
        if let Some(group_by) = &body.group_by {
            report.group_by = match group_by {
                Some(group) => {
                    let group = group.trim().to_lowercase();
                    if !allowed_fields(&report.source).contains(&group.as_str()) {
                        return Err(UnifiedError::bad_request(format!(
                            "group_by \"{group}\" is not available for source \"{}\"",
                            report.source
                        )));
                    }
                    Some(group)
                }
                None => None,
            };
        }
        if let Some(chart_type) = &body.chart_type {
            report.chart_type = validate_chart_type(chart_type)?;
        }
        if let Some(visibility) = &body.visibility {
            report.visibility = match visibility.as_str() {
                "shared" => "shared".to_string(),
                "private" => "private".to_string(),
                other => {
                    return Err(UnifiedError::bad_request(format!(
                        "visibility must be \"shared\" or \"private\", got \"{other}\""
                    )))
                }
            };
        }
        report.updated_at = Utc::now();
        state.reports.insert(report.id, report.clone());
        state.emit(
            "bi.report.updated",
            json!({
                "report_id": report.id.to_string(),
                "company_id": report.company_id,
                "actor_id": caller.actor,
            }),
        );
        Ok(report)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn delete_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_MANAGE)?;
        let report = load_report(&state, &path.into_inner())?;
        // Tenant scope: only the report's owner, or a manage-capable
        // caller inside the report's company, may delete it.
        let can_manage = check_capability(&ctx, &caller.role, PERM_BI_MANAGE);
        if report.owner_id != caller.actor
            && !can_access_company(_scope.as_deref(), &report.company_id, can_manage)
        {
            return Err(UnifiedError::forbidden(
                "Report belongs to another company",
            ));
        }
        state.reports.remove(&report.id);
        state
            .schedules
            .retain(|_, schedule| schedule.report_id != report.id);
        state.emit(
            "bi.report.deleted",
            json!({
                "report_id": report.id.to_string(),
                "company_id": report.company_id,
                "actor_id": caller.actor,
            }),
        );
        Ok(json!({ "deleted": report.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn run_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_VIEW)?;
        let caller_company = _scope.clone();
        let report = load_report(&state, &path.into_inner())?;
        if !can_read_report(&report, &caller.actor, caller_company.as_deref())
            && !check_capability(&ctx, &caller.role, PERM_BI_MANAGE)
        {
            return Err(UnifiedError::forbidden(
                "Report is private to its owner",
            ));
        }
        execute_report(&state, &report, MAX_RUN_ROWS)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn export_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ExportQuery>,
) -> HttpResponse {
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, ctx.is_debug()),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err(error, ctx.is_debug()),
    };
    let debug = ctx.is_debug();
    let format = query.format.trim().to_lowercase();
    if format != "csv" && format != "json" {
        return err(
            UnifiedError::bad_request("format must be \"csv\" or \"json\""),
            debug,
        );
    }
    let result: UnifiedResult<Value> = (|| {
        let caller_company = _scope.clone();
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_VIEW)?;
        let report = load_report(&state, &path.into_inner())?;
        if !can_read_report(&report, &caller.actor, caller_company.as_deref())
            && !check_capability(&ctx, &caller.role, PERM_BI_MANAGE)
        {
            return Err(UnifiedError::forbidden(
                "Report is private to its owner",
            ));
        }
        execute_report(&state, &report, MAX_EXPORT_ROWS)
    })();
    let run = match result {
        Ok(data) => data,
        Err(error) => return err(error, debug),
    };
    let rows = run
        .get("rows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let capped: Vec<Value> = rows.into_iter().take(MAX_EXPORT_ROWS).collect();
    if format == "csv" {
        let mut body = String::new();
        let mut headers: Vec<String> = Vec::new();
        for row in &capped {
            if let Value::Object(map) = row {
                for key in map.keys() {
                    if !headers.contains(key) {
                        headers.push(key.clone());
                    }
                }
            }
        }
        body.push_str(
            &headers
                .iter()
                .map(|header| escape_csv_field(header))
                .collect::<Vec<String>>()
                .join(","),
        );
        body.push('\n');
        for row in &capped {
            let line: Vec<String> = headers
                .iter()
                .map(|header| {
                    let value = row.get(header).map(|value| match value {
                        Value::String(text) => text.clone(),
                        Value::Null => String::new(),
                        other => other.to_string(),
                    });
                    escape_csv_field(&value.unwrap_or_default())
                })
                .collect();
            body.push_str(&line.join(","));
            body.push('\n');
        }
        HttpResponse::Ok()
            .content_type("text/csv; charset=utf-8")
            .insert_header((
                "Content-Disposition",
                format!("attachment; filename=\"bi-report-{}.csv\"", run.get("report_id").and_then(Value::as_str).unwrap_or("export")),
            ))
            .body(body)
    } else {
        // Inline JSON (not an attachment): the standard envelope via
        // the canonical helper, not a hand-built {"data": ...}.
        json::success_response(capped, 200)
    }
}

// ---------------------------------------------------------------------------
// Schedule handlers
// ---------------------------------------------------------------------------


pub(crate) async fn list_schedules(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_VIEW)?;
        let caller_company = _scope.clone();
        let report = load_report(&state, &path.into_inner())?;
        if !can_read_report(&report, &caller.actor, caller_company.as_deref())
            && !check_capability(&ctx, &caller.role, PERM_BI_MANAGE)
        {
            return Err(UnifiedError::forbidden(
                "Report is private to its owner",
            ));
        }
        let params = query.into_inner();
        let mut items: Vec<BiReportSchedule> = state
            .schedules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|schedule| schedule.report_id == report.id)
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn create_schedule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateScheduleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BiReportSchedule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_MANAGE)?;
        let report = load_report(&state, &path.into_inner())?;
        if !can_read_report(&report, &caller.actor, _scope.as_deref())
            && !check_capability(&ctx, &caller.role, PERM_BI_MANAGE)
        {
            return Err(UnifiedError::forbidden(
                "Report is private to its owner",
            ));
        }
        if !valid_schedule(&body.schedule) {
            return Err(UnifiedError::bad_request(
                "schedule must be an interval in seconds or five cron-like fields",
            ));
        }
        if body.recipients.is_empty() || body.recipients.len() > MAX_RECIPIENTS {
            return Err(UnifiedError::bad_request(format!(
                "recipients must list 1..={MAX_RECIPIENTS} user ids"
            )));
        }
        let mut recipients: Vec<String> = Vec::new();
        for recipient in &body.recipients {
            let recipient = require_text(recipient, "recipient", MAX_NAME_LEN)?;
            if !recipients.contains(&recipient) {
                recipients.push(recipient);
            }
        }
        let channel = match body.channel.as_deref().unwrap_or("notification") {
            "email" => "email".to_string(),
            "notification" => "notification".to_string(),
            other => {
                return Err(UnifiedError::bad_request(format!(
                    "channel must be \"email\" or \"notification\", got \"{other}\""
                )))
            }
        };
        let now = Utc::now();
        let mut schedule = BiReportSchedule {
            id: Uuid::new_v4(),
            report_id: report.id,
            company_id: report.company_id.clone(),
            schedule: body.schedule.trim().to_string(),
            recipients,
            channel,
            scheduler_status: "declined".to_string(),
            scheduler_job_id: None,
            last_delivered_at: None,
            created_at: now,
            updated_at: now,
        };
        schedule_delivery(&state, &mut schedule);
        state.schedules.insert(schedule.id, schedule.clone());
        Ok(schedule)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn get_schedule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BiReportSchedule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_VIEW)?;
        let (report_id, schedule_id) = path.into_inner();
        let report = load_report(&state, &report_id)?;
        let caller_company = _scope.clone();
        if !can_read_report(&report, &caller.actor, caller_company.as_deref())
            && !check_capability(&ctx, &caller.role, PERM_BI_MANAGE)
        {
            return Err(UnifiedError::forbidden(
                "Report is private to its owner",
            ));
        }
        let schedule = load_schedule(&state, &schedule_id)?;
        if schedule.report_id != report.id {
            return Err(UnifiedError::not_found(format!(
                "Schedule not found: {schedule_id}"
            )));
        }
        Ok(schedule)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn delete_schedule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_MANAGE)?;
        let (report_id, schedule_id) = path.into_inner();
        let report = load_report(&state, &report_id)?;
        if !can_read_report(&report, &caller.actor, _scope.as_deref())
            && !check_capability(&ctx, &caller.role, PERM_BI_MANAGE)
        {
            return Err(UnifiedError::forbidden(
                "Report is private to its owner",
            ));
        }
        let schedule = load_schedule(&state, &schedule_id)?;
        if schedule.report_id != report.id {
            return Err(UnifiedError::not_found(format!(
                "Schedule not found: {schedule_id}"
            )));
        }
        state.schedules.remove(&schedule.id);
        Ok(json!({ "deleted": schedule.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn run_schedule_now(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_MANAGE)?;
        let (report_id, schedule_id) = path.into_inner();
        let report = load_report(&state, &report_id)?;
        if !can_read_report(&report, &caller.actor, _scope.as_deref())
            && !check_capability(&ctx, &caller.role, PERM_BI_MANAGE)
        {
            return Err(UnifiedError::forbidden(
                "Report is private to its owner",
            ));
        }
        let schedule = load_schedule(&state, &schedule_id)?;
        if schedule.report_id != report.id {
            return Err(UnifiedError::not_found(format!(
                "Schedule not found: {schedule_id}"
            )));
        }
        let row_count = deliver_report_now(&state, &report, Some(&schedule))?;
        Ok(json!({
            "report_id": report.id.to_string(),
            "schedule_id": schedule.id.to_string(),
            "row_count": row_count,
            "recipients": schedule.recipients,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Forecast handlers
// ---------------------------------------------------------------------------


pub(crate) async fn forecast_revenue(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    query: web::Query<ForecastQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_VIEW)?;
        let caller_company = _scope.clone();
        let (currency, horizon_days) = parse_forecast_query(&query)?;
        let source = match query.source.as_deref() {
            Some(source) => validate_source(source)?,
            None => "billing".to_string(),
        };
        if source != "billing" && source != "store" {
            return Err(UnifiedError::bad_request(
                "revenue forecast source must be \"billing\" or \"store\"",
            ));
        }
        build_forecast(
            &state,
            "revenue",
            &[source.as_str()],
            caller_company.as_deref(),
            currency.as_deref(),
            horizon_days,
        )
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn forecast_cashflow(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    query: web::Query<ForecastQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_VIEW)?;
        let caller_company = _scope.clone();
        let (currency, horizon_days) = parse_forecast_query(&query)?;
        // Net cash flow: completed orders and captured payments minus
        // refunds (refunded rows are stored with a negative amount).
        build_forecast(
            &state,
            "cashflow",
            &["billing", "store"],
            caller_company.as_deref(),
            currency.as_deref(),
            horizon_days,
        )
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn forecast_churn(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    query: web::Query<ChurnQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_VIEW)?;
        // Tenant scope: the caller's own company wins over the
        // caller-supplied `company_id` query value, which can only
        // narrow a platform caller's (no company) query, never let
        // a company caller read another company's churn signals.
        let caller_company = _scope.clone();
        // Tenant scope: the caller's own company wins; an
        // unresolved caller reads global rows only, and a
        // caller-supplied `company_id` query value can never widen
        // past the caller scope (naming a company as an unresolved
        // caller is refused outright).
        let company_id = match (&caller_company, &query.company_id) {
            (Some(company), _) => Some(company.as_str()),
            (None, None) => None,
            (None, Some(_)) => {
                return Err(UnifiedError::forbidden(
                    "Company scope mismatch: the requested company_id does not match the caller's company",
                ));
            }
        };
        let inactive_days = query.inactive_days.unwrap_or(30);
        if !(7..=365).contains(&inactive_days) {
            return Err(UnifiedError::bad_request(
                "inactive_days must be between 7 and 365",
            ));
        }
        let cutoff = Utc::now() - Duration::days(inactive_days);
        let mut last_activity: BTreeMap<String, DateTime<Utc>> = BTreeMap::new();
        let mut failed_signals: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut total_customers: HashSet<String> = HashSet::new();
        for entry in state.source_records.iter() {
            let row = entry.value();
            // Unresolved callers see global rows only (no company
            // or an empty company); company callers see only their
            // own company's rows.
            match company_id {
                Some(company) => {
                    if row.company_id.as_deref() != Some(company) {
                        continue;
                    }
                }
                None => {
                    if row.company_id.as_deref().is_some_and(|value| !value.is_empty()) {
                        continue;
                    }
                }
            }
            let Some(customer) = &row.customer_id else {
                continue;
            };
            total_customers.insert(customer.clone());
            let last = last_activity
                .entry(customer.clone())
                .or_insert(row.occurred_at);
            if row.occurred_at > *last {
                *last = row.occurred_at;
            }
            match row.kind.as_str() {
                "license_suspended" | "license_revoked" => {
                    failed_signals
                        .entry(customer.clone())
                        .or_default()
                        .push(row.kind.clone());
                }
                "order_refunded" => {
                    failed_signals
                        .entry(customer.clone())
                        .or_default()
                        .push("order_refunded".to_string());
                }
                _ => {}
            }
        }
        if total_customers.is_empty() {
            return Err(UnifiedError::unprocessable(
                "Insufficient data: churn signals need recorded customer activity, and no customer rows exist yet",
            )
            .with_metadata("company_id", json!(company_id)));
        }
        let mut at_risk: Vec<Value> = Vec::new();
        for (customer, last) in &last_activity {
            if *last < cutoff {
                let signals = failed_signals.get(customer).cloned().unwrap_or_default();
                at_risk.push(json!({
                    "customer_id": customer,
                    "last_activity_at": last.to_rfc3339(),
                    "inactive_days": (Utc::now() - *last).num_days(),
                    "failed_payment_signals": signals,
                }));
            }
        }
        at_risk.sort_by(|a, b| {
            a.get("last_activity_at")
                .and_then(Value::as_str)
                .cmp(&b.get("last_activity_at").and_then(Value::as_str))
        });
        Ok(json!({
            "method": "last_activity_plus_failed_payment_signals",
            "inputs": {
                "inactive_days": inactive_days,
                "company_id": company_id,
                "observed_customers": total_customers.len(),
            },
            "at_risk_count": at_risk.len(),
            "at_risk_customers": at_risk,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// KPI dashboard handlers
// ---------------------------------------------------------------------------

/// Compute one KPI from the mirror rows. Returns `None` (with the
/// caller recording `insufficient_data`) when the metric's source
/// has no rows in the period -- a missing measurement is reported
/// as missing, never as zero.

pub(crate) async fn list_dashboards(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_VIEW)?;
        let caller_company = _scope.clone();
        let params = query.into_inner();
        let company_filter = params.filters.get("company_id").cloned();
        let owner_filter = params.filters.get("owner_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<BiKpiDashboard> = state
            .dashboards
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|dashboard| {
                // Tenant scope, same as the report list: a company
                // caller only ever sees their own company's
                // dashboards, an unresolved caller only global ones.
                if !in_company_scope_str(&dashboard.company_id, &caller_company) {
                    return false;
                }
                if let Some(company) = &company_filter {
                    if &dashboard.company_id != company {
                        return false;
                    }
                }
                if let Some(owner) = &owner_filter {
                    if &dashboard.owner_id != owner {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {}",
                        dashboard.name,
                        dashboard.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn create_dashboard(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    body: web::Json<CreateKpiDashboardRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BiKpiDashboard> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_MANAGE)?;
        let name = require_text(&body.name, "name", MAX_NAME_LEN)?;
        let company_id = require_text(&body.company_id, "company_id", MAX_COMPANY_LEN)?;
        // Tenant scope, same as report creation: a company caller
        // may only create dashboards for their own company, and an
        // unresolved caller may not claim a company at all.
        let company_id =
            require_company_match(&_tenant, &req, &Some(company_id))?.unwrap_or_default();
        let kpis = validate_kpis(&body.kpis)?;
        let description = match &body.description {
            Some(text) if !text.trim().is_empty() => Some(require_text(
                text,
                "description",
                MAX_DESCRIPTION_LEN,
            )?),
            _ => None,
        };
        let now = Utc::now();
        let dashboard = BiKpiDashboard {
            id: Uuid::new_v4(),
            company_id,
            owner_id: caller.actor.clone(),
            name,
            description,
            kpis,
            created_at: now,
            updated_at: now,
        };
        state.dashboards.insert(dashboard.id, dashboard.clone());
        Ok(dashboard)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn get_dashboard(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BiKpiDashboard> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_VIEW)?;
        let dashboard = load_dashboard(&state, &path.into_inner())?;
        // Tenant scope: a company caller may only read their own
        // company's dashboard (manage capability bypasses, as with
        // shared reports).
        let caller_company = _scope.clone();
        let can_manage = check_capability(&ctx, &caller.role, PERM_BI_MANAGE);
        if !can_access_company(caller_company.as_deref(), &dashboard.company_id, can_manage) {
            return Err(UnifiedError::forbidden(
                "Dashboard belongs to another company",
            ));
        }
        Ok(dashboard)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn update_dashboard(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateKpiDashboardRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BiKpiDashboard> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_MANAGE)?;
        let mut dashboard = load_dashboard(&state, &path.into_inner())?;
        // Tenant scope: only a manage-capable caller inside the
        // dashboard's company may change it.
        let can_manage = check_capability(&ctx, &caller.role, PERM_BI_MANAGE);
        if !can_access_company(_scope.as_deref(), &dashboard.company_id, can_manage) {
            return Err(UnifiedError::forbidden(
                "Dashboard belongs to another company",
            ));
        }
        if let Some(name) = &body.name {
            dashboard.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = &body.description {
            dashboard.description = match description {
                Some(text) if !text.trim().is_empty() => Some(require_text(
                    text,
                    "description",
                    MAX_DESCRIPTION_LEN,
                )?),
                _ => None,
            };
        }
        if let Some(kpis) = &body.kpis {
            dashboard.kpis = validate_kpis(kpis)?;
        }
        dashboard.updated_at = Utc::now();
        state.dashboards.insert(dashboard.id, dashboard.clone());
        Ok(dashboard)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn delete_dashboard(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_MANAGE)?;
        let dashboard = load_dashboard(&state, &path.into_inner())?;
        // Tenant scope: only a manage-capable caller inside the
        // dashboard's company may change it.
        let can_manage = check_capability(&ctx, &caller.role, PERM_BI_MANAGE);
        if !can_access_company(_scope.as_deref(), &dashboard.company_id, can_manage) {
            return Err(UnifiedError::forbidden(
                "Dashboard belongs to another company",
            ));
        }
        state.dashboards.remove(&dashboard.id);
        Ok(json!({ "deleted": dashboard.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}


pub(crate) async fn compute_dashboard(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<BiState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_BI_VIEW)?;
        let dashboard = load_dashboard(&state, &path.into_inner())?;
        // Tenant scope, same as reading the dashboard: computing
        // another company's KPIs would leak its numbers.
        let caller_company = _scope.clone();
        let can_manage = check_capability(&ctx, &caller.role, PERM_BI_MANAGE);
        if !can_access_company(caller_company.as_deref(), &dashboard.company_id, can_manage) {
            return Err(UnifiedError::forbidden(
                "Dashboard belongs to another company",
            ));
        }
        let computed: Vec<ComputedKpi> = dashboard
            .kpis
            .iter()
            .map(|kpi| match compute_kpi(&state, kpi, &dashboard.company_id) {
                Some(value) => ComputedKpi {
                    metric: kpi.metric.clone(),
                    source: kpi.source.clone(),
                    period: kpi.period.clone(),
                    value: Some(value),
                    status: "ok".to_string(),
                },
                None => ComputedKpi {
                    metric: kpi.metric.clone(),
                    source: kpi.source.clone(),
                    period: kpi.period.clone(),
                    value: None,
                    status: "insufficient_data".to_string(),
                },
            })
            .collect();
        Ok(json!({
            "dashboard_id": dashboard.id.to_string(),
            "company_id": dashboard.company_id,
            "computed_at": Utc::now().to_rfc3339(),
            "kpis": computed,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

