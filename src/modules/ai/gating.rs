//! ai module - Layered AI gating, personal keys and MCP validation.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use actix_web::HttpRequest;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use std::net::{IpAddr, ToSocketAddrs};
use url::Url;
use uuid::Uuid;

use super::constants::CAP_RESERVATION_TTL_SECS;
use super::constants::EVENT_AI_ACCESS_DENIED;
use super::constants::EVENT_AI_GATING_CHANGED;
use super::constants::EVENT_AI_PRODUCT_INCLUDES_AI;
use super::constants::EVENT_USER_COMPANY_REQUESTED;
use super::constants::KNOWN_USE_CASES;
use super::constants::MAX_MCP_URL_LEN;
use super::constants::MAX_POLICY_LIST_LEN;
use super::context::estimate_cost_usd;
use super::crypto::decrypt_api_key;
use super::crypto::encryption_secret;
use super::helpers::Caller;
use super::helpers::caller_user_id;
use super::helpers::emit_audit;
use super::helpers::ok;
use super::helpers::record_denied_usage;
use super::state::AiState;
use super::types::AiCompanyPolicy;
use super::types::AiGateDecision;
use super::types::AiGateLayerOutcome;
use super::types::AiProviderKind;
use super::types::AiUserMcpServer;
use super::types::AiUserPolicy;
use super::types::CapReservation;
use super::types::GroundingSource;
use super::validation::MonthlyUsage;
use super::validation::effective_user_cap;
use super::validation::effective_user_cost_cap;
use super::validation::is_known_model;
use super::validation::month_start;
use super::validation::monthly_personal_key_usage;
use super::validation::monthly_usage_for_company;
use super::validation::monthly_usage_for_user;

/// What a call needs the allowlist gate to decide.
pub struct AiCallRequest<'a> {
    pub use_case: &'a str,
    pub model: &'a str,
    pub provider_id: Uuid,
    pub provider_kind: &'a str,
    /// Prompt estimate + requested completion budget (total tokens
    /// the call may consume). The token cap is checked against
    /// this total.
    pub estimated_tokens: u32,
    /// Requested completion budget (the call's `max_tokens`):
    /// the completion side of the cost estimate. The cost cap is
    /// checked against prompt tokens at the prompt price plus
    /// this many tokens at the completion price — never against
    /// the prompt price alone.
    pub estimated_completion_tokens: u32,
    pub use_personal_key: bool,
    /// Purchased-product context for the product gate layer
    /// (`X-Product-Id` header, re-resolved from the request on
    /// every call). `None` means no product context is present:
    /// the product layer defaults to allow and the gate decision
    /// records that. Never cached across calls.
    pub product_id: Option<Uuid>,
}

/// The outcome of a successful authorization, carried into
/// `run_completion` so the call runs with the right key and the
/// usage record carries the right identity.
pub struct AiAuthorization {
    pub user_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub use_personal_key: bool,
    /// The caller's decrypted personal key, present only when the
    /// call runs on it. Lives only for the duration of the call.
    pub personal_key: Option<AuthorizedPersonalKey>,
    /// Cap reservation created by the authorizing call (strict
    /// mode only; `None` in open mode, where there are no caps).
    /// The caller must release it once the call's actual usage is
    /// recorded (`release_cap_reservation`); a stale reservation
    /// expires after `CAP_RESERVATION_TTL_SECS`.
    pub reservation_id: Option<Uuid>,
}

pub struct AuthorizedPersonalKey {
    pub key_id: Uuid,
    pub provider_kind: AiProviderKind,
    pub decrypted_key: String,
}

/// Map a personal-key load failure to a denial: a 403 carries its
/// reason code into the denial recorder (so the attempt is recorded
/// and audited like every other denial); anything else — a stored
/// key that cannot be decrypted — propagates unchanged.
pub(crate) fn map_personal_key_denial(
    state: &AiState,
    deny: &dyn Fn(&AiState, &str) -> UnifiedError,
    error: UnifiedError,
) -> UnifiedError {
    if error.code != 403 {
        return error;
    }
    let reason = error
        .metadata
        .get("reason")
        .and_then(|value| value.as_str())
        .unwrap_or("ai_personal_key_not_configured")
        .to_string();
    deny(state, &reason)
}

/// Whether the allowlist is enforced for this call. Default-deny:
/// enforcement is ON unless an admin explicitly opted into legacy
/// open mode (`AiGlobalSettings::open_mode`) AND no company or
/// user policy exists anywhere. With no policies at all and no
/// opt-in, calls are denied — a fresh install is closed until an
/// admin creates a policy or explicitly turns open mode on. Once
/// any policy exists, enforcement is strict regardless of the
/// open-mode flag.
pub(crate) fn enforcement_on(state: &AiState) -> bool {
    let open_mode = state
        .global_settings
        .read()
        .expect("ai global settings lock poisoned")
        .open_mode;
    !open_mode || !state.company_policies.is_empty() || !state.user_policies.is_empty()
}

/// The single allowlist gate. EVERY completion / draft entry point
/// and the connection test call this FIRST, before any provider
/// traffic. Checks run in this order; the first failure denies the
/// call, records a denied usage row, and emits `ai.access.denied`:
///   1. company allowed (policy exists and is enabled),
///   2. user allowed (policy exists and is enabled),
///   3. feature allowed for this use case,
///   4. model allowed (known family for the kind, then policy lists),
///   5. company monthly token / cost cap,
///   6. user monthly token / cost cap (or the inherited company cap),
///   7. personal-key quota when a personal key is used.
///
/// Steps 5-7 are checked and reserved atomically (see
/// `check_caps_and_reserve`): the cost estimate counts prompt
/// tokens at the prompt price plus the requested completion
/// budget at the completion price, and a passing check inserts a
/// cap reservation for the estimate, so concurrent calls cannot
/// each pass before any usage is recorded.
///
/// Before any of the steps above, the layered gate runs first
/// (`evaluate_ai_gate`): the global kill-switch, the purchased
/// product layer, the company and user on/off switches, and the
/// per-feature toggles. The allowlist answers "is this caller
/// allowed at all"; the gate answers "is AI on for this caller /
/// feature right now". Both must allow.
///
/// Denials are 403s with a non-leaking reason code in the `reason`
/// metadata field. The message and the audit event never reveal
/// another user's or the company's limits.
pub(crate) fn authorize_ai_call(
    ctx: &AppContext,
    state: &AiState,
    caller: &Caller,
    company_id: Option<Uuid>,
    request: &AiCallRequest<'_>,
) -> UnifiedResult<AiAuthorization> {
    let user_id = caller_user_id(caller);
    let enforcement_on = enforcement_on(state);
    // Deny helper: record the denial, emit the audit event, and
    // return the non-leaking 403. Defined once so every check below
    // denies exactly the same way.
    let deny = |state: &AiState, reason: &str| -> UnifiedError {
        let denied = record_denied_usage(
            state,
            &caller.actor,
            user_id,
            company_id,
            request.use_case,
            request.model,
            request.provider_id,
            request.provider_kind,
            reason,
        );
        emit_audit(
            ctx,
            EVENT_AI_ACCESS_DENIED,
            &caller.actor,
            "ai-access",
            json!({
                "id": denied.id.to_string(),
                "reason": reason,
                "use_case": request.use_case,
                "user_id": user_id.map(|id| id.to_string()),
                "company_id": company_id.map(|id| id.to_string()),
            }),
        );
        UnifiedError::forbidden("AI access is not allowed for this caller")
            .with_metadata("reason", Value::String(reason.to_string()))
            .with_metadata("use_case", Value::String(request.use_case.to_string()))
    };

    // Layered gating, evaluated FIRST on every call and re-checked
    // on every call (the decision is never cached): the global
    // kill-switch, the purchased-product layer, the company and
    // user on/off switches, and the per-feature toggles
    // (`evaluate_ai_gate`). The allowlist checks below (steps 1-7)
    // still run afterwards: they enforce caps, models, and
    // personal-key quotas, which the gate does not. A gate denial
    // records a denied usage row and emits `ai.access.denied`
    // through the same `deny` helper as every other denial, so a
    // denied read is logged with the reason code and no prompt
    // content.
    let gate = evaluate_ai_gate(
        ctx,
        state,
        user_id,
        company_id,
        request.use_case,
        request.product_id,
    );
    if !gate.allowed {
        return Err(deny(
            state,
            gate.reason_code.as_deref().unwrap_or("ai_not_allowed"),
        ));
    }

    // Explicitly opted-in open mode only: no policy exists
    // anywhere AND an admin turned `open_mode` on in the global
    // settings. Personal-key calls still need a key, and the key
    // still has to match the connection. With no opt-in, the
    // policy checks below deny the call (default-deny).
    if !enforcement_on {
        let personal_key = if request.use_personal_key {
            let key = load_personal_key_for_call(state, ctx, user_id, request)
                .map_err(|error| map_personal_key_denial(state, &deny, error))?;
            Some(key)
        } else {
            None
        };
        return Ok(AiAuthorization {
            user_id,
            company_id,
            use_personal_key: request.use_personal_key,
            personal_key,
            reservation_id: None,
        });
    }

    // 1. Company allowed.
    let company_policy = match company_id {
        Some(company_id) => match state.company_policies.get(&company_id) {
            Some(policy) if policy.enabled => policy.value().clone(),
            Some(_) => return Err(deny(state, "ai_not_allowed_for_company")),
            None => return Err(deny(state, "ai_not_allowed_for_company")),
        },
        None => return Err(deny(state, "ai_company_unknown")),
    };
    // 2. User allowed.
    let user_policy = match user_id {
        Some(user_id) => match state.user_policies.get(&user_id) {
            Some(policy) if policy.enabled => policy.value().clone(),
            Some(_) => return Err(deny(state, "ai_not_allowed_for_user")),
            None => return Err(deny(state, "ai_not_allowed_for_user")),
        },
        None => return Err(deny(state, "ai_user_unknown")),
    };
    // 3. Feature allowed for this use case.
    for allowed in [
        &company_policy.allowed_features,
        &user_policy.allowed_features,
    ] {
        if let Some(features) = allowed {
            if !features.iter().any(|feature| feature == request.use_case) {
                return Err(deny(state, "ai_feature_not_allowed"));
            }
        }
    }
    // 4. Model allowed: known family for the kind, then policy lists.
    let kind = AiProviderKind::from_str(request.provider_kind).unwrap_or(AiProviderKind::Local);
    if !is_known_model(kind, request.model) {
        return Err(deny(state, "ai_model_not_allowed"));
    }
    for allowed in [&company_policy.allowed_models, &user_policy.allowed_models] {
        if let Some(models) = allowed {
            if !models.iter().any(|model| model == request.model) {
                return Err(deny(state, "ai_model_not_allowed"));
            }
        }
    }
    // 5-7. Caps and the personal-key quota, checked and reserved
    // atomically (see `check_caps_and_reserve`). The personal key
    // is loaded BEFORE the locked section so no decryption runs
    // while the ledger lock is held. The cost estimate counts
    // both sides: prompt tokens at the prompt price plus the
    // requested completion budget at the completion price.
    // From here on `user_id` is the policy's user id (a plain
    // `Uuid`): step 2 already proved the caller is that user.
    let user_id = user_policy.user_id;
    let prompt_tokens = request
        .estimated_tokens
        .saturating_sub(request.estimated_completion_tokens);
    let estimated_cost = estimate_cost_usd(
        request.model,
        prompt_tokens,
        request.estimated_completion_tokens,
    );
    let personal_key = if request.use_personal_key {
        Some(
            load_personal_key_for_call(state, ctx, Some(user_id), request)
                .map_err(|error| map_personal_key_denial(state, &deny, error))?,
        )
    } else {
        None
    };
    let reservation_id = match check_caps_and_reserve(
        state,
        &company_policy,
        &user_policy,
        user_id,
        request,
        estimated_cost,
        personal_key.is_some(),
    ) {
        Ok(id) => id,
        Err(reason) => return Err(deny(state, reason)),
    };
    Ok(AiAuthorization {
        user_id: Some(user_id),
        company_id: Some(company_policy.company_id),
        use_personal_key: request.use_personal_key,
        personal_key,
        reservation_id: Some(reservation_id),
    })
}

/// True when a cap reservation is stale: older than
/// `CAP_RESERVATION_TTL_SECS`, or from a previous calendar month.
/// Stale reservations are ignored by cap checks and pruned when
/// the next reservation is made.
pub(crate) fn reservation_is_stale(
    reservation: &CapReservation,
    now: DateTime<Utc>,
    since: DateTime<Utc>,
) -> bool {
    reservation.created_at < since
        || now
            .signed_duration_since(reservation.created_at)
            .num_seconds()
            >= CAP_RESERVATION_TTL_SECS as i64
}

/// Add the live (non-stale) reservations for one user or one
/// company to a monthly usage total, so cap checks see spend
/// that passed authorization but has not recorded actual usage
/// yet. Pass `Some(user_id)` for a user-scoped total,
/// `Some(company_id)` for a company-scoped total.
pub(crate) fn add_reserved_usage(
    state: &AiState,
    usage: &mut MonthlyUsage,
    user_id: Option<Uuid>,
    company_id: Option<Uuid>,
    since: DateTime<Utc>,
) {
    let now = Utc::now();
    for entry in state.cap_reservations.iter() {
        let reservation = entry.value();
        if reservation_is_stale(reservation, now, since) {
            continue;
        }
        let matches = match (user_id, company_id) {
            (Some(user_id), _) => reservation.user_id == Some(user_id),
            (_, Some(company_id)) => reservation.company_id == Some(company_id),
            (None, None) => false,
        };
        if matches {
            usage.tokens += reservation.tokens;
            usage.cost_usd += reservation.cost_usd;
        }
    }
}

/// Remove a cap reservation, once the call's actual usage has been
/// recorded (or the call will never run). A no-op for `None`.
pub(crate) fn release_cap_reservation(state: &AiState, reservation_id: Option<Uuid>) {
    if let Some(id) = reservation_id {
        state.cap_reservations.remove(&id);
    }
}

/// Check the company caps, the user caps, and the personal-key
/// quota against recorded monthly usage PLUS live reservations,
/// and — when every cap passes — insert a reservation for this
/// call's estimate. The check and the insert run under the cap
/// ledger lock, so two concurrent calls cannot both pass against
/// the same pre-call usage. Returns the reservation id, or the
/// non-leaking reason code of the first cap that fails. The
/// caller maps the reason to a denial through its `deny` helper;
/// this function itself records nothing.
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_caps_and_reserve(
    state: &AiState,
    company_policy: &AiCompanyPolicy,
    user_policy: &AiUserPolicy,
    user_id: Uuid,
    request: &AiCallRequest<'_>,
    estimated_cost: Option<f64>,
    has_personal_key: bool,
) -> Result<Uuid, &'static str> {
    let _ledger = state
        .cap_ledger_lock
        .lock()
        .expect("ai cap ledger lock poisoned");
    let since = month_start();
    let now = Utc::now();
    state
        .cap_reservations
        .retain(|_, reservation| !reservation_is_stale(reservation, now, since));
    // 5. Company monthly caps.
    let mut company_usage = monthly_usage_for_company(state, company_policy.company_id, since);
    add_reserved_usage(
        state,
        &mut company_usage,
        None,
        Some(company_policy.company_id),
        since,
    );
    if let Some(cap) = company_policy.monthly_token_cap {
        if company_usage.tokens + request.estimated_tokens as u64 > cap {
            return Err("ai_company_token_cap_exceeded");
        }
    }
    if let Some(cap) = company_policy.monthly_cost_cap_usd {
        if let Some(estimated) = estimated_cost {
            if company_usage.cost_usd + estimated > cap {
                return Err("ai_company_cost_cap_exceeded");
            }
        }
    }
    // 6. User monthly caps (own cap, or the inherited company cap).
    let mut user_usage = monthly_usage_for_user(state, user_id, since);
    add_reserved_usage(state, &mut user_usage, Some(user_id), None, since);
    if let Some(cap) = effective_user_cap(
        user_policy.monthly_token_cap,
        company_policy.monthly_token_cap,
    ) {
        if user_usage.tokens + request.estimated_tokens as u64 > cap {
            return Err("ai_user_token_cap_exceeded");
        }
    }
    if let Some(cap) = effective_user_cost_cap(
        user_policy.monthly_cost_cap_usd,
        company_policy.monthly_cost_cap_usd,
    ) {
        if let Some(estimated) = estimated_cost {
            if user_usage.cost_usd + estimated > cap {
                return Err("ai_user_cost_cap_exceeded");
            }
        }
    }
    // 7. Personal-key quota, when the call runs on one: the key's
    // own quota when set, otherwise the effective user cap already
    // checked in step 6 covers it.
    if has_personal_key {
        if let Some(cap) = key_quota(state, user_id) {
            let mut key_usage = monthly_personal_key_usage(state, user_id, since);
            add_reserved_usage(state, &mut key_usage, Some(user_id), None, since);
            if key_usage.tokens + request.estimated_tokens as u64 > cap {
                return Err("ai_personal_key_quota_exceeded");
            }
        }
    }
    // Every cap passes: reserve the estimate for this call.
    let reservation = CapReservation {
        id: Uuid::now_v7(),
        user_id: Some(user_id),
        company_id: Some(company_policy.company_id),
        tokens: request.estimated_tokens as u64,
        cost_usd: estimated_cost.unwrap_or(0.0),
        created_at: now,
    };
    let id = reservation.id;
    state.cap_reservations.insert(id, reservation);
    Ok(id)
}

/// Load and decrypt the caller's personal key for a call, checking
/// it exists, is enabled, and matches the connection kind (and the
/// key's model when one is set). Failures are 403s whose metadata
/// carries the non-leaking reason code; `authorize_ai_call` maps
/// them through the denial recorder so the attempt is audited.
/// A decryption failure is an internal error, not a denial, and
/// propagates unchanged.
pub(crate) fn load_personal_key_for_call(
    state: &AiState,
    ctx: &AppContext,
    user_id: Option<Uuid>,
    request: &AiCallRequest<'_>,
) -> UnifiedResult<AuthorizedPersonalKey> {
    let user_id = user_id.ok_or_else(|| {
        UnifiedError::forbidden("AI access is not allowed for this caller")
            .with_metadata("reason", Value::String("ai_user_unknown".to_string()))
    })?;
    let key = state.personal_keys.get(&user_id).ok_or_else(|| {
        UnifiedError::forbidden("AI access is not allowed for this caller").with_metadata(
            "reason",
            Value::String("ai_personal_key_not_configured".to_string()),
        )
    })?;
    let key = key.value().clone();
    if !key.enabled {
        return Err(
            UnifiedError::forbidden("AI access is not allowed for this caller").with_metadata(
                "reason",
                Value::String("ai_personal_key_not_configured".to_string()),
            ),
        );
    }
    if key.provider_kind.as_str() != request.provider_kind {
        return Err(
            UnifiedError::forbidden("AI access is not allowed for this caller").with_metadata(
                "reason",
                Value::String("ai_personal_key_mismatch".to_string()),
            ),
        );
    }
    if let Some(model) = &key.model {
        if model != request.model {
            return Err(
                UnifiedError::forbidden("AI access is not allowed for this caller").with_metadata(
                    "reason",
                    Value::String("ai_personal_key_mismatch".to_string()),
                ),
            );
        }
    }
    let secret = encryption_secret(ctx)?;
    let decrypted = decrypt_api_key(&secret, &key.api_key_encrypted)?;
    Ok(AuthorizedPersonalKey {
        key_id: key.id,
        provider_kind: key.provider_kind,
        decrypted_key: decrypted,
    })
}

/// The personal key's own monthly token cap, when one is set and
/// greater than zero. `None` means "inherit the effective user cap".
pub(crate) fn key_quota(state: &AiState, user_id: Uuid) -> Option<u64> {
    state
        .personal_keys
        .get(&user_id)
        .and_then(|entry| entry.monthly_token_cap)
        .filter(|cap| *cap > 0)
}

// ---------------------------------------------------------------------------
// Personal MCP endpoint validation (SSRF guard).
// ---------------------------------------------------------------------------

/// True when the address must never be an MCP target: private,
/// link-local, unspecified, broadcast, multicast, documentation,
/// benchmarking, or reserved ranges (v4), unique-local / link-local
/// / multicast / unspecified (v6). Mirrors the webhook module's
/// `is_forbidden_ip`, with loopback included: personal MCP servers
/// are remote services, so loopback is never a valid target.
pub(crate) fn is_forbidden_mcp_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(addr) => {
            addr.is_loopback()
                || addr.is_private()
                || addr.is_link_local()
                || addr.is_unspecified()
                || addr.is_broadcast()
                || addr.is_multicast()
                || addr.is_documentation()
                || unified_core::net::ipv4_is_benchmarking(&addr)
                || unified_core::net::ipv4_is_reserved(&addr)
        }
        IpAddr::V6(addr) => {
            addr.is_loopback()
                || addr.is_unspecified()
                || addr.is_multicast()
                || addr.is_unique_local()
                || (addr.segments()[0] & 0xfe00) == 0xfc00 // unique-local fc00::/7
                || (addr.segments()[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
        }
    }
}

/// Validate a personal MCP endpoint URL at write time: https only,
/// no embedded credentials, no localhost / internal hostname, and
/// the host must resolve to at least one public address. A hostname
/// that does not resolve is refused: an unresolvable endpoint can
/// never serve context. Residual risk: DNS rebinding between
/// validation and use. This module never fetches an MCP endpoint
/// (servers are offered as grounding references only), so there is
/// no request-time fetch to rebind against; re-check the host
/// before any future live MCP fetch is added.
pub(crate) fn validate_mcp_endpoint_url(raw: &str) -> UnifiedResult<Url> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > MAX_MCP_URL_LEN {
        return Err(UnifiedError::bad_request(format!(
            "endpoint_url must be between 1 and {MAX_MCP_URL_LEN} characters"
        )));
    }
    let url = Url::parse(raw)
        .map_err(|_| UnifiedError::bad_request(format!("Invalid MCP endpoint URL \"{raw}\"")))?;
    if url.scheme() != "https" {
        return Err(UnifiedError::bad_request("MCP endpoint URL must use https"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UnifiedError::bad_request(
            "MCP endpoint URL must not embed credentials; use the auth token field",
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| UnifiedError::bad_request("MCP endpoint URL must include a host"))?;
    let lower = host.to_ascii_lowercase();
    if lower == "localhost"
        || lower.ends_with(".localhost")
        || lower.ends_with(".local")
        || lower.ends_with(".internal")
        || lower.ends_with(".lan")
        || lower.ends_with(".home")
        || lower.ends_with(".corp")
        || lower.ends_with(".intranet")
    {
        return Err(UnifiedError::bad_request(
            "MCP endpoint host must be a public host, not a local or internal name",
        ));
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_forbidden_mcp_ip(ip) {
            return Err(UnifiedError::bad_request(
                "MCP endpoint host must be a public IP address, not a private, loopback, or link-local address",
            ));
        }
        return Ok(url);
    }
    // Hostname: it must resolve, and every resolved address must be
    // public. A name that resolves only to private addresses (or
    // that mixes public and private ones) is refused outright.
    let port = url.port_or_known_default().unwrap_or(443);
    let addresses = (host, port).to_socket_addrs().map_err(|_| {
        UnifiedError::bad_request(format!("MCP endpoint host \"{host}\" does not resolve"))
    })?;
    let mut any = false;
    for socket in addresses {
        any = true;
        if is_forbidden_mcp_ip(socket.ip()) {
            return Err(UnifiedError::bad_request(format!(
                "MCP endpoint host \"{host}\" resolves to a private, loopback, or link-local address"
            )));
        }
    }
    if !any {
        return Err(UnifiedError::bad_request(format!(
            "MCP endpoint host \"{host}\" does not resolve"
        )));
    }
    Ok(url)
}

/// The caller's enabled personal MCP servers as grounding sources
/// for the shared context: each server contributes its name and
/// endpoint URL through the existing `SharedContext` grounding
/// assembly (`assemble_shared_context` /
/// `shared_context_prompt`), so the prompt carries the reference
/// without duplicating context code. The servers are references
/// only: this module never fetches an MCP endpoint at call time.
/// A server is never offered to another user.
pub(crate) fn personal_mcp_grounding_sources(
    state: &AiState,
    user_id: Option<Uuid>,
) -> Vec<GroundingSource> {
    let Some(user_id) = user_id else {
        return Vec::new();
    };
    let mut servers: Vec<AiUserMcpServer> = state
        .personal_mcp_servers
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|server| server.user_id == user_id && server.enabled)
        .collect();
    servers.sort_by(|a, b| a.name.cmp(&b.name).then(a.created_at.cmp(&b.created_at)));
    servers
        .into_iter()
        .map(|server| GroundingSource {
            article_id: format!("mcp:{}", server.id),
            title: format!("Personal MCP server: {}", server.name),
            url: server.endpoint_url.clone(),
            score: 0.0,
            excerpt: Some("Personal MCP context source configured by the caller".to_string()),
        })
        .collect()
}

/// In strict mode, a personal key / MCP change requires the caller
/// to be an allowed user (enabled user policy). In explicitly
/// opted-in open mode any caller holding `ai:use` may configure
/// their own.
pub(crate) fn require_allowed_user(state: &AiState, user_id: Uuid) -> UnifiedResult<()> {
    if !enforcement_on(state) {
        return Ok(());
    }
    match state.user_policies.get(&user_id) {
        Some(policy) if policy.enabled => Ok(()),
        _ => Err(
            UnifiedError::forbidden("AI access is not allowed for this caller").with_metadata(
                "reason",
                Value::String("ai_not_allowed_for_user".to_string()),
            ),
        ),
    }
}

// ---------------------------------------------------------------------------
// Layered AI gating.
//
// Effective access is the AND of four layers; any layer can disable
// AI, and every layer must allow for a call to run:
//
//   (a) SETTINGS - the global kill-switch (`ai.global_enabled`)
//       plus the global per-feature switches
//       (`ai.features.<name>.enabled`). The kill-switch off denies
//       everything immediately, regardless of the other layers.
//   (b) PRODUCT - the purchased product / subscription includes AI
//       or not, resolved through the `ai.product_includes_ai` hook
//       query (see `EVENT_AI_PRODUCT_INCLUDES_AI`). No product
//       context on the call means the product layer defaults to
//       allow and the decision records that; a product context the
//       hook cannot confirm denies the call (fail closed).
//   (c) COMPANY - the company policy's allowlist switch (`enabled`)
//       AND its gate switch (`ai_enabled`), plus the company
//       per-feature off switches (`disabled_features`).
//   (d) USER - the user policy's allowlist switch (`enabled`) AND
//       its gate switch (`ai_enabled`), plus the user per-feature
//       off switches (`disabled_features`). A disabled user is
//       denied for every feature; a switched-off feature denies
//       only that feature.
//
// The allowlist answers "is this caller allowed at all" (identity,
// model lists, caps, personal-key quota); the gate answers "is AI
// on for this caller / feature right now". ONE function composes
// them: `evaluate_ai_gate`, called FIRST inside
// `authorize_ai_call` on every completion / draft entry point, so
// the combined evaluation order is:
//
//   settings kill-switch -> global feature switch -> product ->
//   company allowlist + gate -> user allowlist + gate ->
//   feature toggles (allowlist lists, then per-layer off switches)
//   -> (back in `authorize_ai_call`) model, caps, personal key.
//
// The decision is computed fresh from the live stores on every
// call: no gate decision is cached across calls. A denied call is
// logged by the caller's `deny` helper in `authorize_ai_call`
// (denied usage row + `ai.access.denied` event, count + reason
// code, never prompt content). Every change to any layer emits
// `ai.gating.changed` with the old -> new values through
// `emit_gating_change`, on top of the policy-change audit the
// policy endpoints already emit.
// ---------------------------------------------------------------------------

/// Canonical feature (use-case) names the gate understands, plus
/// the friendly aliases admins may use in gating requests.
/// `support` / `drafting` mean ticket-reply drafting
/// (`support_reply`); `insights` means insight generation, which
/// runs through the generic completion use case (`complete`), so
/// switching `insights` off switches `complete` off as well until
/// a dedicated insights use case exists.
pub(crate) fn canonical_gate_feature(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "complete" | "insights" | "insight" => Some("complete"),
        "support_reply" | "support" | "drafting" | "support_drafting" => Some("support_reply"),
        "chat_reply" | "chat" => Some("chat_reply"),
        "content" => Some("content"),
        "email" => Some("email"),
        "kb_answer" | "kb" | "knowledgebase" => Some("kb_answer"),
        "test" => Some("test"),
        _ => None,
    }
}

/// Validate one feature name for a gating request, canonicalizing
/// friendly aliases to the use-case name the gate checks.
pub(crate) fn validate_gate_feature_name(raw: &str) -> UnifiedResult<String> {
    canonical_gate_feature(raw)
        .map(str::to_string)
        .ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Unknown AI feature \"{raw}\": expected one of {}",
                KNOWN_USE_CASES.join(", ")
            ))
        })
}

/// Validate a per-feature off-switch list: canonicalize every
/// entry, reject unknown names, drop duplicates. `None` means no
/// feature is switched off here.
pub(crate) fn validate_gate_features(
    features: &Option<Vec<String>>,
) -> UnifiedResult<Option<Vec<String>>> {
    let Some(features) = features else {
        return Ok(None);
    };
    if features.len() > MAX_POLICY_LIST_LEN {
        return Err(UnifiedError::bad_request(format!(
            "disabled_features must contain at most {MAX_POLICY_LIST_LEN} entries"
        )));
    }
    let mut cleaned: Vec<String> = Vec::with_capacity(features.len());
    for feature in features {
        let canonical = validate_gate_feature_name(feature)?;
        if !cleaned.iter().any(|item| item == &canonical) {
            cleaned.push(canonical);
        }
    }
    Ok(Some(cleaned))
}

/// The caller's purchased-product context: the `X-Product-Id`
/// header, re-read from the request on every call. `None` means no
/// product context is present and the product layer defaults to
/// allow.
pub(crate) fn resolve_caller_product(req: &HttpRequest) -> Option<Uuid> {
    req.headers()
        .get("x-product-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|raw| Uuid::parse_str(raw).ok())
}

/// One user's company id through the `user.company_requested`
/// hook, for the effective-gate read of a user other than the
/// caller. `None` when the users module does not answer.
pub(crate) fn resolve_user_company(ctx: &AppContext, user_id: Uuid) -> Option<Uuid> {
    let hook = ctx.hooks.emit(
        EVENT_USER_COMPANY_REQUESTED,
        json!({ "user_id": user_id.to_string() }),
    );
    hook.get("company_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok())
}

/// Ask the products / billing module whether the purchased product
/// includes AI (`ai.product_includes_ai` hook query; contract on
/// `EVENT_AI_PRODUCT_INCLUDES_AI`). `Some(true)` = the product
/// includes AI; `Some(false)` = it excludes AI; `None` = no handler
/// answered, so the caller cannot be confirmed.
pub(crate) fn product_includes_ai(
    ctx: &AppContext,
    product_id: Uuid,
    user_id: Option<Uuid>,
    company_id: Option<Uuid>,
) -> Option<bool> {
    let hook = ctx.hooks.emit(
        EVENT_AI_PRODUCT_INCLUDES_AI,
        json!({
            "product_id": product_id.to_string(),
            "user_id": user_id.map(|id| id.to_string()),
            "company_id": company_id.map(|id| id.to_string()),
        }),
    );
    hook.get("includes_ai").and_then(|value| value.as_bool())
}

pub(crate) fn gate_layer_outcome(layer: &str, allowed: bool, note: &str) -> AiGateLayerOutcome {
    AiGateLayerOutcome {
        layer: layer.to_string(),
        allowed,
        note: Some(note.to_string()),
    }
}

pub(crate) fn gate_denied(
    mut layers: Vec<AiGateLayerOutcome>,
    layer: &str,
    feature: &str,
    reason: &str,
    note: &str,
) -> AiGateDecision {
    layers.push(gate_layer_outcome(layer, false, note));
    AiGateDecision {
        allowed: false,
        denied_layer: Some(layer.to_string()),
        denied_feature: Some(feature.to_string()),
        reason_code: Some(reason.to_string()),
        layers,
    }
}

/// The single composed gate: the allowlist ("is this caller
/// allowed at all") AND the layered on/off switches ("is AI on for
/// this caller / feature right now"), evaluated in this exact
/// order on every call, with no caching:
///
///   1. settings kill-switch (`ai.global_enabled`),
///   2. global per-feature switch for this feature,
///   3. product layer (`ai.product_includes_ai` hook),
///   4. company allowlist (`enabled`) + gate (`ai_enabled`),
///   5. user allowlist (`enabled`) + gate (`ai_enabled`),
///   6. feature toggles: allowlist `allowed_features` lists, then
///      the company and user `disabled_features` off switches.
///
/// A missing company or user policy denies only while enforcement
/// is on; in explicitly opted-in open mode (no policy exists and
/// `open_mode` is on) the company / user layers allow, because no
/// admin has switched anything off, while the settings and product
/// layers still apply. The caller maps a denial to the non-leaking
/// 403 through its own `deny` helper.
pub(crate) fn evaluate_ai_gate(
    ctx: &AppContext,
    state: &AiState,
    user_id: Option<Uuid>,
    company_id: Option<Uuid>,
    feature: &str,
    product_id: Option<Uuid>,
) -> AiGateDecision {
    let mut layers: Vec<AiGateLayerOutcome> = Vec::new();

    // 1-2. Settings: kill-switch, then the global feature switch.
    let global = state
        .global_settings
        .read()
        .expect("ai global settings lock poisoned")
        .clone();
    if !global.global_enabled {
        return gate_denied(
            layers,
            "settings",
            feature,
            "ai_globally_disabled",
            "Global AI kill-switch is off",
        );
    }
    layers.push(gate_layer_outcome(
        "settings",
        true,
        "Global AI kill-switch is on",
    ));
    if global.disabled_features.iter().any(|name| name == feature) {
        return gate_denied(
            layers,
            "settings",
            feature,
            "ai_feature_disabled",
            "Feature is switched off globally",
        );
    }

    // 3. Product: only when a product context is present.
    match product_id {
        Some(product_id) => match product_includes_ai(ctx, product_id, user_id, company_id) {
            Some(true) => layers.push(gate_layer_outcome(
                "product",
                true,
                "Purchased product includes AI",
            )),
            Some(false) => {
                return gate_denied(
                    layers,
                    "product",
                    feature,
                    "ai_product_excludes_ai",
                    "Purchased product does not include AI",
                );
            }
            None => {
                return gate_denied(
                    layers,
                    "product",
                    feature,
                    "ai_product_unknown",
                    "Purchased product could not be confirmed",
                );
            }
        },
        None => layers.push(gate_layer_outcome(
            "product",
            true,
            "No product context: product layer defaults to allow",
        )),
    }

    // 4-5. Company and user: allowlist switch + gate switch. In
    // explicitly opted-in open mode there is no policy to switch
    // anything off.
    let enforcement_on = enforcement_on(state);
    let company_policy: Option<AiCompanyPolicy> = if enforcement_on {
        match company_id {
            Some(company_id) => match state.company_policies.get(&company_id) {
                Some(entry) => {
                    let policy = entry.value().clone();
                    if !policy.enabled {
                        return gate_denied(
                            layers,
                            "company",
                            feature,
                            "ai_not_allowed_for_company",
                            "Company is not on the AI allowlist",
                        );
                    }
                    if !policy.ai_enabled {
                        return gate_denied(
                            layers,
                            "company",
                            feature,
                            "ai_disabled_for_company",
                            "AI is switched off for the company",
                        );
                    }
                    layers.push(gate_layer_outcome(
                        "company",
                        true,
                        "Company allowlist and gate allow AI",
                    ));
                    Some(policy)
                }
                None => {
                    return gate_denied(
                        layers,
                        "company",
                        feature,
                        "ai_not_allowed_for_company",
                        "Company is not on the AI allowlist",
                    );
                }
            },
            None => {
                return gate_denied(
                    layers,
                    "company",
                    feature,
                    "ai_company_unknown",
                    "Caller has no resolvable company",
                );
            }
        }
    } else {
        layers.push(gate_layer_outcome(
            "company",
            true,
            "Open mode is on: no company policy switches AI off",
        ));
        None
    };
    let user_policy: Option<AiUserPolicy> = if enforcement_on {
        match user_id {
            Some(user_id) => match state.user_policies.get(&user_id) {
                Some(entry) => {
                    let policy = entry.value().clone();
                    if !policy.enabled {
                        return gate_denied(
                            layers,
                            "user",
                            feature,
                            "ai_not_allowed_for_user",
                            "User is not on the AI allowlist",
                        );
                    }
                    if !policy.ai_enabled {
                        return gate_denied(
                            layers,
                            "user",
                            feature,
                            "ai_disabled_for_user",
                            "AI is switched off for the user",
                        );
                    }
                    layers.push(gate_layer_outcome(
                        "user",
                        true,
                        "User allowlist and gate allow AI",
                    ));
                    Some(policy)
                }
                None => {
                    return gate_denied(
                        layers,
                        "user",
                        feature,
                        "ai_not_allowed_for_user",
                        "User is not on the AI allowlist",
                    );
                }
            },
            None => {
                return gate_denied(
                    layers,
                    "user",
                    feature,
                    "ai_user_unknown",
                    "Caller has no user id",
                );
            }
        }
    } else {
        layers.push(gate_layer_outcome(
            "user",
            true,
            "Open mode is on: no user policy switches AI off",
        ));
        None
    };

    // 6. Feature toggles: allowlist lists first, then the explicit
    // per-layer off switches. A switched-off feature denies only
    // that feature.
    if let Some(policy) = &company_policy {
        if let Some(features) = &policy.allowed_features {
            if !features.iter().any(|name| name == feature) {
                return gate_denied(
                    layers,
                    "feature",
                    feature,
                    "ai_feature_not_allowed",
                    "Feature is not on the company allowlist",
                );
            }
        }
        if let Some(disabled) = &policy.disabled_features {
            if disabled.iter().any(|name| name == feature) {
                return gate_denied(
                    layers,
                    "company",
                    feature,
                    "ai_feature_disabled_for_company",
                    "Feature is switched off for the company",
                );
            }
        }
    }
    if let Some(policy) = &user_policy {
        if let Some(features) = &policy.allowed_features {
            if !features.iter().any(|name| name == feature) {
                return gate_denied(
                    layers,
                    "feature",
                    feature,
                    "ai_feature_not_allowed",
                    "Feature is not on the user allowlist",
                );
            }
        }
        if let Some(disabled) = &policy.disabled_features {
            if disabled.iter().any(|name| name == feature) {
                return gate_denied(
                    layers,
                    "user",
                    feature,
                    "ai_feature_disabled_for_user",
                    "Feature is switched off for the user",
                );
            }
        }
    }
    layers.push(gate_layer_outcome(
        "feature",
        true,
        "Feature is allowed by every layer",
    ));
    AiGateDecision {
        allowed: true,
        denied_layer: None,
        denied_feature: None,
        reason_code: None,
        layers,
    }
}

/// Audit one gate-layer change: the event the audit module records
/// (`ai.gating.changed`), carrying the layer, the target, the
/// field, and the old -> new values. Never carries prompt content.
pub(crate) fn emit_gating_change(
    ctx: &AppContext,
    actor: &str,
    layer: &str,
    target_id: &str,
    field: &str,
    old_value: Value,
    new_value: Value,
) {
    emit_audit(
        ctx,
        EVENT_AI_GATING_CHANGED,
        actor,
        &format!("ai-gating:{layer}:{target_id}"),
        json!({
            "layer": layer,
            "target_id": target_id,
            "field": field,
            "old_value": old_value,
            "new_value": new_value,
        }),
    );
}

// ---------------------------------------------------------------------------
// Layered-gate admin endpoints (all admin-only:
// `ai:manage_providers`, same capability as the policy endpoints).
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct EffectiveGateQuery {
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub feature: Option<String>,
    #[serde(default)]
    pub product_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCompanyGateInput {
    #[serde(default)]
    pub ai_enabled: Option<bool>,
    #[serde(default)]
    pub disabled_features: Option<Option<Vec<String>>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateUserGateInput {
    #[serde(default)]
    pub ai_enabled: Option<bool>,
    #[serde(default)]
    pub disabled_features: Option<Option<Vec<String>>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateGlobalSettingsInput {
    #[serde(default)]
    pub global_enabled: Option<bool>,
    #[serde(default)]
    pub disabled_features: Option<Option<Vec<String>>>,
    /// Explicit opt-in to legacy open mode (default-deny when
    /// absent / false). See `AiGlobalSettings::open_mode`.
    #[serde(default)]
    pub open_mode: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::AiCallRequest;
    use super::authorize_ai_call;
    use super::is_forbidden_mcp_ip;
    use super::release_cap_reservation;
    use unified_core::config::AppConfig;
    use unified_core::context::AppContext;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;
    use crate::modules::ai::AiCompanyPolicy;
    use crate::modules::ai::AiUserPolicy;
    use crate::modules::ai::helpers::Caller;
    use crate::modules::ai::state::AiState;
    use chrono::Utc;
    use std::net::IpAddr;
    use std::sync::Arc;
    use uuid::Uuid;

    fn ip(text: &str) -> IpAddr {
        text.parse().expect("test IP must parse")
    }

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    fn policy_state(
        company_id: Uuid,
        user_id: Uuid,
        token_cap: Option<u64>,
        cost_cap: Option<f64>,
    ) -> AiState {
        let state = AiState::default();
        let now = Utc::now();
        state.company_policies.insert(
            company_id,
            AiCompanyPolicy {
                id: Uuid::now_v7(),
                company_id,
                allowed_models: None,
                allowed_features: None,
                monthly_token_cap: token_cap,
                monthly_cost_cap_usd: cost_cap,
                enabled: true,
                ai_enabled: true,
                disabled_features: None,
                created_at: now,
                updated_at: now,
            },
        );
        state.user_policies.insert(
            user_id,
            AiUserPolicy {
                id: Uuid::now_v7(),
                user_id,
                company_id,
                allowed_models: None,
                allowed_features: None,
                monthly_token_cap: token_cap,
                monthly_cost_cap_usd: cost_cap,
                enabled: true,
                ai_enabled: true,
                disabled_features: None,
                created_at: now,
                updated_at: now,
            },
        );
        state
    }

    fn call_request<'a>(
        model: &'a str,
        estimated_tokens: u32,
        estimated_completion_tokens: u32,
    ) -> AiCallRequest<'a> {
        AiCallRequest {
            use_case: "complete",
            model,
            provider_id: Uuid::now_v7(),
            provider_kind: "openai",
            estimated_tokens,
            estimated_completion_tokens,
            use_personal_key: false,
            product_id: None,
        }
    }

    #[test]
    fn loopback_and_private_are_forbidden() {
        assert!(is_forbidden_mcp_ip(ip("127.0.0.1")));
        assert!(is_forbidden_mcp_ip(ip("10.0.0.1")));
        assert!(is_forbidden_mcp_ip(ip("192.168.1.1")));
        assert!(is_forbidden_mcp_ip(ip("::1")));
    }

    #[test]
    fn public_addresses_are_allowed() {
        assert!(!is_forbidden_mcp_ip(ip("8.8.8.8")));
        assert!(!is_forbidden_mcp_ip(ip("1.1.1.1")));
        assert!(!is_forbidden_mcp_ip(ip("2001:4860:4860::8888")));
    }

    #[test]
    fn no_policies_and_no_opt_in_denies_by_default() {
        let ctx = test_context();
        let state = AiState::default();
        let user_id = Uuid::now_v7();
        let company_id = Uuid::now_v7();
        let caller = Caller {
            actor: user_id.to_string(),
            role: "member".to_string(),
        };
        let error = authorize_ai_call(
            &ctx,
            &state,
            &caller,
            Some(company_id),
            &call_request("gpt-4o-mini", 100, 50),
        )
        .expect_err("fresh install with no policies must deny");
        assert_eq!(error.code, 403);
    }

    #[test]
    fn explicit_open_mode_opt_in_allows_without_policies() {
        let ctx = test_context();
        let state = AiState::default();
        state
            .global_settings
            .write()
            .expect("ai global settings lock poisoned")
            .open_mode = true;
        let user_id = Uuid::now_v7();
        let company_id = Uuid::now_v7();
        let caller = Caller {
            actor: user_id.to_string(),
            role: "member".to_string(),
        };
        let auth = authorize_ai_call(
            &ctx,
            &state,
            &caller,
            Some(company_id),
            &call_request("gpt-4o-mini", 100, 50),
        )
        .expect("open mode opt-in must allow");
        assert!(auth.reservation_id.is_none());
    }

    #[test]
    fn completion_cost_counts_against_the_cost_cap() {
        let ctx = test_context();
        let user_id = Uuid::now_v7();
        let company_id = Uuid::now_v7();
        // gpt-4o-mini: prompt 0.15, completion 0.60 per million.
        // 1000 prompt + 1000 completion = 0.00075 USD. A cap of
        // 0.0005 USD only denies when the completion side is
        // counted: prompt-only pricing would be 0.0003 USD.
        let state = policy_state(company_id, user_id, None, Some(0.0005));
        let caller = Caller {
            actor: user_id.to_string(),
            role: "member".to_string(),
        };
        let error = authorize_ai_call(
            &ctx,
            &state,
            &caller,
            Some(company_id),
            &call_request("gpt-4o-mini", 2000, 1000),
        )
        .expect_err("completion cost must count against the cap");
        assert_eq!(error.code, 403);
        assert_eq!(
            error.metadata.get("reason").and_then(|v| v.as_str()),
            Some("ai_company_cost_cap_exceeded")
        );
    }

    #[test]
    fn reservation_blocks_a_second_call_until_released() {
        let ctx = test_context();
        let user_id = Uuid::now_v7();
        let company_id = Uuid::now_v7();
        let state = policy_state(company_id, user_id, Some(1500), None);
        let caller = Caller {
            actor: user_id.to_string(),
            role: "member".to_string(),
        };
        let first = authorize_ai_call(
            &ctx,
            &state,
            &caller,
            Some(company_id),
            &call_request("gpt-4o-mini", 1000, 500),
        )
        .expect("first call must pass");
        assert_eq!(state.cap_reservations.len(), 1);
        // 1000 reserved + 1000 requested exceeds the 1500 cap,
        // even though no usage has been recorded yet.
        let error = authorize_ai_call(
            &ctx,
            &state,
            &caller,
            Some(company_id),
            &call_request("gpt-4o-mini", 1000, 500),
        )
        .expect_err("reserved estimate must count against the cap");
        assert_eq!(error.code, 403);
        release_cap_reservation(&state, first.reservation_id);
        assert_eq!(state.cap_reservations.len(), 0);
        authorize_ai_call(
            &ctx,
            &state,
            &caller,
            Some(company_id),
            &call_request("gpt-4o-mini", 1000, 500),
        )
        .expect("call must pass again after the reservation is released");
    }
}
