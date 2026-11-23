// Hook handlers.

use unified_core::hooks::HookContext;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::sync::Arc;

use super::*;

// ---------------------------------------------------------------------------
// Hook handlers.
// ---------------------------------------------------------------------------

/// Read a string field from a hook payload, trying several key
/// spellings. Emitters disagree on field names, so a lapse that
/// cannot name its source is skipped instead of erroring.
pub(crate) fn hook_string(ctx: &HookContext, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = ctx.get(key).and_then(|value| value.as_str()) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

pub(crate) fn hook_string_from(ctx: &HookContext, object_key: &str, keys: &[&str]) -> Option<String> {
    ctx.get(object_key)
        .and_then(|object| {
            keys.iter().find_map(|key| {
                object
                    .get(key)
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
        })
        .filter(|value| !value.trim().is_empty())
}

/// Answer an entitlement check query. The licensing leg is
/// evaluated through the `AppContext` stashed at `init`
/// (`effective_for_hook`), so this answer agrees with the HTTP
/// path: a module entitled only via a license is allowed here
/// exactly as `GET /modules/{id}/access` reports it. Before
/// `init` stashes a context, only stored rows are consulted
/// (fail closed, never assumed).
pub(crate) fn on_check_requested(state: Arc<EntitlementsState>, ctx: &mut HookContext) {
    let company_id = hook_string(ctx, &["company_id"]);
    let module_id = hook_string(ctx, &["module_id"]);
    let Some(company_id) = company_id else { return };
    let Some(module_id) = module_id else { return };
    let effective = state.effective_for_hook(&company_id, &module_id);
    ctx.set("allowed", json!(effective.entitled));
    ctx.set("state", json!(effective.state));
    ctx.set(
        "reason",
        json!(if effective.entitled {
            "entitled"
        } else if effective.read_only {
            "grace_read_only"
        } else {
            "no_active_entitlement"
        }),
    );
    ctx.add_result(json!({
        "company_id": company_id,
        "module_id": module_id,
        "allowed": effective.entitled,
        "state": effective.state,
    }));
}

/// Answer a write check query: writes are allowed only while the
/// module is entitled — locked and read-only-grace both refuse.
/// Same licensing leg as `on_check_requested` (stashed context),
/// so the hook gate and the HTTP path agree.
pub(crate) fn on_write_check_requested(state: Arc<EntitlementsState>, ctx: &mut HookContext) {
    let company_id = hook_string(ctx, &["company_id"]);
    let module_id = hook_string(ctx, &["module_id"]);
    let Some(company_id) = company_id else { return };
    let Some(module_id) = module_id else { return };
    let effective = state.effective_for_hook(&company_id, &module_id);
    ctx.set("write_allowed", json!(effective.write_allowed));
    ctx.set("state", json!(effective.state));
    ctx.set(
        "error_code",
        json!(if effective.write_allowed {
            Value::Null
        } else if effective.read_only {
            json!(ERROR_ENTITLEMENT_GRACE_READ_ONLY)
        } else {
            json!(ERROR_ENTITLEMENT_REQUIRED)
        }),
    );
    ctx.add_result(json!({
        "company_id": company_id,
        "module_id": module_id,
        "write_allowed": effective.write_allowed,
        "state": effective.state,
    }));
}

/// Record a grant requested by another module (SaaS packages
/// provisioning, store checkout). Control-plane sources are read
/// only through this command and the owning modules' events —
/// never by querying their tables directly.
///
/// The emitter must name itself (`emitter` in the payload) and
/// be on the `GRANT_EMITTERS` allowlist (`saas_packages`,
/// `store` — the only provisioning modules documented to emit
/// this event, verified: no module in the tree emits it today).
/// Anything else — a missing emitter, or an emitter outside the
/// allowlist — is rejected with `granted: false` and mints
/// nothing. This stops any in-process emitter from minting
/// entitlements for any company/module; it is not authentication
/// against a compromised in-process emitter (hook payloads are
/// trusted input, same as the header identity model documented
/// in the module header).
pub(crate) fn on_grant_requested(state: Arc<EntitlementsState>, ctx: &mut HookContext) {
    let emitter = hook_string(ctx, &["emitter", "emitted_by", "source_module"]);
    let Some(emitter) = emitter else {
        ctx.set("granted", json!(false));
        ctx.set("reason", json!("emitter is required"));
        return;
    };
    if !GRANT_EMITTERS.contains(&emitter.as_str()) {
        ctx.set("granted", json!(false));
        ctx.set("reason", json!("emitter is not allowlisted"));
        return;
    }
    let company_id = hook_string(ctx, &["company_id"]);
    let module_id = hook_string(ctx, &["module_id"]);
    let source_kind = hook_string(ctx, &["source_kind"]).unwrap_or_else(|| SOURCE_SAAS_PACKAGE.to_string());
    let source_id = hook_string(ctx, &["source_id"]);
    let Some(company_id) = company_id else {
        ctx.set("granted", json!(false));
        ctx.set("reason", json!("company_id is required"));
        return;
    };
    let Some(module_id) = module_id else {
        ctx.set("granted", json!(false));
        ctx.set("reason", json!("module_id is required"));
        return;
    };
    let Some(source_id) = source_id else {
        ctx.set("granted", json!(false));
        ctx.set("reason", json!("source_id is required"));
        return;
    };
    if !SOURCE_KINDS.contains(&source_kind.as_str()) {
        ctx.set("granted", json!(false));
        ctx.set("reason", json!("unknown source_kind"));
        return;
    }
    if validate_module_id(&module_id).is_err() {
        ctx.set("granted", json!(false));
        ctx.set("reason", json!("invalid module_id"));
        return;
    }
    // The tenant namespace id: an explicitly named tenant id, else
    // the company id itself when it is a tenant UUID (the tenants
    // module uses the tenant id as the company id). A non-UUID
    // company with no explicit tenant fails closed — no row is
    // minted with a defaulted tenant id.
    let tenant_id =
        match resolve_grant_tenant_id(hook_string(ctx, &["tenant_id"]).as_deref(), &company_id) {
            Ok(tenant_id) => tenant_id,
            Err(_) => {
                ctx.set("granted", json!(false));
                ctx.set(
                    "reason",
                    json!("tenant_id is required: company_id is not a tenant UUID"),
                );
                return;
            }
        };
    let source_ref = hook_string(ctx, &["source_ref"]);
    let expires_at = ctx
        .get("expires_at")
        .and_then(|value| value.as_str())
        .and_then(|value| value.parse::<DateTime<Utc>>().ok());
    let actor_id = hook_string(ctx, &["actor_id"]);
    let entitlement = state.create_entitlement(
        &tenant_id,
        &company_id,
        &source_kind,
        &source_id,
        source_ref,
        &module_id,
        expires_at,
        actor_id.as_deref(),
        None,
    );
    ctx.set("granted", json!(true));
    ctx.set("entitlement_id", json!(entitlement.id));
    ctx.add_result(json!({ "granted": true, "entitlement_id": entitlement.id }));
}

/// Answer the AI suggestion query from the denied-attempt log.
/// Only counts are shared — never buyer personal data.
pub(crate) fn on_ai_suggest_requested(state: Arc<EntitlementsState>, ctx: &mut HookContext) {
    let company_id = hook_string(ctx, &["company_id"]);
    let Some(company_id) = company_id else { return };
    let denied = state.top_denied_modules(&company_id, MAX_SUGGESTIONS);
    let modules: Vec<Value> = denied
        .iter()
        .map(|(module_id, denied_attempts)| {
            json!({ "module_id": module_id, "denied_attempts": denied_attempts })
        })
        .collect();
    ctx.set("suggestion", json!({ "company_id": company_id, "denied_modules": modules }));
    ctx.add_result(json!({ "company_id": company_id, "denied_modules": modules }));
}

/// A license lapsed (`license.revoked`, or the expiry scan's
/// `license.expiring` — which only notes, never lapses). The
/// licensing payload carries `license_id` and no company id; the
/// licensing leg of the effective check already reflects the
/// revocation on the next read, and any stored license-sourced row
/// lapses here.
pub(crate) fn on_license_revoked(state: Arc<EntitlementsState>, ctx: &mut HookContext) {
    let Some(license_id) = hook_string(ctx, &["license_id", "id"]) else {
        return;
    };
    let company_id = hook_string(ctx, &["company_id"]);
    state.apply_source_lapse(
        SOURCE_LICENSE,
        &license_id,
        company_id.as_deref(),
        "license_revoked",
    );
}

pub(crate) fn on_license_expiring(_state: Arc<EntitlementsState>, _ctx: &mut HookContext) {
    // Expiring is a warning, not a lapse: it names no module and no
    // tenant, so there is nothing tenant-scoped to record here. The
    // sweep lapses the entitlement when the expiry actually passes,
    // and the licensing module owns the renewal reminder itself.
}

/// A subscription lapsed. The store payload carries
/// `subscription_id` plus a nested `subscription` object that may
/// name the company.
pub(crate) fn on_subscription_cancelled(state: Arc<EntitlementsState>, ctx: &mut HookContext) {
    let Some(subscription_id) = hook_string(ctx, &["subscription_id", "id"]) else {
        return;
    };
    let company_id = hook_string(ctx, &["company_id"])
        .or_else(|| hook_string_from(ctx, "subscription", &["company_id"]));
    state.apply_source_lapse(
        SOURCE_SUBSCRIPTION,
        &subscription_id,
        company_id.as_deref(),
        "subscription_cancelled",
    );
}

pub(crate) fn on_subscription_expired(state: Arc<EntitlementsState>, ctx: &mut HookContext) {
    let Some(subscription_id) = hook_string(ctx, &["subscription_id", "id"]) else {
        return;
    };
    let company_id = hook_string(ctx, &["company_id"])
        .or_else(|| hook_string_from(ctx, "subscription", &["company_id"]));
    state.apply_source_lapse(
        SOURCE_SUBSCRIPTION,
        &subscription_id,
        company_id.as_deref(),
        "subscription_expired",
    );
}

/// A SaaS package purchase was cancelled. The payload names both
/// the purchase and the package; entitlements may be sourced from
/// either id, so both are lapsed.
pub(crate) fn on_package_cancelled(state: Arc<EntitlementsState>, ctx: &mut HookContext) {
    let company_id = hook_string(ctx, &["company_id"]);
    if let Some(purchase_id) = hook_string(ctx, &["purchase_id"]) {
        state.apply_source_lapse(
            SOURCE_SAAS_PACKAGE,
            &purchase_id,
            company_id.as_deref(),
            "package_cancelled",
        );
    }
    if let Some(package_id) = hook_string(ctx, &["package_id"]) {
        state.apply_source_lapse(
            SOURCE_SAAS_PACKAGE,
            &package_id,
            company_id.as_deref(),
            "package_cancelled",
        );
    }
}

/// Mirror a tenants-module grant into a stored entitlement
/// row, so the runtime gate and the control-plane grant record
/// agree. `granted_via` maps onto this module's source kinds;
/// `preset` and `builtin` map onto nothing: a preset is a settings
/// bundle, not a purchase source, and a builtin (non-sellable)
/// module needs no entitlement at all — the gate already allows
/// it. The row's `source_id` is the tenant id: the grant record
/// itself lives in `tenant_module_grants`, keyed by tenant and
/// module. The purchase behind the grant rides `source_ref`:
/// the tenants module emits the subscription id (a store string,
/// not a UUID) as `source_ref`, and the package/preset id as
/// `granted_via_id`. A stored subscription row MUST carry its
/// subscription id here: `apply_source_lapse` matches a
/// subscription-cancel by `source_id` OR `source_ref`, and a row
/// whose ref is empty can never lapse on cancel.
pub(crate) fn on_tenant_module_granted(state: Arc<EntitlementsState>, ctx: &mut HookContext) {
    let company_id = hook_string(ctx, &["company_id"]);
    let module_id = hook_string(ctx, &["module_id"]);
    let granted_via = hook_string(ctx, &["granted_via"]).unwrap_or_default();
    let Some(company_id) = company_id else { return };
    let Some(module_id) = module_id else { return };
    if validate_module_id(&module_id).is_err() {
        return;
    }
    let source_kind = match granted_via.as_str() {
        "license" => SOURCE_LICENSE,
        "subscription" => SOURCE_SUBSCRIPTION,
        "package" => SOURCE_SAAS_PACKAGE,
        "manual_grant" => SOURCE_MANUAL_GRANT,
        "owner_entitlement" => SOURCE_OWNER_ENTITLEMENT,
        _ => return,
    };
    let tenant_id = hook_string(ctx, &["tenant_id"]).unwrap_or_else(|| company_id.clone());
    let actor_id = hook_string(ctx, &["actor_id"]);
    // The purchase id behind the grant, when the grant names
    // one. It is part of the row identity: two different
    // subscriptions granting the same module are two different
    // rows, and a re-grant of the same subscription is still
    // deduplicated below.
    let source_ref = hook_string(ctx, &["source_ref", "granted_via_id"]);
    let already = state.entitlements.iter().any(|entry| {
        let row = entry.value();
        row.company_id == company_id
            && row.module_id == module_id
            && row.source_kind == source_kind
            && row.source_id == tenant_id
            && row.source_ref == source_ref
            && row.status == STATUS_ACTIVE
    });
    if already {
        return;
    }
    state.create_entitlement(
        &tenant_id,
        &company_id,
        source_kind,
        &tenant_id,
        source_ref,
        &module_id,
        None,
        actor_id.as_deref(),
        Some(format!("tenant.module.granted via {granted_via}")),
    );
}

/// Mirror a tenants-module grant revocation: the module's
/// non-manual rows lapse into grace (or disable). Manual grants
/// survive; see `revoke_module_rows`.
pub(crate) fn on_tenant_module_revoked(state: Arc<EntitlementsState>, ctx: &mut HookContext) {
    let company_id = hook_string(ctx, &["company_id"]);
    let module_id = hook_string(ctx, &["module_id"]);
    let reason = hook_string(ctx, &["reason"]).unwrap_or_else(|| "tenant_module_revoked".to_string());
    let Some(company_id) = company_id else { return };
    let Some(module_id) = module_id else { return };
    state.revoke_module_rows(&company_id, &module_id, &reason);
}

#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookBus;
    use unified_core::modules::{Module, ModuleRegistry};
    use uuid::Uuid;

    struct FakeModule {
        id: &'static str,
        core: bool,
        sellable: bool,
    }

    impl Module for FakeModule {
        fn id(&self) -> &'static str {
            self.id
        }

        fn name(&self) -> &'static str {
            self.id
        }

        fn description(&self) -> &'static str {
            "test-only module"
        }

        fn requires(&self) -> &[&'static str] {
            &[]
        }

        fn available_as_product(&self) -> bool {
            self.sellable
        }

        fn is_core(&self) -> bool {
            self.core
        }
    }

    /// Stash a context whose registry holds `core_mod` (sellable
    /// core: covered by the implicit owner entitlement, the
    /// licensing leg) and `billing` (sellable non-core: needs a
    /// stored row or a license). The stash is process-global and
    /// set-once, like the licensing module's `LICENSING_CTX`.
    fn stash_test_ctx() -> Arc<AppContext> {
        let registry = Arc::new(ModuleRegistry::new());
        registry.register(Arc::new(FakeModule {
            id: "core_mod",
            core: true,
            sellable: true,
        }));
        registry.register(Arc::new(FakeModule {
            id: "billing",
            core: false,
            sellable: true,
        }));
        let ctx = Arc::new(AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            registry,
        ));
        let _ = ENTITLEMENTS_CTX.set(stash_ctx(&ctx));
        ctx
    }

    fn check_ctx(company_id: &str, module_id: &str) -> HookContext {
        HookContext::new(
            EVENT_CHECK_REQUESTED,
            json!({ "company_id": company_id, "module_id": module_id }),
        )
    }

    #[test]
    fn hook_check_agrees_with_http_path_for_license_leg() {
        let ctx = stash_test_ctx();
        let state = Arc::new(EntitlementsState::new());
        let company_id = Uuid::now_v7().to_string();
        // No stored row: the module is entitled only through the
        // licensing leg (implicit owner entitlement for a sellable
        // core module). Stored-rows-only denies it — the old hook
        // behaviour — while both the HTTP path and the hook path
        // now allow it.
        assert_eq!(
            state.effective(None, &company_id, "core_mod").state,
            ACCESS_LOCKED
        );
        assert!(
            state
                .effective(Some(&ctx), &company_id, "core_mod")
                .entitled
        );
        let mut hook = check_ctx(&company_id, "core_mod");
        on_check_requested(state.clone(), &mut hook);
        assert_eq!(
            hook.get("allowed").and_then(|value| value.as_bool()),
            Some(true)
        );
        assert_eq!(
            hook.get("state").and_then(|value| value.as_str()),
            Some(ACCESS_ENTITLED)
        );
        let mut write_hook = HookContext::new(
            EVENT_WRITE_CHECK_REQUESTED,
            json!({ "company_id": company_id, "module_id": "core_mod" }),
        );
        on_write_check_requested(state.clone(), &mut write_hook);
        assert_eq!(
            write_hook
                .get("write_allowed")
                .and_then(|value| value.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn hook_check_and_http_path_agree_when_locked() {
        let ctx = stash_test_ctx();
        let state = Arc::new(EntitlementsState::new());
        let company_id = Uuid::now_v7().to_string();
        assert!(!state.effective(Some(&ctx), &company_id, "billing").entitled);
        let mut hook = check_ctx(&company_id, "billing");
        on_check_requested(state.clone(), &mut hook);
        assert_eq!(
            hook.get("allowed").and_then(|value| value.as_bool()),
            Some(false)
        );
        assert_eq!(
            hook.get("state").and_then(|value| value.as_str()),
            Some(ACCESS_LOCKED)
        );
    }

    fn grant_ctx(payload: Value) -> HookContext {
        HookContext::new(EVENT_GRANT_REQUESTED, payload)
    }

    fn grant_payload(emitter: Option<&str>, company_id: &str) -> Value {
        let mut payload = json!({
            "company_id": company_id,
            "module_id": "billing",
            "source_kind": "saas_package",
            "source_id": "pkg-1",
        });
        if let Some(emitter) = emitter {
            payload
                .as_object_mut()
                .expect("payload is an object")
                .insert("emitter".to_string(), json!(emitter));
        }
        payload
    }

    #[test]
    fn grant_requested_rejects_missing_emitter() {
        let state = Arc::new(EntitlementsState::new());
        let company_id = Uuid::now_v7().to_string();
        let mut hook = grant_ctx(grant_payload(None, &company_id));
        on_grant_requested(state.clone(), &mut hook);
        assert_eq!(
            hook.get("granted").and_then(|value| value.as_bool()),
            Some(false)
        );
        assert!(hook
            .get("reason")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .contains("emitter is required"));
        assert!(state.entitlements.is_empty());
    }

    #[test]
    fn grant_requested_rejects_non_allowlisted_emitter() {
        let state = Arc::new(EntitlementsState::new());
        let company_id = Uuid::now_v7().to_string();
        let mut hook = grant_ctx(grant_payload(Some("billing"), &company_id));
        on_grant_requested(state.clone(), &mut hook);
        assert_eq!(
            hook.get("granted").and_then(|value| value.as_bool()),
            Some(false)
        );
        assert!(hook
            .get("reason")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .contains("not allowlisted"));
        assert!(state.entitlements.is_empty());
    }

    #[test]
    fn grant_requested_accepts_allowlisted_emitters() {
        for emitter in ["saas_packages", "store"] {
            let state = Arc::new(EntitlementsState::new());
            let company_id = Uuid::now_v7().to_string();
            let mut hook = grant_ctx(grant_payload(Some(emitter), &company_id));
            on_grant_requested(state.clone(), &mut hook);
            assert_eq!(
                hook.get("granted").and_then(|value| value.as_bool()),
                Some(true),
                "emitter {emitter} must be allowlisted"
            );
            assert_eq!(state.active_rows(&company_id, "billing").len(), 1);
        }
    }

    #[test]
    fn grant_requested_fails_closed_for_non_uuid_company() {
        let state = Arc::new(EntitlementsState::new());
        let mut hook = grant_ctx(grant_payload(Some("store"), "acme"));
        on_grant_requested(state.clone(), &mut hook);
        assert_eq!(
            hook.get("granted").and_then(|value| value.as_bool()),
            Some(false)
        );
        assert!(state.entitlements.is_empty());
    }

    #[test]
    fn tenant_grant_carries_subscription_id_as_source_ref() {
        let state = Arc::new(EntitlementsState::new());
        let company_id = Uuid::now_v7().to_string();
        let mut hook = HookContext::new(
            TENANT_MODULE_GRANTED_EVENT,
            json!({
                "tenant_id": company_id,
                "company_id": company_id,
                "module_id": "billing",
                "granted_via": "subscription",
                "source_ref": "sub-9",
                "actor_id": "admin",
            }),
        );
        on_tenant_module_granted(state.clone(), &mut hook);
        let rows = state.active_rows(&company_id, "billing");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source_kind, SOURCE_SUBSCRIPTION);
        assert_eq!(rows[0].source_ref.as_deref(), Some("sub-9"));
        // A cancel of that subscription lapses the row: before
        // the fix the row carried no ref and never lapsed.
        state.apply_source_lapse(
            SOURCE_SUBSCRIPTION,
            "sub-9",
            Some(&company_id),
            "subscription_cancelled",
        );
        assert!(state.active_rows(&company_id, "billing").is_empty());
    }

    #[test]
    fn tenant_grant_regrant_with_other_subscription_replaces_row() {
        let state = Arc::new(EntitlementsState::new());
        let company_id = Uuid::now_v7().to_string();
        let mut first = HookContext::new(
            TENANT_MODULE_GRANTED_EVENT,
            json!({
                "tenant_id": company_id,
                "company_id": company_id,
                "module_id": "billing",
                "granted_via": "subscription",
                "source_ref": "sub-1",
            }),
        );
        on_tenant_module_granted(state.clone(), &mut first);
        let mut second = HookContext::new(
            TENANT_MODULE_GRANTED_EVENT,
            json!({
                "tenant_id": company_id,
                "company_id": company_id,
                "module_id": "billing",
                "granted_via": "subscription",
                "source_ref": "sub-2",
            }),
        );
        on_tenant_module_granted(state.clone(), &mut second);
        // The second subscription's row is the live one; a
        // cancel of the first must not lapse the module.
        let rows = state.active_rows(&company_id, "billing");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source_ref.as_deref(), Some("sub-2"));
        state.apply_source_lapse(
            SOURCE_SUBSCRIPTION,
            "sub-1",
            Some(&company_id),
            "subscription_cancelled",
        );
        assert_eq!(state.active_rows(&company_id, "billing").len(), 1);
    }
}
