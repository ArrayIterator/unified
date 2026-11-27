//! Entitlements state.

use super::*;

impl EntitlementsState {

    /// Ask the hook bus which sellable products or packages unlock
    /// a module. Never fabricates: when no handler answers, the
    /// path is null with reason `no_upgrade_path_provider`; when
    /// handlers answer with an empty list, the reason is
    /// `no_product_unlocks_module`.
    pub(crate) fn upgrade_path(&self, company_id: &str, module_id: &str) -> UpgradePath {
        let outcome = self.emit_with_results(
            EVENT_UPGRADE_PATH_REQUESTED,
            json!({
                "company_id": company_id,
                "module_id": module_id,
                "upgrade_paths": [],
            }),
        );
        if let Some(outcome) = outcome {
            if let Some(paths) = outcome.get("upgrade_paths") {
                if let Some(items) = paths.as_array() {
                    let mut products: Vec<UpgradeProduct> = Vec::new();
                    for item in items {
                        let kind = item
                            .get("kind")
                            .and_then(|value| value.as_str())
                            .unwrap_or("package")
                            .to_string();
                        let id = item
                            .get("id")
                            .and_then(|value| value.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let name = item
                            .get("name")
                            .and_then(|value| value.as_str())
                            .unwrap_or_default()
                            .to_string();
                        if !id.is_empty() && !name.is_empty() {
                            products.push(UpgradeProduct { kind, id, name });
                        }
                    }
                    if !products.is_empty() {
                        return UpgradePath {
                            available: true,
                            products,
                            reason: None,
                        };
                    }
                    // A handler answered explicitly with no products.
                    return UpgradePath {
                        available: false,
                        products: Vec::new(),
                        reason: Some("no_product_unlocks_module".to_string()),
                    };
                }
            }
            for result in &outcome.results {
                if let Some(items) = result.get("upgrade_paths").and_then(|value| value.as_array())
                {
                    let mut products: Vec<UpgradeProduct> = Vec::new();
                    for item in items {
                        if let Ok(product) =
                            serde_json::from_value::<UpgradeProduct>(item.clone())
                        {
                            products.push(product);
                        }
                    }
                    if !products.is_empty() {
                        return UpgradePath {
                            available: true,
                            products,
                            reason: None,
                        };
                    }
                }
            }
        }
        UpgradePath {
            available: false,
            products: Vec::new(),
            reason: Some("no_upgrade_path_provider".to_string()),
        }
    }


    /// Ask the hook bus whether the tenant uses a module outside
    /// the package being replaced. Fail-safe: when nothing answers,
    /// the module is treated as used, so the diff lists it under
    /// `requires_confirmation` instead of silently disabling it.
    pub(crate) fn used_outside_package(&self, company_id: &str, module_id: &str) -> bool {
        let outcome = self.emit_with_results(
            EVENT_MODULE_USAGE_REQUESTED,
            json!({
                "company_id": company_id,
                "module_id": module_id,
                "used_outside_package": false,
            }),
        );
        match outcome {
            Some(outcome) => outcome
                .get("used_outside_package")
                .and_then(|value| value.as_bool())
                .unwrap_or(true),
            None => true,
        }
    }


    /// Create one entitlement row (stored sources only; the
    /// licensing leg is read-only and never written here). One
    /// active row per (company, module, source) triple: an
    /// existing active row for the same triple is superseded
    /// (revoked) first, so repeated grants and package applies
    /// can never stack duplicate active rows for the same source.
    pub(crate) fn create_entitlement(
        &self,
        tenant_id: &str,
        company_id: &str,
        source_kind: &str,
        source_id: &str,
        source_ref: Option<String>,
        module_id: &str,
        expires_at: Option<DateTime<Utc>>,
        granted_by: Option<&str>,
        reason: Option<String>,
    ) -> Entitlement {
        // Supersede first: collect ids before taking any write
        // guard (the DashMap shard lock is not re-entrant, so no
        // map iteration may run while a row guard is held).
        let superseded: Vec<Uuid> = self
            .entitlements
            .iter()
            .filter(|entry| {
                let row = entry.value();
                row.company_id == company_id
                    && row.module_id == module_id
                    && row.source_kind == source_kind
                    && row.source_id == source_id
                    && row.status == STATUS_ACTIVE
            })
            .map(|entry| *entry.key())
            .collect();
        for id in superseded {
            if let Some(mut entry) = self.entitlements.get_mut(&id) {
                let now = Utc::now();
                entry.status = STATUS_REVOKED.to_string();
                entry.lapsed_at = Some(now);
                entry.lapse_reason = Some("superseded_by_new_grant".to_string());
                entry.updated_at = now;
                self.record_history(
                    tenant_id,
                    Some(id),
                    company_id,
                    module_id,
                    "superseded",
                    granted_by,
                    json!({ "source_kind": source_kind, "source_id": source_id }),
                );
            }
        }
        let now = Utc::now();
        let entitlement = Entitlement {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.to_string(),
            company_id: company_id.to_string(),
            source_kind: source_kind.to_string(),
            source_id: source_id.to_string(),
            source_ref,
            module_id: module_id.to_string(),
            status: STATUS_ACTIVE.to_string(),
            granted_at: now,
            expires_at,
            granted_by: granted_by.map(str::to_string),
            reason,
            grace_started_at: None,
            grace_ends_at: None,
            lapsed_at: None,
            lapse_reason: None,
            created_at: now,
            updated_at: now,
        };
        self.entitlements
            .insert(entitlement.id, entitlement.clone());
        self.record_history(
            tenant_id,
            Some(entitlement.id),
            company_id,
            module_id,
            "granted",
            granted_by,
            json!({
                "source_kind": source_kind,
                "source_id": source_id,
                "expires_at": expires_at,
            }),
        );
        self.emit(
            EVENT_ENTITLEMENT_GRANTED,
            json!({
                "company_id": company_id,
                "module_id": module_id,
                "source_kind": source_kind,
                "source_id": source_id,
                "entitlement_id": entitlement.id,
                "expires_at": expires_at,
            }),
        );
        entitlement
    }


    /// Move one stored row into read-only grace (when the settings
    /// allow grace) or straight to disabled. Emits the matching
    /// event and history entry. Never deletes anything.
    pub(crate) fn lapse_row(&self, row: &mut Entitlement, reason: &str) {
        let settings = self.settings();
        let now = Utc::now();
        row.lapsed_at = Some(now);
        row.lapse_reason = Some(reason.to_string());
        row.updated_at = now;
        if self.grace_applies() {
            row.status = STATUS_GRACE.to_string();
            row.grace_started_at = Some(now);
            row.grace_ends_at = Some(now + Duration::days(settings.grace_period_days));
            let grace_ends_at = row.grace_ends_at;
            let tenant_id = row.tenant_id.clone();
            let company_id = row.company_id.clone();
            let module_id = row.module_id.clone();
            let entitlement_id = row.id;
            self.record_history(
                &tenant_id,
                Some(entitlement_id),
                &company_id,
                &module_id,
                "grace_started",
                None,
                json!({ "reason": reason, "grace_ends_at": grace_ends_at }),
            );
            self.emit(
                EVENT_ENTITLEMENT_GRACE_STARTED,
                json!({
                    "company_id": company_id,
                    "module_id": module_id,
                    "entitlement_id": entitlement_id,
                    "grace_ends_at": grace_ends_at,
                    "reason": reason,
                }),
            );
        } else {
            row.status = STATUS_DISABLED.to_string();
            row.grace_started_at = Some(now);
            row.grace_ends_at = Some(now);
            let tenant_id = row.tenant_id.clone();
            let company_id = row.company_id.clone();
            let module_id = row.module_id.clone();
            let entitlement_id = row.id;
            self.record_history(
                &tenant_id,
                Some(entitlement_id),
                &company_id,
                &module_id,
                "disabled",
                None,
                json!({ "reason": reason }),
            );
            self.emit(
                EVENT_ENTITLEMENT_DISABLED,
                json!({
                    "company_id": company_id,
                    "module_id": module_id,
                    "entitlement_id": entitlement_id,
                    "reason": reason,
                }),
            );
            self.emit(
                events::MODULE_DISABLED,
                json!({ "id": module_id, "company_id": company_id, "reason": reason }),
            );
        }
    }


    /// Re-evaluate modules for a tenant after a source lapsed.
    /// The lapsed rows themselves are always settled (marked
    /// revoked with a history entry), but a module that another
    /// active source still covers never enters grace: only a
    /// module with no remaining active source lapses into grace
    /// (or disable). The licensing leg cannot be evaluated from a
    /// hook handler (no AppContext), so a lapse that only affects
    /// a license-backed module is re-checked by the sweep and by
    /// the next effective read.
    pub(crate) fn apply_source_lapse(
        &self,
        source_kind: &str,
        source_id: &str,
        company_id: Option<&str>,
        reason: &str,
    ) {
        let now = Utc::now();
        let mut candidates: Vec<Uuid> = Vec::new();
        for entry in self.entitlements.iter() {
            let row = entry.value();
            if row.source_kind != source_kind {
                continue;
            }
            if row.source_id != source_id && row.source_ref.as_deref() != Some(source_id) {
                continue;
            }
            if let Some(company_id) = company_id {
                if row.company_id != company_id {
                    continue;
                }
            }
            if row.status != STATUS_ACTIVE {
                continue;
            }
            candidates.push(row.id);
        }
        for id in candidates {
            // Compute coverage BEFORE taking the write guard: the
            // DashMap shard lock is not re-entrant, so no map
            // iteration may run while a row guard is held.
            let (tenant_id, company_id, module_id, row_source_kind, row_source_id) =
                match self.entitlements.get(&id) {
                    Some(row) => (
                        row.tenant_id.clone(),
                        row.company_id.clone(),
                        row.module_id.clone(),
                        row.source_kind.clone(),
                        row.source_id.clone(),
                    ),
                    None => continue,
                };
            // Another stored source still covers this module: settle
            // this row as revoked only, with no grace transition.
            let other_active = self.entitlements.iter().any(|other| {
                let other = other.value();
                other.id != id
                    && other.company_id == company_id
                    && other.module_id == module_id
                    && other.status == STATUS_ACTIVE
                    && other.expires_at.is_none_or(|expires| expires > now)
                    && !(other.source_kind == row_source_kind
                        && other.source_id == row_source_id)
            });
            let Some(mut entry) = self.entitlements.get_mut(&id) else {
                continue;
            };
            if other_active {
                let row = entry.value_mut();
                row.status = STATUS_REVOKED.to_string();
                row.lapsed_at = Some(now);
                row.lapse_reason = Some(reason.to_string());
                row.updated_at = now;
                self.record_history(
                    &tenant_id,
                    Some(id),
                    &company_id,
                    &module_id,
                    "revoked",
                    None,
                    json!({ "reason": reason, "covered_by_other_source": true }),
                );
                self.emit(
                    EVENT_ENTITLEMENT_REVOKED,
                    json!({
                        "company_id": company_id,
                        "module_id": module_id,
                        "source_kind": row_source_kind,
                        "source_id": row_source_id,
                        "entitlement_id": id,
                        "reason": reason,
                    }),
                );
            } else {
                let row = entry.value_mut();
                self.lapse_row(row, reason);
            }
        }
    }


    /// Revoke every active stored row for one tenant and module
    /// except explicit manual grants. Called when the tenants
    /// module revokes its grant for the module: the grant is gone,
    /// so the module lapses into grace (or disable) - but a manual
    /// grant made here is a separate entitlement the tenants
    /// revoke did not address, so it survives and only an admin
    /// revoke here removes it. Tenant data is never touched.
    pub(crate) fn revoke_module_rows(&self, company_id: &str, module_id: &str, reason: &str) {
        let ids: Vec<Uuid> = self
            .entitlements
            .iter()
            .filter(|entry| {
                let row = entry.value();
                row.company_id == company_id
                    && row.module_id == module_id
                    && row.status == STATUS_ACTIVE
                    && row.source_kind != SOURCE_MANUAL_GRANT
            })
            .map(|entry| *entry.key())
            .collect();
        for id in ids {
            if let Some(mut entry) = self.entitlements.get_mut(&id) {
                self.lapse_row(entry.value_mut(), reason);
            }
        }
    }


    /// Run the lapse/grace sweep: expired active rows enter grace
    /// (or disable), and grace rows past their window are disabled.
    /// Tenant data is never touched: this only changes entitlement
    /// rows and emits events.
    pub(crate) fn sweep(&self) -> SweepSummary {
        let now = Utc::now();
        let mut scanned = 0usize;
        let mut entered_grace = 0usize;
        let mut disabled = 0usize;
        let mut already_settled = 0usize;
        let ids: Vec<Uuid> = self
            .entitlements
            .iter()
            .map(|entry| *entry.key())
            .collect();
        for id in ids {
            let Some(mut entry) = self.entitlements.get_mut(&id) else {
                continue;
            };
            scanned += 1;
            let row = entry.value_mut();
            match row.status.as_str() {
                STATUS_ACTIVE => {
                    let expired = row.expires_at.is_some_and(|expires| expires <= now);
                    if expired {
                        let had_grace = self.grace_applies();
                        self.lapse_row(row, "expired");
                        if had_grace {
                            entered_grace += 1;
                        } else {
                            disabled += 1;
                        }
                    } else {
                        already_settled += 1;
                    }
                }
                STATUS_GRACE => {
                    let past_grace = row.grace_ends_at.is_some_and(|ends| ends <= now);
                    if past_grace {
                        let tenant_id = row.tenant_id.clone();
                        let company_id = row.company_id.clone();
                        let module_id = row.module_id.clone();
                        let entitlement_id = row.id;
                        row.status = STATUS_DISABLED.to_string();
                        row.updated_at = now;
                        self.record_history(
                            &tenant_id,
                            Some(entitlement_id),
                            &company_id,
                            &module_id,
                            "disabled",
                            None,
                            json!({ "reason": "grace_period_ended" }),
                        );
                        self.emit(
                            EVENT_ENTITLEMENT_DISABLED,
                            json!({
                                "company_id": company_id,
                                "module_id": module_id,
                                "entitlement_id": entitlement_id,
                                "reason": "grace_period_ended",
                            }),
                        );
                        self.emit(
                            events::MODULE_DISABLED,
                            json!({ "id": module_id, "company_id": company_id, "reason": "grace_period_ended" }),
                        );
                        disabled += 1;
                    } else {
                        already_settled += 1;
                    }
                }
                _ => {
                    already_settled += 1;
                }
            }
        }
        SweepSummary {
            scanned,
            entered_grace,
            disabled,
            already_settled,
        }
    }


    /// Top denied modules for a tenant in the lookback window, most
    /// denied first. Only counts are returned - never buyer
    /// personal data.
    pub(crate) fn top_denied_modules(&self, company_id: &str, limit: usize) -> Vec<(String, u64)> {
        let since = Utc::now() - Duration::days(DENIED_LOOKBACK_DAYS);
        let mut counts: Vec<(String, u64)> = Vec::new();
        for entry in self.denied_attempts.iter() {
            let attempt = entry.value();
            if attempt.company_id != company_id || attempt.created_at < since {
                continue;
            }
            match counts.iter_mut().find(|(module_id, _)| *module_id == attempt.module_id) {
                Some((_, count)) => *count += 1,
                None => counts.push((attempt.module_id.clone(), 1)),
            }
        }
        counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        counts.truncate(limit.max(1).min(MAX_SUGGESTIONS));
        counts
    }
}
