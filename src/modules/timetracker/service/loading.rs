// service - loading.
// Loading timesheets, teams, and staff company info.
//

use super::super::*;
use super::*;
use actix_web::HttpRequest;
use chrono::{DateTime, Datelike, Duration, Utc};
use serde_json::{Value, json};
use std::collections::HashMap;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookContext;
use unified_core::json;
use unified_core::pagination::PaginationParams;
use unified_core::storage::StorageRegistry;
use uuid::Uuid;

pub(crate) fn load_timesheet(state: &TimeTrackerState, raw_id: &str) -> UnifiedResult<Timesheet> {
    let id = parse_uuid(raw_id, "timesheet")?;
    state
        .timesheets
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Timesheet not found: {id}")))
}

// Handlers: staff management and teams

/// The company a staff call acts on: the `X-Company-Id` claim,
/// verified to exist and to contain the caller. Every staff and
/// team handler starts here, so a company admin can only ever
/// manage their own company's staff.
pub(crate) fn staff_company(
    ctx: &AppContext,
    req: &HttpRequest,
    caller: &Caller,
) -> UnifiedResult<Uuid> {
    let company_id = claimed_company_id(req)?;
    require_company_member(ctx, &caller.actor, company_id)?;
    Ok(company_id)
}

pub(crate) fn load_team(state: &TimeTrackerState, raw_id: &str) -> UnifiedResult<Team> {
    let id = parse_uuid(raw_id, "team")?;
    state
        .teams
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Team not found: {id}")))
}
