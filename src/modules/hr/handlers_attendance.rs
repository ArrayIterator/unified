//! HTTP handlers: attendance.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Handlers: attendance
// ---------------------------------------------------------------------------

pub(crate) async fn list_attendance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AttendanceRecord>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let params = query.into_inner();
        let read_all = check_capability(&ctx, &caller.role, PERM_HR_READ_ALL);
        let own_employee_id = state
            .employee_by_user(&caller.actor)
            .map(|employee| employee.id);
        let from = params
            .filters
            .get("from")
            .and_then(|raw| NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok());
        let to = params
            .filters
            .get("to")
            .and_then(|raw| NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok());
        let mut items: Vec<AttendanceRecord> = state
            .attendance
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                (read_all || Some(record.employee_id.as_str()) == own_employee_id.as_deref())
                    && params
                        .filters
                        .get("employee_id")
                        .is_none_or(|employee| record.employee_id == *employee)
                    && params
                        .filters
                        .get("source")
                        .is_none_or(|source| record.source == *source)
                    && from.is_none_or(|from| record.work_date >= from)
                    && to.is_none_or(|to| record.work_date <= to)
            })
            .collect();
        items.sort_by(|a, b| {
            b.work_date
                .cmp(&a.work_date)
                .then(b.created_at.cmp(&a.created_at))
        });
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

pub(crate) async fn create_attendance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    body: web::Json<CreateAttendance>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AttendanceRecord> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let input = body.into_inner();
        let employee = state
            .employees
            .get(input.employee_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Employee not found"))?;
        let work_date = parse_date(&input.work_date, "work_date")?;
        if input.minutes_worked == 0 || input.minutes_worked > MAX_MINUTES_PER_DAY {
            return Err(UnifiedError::bad_request(
                "minutes_worked must be between 1 and 1440",
            ));
        }
        let entry_id = input
            .entry_id
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if let Some(entry_id) = &entry_id {
            if state
                .attendance
                .iter()
                .any(|entry| entry.value().entry_id.as_deref() == Some(entry_id.as_str()))
            {
                return Err(UnifiedError::conflict(format!(
                    "An attendance record already exists for entry {entry_id}"
                )));
            }
        }
        let source = match input.source {
            Some(source) if !source.trim().is_empty() => {
                require_text(&source, "source", 128)?
            }
            _ => "manual".to_string(),
        };
        let record = AttendanceRecord {
            id: new_id(),
            employee_id: employee.id.clone(),
            user_id: employee.user_id,
            work_date,
            minutes_worked: input.minutes_worked,
            source,
            entry_id,
            created_at: now(),
            updated_at: now(),
        };
        state.attendance.insert(record.id.clone(), record.clone());
        Ok(record)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_attendance_for_employee(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AttendanceRecord>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let employee = load_employee_for_read(&state, &ctx, &caller, path.as_str())?;
        let params = query.into_inner();
        let from = params
            .filters
            .get("from")
            .and_then(|raw| NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok());
        let to = params
            .filters
            .get("to")
            .and_then(|raw| NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok());
        let mut items: Vec<AttendanceRecord> = state
            .attendance
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                record.employee_id == employee.id
                    && from.is_none_or(|from| record.work_date >= from)
                    && to.is_none_or(|to| record.work_date <= to)
            })
            .collect();
        items.sort_by(|a, b| b.work_date.cmp(&a.work_date));
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
