//! advisor - state.

use unified_core::hooks::HookBus;
use chrono::{ Duration, Utc };
use dashmap::DashMap;
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use super::{rules::*, types::*, helpers::*, handlers_suggestions::*, handlers_rules::*, hooks::*, module::*};



// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------
pub struct AdvisorState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    pub(crate) suggestions: DashMap<String, Suggestion>,
    pub(crate) invoices: DashMap<String, InvoiceFact>,
    pub(crate) payments: DashMap<String, PaymentFact>,
    pub(crate) licenses: DashMap<String, LicenseFact>,
    pub(crate) crm_activities: DashMap<String, CrmActivitySnapshot>,
    pub(crate) subscriptions: DashMap<String, SubscriptionSnapshot>,
    pub(crate) rule_runs: DashMap<String, RuleRun>,
}


impl AdvisorState {
    pub(crate) fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            suggestions: DashMap::new(),
            invoices: DashMap::new(),
            payments: DashMap::new(),
            licenses: DashMap::new(),
            crm_activities: DashMap::new(),
            subscriptions: DashMap::new(),
            rule_runs: DashMap::new(),
        }
    }

    pub(crate) fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// Ask the Roles module whether `role` holds `permission`
    /// (`role.check`). Fails closed: no answer means not allowed.
    pub(crate) fn check_capability(&self, role: &str, permission: &str) -> bool {
        let context = self.bus().emit(
            "role.check",
            json!({ "role": role, "permission": permission, "allowed": false }),
        );
        context
            .get("allowed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// Send an alert through the Notifications module. Critical
    /// suggestions only; `user_id` falls back to `system` when the
    /// source record names no user.
    pub(crate) fn notify(&self, user_id: Option<&str>, title: String, body: String) {
        let user_id = user_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("system");
        self.emit(
            "notification.requested",
            json!({
                "user_id": user_id,
                "title": title,
                "body": body,
                "kind": "advisor",
            }),
        );
    }

    /// Dedupe key for a suggestion: one open suggestion per
    /// (company, rule, source). Re-evaluation refreshes the matching
    /// suggestion instead of creating a duplicate.
    pub(crate) fn dedupe_key(company_id: Option<&str>, rule_id: &str, source_type: &str, source_id: &str) -> String {
        format!(
            "{}|{rule_id}|{source_type}|{source_id}",
            company_id.unwrap_or("")
        )
    }

    /// Store a new suggestion, or refresh the open suggestion with
    /// the same dedupe key (figures, severity, expiry). Returns
    /// (suggestion, created_new). Dismissed suggestions are never
    /// resurrected; snoozed suggestions are left alone until their
    /// snooze expires.
    pub(crate) fn upsert_suggestion(&self, mut suggestion: Suggestion) -> (Suggestion, bool) {
        let key = Self::dedupe_key(
            suggestion.company_id.as_deref(),
            &suggestion.rule_id,
            &suggestion.source_type,
            &suggestion.source_id,
        );
        // Find the open suggestion with the same key first, then
        // drop the iterator before mutating the map: inserting into
        // a DashMap while iterating it can deadlock on the shard
        // the iterator currently holds.
        let existing_match: Option<Suggestion> = self
            .suggestions
            .iter()
            .map(|entry| entry.value().clone())
            .find(|existing| {
                Self::dedupe_key(
                    existing.company_id.as_deref(),
                    &existing.rule_id,
                    &existing.source_type,
                    &existing.source_id,
                ) == key
            });
        if let Some(existing) = existing_match {
            if existing.status == "dismissed" {
                return (existing, false);
            }
            if existing.status == "snoozed" {
                if let Some(until) = existing.snoozed_until {
                    if until > Utc::now() {
                        return (existing, false);
                    }
                }
            }
            suggestion.id = existing.id.clone();
            suggestion.status = existing.status.clone();
            suggestion.snoozed_until = existing.snoozed_until;
            suggestion.acknowledged_by = existing.acknowledged_by.clone();
            suggestion.acknowledged_at = existing.acknowledged_at;
            suggestion.created_at = existing.created_at;
            suggestion.updated_at = Utc::now();
            let updated = suggestion.clone();
            self.suggestions.insert(updated.id.clone(), updated.clone());
            return (updated, false);
        }
        let created = suggestion.clone();
        self.suggestions.insert(created.id.clone(), created.clone());
        self.emit(
            "advisor.suggestion_created",
            serde_json::to_value(&created).unwrap_or(Value::Null),
        );
        if created.severity == "critical" {
            let user_id = created
                .figures
                .get("user_id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            self.notify(
                user_id.as_deref(),
                created.title.clone(),
                format!("{} Recommended action: {}", created.body, created.recommended_action),
            );
        }
        (created, true)
    }

    pub(crate) fn record_run(
        &self,
        company_id: Option<String>,
        rule_id: &str,
        created: u32,
        updated: u32,
        inputs_summary: Value,
    ) {
        let run = RuleRun {
            id: new_id(),
            company_id,
            rule_id: rule_id.to_string(),
            ran_at: Utc::now(),
            suggestions_created: created,
            suggestions_updated: updated,
            inputs_summary,
        };
        self.rule_runs.insert(run.id.clone(), run);
    }

    pub(crate) fn new_suggestion(
        company_id: Option<String>,
        rule_id: &str,
        title: String,
        body: String,
        recommended_action: String,
        severity: &str,
        severity_reason: String,
        source_type: &str,
        source_id: &str,
        source_ids: Vec<String>,
        figures: Value,
    ) -> Suggestion {
        let now = Utc::now();
        Suggestion {
            id: new_id(),
            company_id,
            rule_id: rule_id.to_string(),
            title,
            body,
            recommended_action,
            severity: severity.to_string(),
            severity_reason,
            status: "new".to_string(),
            source_type: source_type.to_string(),
            source_id: source_id.to_string(),
            source_ids,
            figures,
            expires_at: Some(now + Duration::days(SUGGESTION_TTL_DAYS)),
            snoozed_until: None,
            acknowledged_by: None,
            acknowledged_at: None,
            dismissed_by: None,
            dismissed_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Evaluate every selected rule for one company (or for all
    /// companies when `company_id` is None) against the cached
    /// facts. Returns (created, updated, rules_evaluated, gaps).
    /// `gaps` names the rules that could not evaluate because their
    /// input data was never observed.
    pub(crate) fn evaluate(
        &self,
        company_id: Option<&str>,
        rule_ids: &[String],
    ) -> (u32, u32, Vec<String>, Vec<String>) {
        let now = Utc::now();
        let wanted = |rule_id: &str| rule_ids.is_empty() || rule_ids.iter().any(|id| id == rule_id);
        let mut created: u32 = 0;
        let mut updated: u32 = 0;
        let mut evaluated: Vec<String> = Vec::new();
        let mut gaps: Vec<String> = Vec::new();
        let in_scope = |fact_company: &Option<String>| match company_id {
            Some(company) => fact_company.as_deref() == Some(company),
            None => true,
        };

        // Rule 1 + 3 share the overdue-invoice facts.
        if wanted("cashflow.overdue_invoices") || wanted("billing.overdue_invoice_followup") {
            let overdue: Vec<InvoiceFact> = self
                .invoices
                .iter()
                .map(|entry| entry.value().clone())
                .filter(|invoice| in_scope(&invoice.company_id) && invoice.is_overdue(now))
                .collect();
            if wanted("cashflow.overdue_invoices") {
                evaluated.push("cashflow.overdue_invoices".to_string());
                let (created_before, updated_before) = (created, updated);
                let overdue_total: i64 = overdue.iter().map(|invoice| invoice.balance).sum();
                let overdue_count = overdue.len();
                // Figures and human-readable text carry major units,
                // as before; the first overdue invoice's currency is
                // the display currency (the old code summed mixed
                // currencies blindly too).
                let display_currency = overdue
                    .first()
                    .map(|invoice| invoice.currency.clone())
                    .unwrap_or_default();
                let overdue_major = minor_to_major(overdue_total, &display_currency);
                let critical_major =
                    minor_to_major(OVERDUE_CRITICAL_BALANCE, &display_currency);
                let source_ids: Vec<String> =
                    overdue.iter().map(|invoice| invoice.id.clone()).collect();
                if overdue_count > 0 {
                    let severity = if overdue_total >= OVERDUE_CRITICAL_BALANCE
                        || overdue_count >= OVERDUE_CRITICAL_COUNT
                    {
                        "critical"
                    } else {
                        "warning"
                    };
                    let severity_reason = format!(
                        "overdue_balance={overdue_major:.2} (critical at >= {critical_major:.2}), overdue_count={overdue_count} (critical at >= {OVERDUE_CRITICAL_COUNT}); severity={severity}"
                    );
                    let suggestion = Self::new_suggestion(
                        company_id.map(str::to_string),
                        "cashflow.overdue_invoices",
                        "Overdue invoices are tying up cash flow".to_string(),
                        format!(
                            "{overdue_count} invoice(s) are overdue with a combined outstanding balance of {overdue_major:.2}."
                        ),
                        "Review the overdue invoices, send payment reminders, and follow up with the largest debtors first.".to_string(),
                        severity,
                        severity_reason,
                        "invoice",
                        "aggregate",
                        source_ids.clone(),
                        json!({
                            "overdue_count": overdue_count,
                            "overdue_balance": overdue_major,
                            "overdue_invoice_ids": source_ids,
                            "currency": overdue.first().map(|invoice| invoice.currency.clone()),
                        }),
                    );
                    let (_, is_new) = self.upsert_suggestion(suggestion);
                    if is_new { created += 1; } else { updated += 1; }
                }
                self.record_run(
                    company_id.map(str::to_string),
                    "cashflow.overdue_invoices",
                    created - created_before,
                    updated - updated_before,
                    json!({ "overdue_count": overdue_count, "overdue_balance": overdue_major }),
                );
            }
            if wanted("billing.overdue_invoice_followup") {
                evaluated.push("billing.overdue_invoice_followup".to_string());
                let (created_before, updated_before) = (created, updated);
                for invoice in &overdue {
                    let days = invoice.days_overdue(now);
                    let (severity, reason) = if days > FOLLOWUP_CRITICAL_DAYS {
                        ("critical", format!("days_overdue={days} (critical at > {FOLLOWUP_CRITICAL_DAYS})"))
                    } else if days > FOLLOWUP_WARNING_DAYS {
                        ("warning", format!("days_overdue={days} (warning at > {FOLLOWUP_WARNING_DAYS}, critical at > {FOLLOWUP_CRITICAL_DAYS})"))
                    } else {
                        ("info", format!("days_overdue={days} (info at <= {FOLLOWUP_WARNING_DAYS})"))
                    };
                    let suggestion = Self::new_suggestion(
                        invoice.company_id.clone(),
                        "billing.overdue_invoice_followup",
                        format!("Invoice {} is overdue", invoice.number),
                        format!(
                            "Invoice {} for {} {} is {} day(s) overdue with an outstanding balance of {:.2}.",
                            invoice.number,
                            minor_to_major(invoice.total, &invoice.currency),
                            invoice.currency,
                            days,
                            minor_to_major(invoice.balance, &invoice.currency)
                        ),
                        "Contact the customer, confirm the payment date, and record the follow-up.".to_string(),
                        severity,
                        reason,
                        "invoice",
                        &invoice.id,
                        vec![invoice.id.clone()],
                        json!({
                            "invoice_id": invoice.id,
                            "invoice_number": invoice.number,
                            "user_id": invoice.user_id,
                            "total": minor_to_major(invoice.total, &invoice.currency),
                            "balance": minor_to_major(invoice.balance, &invoice.currency),
                            "currency": invoice.currency,
                            "days_overdue": days,
                            "due_date": invoice.due_date,
                        }),
                    );
                    let (_, is_new) = self.upsert_suggestion(suggestion);
                    if is_new { created += 1; } else { updated += 1; }
                }
                self.record_run(
                    company_id.map(str::to_string),
                    "billing.overdue_invoice_followup",
                    created - created_before,
                    updated - updated_before,
                    json!({ "overdue_count": overdue.len() }),
                );
            }
        }

        // Rule 2: falling collections from recorded payments.
        if wanted("cashflow.falling_collections") {
            evaluated.push("cashflow.falling_collections".to_string());
            let (created_before, updated_before) = (created, updated);
            let current_start = now - Duration::days(30);
            let previous_start = now - Duration::days(60);
            let mut current_total: i64 = 0;
            let mut previous_total: i64 = 0;
            let mut payment_ids: Vec<String> = Vec::new();
            for entry in self.payments.iter() {
                let payment = entry.value();
                if !in_scope(&payment.company_id) {
                    continue;
                }
                if payment.paid_at >= current_start {
                    current_total += payment.amount;
                    payment_ids.push(payment.id.clone());
                } else if payment.paid_at >= previous_start {
                    previous_total += payment.amount;
                    payment_ids.push(payment.id.clone());
                }
            }
            // Figures carry major units, as before; payments carry
            // no currency, so the two-decimal assumption applies
            // (the same assumption `serde_major` makes).
            let current_major = minor_to_major(current_total, "USD");
            let previous_major = minor_to_major(previous_total, "USD");
            if previous_total > 0 && current_total < previous_total {
                let drop_pct =
                    (previous_total - current_total) as f64 / previous_total as f64 * 100.0;
                let severity = if drop_pct >= COLLECTIONS_CRITICAL_DROP_PCT {
                    "critical"
                } else if drop_pct >= COLLECTIONS_WARNING_DROP_PCT {
                    "warning"
                } else {
                    ""
                };
                if !severity.is_empty() {
                    let severity_reason = format!(
                        "current_30d={current_major:.2}, previous_30d={previous_major:.2}, drop_pct={drop_pct:.1} (warning at >= {COLLECTIONS_WARNING_DROP_PCT}, critical at >= {COLLECTIONS_CRITICAL_DROP_PCT}); severity={severity}"
                    );
                    let suggestion = Self::new_suggestion(
                        company_id.map(str::to_string),
                        "cashflow.falling_collections",
                        "Collections have fallen against the previous 30 days".to_string(),
                        format!(
                            "Recorded payments in the last 30 days total {current_major:.2}, down {drop_pct:.1}% from {previous_major:.2} in the 30 days before that."
                        ),
                        "Check for overdue invoices and stalled payment follow-ups, and review whether a pricing or billing change coincides with the drop.".to_string(),
                        severity,
                        severity_reason,
                        "payment",
                        "aggregate",
                        payment_ids.clone(),
                        json!({
                            "current_30d_total": current_major,
                            "previous_30d_total": previous_major,
                            "drop_pct": drop_pct,
                            "payment_ids": payment_ids,
                        }),
                    );
                    let (_, is_new) = self.upsert_suggestion(suggestion);
                    if is_new { created += 1; } else { updated += 1; }
                }
            }
            self.record_run(
                company_id.map(str::to_string),
                "cashflow.falling_collections",
                created - created_before,
                updated - updated_before,
                json!({ "current_30d_total": current_major, "previous_30d_total": previous_major }),
            );
        }

        // Rule 4: churn signals from CRM activity snapshots.
        if wanted("crm.churn_inactive_client") {
            evaluated.push("crm.churn_inactive_client".to_string());
            let (created_before, updated_before) = (created, updated);
            let mut saw_any = false;
            for entry in self.crm_activities.iter() {
                let activity = entry.value();
                if !in_scope(&activity.company_id) {
                    continue;
                }
                saw_any = true;
                let Some(last) = activity.last_activity_at else {
                    continue;
                };
                let days = (now - last).num_days();
                let (severity, reason) = if days >= CHURN_CRITICAL_DAYS {
                    ("critical", format!("days_since_last_activity={days} (critical at >= {CHURN_CRITICAL_DAYS})"))
                } else if days >= CHURN_WARNING_DAYS {
                    ("warning", format!("days_since_last_activity={days} (warning at >= {CHURN_WARNING_DAYS}, critical at >= {CHURN_CRITICAL_DAYS})"))
                } else {
                    continue;
                };
                let name = activity.contact_name.clone().unwrap_or_else(|| activity.contact_id.clone());
                let suggestion = Self::new_suggestion(
                    activity.company_id.clone(),
                    "crm.churn_inactive_client",
                    format!("Client {name} has gone quiet"),
                    format!(
                        "No recorded activity for contact {name} in {days} day(s) (last activity {}).",
                        last.to_rfc3339()
                    ),
                    "Reach out to the client, check whether the relationship or the subscription is at risk, and log the outreach.".to_string(),
                    severity,
                    reason,
                    "crm_contact",
                    &activity.contact_id,
                    vec![activity.contact_id.clone()],
                    json!({
                        "contact_id": activity.contact_id,
                        "contact_name": activity.contact_name,
                        "days_since_last_activity": days,
                        "last_activity_at": last,
                    }),
                );
                let (_, is_new) = self.upsert_suggestion(suggestion);
                if is_new { created += 1; } else { updated += 1; }
            }
            if !saw_any {
                gaps.push(
                    "crm.churn_inactive_client: no CRM activity snapshots were ever supplied; include crm_activities in the re-evaluate request".to_string(),
                );
            }
            self.record_run(
                company_id.map(str::to_string),
                "crm.churn_inactive_client",
                created - created_before,
                updated - updated_before,
                json!({ "snapshots_seen": saw_any }),
            );
        }

        // Rule 5: expiring licenses from observed license events.
        if wanted("licensing.expiring_license") {
            evaluated.push("licensing.expiring_license".to_string());
            let (created_before, updated_before) = (created, updated);
            let licenses: Vec<LicenseFact> = self
                .licenses
                .iter()
                .map(|entry| entry.value().clone())
                .collect();
            for license in &licenses {
                let days = license.days_remaining;
                let (severity, reason) = if license.expired || days <= LICENSE_CRITICAL_DAYS {
                    ("critical", format!("days_remaining={days}, expired={} (critical at expired or <= {LICENSE_CRITICAL_DAYS})", license.expired))
                } else if days <= LICENSE_WARNING_DAYS {
                    ("warning", format!("days_remaining={days} (warning at <= {LICENSE_WARNING_DAYS}, critical at <= {LICENSE_CRITICAL_DAYS})"))
                } else if days <= LICENSE_INFO_DAYS {
                    ("info", format!("days_remaining={days} (info at <= {LICENSE_INFO_DAYS})"))
                } else {
                    continue;
                };
                let suggestion = Self::new_suggestion(
                    company_id.map(str::to_string),
                    "licensing.expiring_license",
                    format!("License {} expires soon", license.license_key),
                    if license.expired {
                        format!("License {} has expired.", license.license_key)
                    } else {
                        format!(
                            "License {} expires in {days} day(s){}.",
                            license.license_key,
                            license
                                .expires_at
                                .map(|expires| format!(" on {}", expires.to_rfc3339()))
                                .unwrap_or_default()
                        )
                    },
                    "Renew the license or replace it before validation starts failing.".to_string(),
                    severity,
                    reason,
                    "license",
                    &license.license_id,
                    vec![license.license_id.clone()],
                    json!({
                        "license_id": license.license_id,
                        "license_key": license.license_key,
                        "days_remaining": days,
                        "expired": license.expired,
                        "expires_at": license.expires_at,
                    }),
                );
                let (_, is_new) = self.upsert_suggestion(suggestion);
                if is_new { created += 1; } else { updated += 1; }
            }
            self.record_run(
                company_id.map(str::to_string),
                "licensing.expiring_license",
                created - created_before,
                updated - updated_before,
                json!({ "licenses_seen": licenses.len() }),
            );
        }

        // Rule 6: underused subscriptions from supplied snapshots.
        if wanted("billing.underused_subscription") {
            evaluated.push("billing.underused_subscription".to_string());
            let (created_before, updated_before) = (created, updated);
            let mut saw_any = false;
            for entry in self.subscriptions.iter() {
                let subscription = entry.value();
                if !in_scope(&subscription.company_id) {
                    continue;
                }
                saw_any = true;
                if subscription.seats_purchased == 0 {
                    continue;
                }
                let utilisation = subscription.seats_used as f64
                    / subscription.seats_purchased as f64
                    * 100.0;
                let (severity, reason) = if utilisation <= UNDERUSED_CRITICAL_PCT {
                    ("critical", format!("utilisation={utilisation:.1}% (critical at <= {UNDERUSED_CRITICAL_PCT}%)"))
                } else if utilisation < UNDERUSED_WARNING_PCT {
                    ("warning", format!("utilisation={utilisation:.1}% (warning at < {UNDERUSED_WARNING_PCT}%, critical at <= {UNDERUSED_CRITICAL_PCT}%)"))
                } else {
                    continue;
                };
                let suggestion = Self::new_suggestion(
                    subscription.company_id.clone(),
                    "billing.underused_subscription",
                    "Subscription seats are mostly unused".to_string(),
                    format!(
                        "Subscription {} uses {} of {} purchased seats ({utilisation:.1}%) while still being paid for ({} {} per period).",
                        subscription.subscription_id,
                        subscription.seats_used,
                        subscription.seats_purchased,
                        minor_to_major(
                            subscription.amount,
                            subscription.currency.as_deref().unwrap_or("USD")
                        ),
                        subscription.currency.clone().unwrap_or_default()
                    ),
                    "Reduce the seat count at renewal, reassign the unused seats, or cancel the subscription if it is no longer needed.".to_string(),
                    severity,
                    reason,
                    "subscription",
                    &subscription.subscription_id,
                    vec![subscription.subscription_id.clone()],
                    json!({
                        "subscription_id": subscription.subscription_id,
                        "seats_purchased": subscription.seats_purchased,
                        "seats_used": subscription.seats_used,
                        "utilisation_pct": utilisation,
                        "amount": minor_to_major(
                            subscription.amount,
                            subscription.currency.as_deref().unwrap_or("USD")
                        ),
                        "currency": subscription.currency,
                    }),
                );
                let (_, is_new) = self.upsert_suggestion(suggestion);
                if is_new { created += 1; } else { updated += 1; }
            }
            if !saw_any {
                gaps.push(
                    "billing.underused_subscription: no subscription snapshots were ever supplied; include subscriptions in the re-evaluate request".to_string(),
                );
            }
            self.record_run(
                company_id.map(str::to_string),
                "billing.underused_subscription",
                created - created_before,
                updated - updated_before,
                json!({ "snapshots_seen": saw_any }),
            );
        }

        (created, updated, evaluated, gaps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedupe_key_is_stable_and_scoped() {
        let key = AdvisorState::dedupe_key(Some("co-1"), "rule-1", "invoice", "inv-1");
        assert_eq!(
            AdvisorState::dedupe_key(Some("co-1"), "rule-1", "invoice", "inv-1"),
            key
        );
        assert_ne!(
            AdvisorState::dedupe_key(Some("co-2"), "rule-1", "invoice", "inv-1"),
            key
        );
        assert_ne!(
            AdvisorState::dedupe_key(Some("co-1"), "rule-2", "invoice", "inv-1"),
            key
        );
        assert_ne!(
            AdvisorState::dedupe_key(Some("co-1"), "rule-1", "invoice", "inv-2"),
            key
        );
        assert!(AdvisorState::dedupe_key(None, "rule-1", "invoice", "inv-1")
            .starts_with("|rule-1|"));
    }
}
