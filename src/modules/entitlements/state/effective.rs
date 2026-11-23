//! Entitlements state (split from state.rs; moved verbatim).

use super::*;

impl EntitlementsState {

    /// Grace policy — the single source of truth for what grace
    /// means, applied identically by the gate, `effective()`,
    /// `enable_module`, and provisioning:
    ///
    /// * Grace is READ-ONLY. It never entitles (`entitled ==
    ///   false`), never allows writes, never allows (re-)enabling,
    ///   and never passes the licensing gate: the shared
    ///   `entitled_via_live_sources` decision only counts
    ///   `active` rows, so a grace-state module reads as
    ///   `grace_read_only` in `effective()`, is refused by
    ///   `enable_module`, and is 403'd by the gate/provisioning.
    ///   A grace module keeps serving reads until
    ///   `grace_ends_at`; only a live source re-entitles it.
    /// * Whether a lapsed row enters grace at all is this
    ///   predicate: the read-only-grace switch on AND a positive
    ///   grace period. `lapse_row`, `lapsed_in_grace_window`,
    ///   and the sweep all ask here instead of re-reading the
    ///   settings, so the three can never disagree.
    pub(crate) fn grace_applies(&self) -> bool {
        let settings = self.settings();
        settings.read_only_grace_enabled && settings.grace_period_days > 0
    }


    pub fn settings(&self) -> EntitlementSettings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
    }


    pub(crate) fn activation_key(company_id: &str, module_id: &str) -> String {
        format!("{company_id}|{module_id}")
    }


    pub(crate) fn is_enabled(&self, company_id: &str, module_id: &str) -> bool {
        self.activations
            .get(&Self::activation_key(company_id, module_id))
            .map(|entry| entry.enabled)
            .unwrap_or(false)
    }


    pub(crate) fn record_history(
        &self,
        tenant_id: &str,
        entitlement_id: Option<Uuid>,
        company_id: &str,
        module_id: &str,
        action: &str,
        actor_id: Option<&str>,
        detail: Value,
    ) {
        self.history.insert(
            Uuid::now_v7(),
            EntitlementHistoryEntry {
                id: Uuid::now_v7(),
                tenant_id: tenant_id.to_string(),
                entitlement_id,
                company_id: company_id.to_string(),
                module_id: module_id.to_string(),
                action: action.to_string(),
                actor_id: actor_id.map(str::to_string),
                detail,
                created_at: Utc::now(),
            },
        );
    }


    pub(crate) fn record_denied(
        &self,
        tenant_id: &str,
        company_id: &str,
        module_id: &str,
        actor_id: Option<&str>,
        reason: &str,
        error_code: &str,
    ) -> DeniedAttempt {
        let attempt = DeniedAttempt {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.to_string(),
            company_id: company_id.to_string(),
            module_id: module_id.to_string(),
            actor_id: actor_id.map(str::to_string),
            reason: reason.to_string(),
            error_code: error_code.to_string(),
            created_at: Utc::now(),
        };
        self.denied_attempts
            .insert(attempt.id, attempt.clone());
        attempt
    }


    /// Active rows for one tenant and module: status `active` and
    /// not past `expires_at`. Expired rows are not returned here;
    /// the grace computation in `effective` handles them.
    pub(crate) fn active_rows(&self, company_id: &str, module_id: &str) -> Vec<Entitlement> {
        let now = Utc::now();
        let mut rows: Vec<Entitlement> = self
            .entitlements
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                row.company_id == company_id
                    && row.module_id == module_id
                    && row.status == STATUS_ACTIVE
                    && row.expires_at.is_none_or(|expires| expires > now)
            })
            .collect();
        rows.sort_by(|a, b| a.granted_at.cmp(&b.granted_at));
        rows
    }


    /// Grace rows for one tenant and module: status `grace` with a
    /// grace window that has not ended yet.
    pub(crate) fn grace_rows(&self, company_id: &str, module_id: &str) -> Vec<Entitlement> {
        let now = Utc::now();
        let mut rows: Vec<Entitlement> = self
            .entitlements
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                row.company_id == company_id
                    && row.module_id == module_id
                    && row.status == STATUS_GRACE
                    && row.grace_ends_at.is_none_or(|ends| ends > now)
            })
            .collect();
        rows.sort_by(|a, b| a.granted_at.cmp(&b.granted_at));
        rows
    }


    /// Rows whose expiry has passed but whose grace window (expiry
    /// + configured grace period) has not: these read as grace even
    /// before the sweep materializes the transition.
    pub(crate) fn lapsed_in_grace_window(
        &self,
        company_id: &str,
        module_id: &str,
    ) -> Vec<Entitlement> {
        if !self.grace_applies() {
            return Vec::new();
        }
        let settings = self.settings();
        let now = Utc::now();
        let mut rows: Vec<Entitlement> = self
            .entitlements
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                row.company_id == company_id
                    && row.module_id == module_id
                    && row.status == STATUS_ACTIVE
                    && match row.expires_at {
                        Some(expires) => {
                            expires <= now
                                && expires + Duration::days(settings.grace_period_days) > now
                        }
                        None => false,
                    }
            })
            .collect();
        rows.sort_by(|a, b| a.granted_at.cmp(&b.granted_at));
        rows
    }


    /// The effective entitlement for one module and tenant: the
    /// union of active stored sources plus the licensing check.
    /// `ctx` is `None` inside hook handlers that run before `init`
    /// stashed a context, in which case only stored rows are
    /// consulted — the licensing leg is skipped, never assumed.
    /// Fail closed throughout: no active source means locked.
    /// Hook handlers should call `effective_for_hook` instead,
    /// which passes the stashed context when one exists so the
    /// hook path and the HTTP path agree.
    pub fn effective(
        &self,
        ctx: Option<&AppContext>,
        company_id: &str,
        module_id: &str,
    ) -> EffectiveEntitlement {
        let settings = self.settings();
        // One shared decision with the licensing gate: license,
        // subscription, package, manual grant, and the stored-row
        // leg are all consulted here, in gate order, so the two
        // can never disagree about whether a module is entitled.
        let verdict = entitled_via_live_sources(Some(self), ctx, company_id, module_id);
        if verdict.entitled {
            return EffectiveEntitlement {
                module_id: module_id.to_string(),
                company_id: company_id.to_string(),
                state: ACCESS_ENTITLED.to_string(),
                entitled: true,
                read_only: false,
                write_allowed: true,
                sources: verdict.sources(),
                expires_at: verdict.expires_at,
                grace_ends_at: None,
                enabled: self.is_enabled(company_id, module_id),
            };
        }
        // No live source: grace if a grace row is live, or if an
        // expired row is still inside its grace window. Grace is
        // read-only by the single policy (see `grace_applies`):
        // it never entitles, never allows writes, and never
        // enables — exactly as the gate refuses it.
        let mut grace_ends_at: Option<DateTime<Utc>> = None;
        let mut grace_sources: Vec<EntitlementSourceView> = Vec::new();
        for row in self.grace_rows(company_id, module_id) {
            grace_ends_at = match (grace_ends_at, row.grace_ends_at) {
                (Some(current), Some(ends)) if ends > current => Some(ends),
                (None, ends) => ends,
                (current, _) => current,
            };
            grace_sources.push(EntitlementSourceView {
                source_kind: row.source_kind.clone(),
                source_id: Some(row.source_id.clone()),
                entitlement_id: Some(row.id),
                status: STATUS_GRACE.to_string(),
                expires_at: row.expires_at,
            });
        }
        for row in self.lapsed_in_grace_window(company_id, module_id) {
            let ends = row
                .expires_at
                .map(|expires| expires + Duration::days(settings.grace_period_days));
            grace_ends_at = match (grace_ends_at, ends) {
                (Some(current), Some(ends)) if ends > current => Some(ends),
                (None, ends) => ends,
                (current, _) => current,
            };
            grace_sources.push(EntitlementSourceView {
                source_kind: row.source_kind.clone(),
                source_id: Some(row.source_id.clone()),
                entitlement_id: Some(row.id),
                status: STATUS_GRACE.to_string(),
                expires_at: row.expires_at,
            });
        }
        if !grace_sources.is_empty() {
            return EffectiveEntitlement {
                module_id: module_id.to_string(),
                company_id: company_id.to_string(),
                state: ACCESS_GRACE_READ_ONLY.to_string(),
                entitled: false,
                read_only: true,
                write_allowed: false,
                sources: grace_sources,
                expires_at: verdict.expires_at,
                grace_ends_at,
                enabled: self.is_enabled(company_id, module_id),
            };
        }
        EffectiveEntitlement {
            module_id: module_id.to_string(),
            company_id: company_id.to_string(),
            state: ACCESS_LOCKED.to_string(),
            entitled: false,
            read_only: false,
            write_allowed: false,
            sources: Vec::new(),
            expires_at: None,
            grace_ends_at: None,
            enabled: self.is_enabled(company_id, module_id),
        }
    }


    /// Effective entitlement for the hook path: the same
    /// computation as `effective`, but with the `AppContext`
    /// stashed at `init`, so the licensing leg is evaluated
    /// exactly as the HTTP handlers evaluate it. When no context
    /// was stashed yet (module not initialized), this falls back
    /// to stored-rows-only — the same fail-closed behaviour the
    /// hook path had before, never an assumed entitlement.
    pub(crate) fn effective_for_hook(
        &self,
        company_id: &str,
        module_id: &str,
    ) -> EffectiveEntitlement {
        match hook_ctx() {
            Some(ctx) => self.effective(Some(&ctx), company_id, module_id),
            None => self.effective(None, company_id, module_id),
        }
    }


    /// Modules the tenant is effectively entitled to right now
    /// (stored active rows plus the licensing leg, when a context
    /// is available), as a sorted, de-duplicated list. The
    /// licensing leg is the shared `entitled_via_live_sources`
    /// decision (same as the gate and `effective()`): sellable
    /// core modules are covered by the implicit owner
    /// entitlement and sellable non-core modules by an active
    /// license, an active subscription, or a stored row. Modules
    /// that are not sellable need no entitlement and are left out
    /// here; callers keep them unconditionally (see the
    /// package-change handler).
    pub(crate) fn entitled_module_ids(
        &self,
        ctx: Option<&AppContext>,
        company_id: &str,
    ) -> Vec<String> {
        let now = Utc::now();
        let mut ids: HashSet<String> = HashSet::new();
        for entry in self.entitlements.iter() {
            let row = entry.value();
            if row.company_id == company_id
                && row.status == STATUS_ACTIVE
                && row.expires_at.is_none_or(|expires| expires > now)
            {
                ids.insert(row.module_id.clone());
            }
        }
        if let Some(ctx) = ctx {
            for module in ctx.modules.list() {
                let module_id = module.id();
                if !module.available_as_product() {
                    continue;
                }
                // One shared decision with the gate and
                // `effective()`. The short-circuit order inside
                // it matters: the subscription check emits a hook
                // per module, so it only runs for sellable
                // non-core modules with no license.
                if entitled_via_live_sources(Some(self), Some(ctx), company_id, module_id).entitled
                {
                    ids.insert(module_id.to_string());
                }
            }
        }
        let mut sorted: Vec<String> = ids.into_iter().collect();
        sorted.sort();
        sorted
    }
}
