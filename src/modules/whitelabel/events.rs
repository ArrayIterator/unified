// Whitelabel module - hook bus event handlers.
//
// Split from `src/modules/whitelabel.rs`; the module root re-exports the public API of this module.

use unified_core::hooks::HookContext;
use chrono::Utc;
use serde_json::json;

use super::constants::{DOMAIN_REJECTED, DOMAIN_REQUESTED, DOMAIN_VERIFIED, EVENT_DOMAIN_REJECTED, EVENT_DOMAIN_VERIFIED};
use super::state::{WhitelabelState};

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

/// Answer `whitelabel.branding.resolve` with the server-resolved
/// branding for `{"company_id", "tenant_id"?}`. This is the hook
/// theme, portals, mail, and login rendering must call instead of
/// reading branding themselves or trusting client input.
pub(crate) fn on_branding_resolve(state: &WhitelabelState, ctx: &mut HookContext) {
    let company_id = ctx
        .get("company_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if company_id.trim().is_empty() {
        ctx.add_result(json!({
            "error": "whitelabel.branding.resolve requires 'company_id'",
        }));
        return;
    }
    let tenant_id = ctx
        .get("tenant_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let resolved = state.resolve_for(&company_id, &tenant_id);
    match serde_json::to_value(&resolved) {
        Ok(value) => ctx.add_result(value),
        Err(error) => ctx.add_result(json!({ "error": error.to_string() })),
    }
}

/// Answer `whitelabel.allowance.check` with
/// `{"company_id", "allowed"}` for `{"company_id"}`.
pub(crate) fn on_allowance_check(state: &WhitelabelState, ctx: &mut HookContext) {
    let company_id = ctx
        .get("company_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if company_id.trim().is_empty() {
        ctx.add_result(json!({
            "error": "whitelabel.allowance.check requires 'company_id'",
        }));
        return;
    }
    ctx.add_result(json!({
        "company_id": company_id,
        "allowed": state.effective_allowance(&company_id),
    }));
}

/// Apply a `domain.verify` outcome (emitted by the domains module)
/// to the matching branding profile: `verified` when `dns_ok` is
/// true, `rejected` when it is false. Only a verified domain is
/// ever reported as attached by the resolver. The domains module
/// owns verification; this module only records the outcome.
pub(crate) fn on_domain_verify(state: &WhitelabelState, ctx: &mut HookContext) {
    let domain = ctx
        .get("domain")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_lowercase();
    if domain.is_empty() {
        return;
    }
    let dns_ok = ctx
        .get("dns_ok")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let new_status = if dns_ok { DOMAIN_VERIFIED } else { DOMAIN_REJECTED };
    let mut matched: Vec<(String, Option<String>)> = Vec::new();
    for mut entry in state.profiles.iter_mut() {
        let profile = entry.value_mut();
        if profile
            .custom_domain
            .as_deref()
            .map(|custom| custom.to_lowercase() == domain)
            .unwrap_or(false)
            && profile.domain_status != new_status
        {
            profile.domain_status = new_status.to_string();
            profile.updated_at = Utc::now();
            matched.push((profile.company_id.clone(), profile.tenant_id.clone()));
        }
    }
    for (company_id, tenant_id) in matched {
        let event = if dns_ok {
            EVENT_DOMAIN_VERIFIED
        } else {
            EVENT_DOMAIN_REJECTED
        };
        let action = if dns_ok {
            "domain.verified"
        } else {
            "domain.rejected"
        };
        state.record_history(
            &company_id,
            &tenant_id,
            "system:domains",
            action,
            "domain_status",
            Some(DOMAIN_REQUESTED.to_string()),
            Some(new_status.to_string()),
            true,
        );
        state.emit(
            event,
            json!({
                "company_id": company_id,
                "tenant_id": tenant_id,
                "domain": domain,
                "domain_status": new_status,
            }),
        );
    }
}
