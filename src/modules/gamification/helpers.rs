// Gamification — Helpers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the events module.
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

/// Resolve the tenant for this request, failing closed when the
/// tenant-resolution middleware attached no tenant context (401)
/// or the resolved tenant id is not a uuidv7 UUID (403). Every
/// tenant-data handler in this module calls this FIRST, before
/// caller identity or any capability check, so an unresolved
/// request never reaches a store or an emit. There is no fallback
/// to the `public` schema and no default tenant. See
/// `unified_core::tenancy` (`TENANT_SCOPE_CONTRACT`).
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// — a request must not carry one company in its tenant context and
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

/// Verify a request body's `company_id` against the caller's
/// company scope and return the company id to store. Fails closed
/// (403) when the body names a company other than the caller's.
/// When the body omits the company, the caller's scope is adopted
/// so the record is never stored unscoped under a scoped caller.
/// When no caller scope is known (neither the tenant context nor
/// the verified caller carries a company), a body company is refused:
/// an unresolved caller may not claim a company (the shared
/// `CallerScope::Unresolved` rule); omitting the company stores
/// a global row.
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
        (None, caller) => Ok(caller.clone()),
    }
}

/// True when a stored record belongs to the caller's company
/// scope. With no caller scope (an unresolved caller) only
/// global records match: unscoped records are visible, every
/// company-owned record is denied (the shared
/// `CallerScope::Unresolved` rule). With a scope, only records carrying
/// exactly that company match: unscoped (legacy) records are
/// hidden from scoped callers rather than leaked across companies.
pub(crate) fn in_company_scope(
    record_company_id: &Option<String>,
    caller_company: &Option<String>,
) -> bool {
    match caller_company {
        None => record_company_id.is_none(),
        Some(company) => record_company_id.as_deref() == Some(company.as_str()),
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

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the events module's capability check).
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

pub(crate) fn can_manage(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_GAMIFICATION_MANAGE)
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

pub(crate) fn opt_string(data: &Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

pub(crate) fn valid_action(action: &str) -> bool {
    KNOWN_ACTIONS.contains(&action)
}

pub(crate) fn load_rule(state: &GamificationState, raw_id: &str) -> UnifiedResult<GamificationRule> {
    let id = parse_uuid(raw_id, "rule")?;
    state
        .rules
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Rule not found: {id}")))
}

pub(crate) fn load_badge(state: &GamificationState, raw_id: &str) -> UnifiedResult<GamificationBadge> {
    let id = parse_uuid(raw_id, "badge")?;
    state
        .badges
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Badge not found: {id}")))
}

pub(crate) fn load_season(state: &GamificationState, raw_id: &str) -> UnifiedResult<GamificationSeason> {
    let id = parse_uuid(raw_id, "season")?;
    state
        .seasons
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Season not found: {id}")))
}

pub(crate) fn load_reward(state: &GamificationState, raw_id: &str) -> UnifiedResult<GamificationReward> {
    let id = parse_uuid(raw_id, "reward")?;
    state
        .rewards
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Reward not found: {id}")))
}

pub(crate) fn load_reward_request(
    state: &GamificationState,
    raw_id: &str,
) -> UnifiedResult<GamificationRewardRequest> {
    let id = parse_uuid(raw_id, "reward request")?;
    state
        .reward_requests
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Reward request not found: {id}")))
}

/// A user's balance is the sum of their ledger entries, optionally
/// scoped to one company and/or one season. No balance column is
/// ever stored or trusted.
pub(crate) fn user_balance(
    state: &GamificationState,
    user_id: &str,
    company_id: Option<&str>,
    season_id: Option<Uuid>,
) -> i64 {
    state
        .ledger
        .iter()
        .filter(|entry| {
            let row = entry.value();
            if row.user_id != user_id {
                return false;
            }
            if let Some(company) = company_id {
                if row.company_id.as_deref() != Some(company) {
                    return false;
                }
            }
            if let Some(season) = season_id {
                if row.season_id != Some(season) {
                    return false;
                }
            }
            true
        })
        .map(|entry| entry.value().points)
        .sum()
}

/// The active season for a company (None = global scope), when one
/// exists. At most one can be active per company (enforced on
/// start), so the first match is the answer.
pub(crate) fn active_season_for(state: &GamificationState, company_id: Option<&str>) -> Option<GamificationSeason> {
    state
        .seasons
        .iter()
        .map(|entry| entry.value().clone())
        .find(|season| season.status == "active" && season.company_id.as_deref() == company_id)
}

/// Points the user already earned from one rule today (UTC day),
/// for the daily cap check.
pub(crate) fn points_from_rule_today(state: &GamificationState, user_id: &str, rule_id: Uuid) -> i64 {
    let today = Utc::now().date_naive();
    state
        .ledger
        .iter()
        .filter(|entry| {
            let row = entry.value();
            row.user_id == user_id
                && row.rule_id == Some(rule_id)
                && row.created_at.date_naive() == today
                && row.points > 0
        })
        .map(|entry| entry.value().points)
        .sum()
}

/// How many times the user performed one action (ledger entries
/// with positive points for that action), for badge thresholds.
pub(crate) fn action_count(state: &GamificationState, user_id: &str, action: &str) -> i64 {
    state
        .ledger
        .iter()
        .filter(|entry| {
            let row = entry.value();
            row.user_id == user_id && row.action == action && row.points > 0
        })
        .count() as i64
}

/// Award every badge whose criteria the user now meets for the
/// given action. An award is an audited record: one row per
/// (badge, user), never silent, never duplicated.
pub(crate) fn check_badges(state: &GamificationState, user_id: &str, action: &str) {
    let badges: Vec<GamificationBadge> = state
        .badges
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|badge| badge.criteria_action == action)
        .collect();
    if badges.is_empty() {
        return;
    }
    let count = action_count(state, user_id, action);
    // Badge ids this user already holds, collected in one pass over
    // the awards table. Before this change every candidate badge
    // rescanned the whole table, so checking badges cost
    // badges x awards full-map scans.
    let mut awarded: HashSet<Uuid> = state
        .awards
        .iter()
        .filter(|entry| entry.value().user_id == user_id)
        .map(|entry| entry.value().badge_id)
        .collect();
    for badge in badges {
        if count < badge.criteria_threshold {
            continue;
        }
        if awarded.contains(&badge.id) {
            continue;
        }
        let now = Utc::now();
        let award = GamificationAward {
            id: Uuid::now_v7(),
            badge_id: badge.id,
            user_id: user_id.to_string(),
            reason: format!(
                "Reached {count} {} (threshold {})",
                action, badge.criteria_threshold
            ),
            awarded_at: now,
            created_at: now,
        };
        state.awards.insert(award.id, award.clone());
        awarded.insert(badge.id);
        state.emit(
            "gamification.badge.awarded",
            json!({
                "award_id": award.id.to_string(),
                "badge_id": badge.id.to_string(),
                "badge_name": badge.name,
                "user_id": user_id,
                "action": action,
                "count": count,
            }),
        );
        state.notify(
            "gamification.badge.awarded",
            user_id,
            &format!("Badge earned: {}", badge.name),
            &format!(
                "You earned the \"{}\" badge for {} ({} times).",
                badge.name, action, count
            ),
            Some("/api/v1/gamification/progress".to_string()),
        );
    }
}

/// Has this user already been awarded for one action from one
/// external source (same `source_ref`, e.g. the same order id)?
/// Used to make event-driven awards idempotent: one order awards
/// once, no matter how many order events fire for it.
pub(crate) fn has_source_award(
    state: &GamificationState,
    user_id: &str,
    action: &str,
    source_ref: &str,
) -> bool {
    state.ledger.iter().any(|entry| {
        let row = entry.value();
        row.user_id == user_id
            && row.action == action
            && row.source_ref.as_deref() == Some(source_ref)
    })
}

/// Same as `has_source_award`, but only for entries created on the
/// current UTC day. Used where the same source may legitimately
/// award again on a later day (a ticket resolved again after a
/// reopen), but never twice on one day.
pub(crate) fn has_source_award_today(
    state: &GamificationState,
    user_id: &str,
    action: &str,
    source_ref: &str,
) -> bool {
    let today = Utc::now().date_naive();
    state.ledger.iter().any(|entry| {
        let row = entry.value();
        row.user_id == user_id
            && row.action == action
            && row.source_ref.as_deref() == Some(source_ref)
            && row.created_at.date_naive() == today
    })
}

/// Append one ledger entry and emit the matching points event:
/// `gamification.points.earned` for positive points,
/// `gamification.points.deducted` for negative points, and no
/// event for a zero-point entry. The caller is responsible for
/// the points value (rule engine, adjustment, or reward
/// deduction). `balance_after` is the balance in the same
/// (company, season) scope as the entry being appended.
pub(crate) fn append_ledger_entry(
    state: &GamificationState,
    user_id: &str,
    company_id: Option<String>,
    department_id: Option<String>,
    rule_id: Option<Uuid>,
    action: &str,
    points: i64,
    season_id: Option<Uuid>,
    source_ref: Option<String>,
    reason: Option<String>,
) -> PointsLedgerEntry {
    let balance_after =
        user_balance(state, user_id, company_id.as_deref(), season_id) + points;
    let entry = PointsLedgerEntry {
        id: Uuid::now_v7(),
        user_id: user_id.to_string(),
        company_id,
        department_id,
        rule_id,
        action: action.to_string(),
        points,
        balance_after,
        season_id,
        source_ref,
        reason,
        created_at: Utc::now(),
    };
    state.ledger.insert(entry.id, entry.clone());
    if points != 0 {
        let event = if points > 0 {
            "gamification.points.earned"
        } else {
            "gamification.points.deducted"
        };
        state.emit(
            event,
            json!({
                "entry_id": entry.id.to_string(),
                "user_id": entry.user_id,
                "action": entry.action,
                "points": entry.points,
                "balance_after": entry.balance_after,
                "rule_id": entry.rule_id.map(|id| id.to_string()),
                "season_id": entry.season_id.map(|id| id.to_string()),
                "company_id": entry.company_id,
                "department_id": entry.department_id,
            }),
        );
    }
    entry
}

/// Outcome of running the rules engine for one action.
#[derive(Debug, Clone, Serialize)]
pub struct AwardOutcome {
    pub awarded: bool,
    pub points: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The rules engine: find the best matching active rule for the
/// action (a company-specific rule wins over a global one), apply
/// its daily cap, append the ledger entry, and check badges.
/// Returns `awarded: false` with a reason when no rule matches
/// or the daily cap is already reached — never an error, so hook
/// listeners stay safe. `source_ref` records the external id of
/// the thing being awarded for (order id, ticket id); it is stored
/// on the ledger entry so listeners can dedup repeat events.
pub(crate) fn award_points_for_action(
    state: &GamificationState,
    user_id: &str,
    action: &str,
    company_id: Option<String>,
    department_id: Option<String>,
    reason: Option<String>,
    source_ref: Option<String>,
) -> AwardOutcome {
    let rules: Vec<GamificationRule> = state
        .rules
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|rule| rule.active && rule.action == action)
        .collect();
    let rule = rules
        .iter()
        .find(|rule| rule.company_id.as_deref() == company_id.as_deref())
        .or_else(|| rules.iter().find(|rule| rule.company_id.is_none()));
    let Some(rule) = rule else {
        return AwardOutcome {
            awarded: false,
            points: 0,
            entry_id: None,
            rule_id: None,
            reason: Some(format!("No active rule matches action \"{action}\"")),
        };
    };
    let mut points = rule.points;
    if let Some(cap) = rule.daily_cap {
        let earned_today = points_from_rule_today(state, user_id, rule.id);
        if earned_today >= cap {
            return AwardOutcome {
                awarded: false,
                points: 0,
                entry_id: None,
                rule_id: Some(rule.id.to_string()),
                reason: Some(format!(
                    "Daily cap of {cap} points for rule \"{}\" already reached",
                    rule.name
                )),
            };
        }
        if earned_today + points > cap {
            points = cap - earned_today;
        }
    }
    if points <= 0 {
        return AwardOutcome {
            awarded: false,
            points: 0,
            entry_id: None,
            rule_id: Some(rule.id.to_string()),
            reason: Some("Rule awards zero points".to_string()),
        };
    }
    let season_id = active_season_for(state, company_id.as_deref()).map(|season| season.id);
    let entry = append_ledger_entry(
        state,
        user_id,
        company_id,
        department_id,
        Some(rule.id),
        action,
        points,
        season_id,
        source_ref,
        reason,
    );
    check_badges(state, user_id, action);
    AwardOutcome {
        awarded: true,
        points,
        entry_id: Some(entry.id.to_string()),
        rule_id: Some(rule.id.to_string()),
        reason: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tenant context the tenant-resolution middleware would
    /// attach for a request on a tenant host
    /// (`unified_core::middleware::TenantContext`, the exact struct
    /// `tenancy::TenantContext::from_http_request` reads from the
    /// request extensions). The company is empty, so the resolved
    /// scope is unscoped.
    fn tenant_request() -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: String::new(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            },
        );
        req
    }

        /// A request with a tenant context carrying `tenant_company`
    /// (empty = no tenant company), a verified caller carrying
    /// `verified_company` (empty = no company resolved), and a
    /// forged `X-Company-Id` header when `forged_header` is
    /// non-empty. The header must never affect the scope.
    fn scoped_request(
        tenant_company: &str,
        verified_company: &str,
        forged_header: &str,
    ) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let mut req = actix_web::test::TestRequest::default();
        if !forged_header.is_empty() {
            req = req.insert_header(("x-company-id", forged_header));
        }
        let req = req.to_http_request();
        if !verified_company.is_empty() {
            req.extensions_mut().insert(
                unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new())
                    .with_company(verified_company.to_string()),
            );
        }
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: tenant_company.to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req
    }

    #[test]
    fn caller_company_prefers_tenant_company_over_verified_caller() {
        let req = scoped_request("company-a", "company-a", "");
        let tenant = super::require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            super::caller_company(&tenant, &req).unwrap(),
            Some("company-a".to_string())
        );
    }

    #[test]
    fn caller_company_uses_verified_caller_when_tenant_has_none() {
        let req = scoped_request("", "company-b", "");
        let tenant = super::require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            super::caller_company(&tenant, &req).unwrap(),
            Some("company-b".to_string())
        );
    }

    #[test]
    fn caller_company_rejects_verified_caller_mismatch() {
        let req = scoped_request("company-a", "company-b", "");
        let tenant = super::require_tenant(&req).expect("tenant context must resolve");
        assert!(super::caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn caller_company_ignores_a_forged_company_header() {
        let req = scoped_request("", "", "company-forged");
        let tenant = super::require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(super::caller_company(&tenant, &req).unwrap(), None);
    }

#[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = actix_web::test::TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let req = tenant_request();
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
        assert_eq!(caller_company(&tenant, &req).unwrap(), None);
    }

    #[test]
    fn in_company_scope_hides_unscoped_records_from_scoped_callers() {
        assert!(in_company_scope(
            &Some("c1".to_string()),
            &Some("c1".to_string())
        ));
        assert!(!in_company_scope(
            &Some("c2".to_string()),
            &Some("c1".to_string())
        ));
        assert!(!in_company_scope(&None, &Some("c1".to_string())));
        assert!(in_company_scope(&None, &None));
        assert!(!in_company_scope(&Some("c2".to_string()), &None));
    }

    #[test]
    fn require_company_match_for_refuses_body_company_from_unresolved_callers() {
        let body = Some("c1".to_string());
        assert!(require_company_match_for(&None, &body).is_err());
        assert!(require_company_match_for(&None, &None).unwrap().is_none());
        assert_eq!(
            require_company_match_for(&Some("c1".to_string()), &body).unwrap(),
            Some("c1".to_string())
        );
        assert!(
            require_company_match_for(&Some("c1".to_string()), &Some("c2".to_string())).is_err()
        );
    }

    fn ledger_entry(user: &str, action: &str, points: i64) -> PointsLedgerEntry {
        PointsLedgerEntry {
            id: uuid::Uuid::new_v4(),
            user_id: user.to_string(),
            company_id: None,
            department_id: None,
            rule_id: None,
            action: action.to_string(),
            points,
            balance_after: points,
            season_id: None,
            source_ref: None,
            reason: None,
            created_at: chrono::Utc::now(),
        }
    }

    fn season(status: &str, company_id: Option<&str>) -> GamificationSeason {
        GamificationSeason {
            id: uuid::Uuid::new_v4(),
            name: "Season".to_string(),
            company_id: company_id.map(str::to_string),
            starts_at: chrono::Utc::now(),
            ends_at: chrono::Utc::now(),
            status: status.to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn valid_action_accepts_known_actions() {
        assert!(valid_action("ticket_closed"));
        assert!(valid_action("sale_completed"));
        assert!(valid_action("manual_adjustment"));
    }

    #[test]
    fn valid_action_rejects_unknown_actions() {
        assert!(!valid_action("teleport"));
        assert!(!valid_action(""));
    }

    #[test]
    fn user_balance_sums_all_entries() {
        let state = GamificationState::default();
        let first = ledger_entry("u1", "sale_completed", 10);
        let second = ledger_entry("u1", "sale_completed", 5);
        state.ledger.insert(first.id, first);
        state.ledger.insert(second.id, second);
        assert_eq!(user_balance(&state, "u1", None, None), 15);
    }

    #[test]
    fn user_balance_is_scoped_to_one_user() {
        let state = GamificationState::default();
        let mine = ledger_entry("u1", "sale_completed", 10);
        let theirs = ledger_entry("u2", "sale_completed", 100);
        state.ledger.insert(mine.id, mine);
        state.ledger.insert(theirs.id, theirs);
        assert_eq!(user_balance(&state, "u1", None, None), 10);
        assert_eq!(user_balance(&state, "u2", None, None), 100);
        assert_eq!(user_balance(&state, "u3", None, None), 0);
    }

    #[test]
    fn user_balance_is_scoped_by_company() {
        let state = GamificationState::default();
        let mut first = ledger_entry("u1", "sale_completed", 10);
        first.company_id = Some("c1".to_string());
        let mut second = ledger_entry("u1", "sale_completed", 7);
        second.company_id = Some("c2".to_string());
        state.ledger.insert(first.id, first);
        state.ledger.insert(second.id, second);
        assert_eq!(user_balance(&state, "u1", Some("c1"), None), 10);
        assert_eq!(user_balance(&state, "u1", Some("c2"), None), 7);
        assert_eq!(user_balance(&state, "u1", None, None), 17);
    }

    #[test]
    fn user_balance_is_scoped_by_season() {
        let state = GamificationState::default();
        let season_id = uuid::Uuid::new_v4();
        let mut first = ledger_entry("u1", "sale_completed", 10);
        first.season_id = Some(season_id);
        let second = ledger_entry("u1", "sale_completed", 3);
        state.ledger.insert(first.id, first);
        state.ledger.insert(second.id, second);
        assert_eq!(user_balance(&state, "u1", None, Some(season_id)), 10);
        assert_eq!(user_balance(&state, "u1", None, None), 13);
    }

    #[test]
    fn active_season_found_for_matching_scope() {
        let state = GamificationState::default();
        let active = season("active", None);
        state.seasons.insert(active.id, active);
        assert!(active_season_for(&state, None).is_some());
    }

    #[test]
    fn active_season_is_scoped_by_company() {
        let state = GamificationState::default();
        let company_season = season("active", Some("c1"));
        state.seasons.insert(company_season.id, company_season);
        assert!(active_season_for(&state, Some("c1")).is_some());
        assert!(active_season_for(&state, None).is_none());
        assert!(active_season_for(&state, Some("c2")).is_none());
    }

    #[test]
    fn action_count_counts_only_positive_entries() {
        let state = GamificationState::default();
        for points in [10, 5, -3, 0] {
            let entry = ledger_entry("u1", "sale_completed", points);
            state.ledger.insert(entry.id, entry);
        }
        let other = ledger_entry("u1", "ticket_closed", 10);
        state.ledger.insert(other.id, other);
        assert_eq!(action_count(&state, "u1", "sale_completed"), 2);
        assert_eq!(action_count(&state, "u1", "ticket_closed"), 1);
    }

    #[test]
    fn load_rule_returns_error_for_missing_rule() {
        let state = GamificationState::default();
        let missing = uuid::Uuid::new_v4().to_string();
        assert!(load_rule(&state, &missing).is_err());
    }

    fn badge(action: &str, threshold: i64) -> GamificationBadge {
        GamificationBadge {
            id: uuid::Uuid::new_v4(),
            name: "Test Badge".to_string(),
            description: "Test badge".to_string(),
            criteria_action: action.to_string(),
            criteria_threshold: threshold,
            icon: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn check_badges_awards_each_badge_once_and_skips_already_awarded() {
        let state = GamificationState::default();
        let first = badge("sale_completed", 2);
        let second = badge("sale_completed", 5);
        state.badges.insert(first.id, first.clone());
        state.badges.insert(second.id, second.clone());
        // Two qualifying actions: only the threshold-2 badge is earned.
        for _ in 0..2 {
            let entry = ledger_entry("u1", "sale_completed", 10);
            state.ledger.insert(entry.id, entry);
        }
        check_badges(&state, "u1", "sale_completed");
        assert_eq!(state.awards.len(), 1);
        assert!(state
            .awards
            .iter()
            .any(|entry| entry.value().badge_id == first.id));
        // Five actions: the threshold-5 badge is earned too.
        for _ in 0..3 {
            let entry = ledger_entry("u1", "sale_completed", 10);
            state.ledger.insert(entry.id, entry);
        }
        check_badges(&state, "u1", "sale_completed");
        assert_eq!(state.awards.len(), 2);
        assert!(state
            .awards
            .iter()
            .any(|entry| entry.value().badge_id == second.id));
        // Re-checking awards nothing a second time.
        check_badges(&state, "u1", "sale_completed");
        assert_eq!(state.awards.len(), 2);
    }

    #[test]
    fn has_source_award_detects_a_duplicate_order() {
        let state = GamificationState::default();
        let mut entry = ledger_entry("u1", "sale_completed", 10);
        entry.source_ref = Some("order-42".to_string());
        state.ledger.insert(entry.id, entry);
        assert!(has_source_award(&state, "u1", "sale_completed", "order-42"));
        assert!(!has_source_award(&state, "u1", "sale_completed", "order-43"));
        assert!(!has_source_award(&state, "u1", "ticket_closed", "order-42"));
        assert!(!has_source_award(&state, "u2", "sale_completed", "order-42"));
    }

    #[test]
    fn has_source_award_today_ignores_older_entries() {
        let state = GamificationState::default();
        let mut yesterday = ledger_entry("u1", "ticket_closed", 10);
        yesterday.source_ref = Some("ticket-7".to_string());
        yesterday.created_at = chrono::Utc::now() - chrono::Duration::days(1);
        state.ledger.insert(yesterday.id, yesterday);
        assert!(!has_source_award_today(&state, "u1", "ticket_closed", "ticket-7"));
        // The same ticket is still a duplicate ever, just not today.
        assert!(has_source_award(&state, "u1", "ticket_closed", "ticket-7"));
        let mut today = ledger_entry("u1", "ticket_closed", 10);
        today.source_ref = Some("ticket-8".to_string());
        state.ledger.insert(today.id, today);
        assert!(has_source_award_today(&state, "u1", "ticket_closed", "ticket-8"));
    }

    #[test]
    fn append_ledger_entry_balance_after_is_scoped_to_company_and_season() {
        let state = GamificationState::default();
        let season_one = uuid::Uuid::new_v4();
        let season_two = uuid::Uuid::new_v4();
        let mut first = ledger_entry("u1", "sale_completed", 10);
        first.company_id = Some("c1".to_string());
        first.season_id = Some(season_one);
        let mut second = ledger_entry("u1", "sale_completed", 100);
        second.company_id = Some("c1".to_string());
        second.season_id = Some(season_two);
        let mut third = ledger_entry("u1", "sale_completed", 1000);
        third.company_id = Some("c2".to_string());
        third.season_id = Some(season_one);
        state.ledger.insert(first.id, first);
        state.ledger.insert(second.id, second);
        state.ledger.insert(third.id, third);
        // Only the c1 / season_one entries count (10), not the
        // other season (100) or the other company (1000).
        let entry = append_ledger_entry(
            &state,
            "u1",
            Some("c1".to_string()),
            None,
            None,
            "sale_completed",
            5,
            Some(season_one),
            Some("order-1".to_string()),
            None,
        );
        assert_eq!(entry.balance_after, 15);
        assert_eq!(entry.source_ref.as_deref(), Some("order-1"));
    }

    #[test]
    fn append_ledger_entry_emits_earned_only_for_positive_points() {
        let state = GamificationState::default();
        let seen: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let bus = Arc::new(HookBus::new());
        for event in ["gamification.points.earned", "gamification.points.deducted"] {
            let seen = std::sync::Arc::clone(&seen);
            let event_name = event.to_string();
            bus.register(
                event,
                "test",
                std::sync::Arc::new(move |_ctx: &mut HookContext| {
                    seen.lock().expect("seen lock poisoned").push(event_name.clone());
                }),
            );
        }
        *state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&bus));
        append_ledger_entry(&state, "u1", None, None, None, "sale_completed", 10, None, None, None);
        append_ledger_entry(&state, "u1", None, None, None, "reward_redemption", -4, None, None, None);
        append_ledger_entry(&state, "u1", None, None, None, "manual_adjustment", 0, None, None, None);
        let seen = seen.lock().expect("seen lock poisoned").clone();
        assert_eq!(
            seen,
            vec![
                "gamification.points.earned".to_string(),
                "gamification.points.deducted".to_string(),
            ],
            "a deduction must emit deducted, never earned, and zero points emit nothing"
        );
    }
}
