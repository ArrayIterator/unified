// timetracker — HTTP handlers: rates, payroll, and timesheets.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Handlers: rates, payroll, timesheets, reports, dashboard, settings
// ---------------------------------------------------------------------------

pub(crate) async fn list_rates(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<HourlyRate>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let params = query.into_inner();
        let user_filter = params.filters.get("user_id").cloned();
        if let Some(wanted) = &user_filter {
            can_view_user(&ctx, &caller, wanted)?;
        }
        let mut items: Vec<HourlyRate> = state
            .rates
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rate| match &user_filter {
                Some(wanted) => &rate.user_id == wanted,
                None => rate.user_id == caller.actor,
            })
            .collect();
        items.sort_by(|a, b| b.effective_from.cmp(&a.effective_from));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_rate(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateRateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<HourlyRate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_RATES)?;
        let input = body.into_inner();
        let user_id = require_text(&input.user_id, "user_id", 128)?;
        if input.rate < 0.0 || !input.rate.is_finite() {
            return Err(UnifiedError::bad_request("rate must be a finite number >= 0"));
        }
        let currency = match &input.currency {
            Some(raw) => valid_currency(raw)?,
            None => state.global_settings().default_currency,
        };
        let effective_from = input.effective_from.unwrap_or_else(Utc::now);
        if let Some(effective_to) = input.effective_to {
            if effective_to <= effective_from {
                return Err(UnifiedError::bad_request(
                    "effective_to must be after effective_from",
                ));
            }
        }
        let now = Utc::now();
        let rate = HourlyRate {
            id: Uuid::now_v7(),
            user_id,
            currency,
            rate: round4(input.rate),
            effective_from,
            effective_to: input.effective_to,
            created_at: now,
            updated_at: now,
        };
        state.rates.insert(rate.id, rate.clone());
        state.emit(
            events::RATE_CREATED,
            json!({
                "rate_id": rate.id.to_string(),
                "user_id": rate.user_id,
                "currency": rate.currency,
                "rate": rate.rate,
                "created_by": caller.actor,
            }),
        );
        Ok(rate)
    })();
    match result {
        Ok(rate) => json::success_response(rate, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_rate(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<HourlyRate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let id = parse_uuid(&path.into_inner(), "rate")?;
        let rate = state
            .rates
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Rate not found: {id}")))?;
        if rate.user_id != caller.actor {
            can_view_user(&ctx, &caller, &rate.user_id)?;
        }
        Ok(rate)
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_rate(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateRateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<HourlyRate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_RATES)?;
        let input = body.into_inner();
        let id = parse_uuid(&path.into_inner(), "rate")?;
        let mut rate = state
            .rates
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Rate not found: {id}")))?;
        if let Some(raw) = &input.currency {
            rate.currency = valid_currency(raw)?;
        }
        if let Some(value) = input.rate {
            if value < 0.0 || !value.is_finite() {
                return Err(UnifiedError::bad_request("rate must be a finite number >= 0"));
            }
            rate.rate = round4(value);
        }
        if let Some(effective_from) = input.effective_from {
            rate.effective_from = effective_from;
        }
        if let Some(effective_to) = input.effective_to {
            rate.effective_to = effective_to;
        }
        if let Some(effective_to) = rate.effective_to {
            if effective_to <= rate.effective_from {
                return Err(UnifiedError::bad_request(
                    "effective_to must be after effective_from",
                ));
            }
        }
        rate.updated_at = Utc::now();
        state.rates.insert(rate.id, rate.clone());
        state.emit(
            events::RATE_UPDATED,
            json!({
                "rate_id": rate.id.to_string(),
                "user_id": rate.user_id,
                "currency": rate.currency,
                "rate": rate.rate,
                "updated_by": caller.actor,
            }),
        );
        Ok(rate)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_rate(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_RATES)?;
        let id = parse_uuid(&path.into_inner(), "rate")?;
        let rate = state
            .rates
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Rate not found: {id}")))?;
        state.rates.remove(&id);
        state.emit(
            events::RATE_DELETED,
            json!({
                "rate_id": rate.id.to_string(),
                "user_id": rate.user_id,
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "id": rate.id.to_string(), "deleted": true }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_payroll(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PAYROLL)?;
        let params = query.into_inner();
        let (from, to) = parse_window(&params)?;
        let user_filter = params.filters.get("user_id").cloned();
        if let Some(wanted) = &user_filter {
            if wanted != &caller.actor {
                can_view_user(&ctx, &caller, wanted)?;
            }
        }
        let lines = compute_payroll(&state, from, to, user_filter.as_deref());
        let total_pay: f64 = round4(lines.iter().map(|line| line.total_pay).sum());
        Ok(json!({
            "from": from.to_rfc3339(),
            "to": to.to_rfc3339(),
            "lines": lines,
            "total_pay": total_pay,
            "currency_note": "Lines carry their own currency; total_pay is a plain sum across all lines and is only meaningful when every line shares one currency. Sum per currency when a company pays in several currencies.",
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_timesheets(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Timesheet>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let params = query.into_inner();
        let user_filter = params.filters.get("user_id").cloned();
        if let Some(wanted) = &user_filter {
            can_view_user(&ctx, &caller, wanted)?;
        }
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<Timesheet> = state
            .timesheets
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|sheet| match &user_filter {
                Some(wanted) => &sheet.user_id == wanted,
                None => sheet.user_id == caller.actor,
            })
            .filter(|sheet| {
                if let Some(status) = &status_filter {
                    let expected = match status.as_str() {
                        "draft" => TimesheetStatus::Draft,
                        "submitted" => TimesheetStatus::Submitted,
                        "approved" => TimesheetStatus::Approved,
                        "rejected" => TimesheetStatus::Rejected,
                        _ => return false,
                    };
                    if sheet.status != expected {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.period_start.cmp(&a.period_start));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

/// The non-rejected timesheet an entry already sits on, if any.
/// An entry on a draft / submitted / approved sheet is already
/// spoken for; only a rejected sheet frees its entries again.
fn timesheet_holding_entry(state: &TimeTrackerState, entry_id: Uuid) -> Option<Uuid> {
    state
        .timesheets
        .iter()
        .find(|sheet| {
            sheet.status != TimesheetStatus::Rejected && sheet.entry_ids.contains(&entry_id)
        })
        .map(|sheet| sheet.id)
}

/// Resolve the entries for a new timesheet. With no explicit ids
/// the pending entries of the owner inside the period are
/// collected, skipping entries already on another live timesheet.
/// With explicit ids every entry must be the owner's, still
/// pending, started inside the period, and not already on another
/// live timesheet.
fn collect_timesheet_entries(
    state: &TimeTrackerState,
    owner: &str,
    period_start: DateTime<Utc>,
    period_end: DateTime<Utc>,
    requested: &[Uuid],
) -> UnifiedResult<Vec<Uuid>> {
    if requested.is_empty() {
        return Ok(state
            .entries
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| {
                entry.user_id == owner
                    && entry.approval == EntryApproval::Pending
                    && entry.started_at >= period_start
                    && entry.started_at <= period_end
                    && timesheet_holding_entry(state, entry.id).is_none()
            })
            .map(|entry| entry.id)
            .collect());
    }
    for entry_id in requested {
        let entry = state
            .entries
            .get(entry_id)
            .map(|record| record.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Time entry not found: {entry_id}")))?;
        if entry.user_id != owner {
            return Err(UnifiedError::forbidden(
                "A timesheet can only contain the owner's own entries",
            ));
        }
        if entry.approval != EntryApproval::Pending {
            return Err(UnifiedError::conflict(
                "Only pending entries can be added to a timesheet",
            ));
        }
        if entry.started_at < period_start || entry.started_at > period_end {
            return Err(UnifiedError::bad_request(format!(
                "Time entry {entry_id} starts outside the timesheet period"
            )));
        }
        if let Some(sheet_id) = timesheet_holding_entry(state, *entry_id) {
            return Err(UnifiedError::conflict(format!(
                "Time entry {entry_id} is already on timesheet {sheet_id}"
            )));
        }
    }
    Ok(requested.to_vec())
}

/// A report window may span at most `max`: compared on the real
/// duration, not on truncated whole days, so a window of 25 hours
/// is not a "one day" window.
fn check_report_span(
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    max: Duration,
    message: &str,
) -> UnifiedResult<()> {
    if to - from > max {
        return Err(UnifiedError::bad_request(message));
    }
    Ok(())
}

pub(crate) async fn create_timesheet(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateTimesheetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Timesheet> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WRITE)?;
        let input = body.into_inner();
        if input.period_end <= input.period_start {
            return Err(UnifiedError::bad_request(
                "period_end must be after period_start",
            ));
        }
        let owner = match &input.user_id {
            Some(wanted) if wanted != &caller.actor => {
                can_view_user(&ctx, &caller, wanted)?;
                wanted.clone()
            }
            _ => caller.actor.clone(),
        };
        let entry_ids: Vec<Uuid> = collect_timesheet_entries(
            &state,
            &owner,
            input.period_start,
            input.period_end,
            &input.entry_ids,
        )?;
        if entry_ids.is_empty() {
            return Err(UnifiedError::bad_request(
                "No pending entries in this period to put on a timesheet",
            ));
        }
        let now = Utc::now();
        let sheet = Timesheet {
            id: Uuid::now_v7(),
            user_id: owner.clone(),
            period_start: input.period_start,
            period_end: input.period_end,
            status: TimesheetStatus::Draft,
            entry_ids,
            submitted_at: None,
            reviewed_by: None,
            reviewed_at: None,
            review_note: None,
            created_at: now,
            updated_at: now,
        };
        state.timesheets.insert(sheet.id, sheet.clone());
        state.emit(
            events::TIMESHEET_CREATED,
            json!({
                "timesheet_id": sheet.id.to_string(),
                "user_id": owner,
                "period_start": sheet.period_start.to_rfc3339(),
                "period_end": sheet.period_end.to_rfc3339(),
                "entry_count": sheet.entry_ids.len(),
                "created_by": caller.actor,
            }),
        );
        Ok(sheet)
    })();
    match result {
        Ok(sheet) => json::success_response(sheet, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_timesheet(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Timesheet> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let sheet = load_timesheet(&state, &path.into_inner())?;
        if sheet.user_id != caller.actor {
            can_view_user(&ctx, &caller, &sheet.user_id)?;
        }
        Ok(sheet)
    })();
    json::respond(result, debug)
}

pub(crate) async fn submit_timesheet(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Timesheet> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WRITE)?;
        let mut sheet = load_timesheet(&state, &path.into_inner())?;
        if sheet.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the timesheet owner can submit it",
            ));
        }
        if sheet.status != TimesheetStatus::Draft {
            return Err(UnifiedError::conflict(
                "Only a draft timesheet can be submitted",
            ));
        }
        sheet.status = TimesheetStatus::Submitted;
        sheet.submitted_at = Some(Utc::now());
        sheet.updated_at = Utc::now();
        state.timesheets.insert(sheet.id, sheet.clone());
        state.emit(
            events::TIMESHEET_SUBMITTED,
            json!({
                "timesheet_id": sheet.id.to_string(),
                "user_id": sheet.user_id,
                "entry_count": sheet.entry_ids.len(),
                "submitted_by": caller.actor,
            }),
        );
        Ok(sheet)
    })();
    json::respond(result, debug)
}

pub(crate) async fn approve_timesheet(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewTimesheetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Timesheet> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVE)?;
        let input = body.into_inner();
        let note = optional_text(&input.note, "note", MAX_NOTE_LEN)?;
        let mut sheet = load_timesheet(&state, &path.into_inner())?;
        if sheet.user_id == caller.actor {
            return Err(UnifiedError::forbidden(
                "A timesheet cannot be approved by its own owner",
            ));
        }
        if sheet.status != TimesheetStatus::Submitted {
            return Err(UnifiedError::conflict(
                "Only a submitted timesheet can be approved",
            ));
        }
        sheet.status = TimesheetStatus::Approved;
        sheet.reviewed_by = Some(caller.actor.clone());
        sheet.reviewed_at = Some(Utc::now());
        sheet.review_note = note.clone();
        sheet.updated_at = Utc::now();
        state.timesheets.insert(sheet.id, sheet.clone());
        // Approving the sheet approves its still-pending entries,
        // so payroll sees them as payable.
        let now = Utc::now();
        for entry_id in &sheet.entry_ids {
            if let Some(mut entry) = state.entries.get_mut(entry_id) {
                if entry.approval == EntryApproval::Pending {
                    entry.approval = EntryApproval::Approved;
                    entry.approved_by = Some(caller.actor.clone());
                    entry.approved_at = Some(now);
                    entry.updated_at = now;
                }
            }
        }
        state.emit(
            events::TIMESHEET_APPROVED,
            json!({
                "timesheet_id": sheet.id.to_string(),
                "user_id": sheet.user_id,
                "approved_by": caller.actor,
                "entry_count": sheet.entry_ids.len(),
                "note": note,
            }),
        );
        Ok(sheet)
    })();
    json::respond(result, debug)
}

pub(crate) async fn reject_timesheet(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewTimesheetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Timesheet> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVE)?;
        let input = body.into_inner();
        let note = optional_text(&input.note, "note", MAX_NOTE_LEN)?;
        let mut sheet = load_timesheet(&state, &path.into_inner())?;
        if sheet.user_id == caller.actor {
            return Err(UnifiedError::forbidden(
                "A timesheet cannot be rejected by its own owner",
            ));
        }
        if sheet.status != TimesheetStatus::Submitted {
            return Err(UnifiedError::conflict(
                "Only a submitted timesheet can be rejected",
            ));
        }
        sheet.status = TimesheetStatus::Rejected;
        sheet.reviewed_by = Some(caller.actor.clone());
        sheet.reviewed_at = Some(Utc::now());
        sheet.review_note = note.clone();
        sheet.updated_at = Utc::now();
        state.timesheets.insert(sheet.id, sheet.clone());
        state.emit(
            events::TIMESHEET_REJECTED,
            json!({
                "timesheet_id": sheet.id.to_string(),
                "user_id": sheet.user_id,
                "rejected_by": caller.actor,
                "note": note,
            }),
        );
        Ok(sheet)
    })();
    json::respond(result, debug)
}

pub(crate) async fn report_daily(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ReportRow>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let params = query.into_inner();
        let (from, to) = parse_window(&params)?;
        check_report_span(
            from,
            to,
            Duration::days(1),
            "Daily report covers at most one day; pass from/to within a single day",
        )?;
        let user_filter = params.filters.get("user_id").cloned();
        if let Some(wanted) = &user_filter {
            if wanted != &caller.actor {
                can_view_user(&ctx, &caller, wanted)?;
            }
        }
        let project_filter = params
            .filters
            .get("project_id")
            .map(|raw| parse_uuid(raw, "project_id"))
            .transpose()?;
        let rows = report_rows(&state, from, to, user_filter.as_deref(), project_filter);
        Ok(paginate(rows, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn report_weekly(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ReportRow>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let params = query.into_inner();
        let (from, to) = parse_window(&params)?;
        check_report_span(
            from,
            to,
            Duration::days(8),
            "Weekly report covers at most 8 days; pass a one-week from/to window",
        )?;
        let user_filter = params.filters.get("user_id").cloned();
        if let Some(wanted) = &user_filter {
            if wanted != &caller.actor {
                can_view_user(&ctx, &caller, wanted)?;
            }
        }
        let project_filter = params
            .filters
            .get("project_id")
            .map(|raw| parse_uuid(raw, "project_id"))
            .transpose()?;
        let rows = report_rows(&state, from, to, user_filter.as_deref(), project_filter);
        Ok(paginate(rows, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn report_summary(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let params = query.into_inner();
        let (from, to) = parse_window(&params)?;
        let user_filter = params.filters.get("user_id").cloned();
        if let Some(wanted) = &user_filter {
            if wanted != &caller.actor {
                can_view_user(&ctx, &caller, wanted)?;
            }
        }
        let project_filter = params
            .filters
            .get("project_id")
            .map(|raw| parse_uuid(raw, "project_id"))
            .transpose()?;
        let rows = report_rows(&state, from, to, user_filter.as_deref(), project_filter);
        let total_secs: i64 = rows.iter().map(|row| row.total_secs).sum();
        let billable_secs: i64 = rows.iter().map(|row| row.billable_secs).sum();
        let entry_count: usize = rows.iter().map(|row| row.entry_count).sum();
        Ok(json!({
            "from": from.to_rfc3339(),
            "to": to.to_rfc3339(),
            "total_secs": total_secs,
            "total_hours": round4(total_secs as f64 / 3600.0),
            "billable_secs": billable_secs,
            "billable_hours": round4(billable_secs as f64 / 3600.0),
            "non_billable_secs": total_secs - billable_secs,
            "entry_count": entry_count,
            "rows": rows,
        }))
    })();
    json::respond(result, debug)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry_at(user: &str, start: DateTime<Utc>, duration_secs: i64) -> TimeEntry {
        TimeEntry {
            id: Uuid::new_v4(),
            user_id: user.to_string(),
            project_id: None,
            task_id: None,
            timer_id: None,
            description: String::new(),
            started_at: start,
            ended_at: start + Duration::seconds(duration_secs),
            duration_secs,
            billable: true,
            approval: EntryApproval::Pending,
            approved_by: None,
            approved_at: None,
            rejection_note: None,
            reason: None,
            created_at: start,
            updated_at: start,
        }
    }

    fn sheet_with(entry_id: Uuid, status: TimesheetStatus) -> Timesheet {
        let now = Utc::now();
        Timesheet {
            id: Uuid::new_v4(),
            user_id: "u1".to_string(),
            period_start: "2026-11-16T00:00:00Z".parse().unwrap(),
            period_end: "2026-11-23T00:00:00Z".parse().unwrap(),
            status,
            entry_ids: vec![entry_id],
            submitted_at: None,
            reviewed_by: None,
            reviewed_at: None,
            review_note: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn explicit_entries_outside_the_period_are_rejected() {
        let state = TimeTrackerState::default();
        let inside = entry_at("u1", "2026-11-17T09:00:00Z".parse().unwrap(), 3600);
        let outside = entry_at("u1", "2026-11-30T09:00:00Z".parse().unwrap(), 3600);
        state.entries.insert(inside.id, inside.clone());
        state.entries.insert(outside.id, outside.clone());
        let start = "2026-11-16T00:00:00Z".parse().unwrap();
        let end = "2026-11-23T00:00:00Z".parse().unwrap();
        assert!(collect_timesheet_entries(&state, "u1", start, end, &[inside.id]).is_ok());
        assert!(collect_timesheet_entries(&state, "u1", start, end, &[outside.id]).is_err());
    }

    #[test]
    fn entries_on_a_live_timesheet_cannot_be_attached_again() {
        let state = TimeTrackerState::default();
        let entry = entry_at("u1", "2026-11-17T09:00:00Z".parse().unwrap(), 3600);
        state.entries.insert(entry.id, entry.clone());
        let start = "2026-11-16T00:00:00Z".parse().unwrap();
        let end = "2026-11-23T00:00:00Z".parse().unwrap();
        let sheet = sheet_with(entry.id, TimesheetStatus::Draft);
        state.timesheets.insert(sheet.id, sheet.clone());
        assert!(collect_timesheet_entries(&state, "u1", start, end, &[entry.id]).is_err());
        // Auto-collect skips it too.
        assert!(collect_timesheet_entries(&state, "u1", start, end, &[])
            .unwrap()
            .is_empty());
        // A rejected sheet frees the entry again.
        let mut rejected = sheet.clone();
        rejected.status = TimesheetStatus::Rejected;
        state.timesheets.insert(rejected.id, rejected);
        assert_eq!(
            collect_timesheet_entries(&state, "u1", start, end, &[entry.id]).unwrap(),
            vec![entry.id]
        );
    }

    #[test]
    fn report_span_compares_real_duration_not_truncated_days() {
        let from = "2026-11-16T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        // 25 hours spans two calendar days but truncates to 1 day:
        // it must still fail the daily cap.
        let to = from + Duration::hours(25);
        assert!(check_report_span(from, to, Duration::days(1), "too long").is_err());
        assert!(check_report_span(
            from,
            from + Duration::hours(24),
            Duration::days(1),
            "too long"
        )
        .is_ok());
        // 8 days + 1 hour truncates to 8 days: it must fail weekly.
        let weekly_to = from + Duration::days(8) + Duration::hours(1);
        assert!(check_report_span(from, weekly_to, Duration::days(8), "too long").is_err());
        assert!(check_report_span(
            from,
            from + Duration::days(8),
            Duration::days(8),
            "too long"
        )
        .is_ok());
    }
}
