//! Loyalty - caller checks, validation, balances and earn logic.
//!
//! Part of the `loyalty` module split; see `src/modules/loyalty.rs`.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::error::UnifiedResult;
use unified_core::json;
use unified_core::pagination::PageResult;
use unified_core::pagination::PaginationParams;
use actix_web::HttpRequest;
use actix_web::HttpResponse;
use chrono::DateTime;
use chrono::Duration;
use chrono::Utc;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;
use uuid::Uuid;

use super::constants::MAX_AWARD_POINTS;
use super::constants::PERM_LOYALTY_MANAGE;
use super::types::ExpiryMode;
use super::types::LedgerKind;
use super::types::LoyaltyEarnRule;
use super::types::LoyaltyLedgerEntry;
use super::types::LoyaltyProgram;
use super::types::LoyaltyRedemption;
use super::types::LoyaltyState;
use super::types::LoyaltyTier;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn require_permission(
    ctx: &AppContext,
    role: &str,
    permission: &str,
) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 3).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`).
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
/// middleware-resolved tenant context are sources (header trust
/// removal is task 317's job elsewhere).
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
        (None, verified) => Ok(verified),
    }
}

/// Verify a request body's company id against the caller's
/// company scope and return the company id to store. Fails closed
/// (403) when the body names a company other than the caller's.
/// When the body omits the company, the caller's scope is adopted
/// so the record is never stored unscoped under a scoped caller.
/// When no caller scope is known, a body company is refused: an
/// unresolved caller may not claim a company (the shared
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
/// `CallerScope::Unresolved` rule). With a scope, only records
/// carrying exactly that company match; unscoped (legacy) records
/// are hidden from scoped callers.
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

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!(
            "{field} must not be empty"
        )));
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

pub(crate) fn paginate<T: Serialize + Clone>(
    items: Vec<T>,
    params: &PaginationParams,
) -> PageResult<T> {
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

/// Money: finite, non-negative, rounded to 4 decimal places.
pub(crate) fn valid_price(price: f64) -> UnifiedResult<f64> {
    if !price.is_finite() || price < 0.0 {
        return Err(UnifiedError::bad_request(
            "price must be a finite number >= 0",
        ));
    }
    Ok((price * 10_000.0).round() / 10_000.0)
}

pub(crate) fn round_money(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// Currency: 3-letter ISO code, uppercased.
pub(crate) fn valid_currency(currency: &str) -> UnifiedResult<String> {
    let upper = currency.trim().to_uppercase();
    if upper.len() != 3 || !upper.chars().all(|c| c.is_ascii_uppercase()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO code such as \"USD\"",
        ));
    }
    Ok(upper)
}

pub(crate) fn is_manager(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_LOYALTY_MANAGE)
}

/// Customer-scoped access: a manager may see any customer; any
/// other caller may only see their own customer id. Enforced
/// server-side on every customer-scoped read.
pub(crate) fn require_customer_access(
    ctx: &AppContext,
    caller: &Caller,
    customer_id: &str,
) -> UnifiedResult<()> {
    if is_manager(ctx, &caller.role) || caller.actor == customer_id {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "A customer may only access their own loyalty records",
        ))
    }
}

pub(crate) fn load_program(state: &LoyaltyState, raw_id: &str) -> UnifiedResult<LoyaltyProgram> {
    let id = parse_uuid(raw_id, "loyalty program")?;
    state
        .programs
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Loyalty program not found: {id}")))
}

/// Load a program and verify it is in the caller scope: company
/// callers read their own and global programs only, unresolved
/// callers read global programs only. A missing program answers
/// `not_found`; an out-of-scope program answers `forbidden`.
/// Earn rules, tiers, ledger rows, and redemptions carry no
/// company of their own; they are scoped through their parent
/// program with this loader.
pub(crate) fn require_program_in_scope(
    state: &LoyaltyState,
    raw_id: &str,
    scope: &Option<String>,
) -> UnifiedResult<LoyaltyProgram> {
    let program = load_program(state, raw_id)?;
    require_record_in_company_scope(&program.company_id, scope)?;
    Ok(program)
}

pub(crate) fn load_earn_rule(
    state: &LoyaltyState,
    program_id: Uuid,
    raw_id: &str,
) -> UnifiedResult<LoyaltyEarnRule> {
    let id = parse_uuid(raw_id, "earn rule")?;
    state
        .earn_rules
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|rule| rule.program_id == program_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Earn rule not found: {id}")))
}

pub(crate) fn load_tier(
    state: &LoyaltyState,
    program_id: Uuid,
    raw_id: &str,
) -> UnifiedResult<LoyaltyTier> {
    let id = parse_uuid(raw_id, "loyalty tier")?;
    state
        .tiers
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|tier| tier.program_id == program_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Loyalty tier not found: {id}")))
}

pub(crate) fn load_redemption(
    state: &LoyaltyState,
    raw_id: &str,
) -> UnifiedResult<LoyaltyRedemption> {
    let id = parse_uuid(raw_id, "redemption")?;
    state
        .redemptions
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Redemption not found: {id}")))
}

/// The active program for a company, falling back to the active
/// default program (no company) when the company has none. `None`
/// when no program matches at all.
pub(crate) fn active_program_for(
    state: &LoyaltyState,
    company_id: Option<&str>,
) -> Option<LoyaltyProgram> {
    if let Some(company_id) = company_id {
        if let Some(program) = state
            .programs
            .iter()
            .map(|entry| entry.value().clone())
            .find(|program| program.active && program.company_id.as_deref() == Some(company_id))
        {
            return Some(program);
        }
    }
    state
        .programs
        .iter()
        .map(|entry| entry.value().clone())
        .find(|program| program.active && program.company_id.is_none())
}

/// Balance of one customer in one program, computed from the
/// ledger (sum of all signed deltas). Never stored anywhere else.
fn balance(state: &LoyaltyState, program_id: Uuid, customer_id: &str) -> i64 {
    state
        .ledger
        .iter()
        .filter(|entry| {
            entry.value().program_id == program_id && entry.value().customer_id == customer_id
        })
        .map(|entry| entry.value().points)
        .sum()
}

/// Lifetime earned points (sum of positive earn rows) of one
/// customer in one program.
pub(crate) fn lifetime_points(state: &LoyaltyState, program_id: Uuid, customer_id: &str) -> i64 {
    state
        .ledger
        .iter()
        .filter(|entry| {
            entry.value().program_id == program_id
                && entry.value().customer_id == customer_id
                && entry.value().kind == LedgerKind::Earn
        })
        .map(|entry| entry.value().points.max(0))
        .sum()
}

/// Lifetime spend (sum of order totals on purchase earn rows) of
/// one customer in one program.
pub(crate) fn lifetime_spend(state: &LoyaltyState, program_id: Uuid, customer_id: &str) -> f64 {
    state
        .ledger
        .iter()
        .filter(|entry| {
            entry.value().program_id == program_id
                && entry.value().customer_id == customer_id
                && entry.value().kind == LedgerKind::Earn
                && entry.value().earn_kind.as_deref() == Some("purchase")
        })
        .map(|entry| entry.value().reference_amount.unwrap_or(0.0))
        .sum()
}

/// The customer's tier: the highest qualifying tier (by
/// `sort_order`, then thresholds). `None` when no tier qualifies.
pub(crate) fn tier_for(
    state: &LoyaltyState,
    program_id: Uuid,
    customer_id: &str,
) -> Option<LoyaltyTier> {
    let points = lifetime_points(state, program_id, customer_id);
    let spend = lifetime_spend(state, program_id, customer_id);
    let mut tiers: Vec<LoyaltyTier> = state
        .tiers
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|tier| {
            tier.program_id == program_id
                && points >= tier.min_lifetime_points
                && spend >= tier.min_lifetime_spend
        })
        .collect();
    tiers.sort_by(|a, b| {
        b.sort_order
            .cmp(&a.sort_order)
            .then(b.min_lifetime_points.cmp(&a.min_lifetime_points))
            .then(
                b.min_lifetime_spend
                    .partial_cmp(&a.min_lifetime_spend)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });
    tiers.into_iter().next()
}

/// Balance + tier summary for one customer, as returned by the
/// balance endpoints.
pub(crate) fn balance_view(
    state: &LoyaltyState,
    program: &LoyaltyProgram,
    customer_id: &str,
) -> Value {
    let tier = tier_for(state, program.id, customer_id);
    json!({
        "program_id": program.id,
        "customer_id": customer_id,
        "balance": balance(state, program.id, customer_id),
        "lifetime_points": lifetime_points(state, program.id, customer_id),
        "lifetime_spend": round_money(lifetime_spend(state, program.id, customer_id)),
        "points_name": program.points_name,
        "currency": program.currency,
        "tier": tier,
    })
}

/// The lock that serialises ledger mutation. Every balance
/// check + insert, cap check + insert, and dedup check + insert
/// runs while holding this lock, so concurrent redeems, adjusts,
/// and duplicate hook deliveries cannot interleave between the
/// check and the insert and drive a balance below zero or award
/// twice. Callers must never hold it across an `await` or while
/// emitting a hook event (a listener re-entering a ledger
/// mutation on the same thread would deadlock); acquire it,
/// mutate, release, then emit.
pub(crate) fn ledger_lock(state: &LoyaltyState) -> std::sync::MutexGuard<'_, ()> {
    state.ledger_lock.lock().expect("ledger lock poisoned")
}

/// True when the ledger already holds an earn row of `earn_kind`
/// for `program_id` whose `reference_id` matches. Used to make
/// signup, referral, and purchase earns idempotent against a
/// duplicate hook delivery: the reference is the customer id for
/// signup / referral earns and the order id for purchase earns.
pub(crate) fn has_earn_for_reference(
    state: &LoyaltyState,
    program_id: Uuid,
    earn_kind: &str,
    reference_id: &str,
) -> bool {
    state.ledger.iter().any(|entry| {
        let value = entry.value();
        value.program_id == program_id
            && value.kind == LedgerKind::Earn
            && value.earn_kind.as_deref() == Some(earn_kind)
            && value.reference_id.as_deref() == Some(reference_id)
    })
}

/// Points for a purchase: `total * points_per_unit`, floored.
/// Rejects a non-finite or negative total (or rate) and clamps
/// the result to `MAX_AWARD_POINTS`, so an absurd total can never
/// award `i64::MAX` points through an `f64 as i64` saturation
/// cast (`f64::INFINITY as i64 == i64::MAX`).
pub(crate) fn points_from_total(total: f64, points_per_unit: f64) -> UnifiedResult<i64> {
    if !total.is_finite() || total < 0.0 {
        return Err(UnifiedError::bad_request(
            "order total must be a finite number >= 0",
        ));
    }
    if !points_per_unit.is_finite() || points_per_unit < 0.0 {
        return Err(UnifiedError::bad_request(
            "points_per_unit must be a finite number >= 0",
        ));
    }
    let points = total * points_per_unit;
    if !points.is_finite() {
        return Err(UnifiedError::bad_request(
            "computed points are not finite; order total or points_per_unit is too large",
        ));
    }
    let floored = points.floor();
    if floored >= MAX_AWARD_POINTS as f64 {
        return Ok(MAX_AWARD_POINTS);
    }
    Ok(floored as i64)
}

/// Points for a fixed / per-unit rule without an order total
/// (signup, referral, review): the rule's `fixed_points` when
/// set, else `points_per_unit` as a whole number. Rejects a
/// non-finite rate and clamps to `MAX_AWARD_POINTS` instead of
/// saturating through `f64 as i64`.
pub(crate) fn points_from_rate(points_per_unit: f64) -> UnifiedResult<i64> {
    if !points_per_unit.is_finite() || points_per_unit < 0.0 {
        return Err(UnifiedError::bad_request(
            "points_per_unit must be a finite number >= 0",
        ));
    }
    Ok((points_per_unit.floor() as i64).min(MAX_AWARD_POINTS))
}

/// Award size of a rule without an order total: `fixed_points`
/// when set (clamped), else the per-unit rate converted by
/// `points_from_rate`.
pub(crate) fn fixed_or_rate_points(rule: &LoyaltyEarnRule) -> UnifiedResult<i64> {
    match rule.fixed_points {
        Some(fixed) => Ok(fixed.max(0).min(MAX_AWARD_POINTS)),
        None => points_from_rate(rule.points_per_unit),
    }
}

/// Starting status of a new redemption. A discount redemption is
/// `pending`: its points are spent immediately, but the discount
/// is not confirmed until the store applies it (the store emits
/// `store.discount.applied`, handled in `hooks`). A product
/// redemption is `completed` at once: the store owns fulfilment
/// and there is no per-order confirmation to wait for.
pub(crate) fn initial_redemption_status(kind: &str) -> &'static str {
    if kind == "discount" {
        "pending"
    } else {
        "completed"
    }
}

/// Append one ledger row. The balance after the movement is
/// computed from the ledger, and a movement that would drive the
/// balance below zero is refused (points can never go negative).
/// The check and the insert run under `ledger_lock`, so two
/// concurrent spends cannot both pass the guard.
#[allow(clippy::too_many_arguments)]
pub(crate) fn append_ledger(
    state: &LoyaltyState,
    program: &LoyaltyProgram,
    customer_id: &str,
    kind: LedgerKind,
    points: i64,
    reason: &str,
    actor: &str,
    earn_kind: Option<String>,
    reference_id: Option<String>,
    reference_amount: Option<f64>,
    now: DateTime<Utc>,
) -> UnifiedResult<LoyaltyLedgerEntry> {
    let _guard = ledger_lock(state);
    append_ledger_locked(
        state,
        program,
        customer_id,
        kind,
        points,
        reason,
        actor,
        earn_kind,
        reference_id,
        reference_amount,
        now,
    )
}

/// `append_ledger` without locking: only call while holding
/// `ledger_lock` (for example from `earn_under_rule_locked`,
/// which already holds it for its cap checks).
#[allow(clippy::too_many_arguments)]
fn append_ledger_locked(
    state: &LoyaltyState,
    program: &LoyaltyProgram,
    customer_id: &str,
    kind: LedgerKind,
    points: i64,
    reason: &str,
    actor: &str,
    earn_kind: Option<String>,
    reference_id: Option<String>,
    reference_amount: Option<f64>,
    now: DateTime<Utc>,
) -> UnifiedResult<LoyaltyLedgerEntry> {
    if points == 0 {
        return Err(UnifiedError::bad_request("points must not be zero"));
    }
    let current = balance(state, program.id, customer_id);
    let after = current.saturating_add(points);
    if after < 0 {
        return Err(UnifiedError::bad_request(format!(
            "Insufficient points: balance is {current} and the movement is {points}"
        )));
    }
    let expires_at = match (kind, program.expiry_mode, program.expiry_days) {
        (LedgerKind::Earn, ExpiryMode::Fixed, Some(days)) => Some(now + Duration::days(days)),
        _ => None,
    };
    let entry = LoyaltyLedgerEntry {
        id: Uuid::new_v4(),
        program_id: program.id,
        customer_id: customer_id.to_string(),
        kind,
        points,
        balance_after: after,
        reason: reason.to_string(),
        actor: actor.to_string(),
        earn_kind,
        reference_id,
        reference_amount,
        expires_at,
        created_at: now,
    };
    state.ledger.insert(entry.id, entry.clone());
    Ok(entry)
}

/// Outcome of an earn computed under the ledger lock, before
/// any hook event is emitted.
pub(crate) struct EarnOutcome {
    pub entry: Option<LoyaltyLedgerEntry>,
    pub tier_before: Option<Uuid>,
    pub tier_after: Option<Uuid>,
}

/// Emit the events for an earn once the ledger lock is released.
/// Never called while holding the lock: a listener re-entering a
/// ledger mutation on the same thread would deadlock (and the
/// hook bus skips re-entered handlers).
pub(crate) fn emit_earn_outcome(
    state: &LoyaltyState,
    program: &LoyaltyProgram,
    customer_id: &str,
    outcome: &EarnOutcome,
    reason: &str,
) {
    let Some(entry) = &outcome.entry else {
        return;
    };
    let kind_name = entry.earn_kind.clone().unwrap_or_default();
    state.emit(
        "loyalty.points.earned",
        json!({
            "program_id": program.id,
            "customer_id": customer_id,
            "points": entry.points,
            "balance_after": entry.balance_after,
            "earn_kind": kind_name,
            "reason": reason,
        }),
    );
    if outcome.tier_before != outcome.tier_after {
        state.emit(
            "loyalty.tier.changed",
            json!({
                "program_id": program.id,
                "customer_id": customer_id,
                "previous_tier_id": outcome.tier_before,
                "tier_id": outcome.tier_after,
            }),
        );
    }
}

/// Award points under an earn rule, applying its caps. Returns the
/// ledger entry, or `None` when the caps leave nothing to award
/// (the caller reports that as a skip, not an error). Tier changes
/// are emitted when the award moves the customer to another tier.
/// The cap checks and the insert run atomically under
/// `ledger_lock`; the events are emitted after the lock is
/// released.
pub(crate) fn earn_under_rule(
    state: &LoyaltyState,
    program: &LoyaltyProgram,
    rule: &LoyaltyEarnRule,
    customer_id: &str,
    points: i64,
    reason: &str,
    actor: &str,
    reference_id: Option<String>,
    reference_amount: Option<f64>,
    now: DateTime<Utc>,
) -> UnifiedResult<Option<LoyaltyLedgerEntry>> {
    let outcome = {
        let _guard = ledger_lock(state);
        earn_under_rule_locked(
            state,
            program,
            rule,
            customer_id,
            points,
            reason,
            actor,
            reference_id,
            reference_amount,
            now,
        )?
    };
    emit_earn_outcome(state, program, customer_id, &outcome, reason);
    Ok(outcome.entry)
}

/// `earn_under_rule` without locking or emitting: only call while
/// holding `ledger_lock` (for example from a hook handler that
/// must check idempotency and award atomically). The caller emits
/// the outcome with `emit_earn_outcome` after releasing the lock.
#[allow(clippy::too_many_arguments)]
pub(crate) fn earn_under_rule_locked(
    state: &LoyaltyState,
    program: &LoyaltyProgram,
    rule: &LoyaltyEarnRule,
    customer_id: &str,
    mut points: i64,
    reason: &str,
    actor: &str,
    reference_id: Option<String>,
    reference_amount: Option<f64>,
    now: DateTime<Utc>,
) -> UnifiedResult<EarnOutcome> {
    // Defence in depth for callers that computed the award
    // elsewhere: a single award never exceeds MAX_AWARD_POINTS.
    points = points.min(MAX_AWARD_POINTS);
    if points <= 0 {
        return Ok(EarnOutcome {
            entry: None,
            tier_before: None,
            tier_after: None,
        });
    }
    let kind_name = rule.kind.as_str();
    // Daily cap: earns of this kind today (UTC calendar day).
    if let Some(cap) = rule.cap_per_day {
        let earned_today: i64 = state
            .ledger
            .iter()
            .filter(|entry| {
                let value = entry.value();
                value.program_id == program.id
                    && value.customer_id == customer_id
                    && value.kind == LedgerKind::Earn
                    && value.earn_kind.as_deref() == Some(kind_name)
                    && value.created_at.date_naive() == now.date_naive()
            })
            .map(|entry| entry.value().points.max(0))
            .sum();
        let remaining = cap.saturating_sub(earned_today);
        if remaining <= 0 {
            return Ok(EarnOutcome {
                entry: None,
                tier_before: None,
                tier_after: None,
            });
        }
        points = points.min(remaining);
    }
    // Lifetime cap: earns of this kind over all time.
    if let Some(cap) = rule.cap_total {
        let earned_total: i64 = state
            .ledger
            .iter()
            .filter(|entry| {
                let value = entry.value();
                value.program_id == program.id
                    && value.customer_id == customer_id
                    && value.kind == LedgerKind::Earn
                    && value.earn_kind.as_deref() == Some(kind_name)
            })
            .map(|entry| entry.value().points.max(0))
            .sum();
        let remaining = cap.saturating_sub(earned_total);
        if remaining <= 0 {
            return Ok(EarnOutcome {
                entry: None,
                tier_before: None,
                tier_after: None,
            });
        }
        points = points.min(remaining);
    }
    if points <= 0 {
        return Ok(EarnOutcome {
            entry: None,
            tier_before: None,
            tier_after: None,
        });
    }
    let tier_before = tier_for(state, program.id, customer_id).map(|tier| tier.id);
    let entry = append_ledger_locked(
        state,
        program,
        customer_id,
        LedgerKind::Earn,
        points,
        reason,
        actor,
        Some(kind_name.to_string()),
        reference_id,
        reference_amount,
        now,
    )?;
    let tier_after = tier_for(state, program.id, customer_id).map(|tier| tier.id);
    Ok(EarnOutcome {
        entry: Some(entry),
        tier_before,
        tier_after,
    })
}

/// Expiry sweep for one program. Fixed mode expires the aggregate
/// of earn rows whose `expires_at` has passed, minus amounts
/// already expired, capped at the current balance. Inactivity mode
/// expires the whole balance when the customer has had no ledger
/// activity for `expiry_days`. Every expiry is a ledger row; the
/// function returns how many customers were expired and how many
/// points in total.
pub(crate) fn expire_program(
    state: &LoyaltyState,
    program: &LoyaltyProgram,
    actor: &str,
    now: DateTime<Utc>,
) -> (usize, i64) {
    let Some(days) = program.expiry_days else {
        return (0, 0);
    };
    let mut customers: Vec<String> = state
        .ledger
        .iter()
        .filter(|entry| entry.value().program_id == program.id)
        .map(|entry| entry.value().customer_id.clone())
        .collect();
    customers.sort();
    customers.dedup();
    let mut expired_customers = 0usize;
    let mut expired_points = 0i64;
    for customer_id in customers {
        let current = balance(state, program.id, &customer_id);
        if current <= 0 {
            continue;
        }
        let amount = match program.expiry_mode {
            ExpiryMode::Fixed => {
                let expired_earns: i64 = state
                    .ledger
                    .iter()
                    .filter(|entry| {
                        let value = entry.value();
                        value.program_id == program.id
                            && value.customer_id == customer_id
                            && value.kind == LedgerKind::Earn
                            && value.expires_at.is_some_and(|expires_at| expires_at <= now)
                    })
                    .map(|entry| entry.value().points.max(0))
                    .sum();
                let already_expired: i64 = state
                    .ledger
                    .iter()
                    .filter(|entry| {
                        let value = entry.value();
                        value.program_id == program.id
                            && value.customer_id == customer_id
                            && value.kind == LedgerKind::Expire
                    })
                    .map(|entry| entry.value().points.min(0).abs())
                    .sum();
                (expired_earns - already_expired).clamp(0, current)
            }
            ExpiryMode::Inactivity => {
                let last_activity = state
                    .ledger
                    .iter()
                    .filter(|entry| {
                        entry.value().program_id == program.id
                            && entry.value().customer_id == customer_id
                    })
                    .map(|entry| entry.value().created_at)
                    .max();
                match last_activity {
                    Some(last) if now - last > Duration::days(days) => current,
                    _ => 0,
                }
            }
        };
        if amount <= 0 {
            continue;
        }
        let tier_before = tier_for(state, program.id, &customer_id).map(|tier| tier.id);
        let reason = match program.expiry_mode {
            ExpiryMode::Fixed => format!("points expired after {days} days"),
            ExpiryMode::Inactivity => format!("points expired after {days} days of inactivity"),
        };
        if append_ledger(
            state,
            program,
            &customer_id,
            LedgerKind::Expire,
            -amount,
            &reason,
            actor,
            None,
            None,
            None,
            now,
        )
        .is_ok()
        {
            expired_customers += 1;
            expired_points += amount;
            state.emit(
                "loyalty.points.expired",
                json!({
                    "program_id": program.id,
                    "customer_id": customer_id,
                    "points": amount,
                    "expiry_mode": program.expiry_mode.as_str(),
                }),
            );
            let tier_after = tier_for(state, program.id, &customer_id).map(|tier| tier.id);
            if tier_before != tier_after {
                state.emit(
                    "loyalty.tier.changed",
                    json!({
                        "program_id": program.id,
                        "customer_id": customer_id,
                        "previous_tier_id": tier_before,
                        "tier_id": tier_after,
                    }),
                );
            }
        }
    }
    (expired_customers, expired_points)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn price_and_money_helpers() {
        assert_eq!(valid_price(10.0).unwrap(), 10.0);
        assert_eq!(valid_price(1.234567).unwrap(), 1.2346);
        assert!(valid_price(-1.0).is_err());
        assert!(valid_price(f64::NAN).is_err());
        assert_eq!(round_money(1.234567), 1.2346);
        assert_eq!(round_money(2.5), 2.5);
    }

    #[test]
    fn currency_validation() {
        assert_eq!(valid_currency("usd").unwrap(), "USD");
        assert_eq!(valid_currency("EUR").unwrap(), "EUR");
        assert!(valid_currency("US").is_err());
        assert!(valid_currency("USDD").is_err());
        assert!(valid_currency("U1D").is_err());
    }

    #[test]
    fn uuid_and_text_validation() {
        let id = Uuid::new_v4();
        assert_eq!(parse_uuid(&id.to_string(), "id").unwrap(), id);
        assert!(parse_uuid("not-a-uuid", "id").is_err());
        assert_eq!(require_text("  hello ", "name", 10).unwrap(), "hello");
        assert!(require_text("", "name", 10).is_err());
    }

    #[test]
    fn paginate_slices_items() {
        let params = PaginationParams {
            page: Some(2),
            per_page: Some(2),
            sort: None,
            order: None,
            search: None,
            filters: Default::default(),
        };
        let result = paginate(vec![1, 2, 3, 4, 5], &params);
        assert_eq!(result.items, vec![3, 4]);
        assert_eq!(result.pagination.total, 5);
    }

    fn test_program() -> LoyaltyProgram {
        let now = Utc::now();
        LoyaltyProgram {
            id: Uuid::new_v4(),
            company_id: None,
            name: "Test".to_string(),
            description: None,
            points_name: "points".to_string(),
            currency: "USD".to_string(),
            discount_per_point: 0.01,
            active: true,
            expiry_days: None,
            expiry_mode: ExpiryMode::Fixed,
            created_at: now,
            updated_at: now,
        }
    }

    fn test_rule(program_id: Uuid, kind: super::super::types::EarnRuleKind) -> LoyaltyEarnRule {
        let now = Utc::now();
        LoyaltyEarnRule {
            id: Uuid::new_v4(),
            program_id,
            kind,
            points_per_unit: 1.0,
            fixed_points: None,
            cap_per_day: None,
            cap_total: None,
            min_order_amount: None,
            active: true,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn points_from_total_clamps_absurd_totals() {
        // An absurd total used to saturate `as i64` to i64::MAX.
        assert_eq!(
            points_from_total(1e300, 1e10).unwrap(),
            MAX_AWARD_POINTS
        );
        assert_eq!(points_from_total(100.0, 1.5).unwrap(), 150);
        assert_eq!(points_from_total(10.9, 1.0).unwrap(), 10);
        assert!(points_from_total(f64::INFINITY, 1.0).is_err());
        assert!(points_from_total(f64::NAN, 1.0).is_err());
        assert!(points_from_total(-5.0, 1.0).is_err());
        assert!(points_from_total(10.0, f64::INFINITY).is_err());
        assert!(points_from_total(10.0, -1.0).is_err());
    }

    #[test]
    fn points_from_rate_rejects_non_finite_and_clamps() {
        assert!(points_from_rate(f64::INFINITY).is_err());
        assert!(points_from_rate(f64::NAN).is_err());
        assert!(points_from_rate(-1.0).is_err());
        assert_eq!(points_from_rate(1e30).unwrap(), MAX_AWARD_POINTS);
        assert_eq!(points_from_rate(2.7).unwrap(), 2);
    }

    #[test]
    fn fixed_or_rate_points_clamps_fixed_awards() {
        let mut rule = test_rule(Uuid::new_v4(), super::super::types::EarnRuleKind::Signup);
        rule.fixed_points = Some(i64::MAX);
        assert_eq!(fixed_or_rate_points(&rule).unwrap(), MAX_AWARD_POINTS);
        rule.fixed_points = Some(50);
        assert_eq!(fixed_or_rate_points(&rule).unwrap(), 50);
        rule.fixed_points = None;
        rule.points_per_unit = 7.9;
        assert_eq!(fixed_or_rate_points(&rule).unwrap(), 7);
    }

    #[test]
    fn initial_redemption_status_marks_discounts_pending() {
        assert_eq!(initial_redemption_status("discount"), "pending");
        assert_eq!(initial_redemption_status("product"), "completed");
    }

    #[test]
    fn has_earn_for_reference_matches_program_kind_and_reference() {
        let state = LoyaltyState::default();
        let program = test_program();
        let now = Utc::now();
        append_ledger(
            &state,
            &program,
            "cust-1",
            LedgerKind::Earn,
            10,
            "signup",
            "system",
            Some("signup".to_string()),
            Some("cust-1".to_string()),
            None,
            now,
        )
        .unwrap();
        assert!(has_earn_for_reference(&state, program.id, "signup", "cust-1"));
        assert!(!has_earn_for_reference(&state, program.id, "referral", "cust-1"));
        assert!(!has_earn_for_reference(&state, program.id, "signup", "cust-2"));
        assert!(!has_earn_for_reference(&state, Uuid::new_v4(), "signup", "cust-1"));
    }

    #[test]
    fn concurrent_spends_cannot_drive_balance_below_zero() {
        let state = LoyaltyState::default();
        let program = test_program();
        let now = Utc::now();
        append_ledger(
            &state,
            &program,
            "cust-1",
            LedgerKind::Earn,
            100,
            "seed",
            "system",
            None,
            None,
            None,
            now,
        )
        .unwrap();
        // Ten threads each try to spend 30 of a 100-point balance.
        // The check and the insert are serialised, so exactly three
        // succeed and the balance never goes negative.
        std::thread::scope(|scope| {
            for _ in 0..10 {
                let state = &state;
                let program = &program;
                scope.spawn(move || {
                    let _ = append_ledger(
                        state,
                        program,
                        "cust-1",
                        LedgerKind::Spend,
                        -30,
                        "concurrent spend",
                        "system",
                        None,
                        None,
                        None,
                        Utc::now(),
                    );
                });
            }
        });
        let spends: i64 = state
            .ledger
            .iter()
            .filter(|entry| {
                entry.value().kind == LedgerKind::Spend && entry.value().customer_id == "cust-1"
            })
            .map(|entry| entry.value().points)
            .sum();
        assert_eq!(spends, -90);
        assert_eq!(balance(&state, program.id, "cust-1"), 10);
    }

    #[test]
    fn earn_under_rule_clamps_huge_awards() {
        let state = LoyaltyState::default();
        let program = test_program();
        let rule = test_rule(program.id, super::super::types::EarnRuleKind::Signup);
        let entry = earn_under_rule(
            &state,
            &program,
            &rule,
            "cust-1",
            i64::MAX,
            "signup",
            "system",
            Some("cust-1".to_string()),
            None,
            Utc::now(),
        )
        .unwrap()
        .expect("earn must produce an entry");
        assert_eq!(entry.points, MAX_AWARD_POINTS);
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

    fn test_program(company_id: Option<&str>) -> LoyaltyProgram {
        LoyaltyProgram {
            id: Uuid::new_v4(),
            company_id: company_id.map(str::to_string),
            name: "Acme Points".to_string(),
            description: None,
            points_name: "points".to_string(),
            currency: "USD".to_string(),
            discount_per_point: 0.01,
            active: true,
            expiry_days: None,
            expiry_mode: ExpiryMode::Fixed,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let req = tenant_request("acme", None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some("acme".to_string())
        );
    }

    #[test]
    fn caller_company_prefers_verified_caller_when_tenant_has_none() {
        let req = tenant_request("", Some("acme"));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some("acme".to_string())
        );
    }

    #[test]
    fn caller_company_rejects_verified_caller_mismatch() {
        let req = tenant_request("acme", Some("other"));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn in_company_scope_hides_unscoped_records_from_scoped_callers() {
        let scope = Some("acme".to_string());
        assert!(in_company_scope(&Some("acme".to_string()), &scope));
        assert!(!in_company_scope(&Some("other".to_string()), &scope));
        assert!(!in_company_scope(&None, &scope));
        assert!(in_company_scope(&None, &None));
        assert!(!in_company_scope(&Some("other".to_string()), &None));
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

    #[test]
    fn require_program_in_scope_partitions_by_company() {
        let state = LoyaltyState::default();
        let program = test_program(Some("acme"));
        let id = program.id.to_string();
        state.programs.insert(program.id, program);
        let scope = Some("acme".to_string());
        assert!(require_program_in_scope(&state, &id, &scope).is_ok());
        assert!(require_program_in_scope(&state, &id, &Some("other".to_string())).is_err());
        assert!(require_program_in_scope(&state, &id, &None).is_err());
        assert!(require_program_in_scope(&state, &Uuid::new_v4().to_string(), &scope).is_err());
        let global = test_program(None);
        let global_id = global.id.to_string();
        state.programs.insert(global.id, global);
        assert!(require_program_in_scope(&state, &global_id, &None).is_ok());
        assert!(require_program_in_scope(&state, &global_id, &scope).is_err());
    }
}
