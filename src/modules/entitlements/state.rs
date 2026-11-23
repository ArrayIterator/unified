// Entitlement state and store logic.

use unified_core::context::AppContext;
use unified_core::hooks::{events, HookBus, HookContext};
use crate::modules::licensing::{has_module_entitlement, has_subscription_entitlement};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock, RwLock};
use uuid::Uuid;

use super::*;

// Split from the parent file: the implementation now lives
// in the submodules below; the public paths are unchanged
// (this file re-exports everything that used to live here).
pub mod effective;
pub mod lifecycle;

// ---------------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------------

pub struct EntitlementsState {
    pub entitlements: DashMap<Uuid, Entitlement>,
    pub history: DashMap<Uuid, EntitlementHistoryEntry>,
    pub denied_attempts: DashMap<Uuid, DeniedAttempt>,
    pub package_changes: DashMap<Uuid, PackageChange>,
    pub activations: DashMap<String, ModuleActivation>,
    pub settings: RwLock<EntitlementSettings>,
    pub hooks: RwLock<Option<Arc<HookBus>>>,
    /// Serializes package-change confirms: the load-check-insert
    /// in `confirm_package_change` is synchronous, but two
    /// confirms on different threads could still interleave
    /// between the load and the insert and both disable the same
    /// modules. Holding this lock across the whole confirm makes
    /// the second confirm see the settled status and get a 409.
    /// Confirms are rare admin operations, so one coarse lock for
    /// all changes is the right trade, not per-change locks.
    pub(crate) confirm_lock: std::sync::Mutex<()>,
}

/// Application context stashed at `init` so the synchronous hook
/// handlers can run the licensing leg of the effective check: hook
/// handlers receive no `AppContext`, and the `HookHandler` type
/// (`Fn(&mut HookContext)`) cannot carry one. This is the same
/// pattern the licensing, geo, users, network-tools, and company
/// modules use (`LICENSING_CTX`, `GEO_CTX`, `USERS_CTX`, ...). The
/// stashed context shares the live config/hook-bus/registry/cache/
/// pool locks with the real context; only uptime and the pause
/// flags are snapshots. Registration order does not matter: hook
/// handlers read it lazily, and when `init` has not run yet they
/// fall back to stored-rows-only (fail closed, as before).
pub(crate) static ENTITLEMENTS_CTX: OnceLock<Arc<AppContext>> = OnceLock::new();

/// The entitlements state stashed at module `init`, so the
/// licensing gate (`licensing::check_module_entitlement`, which
/// only ever sees an `AppContext`) can consult the same stored
/// rows `effective()` reads through the one shared
/// `entitled_via_live_sources` decision. `None` until the
/// entitlements module initializes; the gate treats that as "no
/// stored rows" (fail closed — the pre-fix behaviour), never as
/// an entitlement.
pub(crate) static ENTITLEMENTS_STATE: OnceLock<Arc<EntitlementsState>> = OnceLock::new();

pub(crate) fn stash_entitlements_state(state: Arc<EntitlementsState>) {
    let _ = ENTITLEMENTS_STATE.set(state);
}

pub(crate) fn entitlements_state() -> Option<Arc<EntitlementsState>> {
    ENTITLEMENTS_STATE.get().cloned()
}

pub(crate) fn stash_ctx(ctx: &AppContext) -> Arc<AppContext> {
    Arc::new(AppContext {
        config: ctx.config.clone(),
        hooks: ctx.hooks.clone(),
        modules: ctx.modules.clone(),
        cache: ctx.cache.clone(),
        geo: ctx.geo.clone(),
        db: ctx.db.clone(),
        storage_registry: ctx.storage_registry.clone(),
        routes: ctx.routes.clone(),
        tls: ctx.tls.clone(),
        start_time: std::time::Instant::now(),
        paused: AtomicBool::new(ctx.is_paused()),
        maintenance: AtomicBool::new(ctx.is_maintenance()),
    })
}

pub(crate) fn hook_ctx() -> Option<Arc<AppContext>> {
    ENTITLEMENTS_CTX.get().cloned()
}

/// The outcome of the one shared live-source entitlement
/// decision (`entitled_via_live_sources`), used by BOTH the
/// licensing gate and `effective()` so the two can never disagree
/// about whether a module is entitled.
#[derive(Debug, Clone)]
pub(crate) struct LiveEntitlement {
    /// True when any live source covers the module right now.
    pub entitled: bool,
    /// The covering source, in gate order: `"builtin"` (module
    /// is not sellable), `"owner_entitlement"` (sellable core),
    /// `"license"`, `"subscription"`, `"stored_row"` (an active
    /// stored row: SaaS package, manual grant, or a mirrored
    /// license/subscription row), or `"none"`.
    pub reason: &'static str,
    /// Active stored rows covering the module, oldest grant
    /// first, as source views (they carry the entitlement id and
    /// expiry the derived leg has no id for).
    pub stored: Vec<EntitlementSourceView>,
    /// The derived (non-stored) licensing source, when the
    /// licensing leg covers the module.
    pub derived: Option<EntitlementSourceView>,
    /// Earliest expiry among the stored rows, if any.
    pub expires_at: Option<DateTime<Utc>>,
}

impl LiveEntitlement {
    /// Every covering source, stored rows first, then the derived
    /// licensing source — the same list `effective()` reported
    /// before this decision was shared.
    pub(crate) fn sources(&self) -> Vec<EntitlementSourceView> {
        let mut sources = self.stored.clone();
        if let Some(derived) = &self.derived {
            sources.push(derived.clone());
        }
        sources
    }
}

/// The ONE shared live-source entitlement decision, used by BOTH
/// the licensing gate (`licensing::check_module_entitlement`)
/// and `effective()` (and `entitled_module_ids`). Both consult
/// the same live sources, in gate order:
///
/// 1. Not sellable: no entitlement needed (`builtin`).
/// 2. Sellable core: the implicit owner entitlement.
/// 3. Sellable non-core: an active license naming the module.
/// 4. Sellable non-core without a license: an active
///    subscription covering the module (asked through the hook
///    bus, same as the gate always did).
/// 5. An active stored row for the module — SaaS package,
///    manual grant, or a mirrored license/subscription row.
///    This is the leg the gate never consulted before: a module
///    covered only by a stored row was entitled in
///    `effective()` but 403 in the gate.
/// 6. Otherwise not entitled.
///
/// The REASON follows gate order: a live license/subscription
/// outranks a stored row, so a mirrored row never shadows the
/// live source it mirrors. Grace is deliberately not a leg:
/// grace rows are not `active` rows, so a grace-state module
/// is never entitled here (see `grace_applies`).
///
/// `state` is `None` when the caller has no entitlements state
/// (the gate before the entitlements module initializes): the
/// stored-row leg is skipped, never assumed. `ctx` is `None`
/// on the hook path before `init` stashed one: the licensing
/// leg is skipped, never assumed. Fail closed throughout: no
/// live source means not entitled.
pub(crate) fn entitled_via_live_sources(
    state: Option<&EntitlementsState>,
    ctx: Option<&AppContext>,
    company_id: &str,
    module_id: &str,
) -> LiveEntitlement {
    let mut stored: Vec<EntitlementSourceView> = Vec::new();
    let mut expires_at: Option<DateTime<Utc>> = None;
    if let Some(state) = state {
        for row in state.active_rows(company_id, module_id) {
            if let Some(expires) = row.expires_at {
                expires_at = Some(match expires_at {
                    Some(current) if current < expires => current,
                    Some(current) => current,
                    None => expires,
                });
            }
            stored.push(EntitlementSourceView {
                source_kind: row.source_kind.clone(),
                source_id: Some(row.source_id.clone()),
                entitlement_id: Some(row.id),
                status: STATUS_ACTIVE.to_string(),
                expires_at: row.expires_at,
            });
        }
    }
    let stored_entitled = !stored.is_empty();
    // Licensing leg, in gate order. The derived source is
    // identified by kind only: the licensing API exposes no
    // license id, and the store's subscription id is not needed
    // for the verdict (it is recorded on the stored row instead).
    let mut derived: Option<EntitlementSourceView> = None;
    let mut reason: &'static str = "none";
    if let Some(ctx) = ctx {
        if let Some(module) = ctx.modules.get(module_id) {
            if !module.available_as_product() {
                derived = Some(EntitlementSourceView {
                    source_kind: SOURCE_BUILTIN.to_string(),
                    source_id: None,
                    entitlement_id: None,
                    status: STATUS_ACTIVE.to_string(),
                    expires_at: None,
                });
                reason = "builtin";
            } else if module.is_core() {
                derived = Some(EntitlementSourceView {
                    source_kind: SOURCE_OWNER_ENTITLEMENT.to_string(),
                    source_id: None,
                    entitlement_id: None,
                    status: STATUS_ACTIVE.to_string(),
                    expires_at: None,
                });
                reason = "owner_entitlement";
            } else if has_module_entitlement(ctx, company_id, module_id) {
                derived = Some(EntitlementSourceView {
                    source_kind: SOURCE_LICENSE.to_string(),
                    source_id: None,
                    entitlement_id: None,
                    status: STATUS_ACTIVE.to_string(),
                    expires_at: None,
                });
                reason = "license";
            } else if has_subscription_entitlement(ctx, company_id, module_id) {
                derived = Some(EntitlementSourceView {
                    source_kind: SOURCE_SUBSCRIPTION.to_string(),
                    source_id: None,
                    entitlement_id: None,
                    status: STATUS_ACTIVE.to_string(),
                    expires_at: None,
                });
                reason = "subscription";
            }
        }
    }
    if derived.is_none() && stored_entitled {
        reason = "stored_row";
    }
    LiveEntitlement {
        entitled: derived.is_some() || stored_entitled,
        reason,
        stored,
        derived,
        expires_at,
    }
}

impl Default for EntitlementsState {
    fn default() -> Self {
        Self {
            entitlements: DashMap::new(),
            history: DashMap::new(),
            denied_attempts: DashMap::new(),
            package_changes: DashMap::new(),
            activations: DashMap::new(),
            settings: RwLock::new(EntitlementSettings::default()),
            hooks: RwLock::new(None),
            confirm_lock: std::sync::Mutex::new(()),
        }
    }
}

impl EntitlementsState {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    pub(crate) fn emit_with_results(&self, event: &str, data: Value) -> Option<HookContext> {
        self.bus().map(|bus| bus.emit(event, data))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn company_id() -> String {
        Uuid::now_v7().to_string()
    }

    fn disable_grace(state: &EntitlementsState) {
        *state.settings.write().expect("settings lock poisoned") = EntitlementSettings {
            grace_period_days: 0,
            read_only_grace_enabled: false,
            default_deny: true,
        };
    }

    fn grant_package_row(
        state: &EntitlementsState,
        company_id: &str,
        module_id: &str,
        source_id: &str,
    ) -> Entitlement {
        state.create_entitlement(
            company_id,
            company_id,
            SOURCE_SAAS_PACKAGE,
            source_id,
            None,
            module_id,
            None,
            Some("admin"),
            None,
        )
    }

    #[test]
    fn downgrade_lapse_locks_module_and_removes_active_rows() {
        let state = EntitlementsState::new();
        disable_grace(&state);
        let company_id = company_id();
        grant_package_row(&state, &company_id, "billing", "pkg-old");
        assert_eq!(
            state.effective(None, &company_id, "billing").state,
            ACCESS_ENTITLED
        );
        // The exact call the package-change downgrade path now
        // makes for a lost module with no other source.
        state.apply_source_lapse(
            SOURCE_SAAS_PACKAGE,
            "pkg-old",
            Some(&company_id),
            "package_change",
        );
        assert!(state.active_rows(&company_id, "billing").is_empty());
        let effective = state.effective(None, &company_id, "billing");
        assert_eq!(effective.state, ACCESS_LOCKED);
        assert!(!effective.entitled);
        assert!(!effective.write_allowed);
    }

    #[test]
    fn downgrade_lapse_enters_grace_when_grace_is_enabled() {
        let state = EntitlementsState::new();
        let company_id = company_id();
        grant_package_row(&state, &company_id, "billing", "pkg-old");
        state.apply_source_lapse(
            SOURCE_SAAS_PACKAGE,
            "pkg-old",
            Some(&company_id),
            "package_change",
        );
        // No ACTIVE row survives the downgrade even in grace: the
        // module reads as grace_read_only, never entitled.
        assert!(state.active_rows(&company_id, "billing").is_empty());
        let effective = state.effective(None, &company_id, "billing");
        assert_eq!(effective.state, ACCESS_GRACE_READ_ONLY);
        assert!(!effective.entitled);
    }

    #[test]
    fn lapse_with_other_source_revokes_only_the_lapsed_row() {
        let state = EntitlementsState::new();
        disable_grace(&state);
        let company_id = company_id();
        grant_package_row(&state, &company_id, "billing", "pkg-old");
        state.create_entitlement(
            &company_id,
            &company_id,
            SOURCE_MANUAL_GRANT,
            "manual",
            None,
            "billing",
            None,
            Some("admin"),
            None,
        );
        state.apply_source_lapse(
            SOURCE_SAAS_PACKAGE,
            "pkg-old",
            Some(&company_id),
            "package_change",
        );
        assert_eq!(state.active_rows(&company_id, "billing").len(), 1);
        assert_eq!(
            state.effective(None, &company_id, "billing").state,
            ACCESS_ENTITLED
        );
    }

    #[test]
    fn create_entitlement_supersedes_duplicate_active_triple() {
        let state = EntitlementsState::new();
        let company_id = company_id();
        let first = grant_package_row(&state, &company_id, "billing", "pkg-1");
        let second = grant_package_row(&state, &company_id, "billing", "pkg-1");
        assert_ne!(first.id, second.id);
        let active = state.active_rows(&company_id, "billing");
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, second.id);
        let old = state.entitlements.get(&first.id).expect("old row kept");
        assert_eq!(old.status, STATUS_REVOKED);
        assert_eq!(old.lapse_reason.as_deref(), Some("superseded_by_new_grant"));
        assert_eq!(
            state.effective(None, &company_id, "billing").sources.len(),
            1
        );
    }

    #[test]
    fn create_entitlement_keeps_rows_from_different_sources() {
        let state = EntitlementsState::new();
        let company_id = company_id();
        grant_package_row(&state, &company_id, "billing", "pkg-1");
        grant_package_row(&state, &company_id, "billing", "pkg-2");
        assert_eq!(state.active_rows(&company_id, "billing").len(), 2);
    }

    /// Test module with a configurable sellable/core shape, so the
    /// licensing leg of `effective()` can be exercised without a
    /// real module.
    struct LicensingLegModule {
        id: &'static str,
        sellable: bool,
        core: bool,
    }

    impl unified_core::modules::Module for LicensingLegModule {
        fn id(&self) -> &'static str {
            self.id
        }

        fn name(&self) -> &'static str {
            "Licensing leg test module"
        }

        fn description(&self) -> &'static str {
            "Test-only module for the licensing leg"
        }

        fn is_core(&self) -> bool {
            self.core
        }

        fn available_as_product(&self) -> bool {
            self.sellable
        }

        fn product_category(&self) -> Option<&'static str> {
            if self.sellable {
                Some("commerce")
            } else {
                None
            }
        }
    }

    fn licensing_leg_context(subscription_entitled: bool) -> AppContext {
        use unified_core::config::AppConfig;
        use unified_core::modules::ModuleRegistry;
        let bus = Arc::new(HookBus::new());
        bus.register(
            crate::modules::licensing::validation::SUBSCRIPTION_ENTITLEMENT_CHECK_EVENT,
            "test",
            Arc::new(move |ctx: &mut HookContext| {
                ctx.add_result(serde_json::json!({
                    "entitled": subscription_entitled,
                    "source": "subscription",
                }));
            }),
        );
        let modules = Arc::new(ModuleRegistry::new());
        modules.register(Arc::new(LicensingLegModule {
            id: "free-mod",
            sellable: false,
            core: true,
        }));
        modules.register(Arc::new(LicensingLegModule {
            id: "core-mod",
            sellable: true,
            core: true,
        }));
        modules.register(Arc::new(LicensingLegModule {
            id: "addon-mod",
            sellable: true,
            core: false,
        }));
        AppContext::new(AppConfig::default(), bus, modules)
    }

    #[test]
    fn effective_entitles_non_sellable_module_through_builtin_source() {
        let state = EntitlementsState::new();
        let company_id = company_id();
        let ctx = licensing_leg_context(false);
        let effective = state.effective(Some(&ctx), &company_id, "free-mod");
        assert!(effective.entitled);
        assert_eq!(effective.state, ACCESS_ENTITLED);
        assert_eq!(effective.sources.len(), 1);
        assert_eq!(effective.sources[0].source_kind, SOURCE_BUILTIN);
    }

    #[test]
    fn effective_without_context_stays_locked_for_non_sellable_module() {
        let state = EntitlementsState::new();
        let company_id = company_id();
        let effective = state.effective(None, &company_id, "free-mod");
        assert!(!effective.entitled);
        assert_eq!(effective.state, ACCESS_LOCKED);
    }

    #[test]
    fn effective_entitles_sellable_core_module_through_owner_entitlement() {
        let state = EntitlementsState::new();
        let company_id = company_id();
        let ctx = licensing_leg_context(false);
        let effective = state.effective(Some(&ctx), &company_id, "core-mod");
        assert!(effective.entitled);
        assert_eq!(effective.sources[0].source_kind, SOURCE_OWNER_ENTITLEMENT);
    }

    #[test]
    fn effective_entitles_sellable_addon_through_subscription() {
        let state = EntitlementsState::new();
        let company_id = company_id();
        let ctx = licensing_leg_context(true);
        let effective = state.effective(Some(&ctx), &company_id, "addon-mod");
        assert!(effective.entitled);
        assert_eq!(effective.sources[0].source_kind, SOURCE_SUBSCRIPTION);
    }

    #[test]
    fn effective_locks_sellable_addon_without_license_or_subscription() {
        let state = EntitlementsState::new();
        let company_id = company_id();
        let ctx = licensing_leg_context(false);
        let effective = state.effective(Some(&ctx), &company_id, "addon-mod");
        assert!(!effective.entitled);
        assert_eq!(effective.state, ACCESS_LOCKED);
    }

    #[test]
    fn entitled_module_ids_includes_core_but_not_free_modules() {
        let state = EntitlementsState::new();
        let company_id = company_id();
        let ctx = licensing_leg_context(false);
        let ids = state.entitled_module_ids(Some(&ctx), &company_id);
        assert!(ids.contains(&"core-mod".to_string()));
        assert!(!ids.contains(&"free-mod".to_string()));
        assert!(!ids.contains(&"addon-mod".to_string()));
    }

    #[test]
    fn entitled_module_ids_includes_subscription_addon() {
        let state = EntitlementsState::new();
        let company_id = company_id();
        let ctx = licensing_leg_context(true);
        let ids = state.entitled_module_ids(Some(&ctx), &company_id);
        assert!(ids.contains(&"addon-mod".to_string()));
    }

    #[test]
    fn live_sources_entitle_through_stored_row_alone() {
        let state = EntitlementsState::new();
        let company_id = company_id();
        let ctx = licensing_leg_context(false);
        // No license, no subscription: the stored package row is
        // the only live source — the leg the gate never consulted
        // before, so `effective()` and the gate disagreed here.
        grant_package_row(&state, &company_id, "addon-mod", "pkg-1");
        let verdict = entitled_via_live_sources(Some(&state), Some(&ctx), &company_id, "addon-mod");
        assert!(verdict.entitled);
        assert_eq!(verdict.reason, "stored_row");
        assert_eq!(verdict.sources().len(), 1);
        assert_eq!(verdict.sources()[0].source_kind, SOURCE_SAAS_PACKAGE);
        assert!(
            state
                .effective(Some(&ctx), &company_id, "addon-mod")
                .entitled
        );
    }

    #[test]
    fn live_sources_never_entitle_through_grace_row() {
        let state = EntitlementsState::new();
        let company_id = company_id();
        let ctx = licensing_leg_context(false);
        grant_package_row(&state, &company_id, "addon-mod", "pkg-1");
        state.apply_source_lapse(
            SOURCE_SAAS_PACKAGE,
            "pkg-1",
            Some(&company_id),
            "package_change",
        );
        // Grace is enabled by default, so the module reads as
        // grace_read_only — but the shared decision never
        // entitles through it, exactly as the gate refuses it.
        let effective = state.effective(Some(&ctx), &company_id, "addon-mod");
        assert_eq!(effective.state, ACCESS_GRACE_READ_ONLY);
        assert!(!effective.entitled);
        let verdict = entitled_via_live_sources(Some(&state), Some(&ctx), &company_id, "addon-mod");
        assert!(!verdict.entitled);
        assert_eq!(verdict.reason, "none");
    }

    #[test]
    fn subscription_cancel_lapses_row_by_source_ref() {
        let state = EntitlementsState::new();
        let company_id = company_id();
        // A stored subscription row from a tenant grant: the
        // row's `source_id` is the tenant id, the subscription id
        // rides `source_ref` — the only link a cancel can match.
        state.create_entitlement(
            &company_id,
            &company_id,
            SOURCE_SUBSCRIPTION,
            &company_id,
            Some("sub-1".to_string()),
            "billing",
            None,
            Some("admin"),
            None,
        );
        assert_eq!(
            state.effective(None, &company_id, "billing").state,
            ACCESS_ENTITLED
        );
        // The exact call `on_subscription_cancelled` makes.
        state.apply_source_lapse(
            SOURCE_SUBSCRIPTION,
            "sub-1",
            Some(&company_id),
            "subscription_cancelled",
        );
        assert!(state.active_rows(&company_id, "billing").is_empty());
        assert_eq!(
            state.effective(None, &company_id, "billing").state,
            ACCESS_GRACE_READ_ONLY
        );
    }

    #[test]
    fn subscription_cancel_ignores_row_with_other_ref() {
        let state = EntitlementsState::new();
        let company_id = company_id();
        state.create_entitlement(
            &company_id,
            &company_id,
            SOURCE_SUBSCRIPTION,
            &company_id,
            Some("sub-1".to_string()),
            "billing",
            None,
            Some("admin"),
            None,
        );
        // A cancel for a different subscription must not lapse
        // this row.
        state.apply_source_lapse(
            SOURCE_SUBSCRIPTION,
            "sub-2",
            Some(&company_id),
            "subscription_cancelled",
        );
        assert_eq!(state.active_rows(&company_id, "billing").len(), 1);
    }
}
