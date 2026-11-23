//! ai module - Policy, token cap, cost cap and usage validation.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::middleware::TenantContext;
use unified_core::tenancy;
use actix_web::HttpRequest;
use chrono::{DateTime, Datelike, TimeZone, Utc};
use uuid::Uuid;

use super::capabilities::ai_capability_registry;
use super::constants::KNOWN_USE_CASES;
use super::constants::MAX_MODEL_NAME_LEN;
use super::constants::MAX_MONTHLY_COST_CAP_USD;
use super::constants::MAX_MONTHLY_TOKEN_CAP;
use super::constants::MAX_POLICY_LIST_LEN;
use super::helpers::Caller;
use super::helpers::ok;
use super::helpers::resolve_caller_company;
use super::state::AiState;
use super::types::AiProviderKind;
use super::types::AiUsageRecord;

/// Tenant-first company resolution for AI capability calls
/// (tenant-scoped data access rule). The middleware attaches a
/// verified `TenantContext` (resolved from the Host header) to
/// tenant requests; on a tenant request that context is
/// authoritative for the company id, because it is the tenant's
/// own identity, not a caller-supplied claim. The resolved
/// context is validated through the sanctioned tenancy extractor
/// (`unified_core::tenancy::TenantContext::from_resolved`, which
/// rejects a tenant id that is not a UUID): a present-but-invalid
/// tenant context, or a tenant context with no parseable company
/// id, resolves to `None` (the gate then denies the call when
/// enforcement is on) instead of falling back to a weaker claim —
/// a broken tenant context must never be overridden by a header.
/// Control-plane requests carry no tenant context, so they fall
/// back to `resolve_caller_company` (verified caller, then hook) —
/// the pre-existing control-plane path. Nothing here falls back to a
/// `public`-schema query: this module's stores are in-memory and
/// no capability path touches Postgres at all (see the tenant
/// scoping notes on the Capabilities Contract).
pub fn resolve_capability_company(
    ctx: &AppContext,
    req: &HttpRequest,
    caller: &Caller,
) -> Option<Uuid> {
    if let Some(resolved) = TenantContext::from_request(req) {
        let tenant = tenancy::TenantContext::from_resolved(&resolved).ok()?;
        return tenant
            .company_id
            .as_deref()
            .and_then(|id| Uuid::parse_str(id).ok());
    }
    resolve_caller_company(ctx, req, caller)
}

/// Company scoping for admin writes: a caller who belongs to a
/// company may only manage policies of that company. A caller with
/// no resolvable company (unresolved) may manage nothing: platform
/// access is never the default for a missing company (the shared
/// `CallerScope::Unresolved` rule); an explicit platform decision
/// is required and this helper does not make one. The capability
/// check (`ai:manage_providers`) happens separately in every handler.
pub(crate) fn require_company_scope(
    caller_company: Option<Uuid>,
    target_company: Uuid,
) -> UnifiedResult<()> {
    match caller_company {
        Some(company) if company != target_company => Err(UnifiedError::forbidden(
            "A company admin may only manage policies of their own company",
        )),
        Some(_) => Ok(()),
        None => Err(UnifiedError::forbidden(
            "A caller with no resolvable company may not manage policies of any company",
        )),
    }
}

/// Validate one model name for a policy list: non-empty, bounded,
/// and made only of safe characters. Known provider families are
/// accepted by prefix; anything else must still be a plausible
/// custom-model name (openai-compatible / local endpoints accept
/// arbitrary model ids). The call-time check in
/// `authorize_ai_call` additionally requires fixed-kind providers
/// to use their known families.
pub(crate) fn validate_policy_model_name(model: &str) -> UnifiedResult<String> {
    let model = model.trim();
    if model.is_empty() || model.len() > MAX_MODEL_NAME_LEN {
        return Err(UnifiedError::bad_request(format!(
            "Model names must be between 1 and {MAX_MODEL_NAME_LEN} characters"
        )));
    }
    if !model
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "-._/:".contains(character))
    {
        return Err(UnifiedError::bad_request(format!(
            "Model name \"{model}\" contains characters outside [A-Za-z0-9._/:]"
        )));
    }
    Ok(model.to_string())
}

/// Validate a policy model list: bounded, de-duplicated, every entry
/// a valid model name. `None` (field absent) means unrestricted.
pub(crate) fn validate_policy_models(
    models: &Option<Vec<String>>,
) -> UnifiedResult<Option<Vec<String>>> {
    let Some(models) = models else {
        return Ok(None);
    };
    if models.len() > MAX_POLICY_LIST_LEN {
        return Err(UnifiedError::bad_request(format!(
            "allowed_models must contain at most {MAX_POLICY_LIST_LEN} entries"
        )));
    }
    let mut cleaned: Vec<String> = Vec::with_capacity(models.len());
    for model in models {
        let model = validate_policy_model_name(model)?;
        if !cleaned.contains(&model) {
            cleaned.push(model);
        }
    }
    Ok(Some(cleaned))
}

/// Validate a policy feature list: every entry must be a known use
/// case, or the use case of a capability registered through the AI
/// Capabilities Contract (`register_ai_capabilities`). Capability
/// use cases are module-scoped (for example
/// `exams.draft_questions`) and are declared per-module in the
/// registry, never in a hard-coded list here. `None` (field absent)
/// means unrestricted.
pub(crate) fn validate_policy_features(
    features: &Option<Vec<String>>,
) -> UnifiedResult<Option<Vec<String>>> {
    let Some(features) = features else {
        return Ok(None);
    };
    if features.len() > MAX_POLICY_LIST_LEN {
        return Err(UnifiedError::bad_request(format!(
            "allowed_features must contain at most {MAX_POLICY_LIST_LEN} entries"
        )));
    }
    let mut cleaned: Vec<String> = Vec::with_capacity(features.len());
    for feature in features {
        let feature = feature.trim();
        if !KNOWN_USE_CASES.contains(&feature) && !ai_capability_registry().has_use_case(feature) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown AI feature \"{feature}\": expected one of {} or the use case of a registered AI capability",
                KNOWN_USE_CASES.join(", ")
            )));
        }
        if !cleaned.iter().any(|item| item == feature) {
            cleaned.push(feature.to_string());
        }
    }
    Ok(Some(cleaned))
}

/// Validate a monthly token cap: bounded. `None` means no cap.
pub(crate) fn validate_token_cap(cap: Option<u64>) -> UnifiedResult<Option<u64>> {
    match cap {
        Some(cap) if cap > MAX_MONTHLY_TOKEN_CAP => Err(UnifiedError::bad_request(format!(
            "monthly_token_cap must be at most {MAX_MONTHLY_TOKEN_CAP}"
        ))),
        cap => Ok(cap),
    }
}

/// Validate a monthly cost cap (USD): finite, non-negative, bounded.
/// `None` means no cap.
pub(crate) fn validate_cost_cap(cap: Option<f64>) -> UnifiedResult<Option<f64>> {
    match cap {
        Some(cap) if !cap.is_finite() || cap < 0.0 || cap > MAX_MONTHLY_COST_CAP_USD => {
            Err(UnifiedError::bad_request(format!(
                "monthly_cost_cap_usd must be between 0 and {MAX_MONTHLY_COST_CAP_USD}"
            )))
        }
        cap => Ok(cap),
    }
}

/// Validate a caller-supplied use case against the known list.
pub(crate) fn validate_use_case(use_case: &str) -> UnifiedResult<String> {
    let use_case = use_case.trim();
    if !KNOWN_USE_CASES.contains(&use_case) {
        return Err(UnifiedError::bad_request(format!(
            "Unknown AI use case \"{use_case}\": expected one of {}",
            KNOWN_USE_CASES.join(", ")
        )));
    }
    Ok(use_case.to_string())
}

/// True when the model belongs to the provider kind's known family.
/// Fixed-kind providers only: compatible and local endpoints accept
/// any model id. Fine-tuned OpenAI models (`ft:...`) are accepted.
pub(crate) fn is_known_model(kind: AiProviderKind, model: &str) -> bool {
    match kind {
        AiProviderKind::Anthropic => model.starts_with("claude-"),
        AiProviderKind::OpenAi => {
            model.starts_with("gpt-")
                || model.starts_with("o1")
                || model.starts_with("o3")
                || model.starts_with("chatgpt-")
                || model.starts_with("ft:")
        }
        AiProviderKind::OpenAiCompatible | AiProviderKind::Local => !model.trim().is_empty(),
    }
}

/// Rough token estimate for a prompt: ~4 characters per token.
/// Used only for pre-call cap checks; actual billing uses the
/// provider's reported counts.
pub(crate) fn estimate_prompt_tokens(text: &str) -> u32 {
    (text.chars().count() as u32).div_ceil(4)
}

/// Start of the current UTC calendar month: the window monthly caps
/// are measured over.
pub(crate) fn month_start() -> DateTime<Utc> {
    let now = Utc::now();
    Utc.with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .single()
        .unwrap_or(now)
}

/// Tokens and estimated cost consumed in the current month by one
/// user / one company / one user's personal key. Only successful
/// and failed provider calls count; denied calls carry zero tokens.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct MonthlyUsage {
    pub(crate) tokens: u64,
    pub(crate) cost_usd: f64,
}

pub(crate) fn monthly_usage_for_user(
    state: &AiState,
    user_id: Uuid,
    since: DateTime<Utc>,
) -> MonthlyUsage {
    sum_monthly_usage(
        state
            .usage
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| record.user_id == Some(user_id) && record.created_at >= since),
    )
}

pub(crate) fn monthly_usage_for_company(
    state: &AiState,
    company_id: Uuid,
    since: DateTime<Utc>,
) -> MonthlyUsage {
    sum_monthly_usage(
        state
            .usage
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| record.company_id == Some(company_id) && record.created_at >= since),
    )
}

pub(crate) fn monthly_personal_key_usage(
    state: &AiState,
    user_id: Uuid,
    since: DateTime<Utc>,
) -> MonthlyUsage {
    sum_monthly_usage(
        state
            .usage
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                record.user_id == Some(user_id)
                    && record.used_personal_key
                    && record.created_at >= since
            }),
    )
}

pub(crate) fn sum_monthly_usage(records: impl Iterator<Item = AiUsageRecord>) -> MonthlyUsage {
    let mut usage = MonthlyUsage::default();
    for record in records {
        // Prefer the recorded total; when a provider reported no
        // counts and no fallback was available, count the prompt
        // tokens rather than silently counting 0 for the call.
        usage.tokens += record.total_tokens.or(record.prompt_tokens).unwrap_or(0) as u64;
        usage.cost_usd += record.estimated_cost_usd.unwrap_or(0.0);
    }
    usage
}

/// The effective monthly token cap for a user: the user's own cap
/// when it is set and greater than zero, otherwise the company cap.
/// `None` means no cap at all.
pub(crate) fn effective_user_cap(user_cap: Option<u64>, company_cap: Option<u64>) -> Option<u64> {
    match user_cap {
        Some(cap) if cap > 0 => Some(cap),
        _ => company_cap,
    }
}

/// The effective monthly cost cap for a user: same inheritance rule
/// as `effective_user_cap`.
pub(crate) fn effective_user_cost_cap(
    user_cap: Option<f64>,
    company_cap: Option<f64>,
) -> Option<f64> {
    match user_cap {
        Some(cap) if cap > 0.0 => Some(cap),
        _ => company_cap,
    }
}

#[cfg(test)]
mod tests {
    use super::estimate_prompt_tokens;
    use super::is_known_model;
    use super::sum_monthly_usage;
    use super::validate_cost_cap;
    use super::validate_token_cap;
    use crate::modules::ai::AiProviderKind;
    use crate::modules::ai::AiUsageRecord;
    use chrono::Utc;
    use uuid::Uuid;

    fn usage_record(total: Option<u32>, prompt: Option<u32>, cost: Option<f64>) -> AiUsageRecord {
        AiUsageRecord {
            id: Uuid::now_v7(),
            provider_id: Uuid::now_v7(),
            provider_kind: "local".to_string(),
            actor: "actor".to_string(),
            user_id: None,
            company_id: None,
            use_case: "complete".to_string(),
            model: "gpt-4o-mini".to_string(),
            prompt_tokens: prompt,
            completion_tokens: None,
            total_tokens: total,
            estimated_cost_usd: cost,
            latency_ms: 0,
            status: "success".to_string(),
            error_code: None,
            denial_reason: None,
            used_personal_key: false,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn monthly_usage_counts_prompt_tokens_when_total_is_missing() {
        let usage = sum_monthly_usage(
            [
                usage_record(Some(10), Some(6), Some(0.001)),
                usage_record(None, Some(40), None),
                usage_record(None, None, None),
            ]
            .into_iter(),
        );
        assert_eq!(usage.tokens, 50);
        assert!((usage.cost_usd - 0.001).abs() < 1e-9);
    }

    #[test]
    fn prompt_tokens_round_up_per_four_chars() {
        assert_eq!(estimate_prompt_tokens(""), 0);
        assert_eq!(estimate_prompt_tokens("abcd"), 1);
        assert_eq!(estimate_prompt_tokens("abcde"), 2);
        assert_eq!(estimate_prompt_tokens("abcdefgh"), 2);
    }

    #[test]
    fn token_cap_bounds() {
        assert_eq!(validate_token_cap(None).unwrap(), None);
        assert_eq!(validate_token_cap(Some(100)).unwrap(), Some(100));
        assert!(validate_token_cap(Some(1_000_000_001)).is_err());
    }

    #[test]
    fn cost_cap_bounds() {
        assert_eq!(validate_cost_cap(None).unwrap(), None);
        assert_eq!(validate_cost_cap(Some(10.0)).unwrap(), Some(10.0));
        assert!(validate_cost_cap(Some(-1.0)).is_err());
        assert!(validate_cost_cap(Some(f64::NAN)).is_err());
        assert!(validate_cost_cap(Some(1_000_001.0)).is_err());
    }

    #[test]
    fn known_model_families() {
        assert!(is_known_model(AiProviderKind::Anthropic, "claude-3-haiku"));
        assert!(!is_known_model(AiProviderKind::Anthropic, "gpt-4o"));
        assert!(is_known_model(AiProviderKind::OpenAi, "gpt-4o"));
        assert!(is_known_model(
            AiProviderKind::OpenAi,
            "ft:gpt-4o:org:custom"
        ));
        assert!(!is_known_model(AiProviderKind::OpenAi, "claude-3-haiku"));
        assert!(is_known_model(AiProviderKind::Local, "anything-goes"));
        assert!(!is_known_model(AiProviderKind::Local, "   "));
    }

    #[test]
    fn require_company_scope_denies_unresolved_callers() {
        let company = Uuid::now_v7();
        assert!(super::require_company_scope(Some(company), company).is_ok());
        assert!(super::require_company_scope(Some(Uuid::now_v7()), company).is_err());
        assert!(super::require_company_scope(None, company).is_err());
    }
}
