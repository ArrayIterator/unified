// Plan helpers: loaders, entitlement, usage, metering.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use crate::modules::licensing;
use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;

use super::*;

// Plan helpers: loaders, entitlement, usage, metering.

pub(crate) fn load_plan(state: &StorageManagerState, raw_id: &str) -> UnifiedResult<StoragePlan> {
    let id = parse_uuid(raw_id, "storage plan")?;
    state
        .plans
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Storage plan not found: {id}")))
}

pub(crate) fn assignment_key(company_id: Option<&str>, user_id: &str) -> String {
    format!("{}|{user_id}", company_id.unwrap_or(""))
}

pub(crate) fn load_assignment(
    state: &StorageManagerState,
    raw_id: &str,
) -> UnifiedResult<PlanAssignment> {
    let id = parse_uuid(raw_id, "plan assignment")?;
    state
        .plan_assignments
        .iter()
        .find(|entry| entry.value().id == id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Plan assignment not found: {id}")))
}

/// The buyer's ACTIVE assignment, if any: the most recently
/// assigned active row for the buyer key. Cancelled (or expired)
/// rows are skipped, never overwritten - re-assigning a buyer whose
/// only row is cancelled inserts a fresh row with a new id and the
/// cancelled row stays resolvable via `load_assignment`.
pub(crate) fn active_assignment_for_buyer(
    state: &StorageManagerState,
    company_id: Option<&str>,
    user_id: &str,
) -> Option<PlanAssignment> {
    let now = Utc::now();
    state
        .plan_assignments
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|assignment| {
            assignment.status == "active"
                && assignment.company_id.as_deref() == company_id
                && assignment.user_id == user_id
                && !assignment
                    .expires_at
                    .is_some_and(|expires| expires <= now)
        })
        .max_by(|a, b| a.assigned_at.cmp(&b.assigned_at))
}

/// Resolve the company for `get_my_plan`. Platform admins (see
/// `is_platform_admin`, C3 decision (a)) may ask about
/// any buyer via `?company_id=`; a non-admin's company comes ONLY
/// from the verified caller scope - the query parameter is ignored
/// for them, so a non-admin cannot read another company's plan,
/// assignment status or live usage.
pub(crate) fn resolve_my_plan_company(
    is_admin: bool,
    query_company_id: Option<&str>,
    verified_company: Option<&str>,
) -> Option<String> {
    if is_admin {
        query_company_id.map(str::to_string)
    } else {
        verified_company.map(str::to_string)
    }
}

/// Usage for one buyer in one `YYYY-MM` period, from the daily
/// rollup (`usage_daily`) instead of live objects. For each
/// (bucket, company, user, module) scope the latest row dated
/// inside the period is the end-of-period snapshot for that scope;
/// the period totals are the sum of those snapshots. A backdated
/// report therefore states that period's usage, not today's.
/// BYO objects are not in the daily rollup (their usage lives on
/// the connection row), so past-period reports cover hub objects
/// only - the current period instead uses live `buyer_usage`,
/// which includes BYO.
pub(crate) fn period_usage_from_daily(
    state: &StorageManagerState,
    company_id: Option<&str>,
    user_id: &str,
    period: &str,
) -> (i64, i64) {
    use std::collections::HashMap;
    let mut latest: HashMap<(Uuid, String, String, String), (String, i64, i64)> =
        HashMap::new();
    for entry in state.usage_daily.iter() {
        let row = entry.value();
        if !row.date.starts_with(period) {
            continue;
        }
        let matches = match company_id {
            Some(company) => row.company_id.as_deref() == Some(company),
            None => row.user_id == user_id && row.company_id.is_none(),
        };
        if !matches {
            continue;
        }
        let scope = (
            row.bucket_id,
            row.company_id.clone().unwrap_or_default(),
            row.user_id.clone(),
            row.module_id.clone(),
        );
        let replace = match latest.get(&scope) {
            Some((date, _, _)) => row.date > *date,
            None => true,
        };
        if replace {
            latest.insert(scope, (row.date.clone(), row.bytes_total, row.objects_total));
        }
    }
    let mut bytes: i64 = 0;
    let mut files: i64 = 0;
    for (_, (_, row_bytes, row_files)) in latest {
        bytes += row_bytes;
        files += row_files;
    }
    (bytes, files)
}

/// Bytes + files one buyer currently stores: recorded hub objects
/// plus BYO objects. A company-scoped buyer counts the whole
/// company; a personal buyer counts only their company-less
/// objects. Used for plan-change quota checks and metering.
pub(crate) fn buyer_usage(
    state: &StorageManagerState,
    company_id: Option<&str>,
    user_id: &str,
) -> (i64, i64) {
    let mut bytes: i64 = 0;
    let mut files: i64 = 0;
    for entry in state.objects.iter() {
        let object = entry.value();
        let matches = match company_id {
            Some(company) => object.company_id.as_deref() == Some(company),
            None => object.owner_user == user_id && object.company_id.is_none(),
        };
        if matches {
            bytes += object.size_bytes;
            files += 1;
        }
    }
    for entry in state.byo_objects.iter() {
        let object = entry.value();
        let matches = match company_id {
            Some(company) => object.company_id.as_deref() == Some(company),
            None => object.owner_user == user_id && object.company_id.is_none(),
        };
        if matches {
            bytes += object.size_bytes;
            files += 1;
        }
    }
    (bytes, files)
}

/// Emit one metering report event for billing. The caller also
/// stores the report row; the event is the billing handoff (hook
/// bus only - this module never calls billing directly).
pub(crate) fn emit_metering(
    state: &StorageManagerState,
    report: &MeteringReport,
    source: &str,
) {
    state.emit(
        events::METERING_REPORTED,
        json!({
            "report_id": report.id.to_string(),
            "company_id": report.company_id,
            "user_id": report.user_id,
            "plan_id": report.plan_id.map(|id| id.to_string()),
            "period": report.period,
            "bytes_stored": report.bytes_stored,
            "files_stored": report.files_stored,
            "source": source,
            "reported_at": report.reported_at.to_rfc3339(),
        }),
    );
}

/// Gate a plan-gated action on the Storage Manager module
/// entitlement for one company, through the licensing module.
///
/// When the module is registered (the integrator step), the
/// canonical registry gate `licensing::check_module_entitlement`
/// answers, including its granted / denied events. Before
/// registration the registry lookup would answer 404 for a module
/// that does exist, so the same license match is checked directly
/// with `licensing::has_module_entitlement` instead. Both paths
/// fail closed: no active license means no plan-gated use.
pub(crate) fn require_storage_module_entitlement(
    ctx: &AppContext,
    company_id: &str,
) -> UnifiedResult<()> {
    if ctx.modules.get(STORAGE_MODULE_ID).is_some() {
        return licensing::check_module_entitlement(ctx, company_id, STORAGE_MODULE_ID);
    }
    if licensing::has_module_entitlement(ctx, company_id, STORAGE_MODULE_ID) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Module \"{STORAGE_MODULE_ID}\" requires an active license entitlement for this company"
        ))
        .with_metadata("module_id", serde_json::json!(STORAGE_MODULE_ID))
        .with_metadata("company_id", serde_json::json!(company_id)))
    }
}

/// Gate on one plan's billing product entitlement, through the
/// `product.entitlement.check` hook - the same contract the
/// meetings module uses for paid rooms: the first hook result must
/// carry `{"entitled": true}`. No answer fails closed with 503;
/// an explicit error answers 502; anything else answers 403.
/// Plans without a `product_id` need no product check.
pub(crate) fn check_plan_product_entitlement(
    ctx: &AppContext,
    plan: &StoragePlan,
    user_id: &str,
    company_id: Option<&str>,
) -> UnifiedResult<()> {
    let Some(product_id) = &plan.product_id else {
        return Ok(());
    };
    let answer = ctx.hooks.emit(
        "product.entitlement.check",
        json!({
            "product_id": product_id,
            "user_id": user_id,
            "company_id": company_id,
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("entitled").and_then(Value::as_bool) == Some(true) => Ok(()),
        Some(result) if result.get("error").is_some() => {
            let reason = result
                .get("error")
                .and_then(|value| value.as_str())
                .unwrap_or("entitlement check failed");
            Err(UnifiedError::new(
                format!("Entitlement check failed: {reason}"),
                502,
            ))
        }
        Some(_) => Err(UnifiedError::forbidden(format!(
            "A paid product entitlement for \"{product_id}\" is required to use storage plan \"{}\"",
            plan.name
        ))),
        None => Err(UnifiedError::new(
            "Entitlement service did not answer product.entitlement.check: no handler is registered",
            503,
        )),
    }
}

/// Refuse a plan change when current usage exceeds the new plan's
/// quota. Returns the 409 error to surface, or `Ok(())`.
pub(crate) fn check_plan_change_quota(
    state: &StorageManagerState,
    plan: &StoragePlan,
    company_id: Option<&str>,
    user_id: &str,
) -> UnifiedResult<()> {
    let (bytes, files) = buyer_usage(state, company_id, user_id);
    if bytes > plan.quota_bytes {
        state.emit(
            events::PLAN_QUOTA_REFUSED,
            json!({
                "plan_id": plan.id.to_string(),
                "plan_name": plan.name,
                "company_id": company_id,
                "user_id": user_id,
                "bytes_stored": bytes,
                "quota_bytes": plan.quota_bytes,
                "reason": "bytes exceed the new plan quota",
            }),
        );
        return Err(UnifiedError::conflict(format!(
            "Cannot change to plan \"{}\": {} bytes are stored, which exceeds the plan quota of {} bytes. Delete data or choose a larger plan first.",
            plan.name, bytes, plan.quota_bytes
        )));
    }
    if plan.max_files > 0 && files > plan.max_files {
        state.emit(
            events::PLAN_QUOTA_REFUSED,
            json!({
                "plan_id": plan.id.to_string(),
                "plan_name": plan.name,
                "company_id": company_id,
                "user_id": user_id,
                "files_stored": files,
                "max_files": plan.max_files,
                "reason": "file count exceeds the new plan quota",
            }),
        );
        return Err(UnifiedError::conflict(format!(
            "Cannot change to plan \"{}\": {} files are stored, which exceeds the plan limit of {} files. Delete files or choose a larger plan first.",
            plan.name, files, plan.max_files
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn assignment_key_scopes_by_company_or_user_only() {
        assert_eq!(assignment_key(Some("acme"), "u-1"), "acme|u-1");
        assert_eq!(assignment_key(None, "u-1"), "|u-1");
        assert_eq!(assignment_key(Some(""), "u-1"), "|u-1");
    }

    #[test]
    fn my_plan_company_ignores_query_for_non_admins() {
        // Admins may ask about any company via the query string.
        assert_eq!(
            resolve_my_plan_company(true, Some("other"), Some("acme")),
            Some("other".to_string())
        );
        assert_eq!(resolve_my_plan_company(true, None, Some("acme")), None);
        // Non-admins get their own company from the header only:
        // a ?company_id= for another company is ignored.
        assert_eq!(
            resolve_my_plan_company(false, Some("other"), Some("acme")),
            Some("acme".to_string())
        );
        assert_eq!(resolve_my_plan_company(false, Some("other"), None), None);
    }

    fn daily_row(
        date: &str,
        bucket_id: Uuid,
        company_id: Option<&str>,
        user_id: &str,
        bytes_total: i64,
        objects_total: i64,
    ) -> UsageDaily {
        let now = Utc::now();
        UsageDaily {
            date: date.to_string(),
            bucket_id,
            company_id: company_id.map(str::to_string),
            user_id: user_id.to_string(),
            module_id: "files".to_string(),
            bytes_total,
            objects_total,
            bytes_delta: bytes_total,
            objects_delta: objects_total,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn period_usage_uses_the_rollup_snapshot_not_live_usage() {
        let state = StorageManagerState::default();
        let bucket = Uuid::now_v7();
        // January snapshot: 100 bytes / 2 files; February snapshot
        // (same scope, later row): 900 bytes / 9 files.
        state.usage_daily.insert(
            "2026-01-15|b|acme|u-1|files".to_string(),
            daily_row("2026-01-15", bucket, Some("acme"), "u-1", 100, 2),
        );
        state.usage_daily.insert(
            "2026-01-31|b|acme|u-1|files".to_string(),
            daily_row("2026-01-31", bucket, Some("acme"), "u-1", 150, 3),
        );
        state.usage_daily.insert(
            "2026-02-10|b|acme|u-1|files".to_string(),
            daily_row("2026-02-10", bucket, Some("acme"), "u-1", 900, 9),
        );
        // January report uses the latest January row (150/3), not
        // the February/live totals.
        assert_eq!(
            period_usage_from_daily(&state, Some("acme"), "u-1", "2026-01"),
            (150, 3)
        );
        assert_eq!(
            period_usage_from_daily(&state, Some("acme"), "u-1", "2026-02"),
            (900, 9)
        );
        // Another company sees nothing in this scope.
        assert_eq!(
            period_usage_from_daily(&state, Some("other"), "u-1", "2026-01"),
            (0, 0)
        );
        // A month with no rows reports zero, not live usage.
        assert_eq!(
            period_usage_from_daily(&state, Some("acme"), "u-1", "2025-12"),
            (0, 0)
        );
    }

    fn assignment(id: Uuid, status: &str, company_id: Option<&str>) -> PlanAssignment {
        let now = Utc::now();
        PlanAssignment {
            id,
            plan_id: Uuid::now_v7(),
            company_id: company_id.map(str::to_string),
            user_id: "u-1".to_string(),
            status: status.to_string(),
            assigned_by: "admin".to_string(),
            assigned_at: now,
            expires_at: None,
            cancelled_at: None,
            cancel_reason: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn cancelled_assignment_is_retained_and_not_the_active_one() {
        let state = StorageManagerState::default();
        let cancelled = assignment(Uuid::now_v7(), "cancelled", Some("acme"));
        state.plan_assignments.insert(cancelled.id, cancelled.clone());
        // No active assignment while only the cancelled row exists.
        assert!(active_assignment_for_buyer(&state, Some("acme"), "u-1").is_none());
        // The cancelled row is still resolvable by id.
        assert_eq!(
            load_assignment(&state, &cancelled.id.to_string()).unwrap().id,
            cancelled.id
        );
        // A later active assignment coexists with the cancelled one.
        let active = assignment(Uuid::now_v7(), "active", Some("acme"));
        state.plan_assignments.insert(active.id, active.clone());
        assert_eq!(
            active_assignment_for_buyer(&state, Some("acme"), "u-1").unwrap().id,
            active.id
        );
        assert_eq!(state.plan_assignments.len(), 2);
        assert!(load_assignment(&state, &cancelled.id.to_string()).is_ok());
    }
}
