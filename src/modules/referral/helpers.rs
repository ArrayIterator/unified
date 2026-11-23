//! Caller, permission, loading, validation, audit,
//! anti-gaming, and reward helpers.

use super::state::ReferralState;
use super::types::{
    AntiGamingKind, AntiGamingRule, AuditRecord, CreateAntiGamingRuleRequest,
    CreateRewardRuleRequest, CreateStageRequest, Enforcement, JobEligibility, LinkCreated,
    MAX_CONDITIONS, MAX_CURRENCY_LEN, MAX_DESCRIPTION_LEN, MAX_EMAIL_LEN, MAX_NAME_LEN,
    MAX_NEXT_STAGES, MAX_NOTE_LEN, MAX_PHONE_LEN, PERM_REFERRAL_MANAGE, PERM_REFERRAL_VIEW,
    PipelineStage, Referral, ReferralLink, ReferralStageHistory, Reward, RewardRule, RewardStatus,
    VERIFICATION_UNVERIFIED, VERIFICATION_VERIFIED, generate_link_token, hash_value,
    normalize_email, reward_eligible, reward_payable_at, valid_currency, valid_email, valid_slug,
};
use actix_web::{HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use uuid::Uuid;

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the sign module.
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
/// the `role.check` hook. Fails closed: no answer means not allowed.
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

pub(crate) fn require_view(ctx: &AppContext, role: &str) -> UnifiedResult<()> {
    require_permission(ctx, role, PERM_REFERRAL_VIEW)
}

pub(crate) fn require_manage(ctx: &AppContext, role: &str) -> UnifiedResult<()> {
    require_permission(ctx, role, PERM_REFERRAL_MANAGE)
}

/// Resolve the tenant id for this request from the middleware's
/// resolved tenant context. Fails closed: a request with no
/// resolved tenant is rejected, and there is never a fallback to
/// the control plane.
pub(crate) fn resolve_tenant(req: &HttpRequest) -> UnifiedResult<String> {
    unified_core::tenancy::TenantContext::from_http_request(req)
        .map(|context| context.tenant_id.to_string())
}

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 5, task 319).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
///
/// The records in this module carry a required `tenant_id` (not a
/// `company_id`): partitioning is by tenant, through the pre-existing
/// `load_*` helpers (cross-tenant row reads as 404) and the list
/// filters. `caller_company` below is therefore the mismatch-only
/// company check (pilot 4 group D idiom), not a strict
/// `require_company` gate: a tenant/caller company mismatch is 403,
/// but a write is never refused only because no company resolves —
/// the write is still fully tenant-partitioned by `tenant_id`.
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
/// middleware-resolved tenant context are sources (header trust was
/// removed by task 317; this helper must never reintroduce it).
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

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
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

pub(crate) fn optional_text(
    raw: &Option<String>,
    field: &str,
    max: usize,
) -> UnifiedResult<Option<String>> {
    match raw {
        Some(text) => Ok(Some(require_text(text, field, max)?)),
        None => Ok(None),
    }
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

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate a candidate email: well-formed and normalized to
/// lowercase for storage and duplicate detection.
pub(crate) fn validate_candidate_email(raw: &str) -> UnifiedResult<String> {
    let email = raw.trim();
    if email.is_empty() || email.len() > MAX_EMAIL_LEN {
        return Err(UnifiedError::bad_request(
            "candidate_email must not be empty".to_string(),
        ));
    }
    if !valid_email(email) {
        return Err(UnifiedError::bad_request(format!(
            "candidate_email is not a valid email address: {email}"
        )));
    }
    Ok(normalize_email(email))
}

/// Validate an optional phone number.
pub(crate) fn validate_phone(raw: &Option<String>) -> UnifiedResult<Option<String>> {
    optional_text(raw, "candidate_phone", MAX_PHONE_LEN)
}

/// Validate a currency code (three uppercase letters).
pub(crate) fn validate_currency_code(raw: &str) -> UnifiedResult<String> {
    let code = raw.trim().to_uppercase();
    if code.len() != MAX_CURRENCY_LEN || !valid_currency(&code) {
        return Err(UnifiedError::bad_request(
            "currency must be a three-letter ISO code such as USD".to_string(),
        ));
    }
    Ok(code)
}

/// Validate reward-rule eligibility conditions: free-text
/// conditions, at most the module bound, no repeats, no
/// empties.
pub(crate) fn validate_conditions(conditions: &[String]) -> UnifiedResult<Vec<String>> {
    if conditions.len() > MAX_CONDITIONS {
        return Err(UnifiedError::bad_request(format!(
            "At most {MAX_CONDITIONS} eligibility conditions per reward rule"
        )));
    }
    let mut seen = std::collections::HashSet::new();
    let mut clean = Vec::new();
    for condition in conditions {
        let condition = require_text(condition, "eligibility_condition", MAX_NOTE_LEN)?;
        if !seen.insert(condition.clone()) {
            return Err(UnifiedError::bad_request(format!(
                "eligibility_conditions must not repeat \"{condition}\""
            )));
        }
        clean.push(condition);
    }
    Ok(clean)
}

/// Validate a reward-rule create body: name, non-negative
/// amount in minor units, currency, non-negative retention,
/// conditions, and an existing trigger stage when one is
/// named.
pub(crate) fn validate_reward_rule_input(
    state: &ReferralState,
    tenant_id: &str,
    input: &CreateRewardRuleRequest,
) -> UnifiedResult<(
    String,
    Option<String>,
    i64,
    String,
    Option<Uuid>,
    i64,
    Vec<String>,
)> {
    let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
    let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
    if input.amount_minor < 0 {
        return Err(UnifiedError::bad_request(
            "amount_minor must not be negative".to_string(),
        ));
    }
    let currency = validate_currency_code(&input.currency)?;
    let trigger_stage_id = match &input.trigger_stage_id {
        Some(raw) => Some(load_stage(state, tenant_id, raw)?).map(|stage| stage.id),
        None => None,
    };
    if input.retention_days < 0 {
        return Err(UnifiedError::bad_request(
            "retention_days must not be negative".to_string(),
        ));
    }
    let conditions = validate_conditions(&input.eligibility_conditions)?;
    Ok((
        name,
        description,
        input.amount_minor,
        currency,
        trigger_stage_id,
        input.retention_days,
        conditions,
    ))
}

/// Validate a pipeline-stage create body: name, slug, and
/// the allowed next stages, which must all be existing
/// stages of this tenant.
pub(crate) fn validate_stage_input(
    state: &ReferralState,
    tenant_id: &str,
    input: &CreateStageRequest,
) -> UnifiedResult<(String, String, Vec<Uuid>)> {
    let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
    let slug = input.slug.trim().to_lowercase();
    if !valid_slug(&slug) {
        return Err(UnifiedError::bad_request(
            "slug must be lowercase letters, digits, and dashes, starting with a letter or digit"
                .to_string(),
        ));
    }
    if input.allowed_next_stage_ids.len() > MAX_NEXT_STAGES {
        return Err(UnifiedError::bad_request(format!(
            "At most {MAX_NEXT_STAGES} allowed next stages per stage"
        )));
    }
    let mut seen = std::collections::HashSet::new();
    let mut next_ids = Vec::new();
    for raw in &input.allowed_next_stage_ids {
        let stage = load_stage(state, tenant_id, raw)?;
        if !seen.insert(stage.id) {
            return Err(UnifiedError::bad_request(format!(
                "allowed_next_stage_ids must not repeat \"{raw}\""
            )));
        }
        next_ids.push(stage.id);
    }
    Ok((name, slug, next_ids))
}

/// Validate an anti-gaming rule create body: the kind must
/// be known, the enforcement must be `block` or `flag`, and
/// each kind needs its own thresholds (a cooldown needs
/// `cooldown_days`, a per-period cap needs `limit_count`
/// plus `window_days`).
pub(crate) fn validate_gaming_rule_input(
    input: &CreateAntiGamingRuleRequest,
) -> UnifiedResult<(String, AntiGamingKind, Enforcement)> {
    let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
    let kind = AntiGamingKind::from_str(input.rule_kind.trim()).ok_or_else(|| {
        UnifiedError::bad_request(
            "rule_kind must be block_self_referral, same_candidate_cooldown, max_referrals_per_period, or require_active_employee"
                .to_string(),
        )
    })?;
    let enforcement = Enforcement::from_str(input.enforcement.trim()).ok_or_else(|| {
        UnifiedError::bad_request("enforcement must be block or flag".to_string())
    })?;
    match kind {
        AntiGamingKind::SameCandidateCooldown => {
            let days = input.cooldown_days.unwrap_or(0);
            if days <= 0 {
                return Err(UnifiedError::bad_request(
                    "same_candidate_cooldown needs cooldown_days greater than zero".to_string(),
                ));
            }
        }
        AntiGamingKind::MaxReferralsPerPeriod => {
            let limit = input.limit_count.unwrap_or(0);
            let window = input.window_days.unwrap_or(0);
            if limit <= 0 || window <= 0 {
                return Err(UnifiedError::bad_request(
                    "max_referrals_per_period needs limit_count and window_days greater than zero"
                        .to_string(),
                ));
            }
        }
        _ => {}
    }
    if let Some(days) = input.cooldown_days {
        if days < 0 {
            return Err(UnifiedError::bad_request(
                "cooldown_days must not be negative".to_string(),
            ));
        }
    }
    if let Some(limit) = input.limit_count {
        if limit < 0 {
            return Err(UnifiedError::bad_request(
                "limit_count must not be negative".to_string(),
            ));
        }
    }
    if let Some(window) = input.window_days {
        if window < 0 {
            return Err(UnifiedError::bad_request(
                "window_days must not be negative".to_string(),
            ));
        }
    }
    Ok((name, kind, enforcement))
}

// ---------------------------------------------------------------------------
// Tenant-scoped loaders (cross-tenant reads as 404, never 403)
// ---------------------------------------------------------------------------

fn tenant_match(row_tenant_id: &str, tenant_id: &str, id: Uuid, what: &str) -> UnifiedResult<()> {
    if row_tenant_id != tenant_id {
        return Err(UnifiedError::not_found(format!("{what} not found: {id}")));
    }
    Ok(())
}

/// The eligibility row for one job in a tenant, when one
/// exists. No row means the job is eligible by default.
pub(crate) fn eligibility_for_job(
    state: &ReferralState,
    tenant_id: &str,
    job_id: Uuid,
) -> Option<JobEligibility> {
    state
        .eligibility
        .iter()
        .map(|entry| entry.value().clone())
        .find(|row| row.tenant_id == tenant_id && row.job_id == job_id)
}

pub(crate) fn load_link(
    state: &ReferralState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<ReferralLink> {
    let id = parse_uuid(raw_id, "link")?;
    let link = state
        .links
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Referral link not found: {id}")))?;
    tenant_match(&link.tenant_id, tenant_id, id, "Referral link")?;
    Ok(link)
}

pub(crate) fn load_referral(
    state: &ReferralState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<Referral> {
    let id = parse_uuid(raw_id, "referral")?;
    let referral = state
        .referrals
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Referral not found: {id}")))?;
    tenant_match(&referral.tenant_id, tenant_id, id, "Referral")?;
    Ok(referral)
}

pub(crate) fn load_reward(
    state: &ReferralState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<Reward> {
    let id = parse_uuid(raw_id, "reward")?;
    let reward = state
        .rewards
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Reward not found: {id}")))?;
    tenant_match(&reward.tenant_id, tenant_id, id, "Reward")?;
    Ok(reward)
}

pub(crate) fn load_rule(
    state: &ReferralState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<RewardRule> {
    let id = parse_uuid(raw_id, "reward rule")?;
    let rule = state
        .reward_rules
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Reward rule not found: {id}")))?;
    tenant_match(&rule.tenant_id, tenant_id, id, "Reward rule")?;
    Ok(rule)
}

pub(crate) fn load_stage(
    state: &ReferralState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<PipelineStage> {
    let id = parse_uuid(raw_id, "pipeline stage")?;
    let stage = state
        .stages
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Pipeline stage not found: {id}")))?;
    tenant_match(&stage.tenant_id, tenant_id, id, "Pipeline stage")?;
    Ok(stage)
}

/// Load a stage by slug within a tenant.
pub(crate) fn load_stage_by_slug(
    state: &ReferralState,
    tenant_id: &str,
    slug: &str,
) -> UnifiedResult<PipelineStage> {
    let wanted = slug.trim().to_lowercase();
    state
        .stages_of(tenant_id)
        .into_iter()
        .find(|stage| stage.slug == wanted)
        .ok_or_else(|| UnifiedError::not_found(format!("Pipeline stage not found: {slug}")))
}

pub(crate) fn load_gaming_rule(
    state: &ReferralState,
    tenant_id: &str,
    raw_id: &str,
) -> UnifiedResult<AntiGamingRule> {
    let id = parse_uuid(raw_id, "anti-gaming rule")?;
    let rule = state
        .anti_gaming_rules
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Anti-gaming rule not found: {id}")))?;
    tenant_match(&rule.tenant_id, tenant_id, id, "Anti-gaming rule")?;
    Ok(rule)
}

// ---------------------------------------------------------------------------
// Audit
// ---------------------------------------------------------------------------

/// Append one audit row for a change. `before` / `after` are
/// the serialized rows (or `None` for creates / deletes).
pub(crate) fn record_audit(
    state: &ReferralState,
    tenant_id: &str,
    entity_type: &str,
    entity_id: &str,
    action: &str,
    actor: &str,
    before: Option<Value>,
    after: Option<Value>,
) {
    let row = AuditRecord {
        id: Uuid::new_v4(),
        tenant_id: tenant_id.to_string(),
        entity_type: entity_type.to_string(),
        entity_id: entity_id.to_string(),
        action: action.to_string(),
        actor: actor.to_string(),
        before,
        after,
        created_at: Utc::now(),
    };
    state.audit.insert(row.id, row);
}

// ---------------------------------------------------------------------------
// Links
// ---------------------------------------------------------------------------

/// Mint a fresh link row for one (employee, job) pair: a new
/// token is generated and only its hash stored; the raw
/// token is returned once in the response.
pub(crate) fn mint_link(
    state: &ReferralState,
    tenant_id: &str,
    job_id: Uuid,
    referrer_user_id: &str,
    actor: &str,
) -> LinkCreated {
    let now = Utc::now();
    let token = generate_link_token();
    let link = ReferralLink {
        id: Uuid::new_v4(),
        tenant_id: tenant_id.to_string(),
        job_id,
        referrer_user_id: referrer_user_id.to_string(),
        token_hash: hash_value(&token),
        click_count: 0,
        last_clicked_at: None,
        active: true,
        created_by: actor.to_string(),
        created_at: now,
        updated_at: now,
    };
    state.links.insert(link.id, link.clone());
    LinkCreated { link, token }
}

// ---------------------------------------------------------------------------
// Anti-gaming evaluation
// ---------------------------------------------------------------------------

/// Outcome of evaluating the tenant's enabled anti-gaming
/// rules for one submission: whether the submission is
/// blocked, and the notes recorded on the referral row.
/// Rules that cannot be evaluated from the data this module
/// has (self-referral without a known referrer email,
/// active-employee without an HR existence answer) never
/// block — they add an `unverified` note instead.
pub struct GamingOutcome {
    pub blocked: bool,
    pub notes: Vec<String>,
}

pub(crate) fn evaluate_anti_gaming(
    state: &ReferralState,
    tenant_id: &str,
    referrer_user_id: &str,
    candidate_email: &str,
    now: DateTime<Utc>,
) -> GamingOutcome {
    let mut blocked = false;
    let mut notes: Vec<String> = Vec::new();
    let rules = state.anti_gaming_rules_of(tenant_id);
    for rule in rules.iter().filter(|rule| rule.enabled) {
        match rule.rule_kind {
            AntiGamingKind::BlockSelfReferral => {
                // No user-email lookup hook exists in this
                // tree, so the referrer's email is unknown
                // and the check cannot run. Record that
                // instead of a false pass.
                notes.push(format!(
                    "self-referral check unverified (rule {}): referrer email unknown",
                    rule.name
                ));
            }
            AntiGamingKind::SameCandidateCooldown => {
                let cooldown = rule.cooldown_days.unwrap_or(0);
                let last = state
                    .latest_referral_for_email(tenant_id, candidate_email)
                    .map(|referral| referral.created_at);
                if super::types::cooldown_violated(last, cooldown, now) {
                    let message = format!(
                        "same-candidate cooldown violated (rule {}): candidate referred within the last {cooldown} days",
                        rule.name
                    );
                    if rule.enforcement == Enforcement::Block {
                        blocked = true;
                    }
                    notes.push(message);
                }
            }
            AntiGamingKind::MaxReferralsPerPeriod => {
                let limit = rule.limit_count.unwrap_or(0);
                let window = rule.window_days.unwrap_or(0);
                if limit > 0 && window > 0 {
                    let window_start = now - chrono::Duration::days(window);
                    let count = state
                        .referrals_of_referrer(tenant_id, referrer_user_id)
                        .iter()
                        .filter(|referral| referral.created_at >= window_start)
                        .count() as i64;
                    if super::types::max_referrals_violated(count, limit) {
                        let message = format!(
                            "max referrals per period violated (rule {}): {count} referrals in the last {window} days, limit {limit}",
                            rule.name
                        );
                        if rule.enforcement == Enforcement::Block {
                            blocked = true;
                        }
                        notes.push(message);
                    }
                }
            }
            AntiGamingKind::RequireActiveEmployee => {
                if state.is_known_employee(referrer_user_id) {
                    notes.push(format!("active-employee check passed (rule {})", rule.name));
                } else {
                    // Absence from the registry proves
                    // nothing: HR only emits
                    // `hr.employee_created` for employees
                    // created after this module was installed,
                    // and no HR existence hook exists. Never
                    // block on an unanswerable check.
                    notes.push(format!(
                        "active-employee check unverified (rule {}): no HR record for the referrer",
                        rule.name
                    ));
                }
            }
        }
    }
    GamingOutcome { blocked, notes }
}

/// The referrer verification recorded on a new referral:
/// `verified` when the HR registry knows the referrer,
/// otherwise `unverified` with an explanation. The check is
/// independent of the anti-gaming rules so the row always
/// carries a verdict.
pub(crate) fn verify_referrer(state: &ReferralState, referrer_user_id: &str) -> (bool, String) {
    if state.is_known_employee(referrer_user_id) {
        (true, VERIFICATION_VERIFIED.to_string())
    } else {
        (
            false,
            format!(
                "{VERIFICATION_UNVERIFIED}: no HR employee record found for the referrer; no hr.employee.exists hook exists to confirm active employment"
            ),
        )
    }
}

// ---------------------------------------------------------------------------
// Stage moves and rewards (shared by handlers and hooks)
// ---------------------------------------------------------------------------

/// Apply a pipeline move to a referral: validate the
/// transition against the tenant's stage data, append the
/// history row, update the referral, write the audit row,
/// and emit `referral.stage_changed`. Hired and rejected
/// stages have side effects handled by the caller-facing
/// wrappers below — this function only moves the stage.
pub(crate) fn record_stage_move(
    state: &ReferralState,
    tenant_id: &str,
    referral: &Referral,
    to_stage: &PipelineStage,
    actor: &str,
    reason: &Option<String>,
) -> UnifiedResult<Referral> {
    let stages = state.stages_of(tenant_id);
    if !super::types::transition_allowed(&stages, referral.stage_id, to_stage.id) {
        return Err(UnifiedError::bad_request(format!(
            "Stage transition from {} to {} is not allowed by the pipeline stages",
            referral.stage_slug, to_stage.slug
        )));
    }
    let now = Utc::now();
    let mut updated = referral.clone();
    let from_stage_id = Some(referral.stage_id);
    let from_stage_slug = Some(referral.stage_slug.clone());
    updated.stage_id = to_stage.id;
    updated.stage_slug = to_stage.slug.clone();
    updated.updated_at = now;
    state.referrals.insert(updated.id, updated.clone());
    let history = ReferralStageHistory {
        id: Uuid::new_v4(),
        tenant_id: tenant_id.to_string(),
        referral_id: referral.id,
        job_id: referral.job_id,
        from_stage_id,
        from_stage_slug,
        to_stage_id: to_stage.id,
        to_stage_slug: to_stage.slug.clone(),
        moved_by: actor.to_string(),
        reason: reason.clone(),
        created_at: now,
    };
    state.stage_history.insert(history.id, history.clone());
    record_audit(
        state,
        tenant_id,
        "referral",
        &referral.id.to_string(),
        "stage_changed",
        actor,
        serde_json::to_value(referral).ok(),
        serde_json::to_value(&updated).ok(),
    );
    state.emit(
        super::types::EVENT_STAGE_CHANGED,
        json!({
            "tenant_id": tenant_id,
            "referral_id": referral.id.to_string(),
            "job_id": referral.job_id.to_string(),
            "referrer_user_id": referral.referrer_user_id,
            "from_stage": from_stage_slug,
            "to_stage": to_stage.slug,
            "actor": actor,
        }),
    );
    Ok(updated)
}

/// Handle the side effects of entering a hired stage:
/// create the referral's reward under the job's reward rule
/// (when one applies) and emit `referral.hired` plus
/// `referral.reward.payable` when the reward is already
/// eligible (zero retention). Returns the created reward,
/// when one was created.
pub(crate) fn on_referral_hired(
    state: &ReferralState,
    tenant_id: &str,
    referral: &Referral,
    reached_at: DateTime<Utc>,
    actor: &str,
) -> Option<Reward> {
    state.emit(
        super::types::EVENT_HIRED,
        json!({
            "tenant_id": tenant_id,
            "referral_id": referral.id.to_string(),
            "job_id": referral.job_id.to_string(),
            "referrer_user_id": referral.referrer_user_id,
            "candidate_email": referral.candidate_email,
            "actor": actor,
        }),
    );
    let rule = state.reward_rule_for_job(tenant_id, referral.job_id)?;
    if !rule.active {
        return None;
    }
    // The reward triggers on the rule's trigger stage when
    // one is set; a rule with no trigger stage triggers on
    // the hired stage itself.
    if let Some(trigger_stage_id) = rule.trigger_stage_id {
        let trigger_reached_at = state.first_entered_stage_at(referral.id, trigger_stage_id);
        if trigger_reached_at.is_none() {
            return None;
        }
        return create_reward(state, tenant_id, referral, &rule, trigger_reached_at, actor);
    }
    create_reward(state, tenant_id, referral, &rule, Some(reached_at), actor)
}

/// Create the reward row for one referral under one rule.
/// At most one reward per (referral, rule). The status is
/// `eligible` immediately when the retention period has
/// already elapsed, otherwise `pending`; reaching
/// eligibility emits `referral.reward.payable` so billing /
/// HR can pay it out — this module never moves money.
pub(crate) fn create_reward(
    state: &ReferralState,
    tenant_id: &str,
    referral: &Referral,
    rule: &RewardRule,
    trigger_reached_at: Option<DateTime<Utc>>,
    actor: &str,
) -> Option<Reward> {
    if state.reward_for(referral.id, rule.id).is_some() {
        return None;
    }
    let now = Utc::now();
    let payable_at = reward_payable_at(trigger_reached_at, rule.retention_days);
    let eligible = reward_eligible(trigger_reached_at, rule.retention_days, now);
    let reward = Reward {
        id: Uuid::new_v4(),
        tenant_id: tenant_id.to_string(),
        referral_id: referral.id,
        job_id: referral.job_id,
        referrer_user_id: referral.referrer_user_id.clone(),
        rule_id: rule.id,
        rule_name: rule.name.clone(),
        amount_minor: rule.amount_minor,
        currency: rule.currency.clone(),
        status: if eligible {
            RewardStatus::Eligible
        } else {
            RewardStatus::Pending
        },
        eligible_at: payable_at,
        paid_at: None,
        paid_by: None,
        void_reason: None,
        created_by: actor.to_string(),
        created_at: now,
        updated_at: now,
    };
    state.rewards.insert(reward.id, reward.clone());
    if eligible {
        state.emit(
            super::types::EVENT_REWARD_PAYABLE,
            json!({
                "tenant_id": tenant_id,
                "reward_id": reward.id.to_string(),
                "referral_id": referral.id.to_string(),
                "job_id": referral.job_id.to_string(),
                "referrer_user_id": referral.referrer_user_id,
                "rule_id": rule.id.to_string(),
                "amount_minor": reward.amount_minor,
                "currency": reward.currency,
            }),
        );
    }
    Some(reward)
}

/// Void every open (pending / eligible) reward of a
/// referral, e.g. when the referral is rejected after a
/// reward was created. Emits `referral.reward.voided` per
/// reward and writes an audit row per reward.
pub(crate) fn void_open_rewards(
    state: &ReferralState,
    tenant_id: &str,
    referral: &Referral,
    reason: &str,
    actor: &str,
) {
    let open: Vec<Reward> = state
        .rewards_of(tenant_id)
        .into_iter()
        .filter(|reward| {
            reward.referral_id == referral.id
                && matches!(
                    reward.status,
                    RewardStatus::Pending | RewardStatus::Eligible
                )
        })
        .collect();
    for reward in open {
        let mut updated = reward.clone();
        updated.status = RewardStatus::Voided;
        updated.void_reason = Some(reason.to_string());
        updated.updated_at = Utc::now();
        state.rewards.insert(updated.id, updated.clone());
        record_audit(
            state,
            tenant_id,
            "reward",
            &reward.id.to_string(),
            "voided",
            actor,
            serde_json::to_value(&reward).ok(),
            serde_json::to_value(&updated).ok(),
        );
        state.emit(
            super::types::EVENT_REWARD_VOIDED,
            json!({
                "tenant_id": tenant_id,
                "reward_id": reward.id.to_string(),
                "referral_id": referral.id.to_string(),
                "job_id": referral.job_id.to_string(),
                "referrer_user_id": referral.referrer_user_id,
                "reason": reason,
            }),
        );
    }
}

/// Promote a pending reward to eligible when its retention
/// period has elapsed. Returns true when the reward changed
/// (and `referral.reward.payable` was emitted).
pub(crate) fn refresh_reward_eligibility(
    state: &ReferralState,
    tenant_id: &str,
    reward: &Reward,
    now: DateTime<Utc>,
) -> bool {
    if reward.status != RewardStatus::Pending {
        return false;
    }
    let Some(eligible_at) = reward.eligible_at else {
        return false;
    };
    if now < eligible_at {
        return false;
    }
    let mut updated = reward.clone();
    updated.status = RewardStatus::Eligible;
    updated.updated_at = now;
    state.rewards.insert(updated.id, updated.clone());
    state.emit(
        super::types::EVENT_REWARD_PAYABLE,
        json!({
            "tenant_id": tenant_id,
            "reward_id": reward.id.to_string(),
            "referral_id": reward.referral_id.to_string(),
            "job_id": reward.job_id.to_string(),
            "referrer_user_id": reward.referrer_user_id,
            "rule_id": reward.rule_id.to_string(),
            "amount_minor": reward.amount_minor,
            "currency": reward.currency,
        }),
    );
    true
}

#[cfg(test)]
mod tenant_guard_tests {
    use super::*;
    use actix_web::test::TestRequest;

    fn tenant_request(company_id: &str, caller_company_id: Option<&str>) -> HttpRequest {
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
        if let Some(company) = caller_company_id {
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
    fn caller_company_ignores_the_x_company_id_header() {
        // A forged header naming another company must not move
        // the scope: only the tenant context and the verified
        // caller count.
        let req = TestRequest::default()
            .insert_header(("x-company-id", "other"))
            .to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "acme".to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid("alice"), Vec::new()),
        );
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some("acme".to_string())
        );
    }
}
