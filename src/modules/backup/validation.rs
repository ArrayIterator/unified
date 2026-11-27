// Backup module - validation helpers.
//
// Split from `src/modules/backup.rs`; the module root re-exports the public API of this module.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use actix_web::HttpRequest;
use serde_json::json;
use uuid::Uuid;

use super::constants::{MAX_NAME_LEN, MAX_PATH_LEN, NOTIFICATION_REQUESTED};
use super::state::{BackupState};
use super::types::{BackupJob, BackupRun};

/// Validate one field of a five-field cron expression against its
/// numeric bounds. Accepts `*`, `*/N`, `N`, `N-M`, `N-M/S`, and
/// comma-separated lists of those.
pub(crate) fn valid_cron_field(field: &str, min: u32, max: u32) -> bool {
    if field.is_empty() {
        return false;
    }
    field.split(',').all(|item| {
        if item.is_empty() {
            return false;
        }
        let (base, step) = match item.split_once('/') {
            Some((base, step)) => (base, Some(step)),
            None => (item, None),
        };
        if let Some(step) = step {
            let Ok(step_value) = step.parse::<u32>() else {
                return false;
            };
            if step_value == 0 {
                return false;
            }
        }
        if base == "*" {
            return true;
        }
        if let Some((low, high)) = base.split_once('-') {
            let (Ok(low_value), Ok(high_value)) =
                (low.parse::<u32>(), high.parse::<u32>())
            else {
                return false;
            };
            return low_value >= min && high_value <= max && low_value <= high_value;
        }
        match base.parse::<u32>() {
            Ok(value) => value >= min && value <= max,
            Err(_) => false,
        }
    })
}

/// A schedule is empty (manual-only) or a five-field cron expression:
/// minute hour day-of-month month day-of-week.
pub(crate) fn valid_schedule(schedule: &str) -> bool {
    let schedule = schedule.trim();
    if schedule.is_empty() {
        return true;
    }
    let fields: Vec<&str> = schedule.split_whitespace().collect();
    if fields.len() != 5 {
        return false;
    }
    let bounds = [(0, 59), (0, 23), (1, 31), (1, 12), (0, 7)];
    fields
        .iter()
        .zip(bounds)
        .all(|(field, (min, max))| valid_cron_field(field, min, max))
}

pub(crate) fn valid_storage_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

pub(crate) fn valid_database_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub(crate) fn valid_config_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_PATH_LEN
        && key.split('.').all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
}

// Helpers: caller, capabilities, company scoping.

/// The server-verified caller (see `unified_core::caller::Caller`):
/// the user id identifies the actor, the server-resolved role is the
/// role slug or id the capability check runs against. The company
/// scope is NOT carried here: handlers resolve it from the tenant
/// context plus the verified caller via `caller_company` (C3 pilot 3).
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

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check
/// (C3 pilot 3).
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// - a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here: only the verified caller (company resolved
/// from the database at authentication time) and the
/// middleware-resolved tenant context are sources.
pub(crate) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    match (&tenant.company_id, &verified) {
        (Some(tenant_company), Some(verified_company))
            if tenant_company != verified_company =>
        {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified.clone()),
    }
}

/// Resolve the caller's company for a WRITE (C3, strict by
/// default): a write requires a resolved company scope, so an
/// unresolved caller is refused with 403 instead of silently
/// writing a global (unscoped) record. Mutation handlers call
/// this instead of `caller_company`; read handlers keep
/// `caller_company`, and the named carve-outs (self-service
/// writes, settings with no company field, scan/test triggers,
/// platform surfaces) keep it too.
pub(crate) fn require_company_for_write(
    tenant: &unified_core::tenancy::TenantContext,
    req: &actix_web::HttpRequest,
) -> UnifiedResult<Option<String>> {
    match caller_company(tenant, req)? {
        Some(company) => Ok(Some(company)),
        None => Err(UnifiedError::forbidden(
            "A write requires a resolved company scope",
        )),
    }
}


/// True when a stored record belongs to the caller's company scope.
/// With no caller scope (an unresolved caller) only global records
/// match: unscoped records are visible, every company-owned record
/// is denied (the shared `CallerScope::Unresolved` rule). With a
/// scope, records carrying exactly that company match, plus unscoped
/// (global/legacy) records, which are shared content visible to any
/// resolved scope (C3, rule A). This
/// is the generic form of `job_visible` / `run_visible` below.
pub(crate) fn in_company_scope(
    record_company_id: &Option<String>,
    caller_company: &Option<String>,
) -> bool {
    match caller_company {
        None => record_company_id.is_none(),
        Some(company) => record_company_id.is_none()
                || record_company_id.as_deref() == Some(company.as_str()),
    }
}

/// Fail closed (403) when a stored record is outside the caller's
/// company scope. Single-record reads and mutations call this
/// right after loading the record; list handlers use
/// `in_company_scope` as a filter instead.
pub(crate) fn require_record_in_company_scope(
    record_company_id: &Option<String>,
    caller_company: &Option<String>,
) -> UnifiedResult<()> {
    if in_company_scope(record_company_id, caller_company) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "The record belongs to a different company",
        ))
    }
}

/// Fail closed (403) when a stored record is outside the caller's
/// company scope FOR A WRITE (C3, strict by default).
/// Unlike the read check above (rule A), a write requires an exact
/// company match: a company caller may write only their own
/// company's records - global (unscoped) records are platform-only
/// - and an unresolved caller may write nothing. Mutation handlers
/// call this right after loading the record they mutate; handlers
/// that only read the record, or load a parent to scope a child
/// write, keep the read check.
pub(crate) fn require_record_writable_in_company_scope(
    record_company_id: &Option<String>,
    caller_company: &Option<String>,
) -> UnifiedResult<()> {
    match (record_company_id, caller_company) {
        (Some(record), Some(caller)) if record == caller => Ok(()),
        _ => Err(UnifiedError::forbidden(
            "A write requires an exact company match: the record belongs to a different company or is a shared (global) record",
        )),
    }
}


/// Verify a request body's `company_id` against the caller's
/// company scope and return the company id to store. Fails closed
/// (403) when the body names a company other than the caller's.
/// When the body omits the company, the caller's scope is adopted
/// so the record is never stored unscoped under a scoped caller.
/// When no caller scope is known, a body company is refused: an
/// unresolved caller may not claim a company (the shared
/// `CallerScope::Unresolved` rule); omitting the company is refused (C3):
/// a write requires a resolved company scope. Backup create bodies carry no company field today,
/// so creates stamp the caller scope directly; this helper covers
/// any body company a future create/update gains.
pub(crate) fn require_company_match(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
    body_company_id: &Option<String>,
) -> UnifiedResult<Option<String>> {
    let caller = caller_company(tenant, req)?;
    require_company_match_for(&caller, body_company_id)
}

/// `require_company_match` against an already-resolved caller
/// scope, for handlers that computed `caller_company` first.
pub(crate) fn require_company_match_for(
    caller: &Option<String>,
    body_company_id: &Option<String>,
) -> UnifiedResult<Option<String>> {
    match (body_company_id, caller) {
        (Some(body), Some(caller_company)) if body != caller_company => Err(
            UnifiedError::forbidden("company_id does not match the caller's company scope"),
        ),
        (Some(body), Some(_)) => Ok(Some(body.clone())),
        (Some(body), None) => Err(UnifiedError::forbidden(
            "company_id does not match the caller's company scope",
        )),
        (None, Some(caller)) => Ok(Some(caller.clone())),
        (None, None) => Err(UnifiedError::forbidden(
            "A write requires a resolved company scope",
        )),
    }
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
/// (Same fail-closed shape as the ai module's capability check.)
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

/// A job is visible to the caller when the job belongs to the
/// caller's company. A caller with no resolvable company
/// (unresolved) sees only jobs with no company (global jobs);
/// platform access is never the default for a missing company
/// (the shared `CallerScope::Unresolved` rule). Jobs with no
/// company are global jobs and are invisible to company callers.
pub(crate) fn job_visible(job: &BackupJob, company: &Option<String>) -> bool {
    match company {
        None => job.company_id.is_none(),
        Some(company_id) => job.company_id.as_deref() == Some(company_id.as_str()),
    }
}

/// A run is visible under the same rule as a job: an unresolved
/// caller (no company) sees only runs with no company.
pub(crate) fn run_visible(run: &BackupRun, company: &Option<String>) -> bool {
    match company {
        None => run.company_id.is_none(),
        Some(company_id) => run.company_id.as_deref() == Some(company_id.as_str()),
    }
}

/// Load one job the caller may see; cross-company or missing jobs
/// both answer 404 so existence is never leaked.
pub(crate) fn visible_job(state: &BackupState, id: &str, company: &Option<String>) -> UnifiedResult<BackupJob> {
    let job = state
        .jobs
        .get(id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Backup job not found"))?;
    if job_visible(&job, company) {
        Ok(job)
    } else {
        Err(UnifiedError::not_found("Backup job not found"))
    }
}

pub(crate) fn visible_run(state: &BackupState, id: &str, company: &Option<String>) -> UnifiedResult<BackupRun> {
    let run = state
        .runs
        .get(id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Backup run not found"))?;
    if run_visible(&run, company) {
        Ok(run)
    } else {
        Err(UnifiedError::not_found("Backup run not found"))
    }
}

/// Emit a `notification.requested` alert to the `system` user, the
/// same payload shape the certificates module uses for expiry
/// alerts. Used for verification and drill failures, which the
/// notifications module has no listener for yet.
pub(crate) fn notify_system(hooks: &HookBus, kind: &str, title: String, body: String) {
    hooks.emit(
        NOTIFICATION_REQUESTED,
        json!({
            "kind": kind,
            "user_id": "system",
            "title": title,
            "body": body,
            "module": "backup",
            "category": "backup",
            "priority": "high",
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_cron_field_bounds() {
        assert!(valid_cron_field("*", 0, 59));
        assert!(valid_cron_field("59", 0, 59));
        assert!(!valid_cron_field("60", 0, 59));
        assert!(valid_cron_field("0-30", 0, 59));
        assert!(!valid_cron_field("30-10", 0, 59));
        assert!(!valid_cron_field("*/0", 0, 59));
        assert!(valid_cron_field("*/5", 0, 59));
        assert!(valid_cron_field("1,2,3", 0, 59));
        assert!(!valid_cron_field("1,,2", 0, 59));
        assert!(!valid_cron_field("", 0, 59));
        assert!(valid_cron_field("1-2/3", 0, 59));
        assert!(!valid_cron_field("0", 1, 31));
    }

    #[test]
    fn valid_schedule_shapes() {
        assert!(valid_schedule(""));
        assert!(valid_schedule("0 0 * * *"));
        assert!(valid_schedule("  0 0 * * *  "));
        assert!(!valid_schedule("0 0 * *"));
        assert!(!valid_schedule("61 0 * * *"));
        assert!(!valid_schedule("0 25 * * *"));
        assert!(!valid_schedule("0 0 32 * *"));
    }

    #[test]
    fn valid_storage_name_rules() {
        assert!(valid_storage_name("s3-main"));
        assert!(valid_storage_name("a.b_c-d"));
        assert!(!valid_storage_name(""));
        assert!(!valid_storage_name("bad name"));
        assert!(!valid_storage_name("bad/name"));
    }

    #[test]
    fn valid_database_name_rules() {
        assert!(valid_database_name("app_db"));
        assert!(valid_database_name("UPPER-1"));
        assert!(!valid_database_name(""));
        assert!(!valid_database_name("bad.name"));
        assert!(!valid_database_name(&"a".repeat(64)));
    }

    #[test]
    fn valid_config_key_rules() {
        assert!(valid_config_key("a.b_c-d"));
        assert!(!valid_config_key("a..b"));
        assert!(!valid_config_key(""));
        assert!(!valid_config_key("a b"));
    }

    #[test]
    fn job_and_run_visibility_denies_company_records_to_unresolved_callers() {
        let now = chrono::Utc::now();
        let job = BackupJob {
            id: "job-1".to_string(),
            name: "nightly".to_string(),
            company_id: Some("co-1".to_string()),
            kind: BackupKind::Database,
            database_name: String::new(),
            paths: Vec::new(),
            config_keys: Vec::new(),
            target: "local".to_string(),
            schedule: String::new(),
            retention_keep_last: 7,
            retention_keep_days: 30,
            enabled: true,
            scheduling: SchedulingOutcome {
                enqueued: false,
                message: "Not scheduled yet.".to_string(),
            },
            status: BackupStatus::Pending,
            checksum: None,
            size_bytes: None,
            storage_key: None,
            last_run_id: None,
            started_at: None,
            finished_at: None,
            error: None,
            created_at: now,
            updated_at: now,
        };
        assert!(job_visible(&job, &Some("co-1".to_string())));
        assert!(!job_visible(&job, &Some("co-2".to_string())));
        assert!(!job_visible(&job, &None));
        let global = BackupJob {
            company_id: None,
            ..job.clone()
        };
        assert!(job_visible(&global, &None));
        assert!(!job_visible(&global, &Some("co-1".to_string())));
        let run = BackupRun {
            id: "run-1".to_string(),
            job_id: job.id.clone(),
            job_name: job.name.clone(),
            company_id: Some("co-1".to_string()),
            kind: BackupKind::Database,
            target: "local".to_string(),
            storage_key: None,
            status: BackupStatus::Running,
            checksum: None,
            size_bytes: None,
            file_count: None,
            verified: None,
            last_verified_at: None,
            manifest_verified: None,
            verification_error: None,
            last_drill_at: None,
            drill_verified: None,
            encryption: Some("none".to_string()),
            started_at: now,
            finished_at: None,
            error: None,
            created_at: now,
        };
        assert!(run_visible(&run, &Some("co-1".to_string())));
        assert!(!run_visible(&run, &None));
    }
}

#[cfg(test)]
mod tenant_guard_tests {
    use super::*;
    use actix_web::test::TestRequest;
    use actix_web::HttpMessage;

    /// Request carrying the middleware-resolved tenant context the
    /// tenant-resolution middleware would attach, plus a verified
    /// caller (optionally with a server-resolved company).
    fn tenant_request(company_id: &str, caller_company: Option<&str>) -> HttpRequest {
        let req = TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: company_id.to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        let mut caller =
            unified_core::auth::Caller::verified(test_user_uuid("alice"), Vec::new());
        if let Some(company) = caller_company {
            caller = caller.with_company(company.to_string());
        }
        req.extensions_mut().insert(caller);
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let req = tenant_request("", None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(tenant.company_id.is_none());
    }

    #[test]
    fn caller_company_prefers_tenant_company_over_verified_caller() {
        let req = tenant_request("co-1", Some("co-1"));
        let tenant = require_tenant(&req).unwrap();
        assert_eq!(
            caller_company(&tenant, &req).unwrap().as_deref(),
            Some("co-1")
        );
    }

    #[test]
    fn caller_company_falls_back_to_verified_caller_company() {
        let req = tenant_request("", Some("co-1"));
        let tenant = require_tenant(&req).unwrap();
        assert_eq!(
            caller_company(&tenant, &req).unwrap().as_deref(),
            Some("co-1")
        );
    }

    #[test]
    fn caller_company_rejects_tenant_and_verified_company_mismatch() {
        let req = tenant_request("co-1", Some("co-2"));
        let tenant = require_tenant(&req).unwrap();
        assert!(caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn in_company_scope_is_fail_closed_for_unresolved_callers() {
        assert!(in_company_scope(&None, &None));
        assert!(!in_company_scope(&Some("co-1".to_string()), &None));
        assert!(in_company_scope(
            &Some("co-1".to_string()),
            &Some("co-1".to_string())
        ));
        assert!(!in_company_scope(
            &Some("co-1".to_string()),
            &Some("co-2".to_string())
        ));
        assert!(in_company_scope(&None, &Some("co-1".to_string())));
    }

    #[test]
    fn require_company_match_for_refuses_writes_from_unresolved_callers() {
        assert!(
            require_company_match_for(&None, &Some("co-1".to_string())).is_err()
        );
        assert!(
            require_company_match_for(&Some("co-1".to_string()), &Some("co-1".to_string()))
                .is_ok()
        );
        assert!(
            require_company_match_for(&Some("co-1".to_string()), &Some("co-2".to_string()))
                .is_err()
        );
        assert_eq!(
            require_company_match_for(&Some("co-1".to_string()), &None).unwrap(),
            Some("co-1".to_string())
        );
        assert!(require_company_match_for(&None, &None).is_err());
    }
}
