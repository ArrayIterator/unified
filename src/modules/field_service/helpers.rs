// field_service module - Helpers.
//
// Split from the former single-file module `src/modules/field_service.rs`.
// The module root (`field_service.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, NaiveDate, Timelike, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::{MAX_SLUG_LEN, PERM_FIELD_SERVICE_MANAGE, PERM_FIELD_SERVICE_TECHNICIAN};
use super::types::{DispatchAssignment, Equipment, EquipmentType, MaintenancePlan, MaintenanceTemplate, WorkOrder, WorkOrderItem, WorkOrderPart};
use super::state::FieldServiceState;

// Helpers

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the ai module.
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the ai module's capability check).
pub(crate) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}
// Tenant edge guard (C3 pilot 4, gate-only).

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` / `authorize` or any
/// capability check.
///
/// This module's records carry no `company_id` / `tenant_id` scope
/// field, so the guard is a fail-closed gate only: no per-record
/// company check is applied. Real partitioning of these records
/// needs a `tenant_id` on the record (C11 follow-up).
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// Company scope of the verified caller: the tenant context's
/// company first, the server-resolved caller company second; a
/// mismatch between the two is a 403. The `X-Company-Id` header is
/// never read. Resolved in every gated handler so a tenant/caller
/// mismatch fails closed even though these records carry no company
/// field to filter on (gate-only; see `require_tenant`).
pub(crate) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    match (&tenant.company_id, &verified) {
        (Some(tenant_company), Some(verified_company)) if tenant_company != verified_company => {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified.clone()),
    }
}

/// A caller may act on a work order (status changes, time logs,
/// parts, sign-off) when they hold `field_service.manage`, or
/// when they hold `field_service.technician` and are the assigned
/// technician. Anyone else is forbidden.
pub(crate) fn can_act_on_order(ctx: &AppContext, caller: &Caller, order: &WorkOrder) -> UnifiedResult<()> {
    if check_capability(ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE) {
        return Ok(());
    }
    if check_capability(ctx, &caller.role, PERM_FIELD_SERVICE_TECHNICIAN)
        && order.assignee_id.as_deref() == Some(caller.actor.as_str())
    {
        return Ok(());
    }
    Err(UnifiedError::forbidden(
        "Only a manager or the assigned technician may act on this work order",
    ))
}

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        Some(raw) => Some(require_text(raw, field, max)).transpose(),
        None => Ok(None),
    }
}

/// A registry slug: lowercase letters, digits, `_`, and `-` only.
pub(crate) fn valid_slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SLUG_LEN
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

pub(crate) fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

pub(crate) fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

pub(crate) fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}

pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

/// Add `months` calendar months to a date, clamping the day to the
/// last day of the target month (so 31 January + 1 month becomes
/// 28/29 February, never an invalid date).
pub(crate) fn add_months(date: NaiveDate, months: i64) -> NaiveDate {
    let total_months = date.year() * 12 + (date.month() as i32 - 1) + months as i32;
    let year = total_months.div_euclid(12);
    let month = (total_months.rem_euclid(12) + 1) as u32;
    let last_day = days_in_month(year, month);
    let day = date.day().min(last_day);
    NaiveDate::from_ymd_opt(year, month, day).unwrap_or(date)
}

pub(crate) fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

/// Compute the next due date / usage of a plan from its interval
/// and its last service. Time-based plans anchor on
/// `last_service_at` (or `created_at` when never serviced);
/// usage-based plans anchor on `usage_at_last_service` (or 0).
/// Returns `(next_due_at, next_due_usage)`.
pub(crate) fn compute_next_due(
    state: &FieldServiceState,
    plan: &MaintenancePlan,
) -> UnifiedResult<(Option<DateTime<Utc>>, Option<f64>)> {
    let unit = state.interval_unit(&plan.interval_unit).ok_or_else(|| {
        UnifiedError::bad_request(format!("Unknown interval unit: {}", plan.interval_unit))
    })?;
    if unit.is_usage_based {
        let base = plan.usage_at_last_service.unwrap_or(0.0);
        return Ok((None, Some(base + plan.interval_value as f64)));
    }
    let base = plan.last_service_at.unwrap_or(plan.created_at);
    let base_date = base.date_naive();
    let due_date = if unit.is_calendar_month {
        add_months(base_date, plan.interval_value)
    } else {
        let days = unit.days_per_unit.unwrap_or(1) * plan.interval_value;
        base_date + chrono::Duration::days(days)
    };
    let due = due_date
        .and_hms_opt(base.time().hour(), base.time().minute(), base.time().second())
        .map(|naive| DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
        .unwrap_or(base);
    Ok((Some(due), None))
}

/// True when a plan is past due right now: for time-based plans
/// the due date has passed; for usage-based plans the equipment
/// meter reading has reached the due reading.
pub(crate) fn plan_is_overdue(state: &FieldServiceState, plan: &MaintenancePlan) -> bool {
    if !plan.is_active {
        return false;
    }
    let now = Utc::now();
    if let Some(due_at) = plan.next_due_at {
        return due_at < now;
    }
    if let Some(due_usage) = plan.next_due_usage {
        if let Some(equipment) = state
            .equipment
            .get(&plan.equipment_id)
            .map(|entry| entry.value().clone())
        {
            if let Some(reading) = equipment.usage_reading {
                return reading >= due_usage;
            }
        }
    }
    false
}

/// Days (negative when overdue) until a time-based plan falls due;
/// `None` for usage-based plans.
pub(crate) fn days_until_due(plan: &MaintenancePlan) -> Option<i64> {
    plan.next_due_at
        .map(|due_at| (due_at.date_naive() - Utc::now().date_naive()).num_days())
}

/// Priority rank for dispatch ordering: lower sorts first.
pub(crate) fn priority_rank(priority: &str) -> i64 {
    match priority {
        "urgent" => 0,
        "high" => 1,
        "normal" => 2,
        "low" => 3,
        _ => 4,
    }
}

pub(crate) fn load_equipment_type(state: &FieldServiceState, raw_id: &str) -> UnifiedResult<EquipmentType> {
    let id = parse_uuid(raw_id, "equipment type")?;
    state
        .equipment_types
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Equipment type not found: {id}")))
}

pub(crate) fn load_equipment(state: &FieldServiceState, raw_id: &str) -> UnifiedResult<Equipment> {
    let id = parse_uuid(raw_id, "equipment")?;
    state
        .equipment
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Equipment not found: {id}")))
}

pub(crate) fn load_template(state: &FieldServiceState, raw_id: &str) -> UnifiedResult<MaintenanceTemplate> {
    let id = parse_uuid(raw_id, "maintenance template")?;
    state
        .templates
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Maintenance template not found: {id}")))
}

pub(crate) fn load_plan(state: &FieldServiceState, raw_id: &str) -> UnifiedResult<MaintenancePlan> {
    let id = parse_uuid(raw_id, "maintenance plan")?;
    state
        .plans
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Maintenance plan not found: {id}")))
}

pub(crate) fn load_work_order(state: &FieldServiceState, raw_id: &str) -> UnifiedResult<WorkOrder> {
    let id = parse_uuid(raw_id, "work order")?;
    state
        .work_orders
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Work order not found: {id}")))
}

pub(crate) fn load_item(
    state: &FieldServiceState,
    work_order_id: Uuid,
    raw_id: &str,
) -> UnifiedResult<WorkOrderItem> {
    let id = parse_uuid(raw_id, "checklist item")?;
    state
        .items
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|item| item.work_order_id == work_order_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Checklist item not found: {id}")))
}

pub(crate) fn load_part(
    state: &FieldServiceState,
    work_order_id: Uuid,
    raw_id: &str,
) -> UnifiedResult<WorkOrderPart> {
    let id = parse_uuid(raw_id, "part")?;
    state
        .parts
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|part| part.work_order_id == work_order_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Part not found: {id}")))
}

pub(crate) fn load_dispatch_assignment(
    state: &FieldServiceState,
    raw_id: &str,
) -> UnifiedResult<DispatchAssignment> {
    let id = parse_uuid(raw_id, "dispatch assignment")?;
    state
        .dispatch_assignments
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Dispatch assignment not found: {id}")))
}

pub(crate) fn items_of(state: &FieldServiceState, work_order_id: Uuid) -> Vec<WorkOrderItem> {
    let mut rows: Vec<WorkOrderItem> = state
        .items
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| item.work_order_id == work_order_id)
        .collect();
    rows.sort_by_key(|item| (item.position, item.created_at));
    rows
}

pub(crate) fn parts_of(state: &FieldServiceState, work_order_id: Uuid) -> Vec<WorkOrderPart> {
    let mut rows: Vec<WorkOrderPart> = state
        .parts
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|part| part.work_order_id == work_order_id)
        .collect();
    rows.sort_by_key(|part| part.created_at);
    rows
}

/// One due/overdue check result, for the check-due report.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DueAlert {
    pub(crate) plan_id: Uuid,
    pub(crate) plan_name: String,
    pub(crate) equipment_id: Uuid,
    pub(crate) kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) days_until_due: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) next_due_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) next_due_usage: Option<f64>,
}

/// Run the due/overdue check: every active plan that is overdue
/// or falls due within the alert window gets a
/// `notification.requested` to the plan creator plus a
/// `field_service.plan.overdue` or `field_service.plan.due_soon`
/// event, matching the alert kind. Returns the alerts raised.
pub(crate) fn run_due_check(state: &FieldServiceState) -> Vec<DueAlert> {
    let window = state.settings().overdue_alert_days;
    let mut alerts: Vec<DueAlert> = Vec::new();
    for entry in state.plans.iter() {
        let plan = entry.value().clone();
        if !plan.is_active {
            continue;
        }
        let overdue = plan_is_overdue(state, &plan);
        let due_soon = match days_until_due(&plan) {
            Some(days) => days >= 0 && days <= window,
            None => false,
        };
        if !overdue && !due_soon {
            continue;
        }
        let kind = if overdue { "overdue" } else { "due_soon" };
        let title = if overdue {
            format!("Maintenance overdue: {}", plan.name)
        } else {
            format!("Maintenance due soon: {}", plan.name)
        };
        let body = match (plan.next_due_at, plan.next_due_usage) {
            (Some(due_at), _) => format!(
                "Plan \"{}\" for equipment {} falls due on {}.",
                plan.name, plan.equipment_id, due_at
            ),
            (None, Some(due_usage)) => format!(
                "Plan \"{}\" for equipment {} falls due at meter reading {}.",
                plan.name, plan.equipment_id, due_usage
            ),
            _ => format!("Plan \"{}\" for equipment {} is due.", plan.name, plan.equipment_id),
        };
        let link = format!("/api/v1/field-service/plans/{}", plan.id);
        state.notify(
            &format!("field_service.plan.{kind}"),
            &plan.created_by,
            &title,
            &body,
            Some(link),
            if overdue { "high" } else { "normal" },
        );
        state.emit(
            &format!("field_service.plan.{kind}"),
            json!({
                "plan_id": plan.id.to_string(),
                "equipment_id": plan.equipment_id.to_string(),
                "kind": kind,
                "days_until_due": days_until_due(&plan),
            }),
        );
        alerts.push(DueAlert {
            plan_id: plan.id,
            plan_name: plan.name.clone(),
            equipment_id: plan.equipment_id,
            kind: kind.to_string(),
            days_until_due: days_until_due(&plan),
            next_due_at: plan.next_due_at,
            next_due_usage: plan.next_due_usage,
        });
    }
    alerts.sort_by_key(|alert| alert.days_until_due.unwrap_or(i64::MAX));
    alerts
}

pub(crate) fn on_assets_updated(state: FieldServiceState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(asset_id) = data
        .get("asset_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok())
    else {
        // Only the asset id is required: without it the event cannot
        // be matched to any equipment, but the skip is recorded
        // instead of swallowed.
        ctx.add_result(json!({ "skipped": "asset_id missing or invalid; no equipment synced" }));
        return;
    };
    let Some(warranty_end) = data
        .get("warranty_end")
        .and_then(|value| value.as_str())
        .and_then(|raw| NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok())
    else {
        // A missing or unparseable warranty date no longer drops
        // the event silently: the observation is recorded with an
        // explanation of what was skipped.
        ctx.add_result(json!({
            "asset_id": asset_id.to_string(),
            "synced_equipment": 0,
            "skipped": "warranty_end missing or invalid; no warranty date synced",
        }));
        return;
    };
    let mut updated = 0;
    for mut entry in state.equipment.iter_mut() {
        if entry.value().asset_id == Some(asset_id) {
            entry.value_mut().warranty_end = Some(warranty_end);
            entry.value_mut().updated_at = Utc::now();
            updated += 1;
        }
    }
    ctx.add_result(json!({ "synced_equipment": updated, "asset_id": asset_id.to_string() }));
}

pub(crate) fn on_plans_check_requested(state: FieldServiceState, ctx: &mut HookContext) {
    let alerts = run_due_check(&state);
    ctx.add_result(json!({
        "checked": true,
        "alerts": alerts.len(),
        "alerts_detail": alerts,
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::field_service::{FieldServiceState, MaintenancePlan};
    use chrono::{DateTime, NaiveDate, Utc};

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    fn datetime(year: i32, month: u32, day: u32) -> DateTime<Utc> {
        DateTime::from_naive_utc_and_offset(
            date(year, month, day).and_hms_opt(0, 0, 0).unwrap(),
            Utc,
        )
    }

    fn plan(interval_value: i64, interval_unit: &str) -> MaintenancePlan {
        MaintenancePlan {
            id: Uuid::new_v4(),
            equipment_id: Uuid::new_v4(),
            template_id: None,
            name: "Quarterly check".to_string(),
            description: None,
            interval_value,
            interval_unit: interval_unit.to_string(),
            last_service_at: Some(datetime(2026, 1, 1)),
            usage_at_last_service: None,
            next_due_at: None,
            next_due_usage: None,
            is_active: true,
            created_by: "test".to_string(),
            created_at: datetime(2026, 1, 1),
            updated_at: datetime(2026, 1, 1),
        }
    }

    #[test]
    fn add_months_clamps_to_the_last_day_of_the_target_month() {
        assert_eq!(add_months(date(2025, 1, 31), 1), date(2025, 2, 28));
        assert_eq!(add_months(date(2024, 1, 31), 1), date(2024, 2, 29));
        assert_eq!(add_months(date(2025, 12, 15), 1), date(2026, 1, 15));
        assert_eq!(add_months(date(2025, 3, 31), -1), date(2025, 2, 28));
    }

    #[test]
    fn days_in_month_handles_leap_years() {
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2025, 2), 28);
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(days_in_month(1900, 2), 28);
        assert_eq!(days_in_month(2025, 4), 30);
        assert_eq!(days_in_month(2025, 1), 31);
    }

    #[test]
    fn priority_rank_orders_urgent_first() {
        assert!(priority_rank("urgent") < priority_rank("high"));
        assert!(priority_rank("high") < priority_rank("normal"));
        assert!(priority_rank("normal") < priority_rank("low"));
        assert!(priority_rank("low") < priority_rank("unknown-priority"));
        assert_eq!(priority_rank("unknown-priority"), 4);
    }

    #[test]
    fn valid_slug_accepts_only_lowercase_slugs() {
        assert!(valid_slug("pump-a_1"));
        assert!(!valid_slug("Pump"));
        assert!(!valid_slug("has space"));
        assert!(!valid_slug(""));
        assert!(!valid_slug(&"a".repeat(MAX_SLUG_LEN + 1)));
    }

    #[test]
    fn require_text_trims_and_rejects_empty_or_long_values() {
        assert_eq!(require_text("  ok  ", "name", 10).unwrap(), "ok");
        assert!(require_text("   ", "name", 10).is_err());
        assert!(require_text("toolong", "name", 3).is_err());
        assert_eq!(optional_text(&None, "name", 10).unwrap(), None);
        assert_eq!(
            optional_text(&Some(" x ".to_string()), "name", 10).unwrap(),
            Some("x".to_string())
        );
    }

    #[test]
    fn compute_next_due_uses_days_for_day_based_plans() {
        let state = FieldServiceState::seeded();
        let (due, usage) = compute_next_due(&state, &plan(7, "days")).unwrap();
        assert_eq!(due, Some(datetime(2026, 1, 8)));
        assert_eq!(usage, None);
    }

    #[test]
    fn compute_next_due_uses_calendar_months_for_month_plans() {
        let state = FieldServiceState::seeded();
        let (due, usage) = compute_next_due(&state, &plan(1, "months")).unwrap();
        assert_eq!(due, Some(datetime(2026, 2, 1)));
        assert_eq!(usage, None);
    }

    #[test]
    fn compute_next_due_returns_a_usage_target_for_usage_plans() {
        let state = FieldServiceState::seeded();
        let mut usage_plan = plan(100, "usage_hours");
        usage_plan.usage_at_last_service = Some(50.0);
        let (due, usage) = compute_next_due(&state, &usage_plan).unwrap();
        assert_eq!(due, None);
        assert_eq!(usage, Some(150.0));
    }

    #[test]
    fn compute_next_due_rejects_unknown_interval_units() {
        let state = FieldServiceState::seeded();
        assert!(compute_next_due(&state, &plan(1, "fortnights")).is_err());
    }

    #[test]
    fn overdue_and_days_until_due_follow_the_due_date() {
        let state = FieldServiceState::seeded();
        let mut overdue = plan(7, "days");
        overdue.next_due_at = Some(datetime(2020, 1, 1));
        assert!(plan_is_overdue(&state, &overdue));
        assert!(days_until_due(&overdue).unwrap() < 0);
        let mut inactive = plan(7, "days");
        inactive.is_active = false;
        inactive.next_due_at = Some(datetime(2020, 1, 1));
        assert!(!plan_is_overdue(&state, &inactive));
    }

    #[test]
    fn run_due_check_emits_due_soon_event_for_due_soon_plans() {
        use std::sync::Mutex;

        let state = FieldServiceState::seeded();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let bus = Arc::new(HookBus::new());
        for event in ["field_service.plan.overdue", "field_service.plan.due_soon"] {
            let seen = Arc::clone(&seen);
            bus.register(
                event,
                "test",
                Arc::new(move |ctx: &mut HookContext| {
                    seen.lock()
                        .expect("seen lock poisoned")
                        .push(ctx.event.clone());
                }),
            );
        }
        *state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&bus));
        let mut due_soon = plan(7, "days");
        due_soon.next_due_at = Some(Utc::now() + chrono::Duration::days(3));
        state.plans.insert(due_soon.id, due_soon.clone());
        let mut overdue = plan(7, "days");
        overdue.next_due_at = Some(Utc::now() - chrono::Duration::days(1));
        state.plans.insert(overdue.id, overdue.clone());
        let alerts = run_due_check(&state);
        assert_eq!(alerts.len(), 2);
        let seen = seen.lock().expect("seen lock poisoned");
        assert_eq!(
            seen.iter()
                .filter(|event| event.as_str() == "field_service.plan.overdue")
                .count(),
            1,
            "only the overdue plan may emit field_service.plan.overdue"
        );
        assert!(seen.contains(&"field_service.plan.due_soon".to_string()));
    }

    #[test]
    fn on_assets_updated_records_a_result_when_warranty_end_is_missing() {
        let state = FieldServiceState::seeded();
        let asset_id = Uuid::new_v4();
        let mut ctx = HookContext::new(
            "assets.updated",
            json!({ "asset_id": asset_id.to_string() }),
        );
        on_assets_updated(state, &mut ctx);
        assert_eq!(ctx.results.len(), 1);
        assert_eq!(ctx.results[0]["asset_id"], json!(asset_id.to_string()));
        assert_eq!(ctx.results[0]["synced_equipment"], json!(0));
        assert!(ctx.results[0]["skipped"]
            .as_str()
            .is_some_and(|text| text.contains("warranty_end")));

        let mut empty_ctx = HookContext::new("assets.updated", json!({}));
        on_assets_updated(FieldServiceState::seeded(), &mut empty_ctx);
        assert_eq!(empty_ctx.results.len(), 1);
        assert!(empty_ctx.results[0]["skipped"]
            .as_str()
            .is_some_and(|text| text.contains("asset_id")));
    }

    #[test]
    fn on_assets_updated_syncs_warranty_end_when_present() {
        use crate::modules::field_service::Equipment;

        let state = FieldServiceState::seeded();
        let asset_id = Uuid::new_v4();
        let equipment = Equipment {
            id: Uuid::new_v4(),
            asset_id: Some(asset_id),
            equipment_type_id: Uuid::new_v4(),
            name: "Pump A".to_string(),
            serial_number: None,
            location: None,
            install_date: None,
            warranty_end: None,
            status: "active".to_string(),
            usage_reading: None,
            usage_unit: None,
            created_by: "test".to_string(),
            created_at: datetime(2026, 1, 1),
            updated_at: datetime(2026, 1, 1),
        };
        state.equipment.insert(equipment.id, equipment.clone());
        let mut ctx = HookContext::new(
            "assets.updated",
            json!({ "asset_id": asset_id.to_string(), "warranty_end": "2027-05-01" }),
        );
        on_assets_updated(state.clone(), &mut ctx);
        assert_eq!(ctx.results[0]["synced_equipment"], json!(1));
        let stored = state
            .equipment
            .get(&equipment.id)
            .expect("equipment still stored");
        assert_eq!(stored.value().warranty_end, Some(date(2027, 5, 1)));
    }
}
