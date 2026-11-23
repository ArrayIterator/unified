//! advisor - rules.

use serde::Serialize;



#[derive(Debug, Clone, Serialize)]
// ---------------------------------------------------------------------------
// Rule definitions (the single source of truth, also served at runtime)
// ---------------------------------------------------------------------------
/// One rule, with its condition and severity formula written out in
/// plain language so the formula is visible next to the code that
/// implements it. `data_gap` names the missing source data when the
/// rule cannot evaluate from observed events alone.
pub struct RuleDefinition {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub condition: &'static str,
    pub severity_formula: &'static str,
    pub source_events: &'static [&'static str],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_gap: Option<&'static str>,
}


pub const RULE_DEFINITIONS: &[RuleDefinition] = &[
    RuleDefinition {
        id: "cashflow.overdue_invoices",
        name: "Overdue invoices (cash flow)",
        description: "Aggregate cash-flow warning when a company has overdue invoices with an outstanding balance.",
        condition: "At least one invoice for the company is overdue (past its due date with balance > 0).",
        severity_formula: "critical when overdue balance >= 5000 or overdue invoice count >= 10; warning otherwise.",
        source_events: &["invoice.created", "invoice.paid", "invoice.payment.recorded", "invoice.voided"],
        data_gap: None,
    },
    RuleDefinition {
        id: "cashflow.falling_collections",
        name: "Falling collections",
        description: "Collections trend warning when recorded payments fell against the previous 30-day window.",
        condition: "Payments recorded in the last 30 days total less than payments in the 30 days before that.",
        severity_formula: "drop_pct = (previous_30d - current_30d) / previous_30d * 100; critical when drop_pct >= 50, warning when drop_pct >= 25. Requires previous_30d > 0.",
        source_events: &["invoice.payment.recorded"],
        data_gap: None,
    },
    RuleDefinition {
        id: "billing.overdue_invoice_followup",
        name: "Overdue invoice follow-up",
        description: "One follow-up suggestion per overdue invoice, graded by how long it has been overdue.",
        condition: "An invoice is overdue (past its due date with balance > 0).",
        severity_formula: "From days overdue: 1-7 days is info, 8-30 days is warning, more than 30 days is critical.",
        source_events: &["invoice.created", "invoice.paid", "invoice.payment.recorded", "invoice.voided"],
        data_gap: None,
    },
    RuleDefinition {
        id: "crm.churn_inactive_client",
        name: "Inactive client (churn signal)",
        description: "Churn signal when a CRM contact has had no recorded activity for a long time.",
        condition: "A contact has had no recorded activity for at least 90 days.",
        severity_formula: "From days since last activity: 90-179 days is warning, 180 days or more is critical.",
        source_events: &[],
        data_gap: Some("CRM emits no contact-activity event and Contact carries no last-activity field. This rule only evaluates activity snapshots supplied in the re-evaluate request payload (crm_activities)."),
    },
    RuleDefinition {
        id: "licensing.expiring_license",
        name: "Expiring license",
        description: "Warning when a license approaches expiry, from the licensing module's own expiry scan.",
        condition: "A license.expiring event arrived for a license.",
        severity_formula: "From days remaining: expired or 7 days or fewer is critical, 8-30 days is warning, 31-60 days is info.",
        source_events: &["license.expiring"],
        data_gap: None,
    },
    RuleDefinition {
        id: "billing.underused_subscription",
        name: "Underused subscription",
        description: "Warning when a subscription is paid for but its seats are mostly unused.",
        condition: "A subscription snapshot reports seats used below half of the seats purchased.",
        severity_formula: "utilisation = seats_used / seats_purchased * 100; 10% or below is critical, above 10% and below 50% is warning.",
        source_events: &[],
        data_gap: Some("No source module emits seat-usage data. This rule only evaluates subscription snapshots supplied in the re-evaluate request payload (subscriptions)."),
    },
];


/// Severity thresholds, kept as named constants so the formulas in
/// the rule definitions and the code below cannot drift apart.
/// `OVERDUE_CRITICAL_BALANCE` is in minor units (500_000 = 5000.00
/// in a two-decimal currency).
pub(crate) const OVERDUE_CRITICAL_BALANCE: i64 = 500_000;

pub(crate) const OVERDUE_CRITICAL_COUNT: usize = 10;

pub(crate) const COLLECTIONS_WARNING_DROP_PCT: f64 = 25.0;

pub(crate) const COLLECTIONS_CRITICAL_DROP_PCT: f64 = 50.0;

pub(crate) const FOLLOWUP_WARNING_DAYS: i64 = 7;

pub(crate) const FOLLOWUP_CRITICAL_DAYS: i64 = 30;

pub(crate) const CHURN_WARNING_DAYS: i64 = 90;

pub(crate) const CHURN_CRITICAL_DAYS: i64 = 180;

pub(crate) const LICENSE_CRITICAL_DAYS: i64 = 7;

pub(crate) const LICENSE_WARNING_DAYS: i64 = 30;

pub(crate) const LICENSE_INFO_DAYS: i64 = 60;

pub(crate) const UNDERUSED_WARNING_PCT: f64 = 50.0;

pub(crate) const UNDERUSED_CRITICAL_PCT: f64 = 10.0;

/// A suggestion stays actionable for 30 days unless refreshed by a
/// later evaluation.
pub(crate) const SUGGESTION_TTL_DAYS: i64 = 30;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn rule_definitions_are_non_empty_with_unique_ids() {
        assert!(!RULE_DEFINITIONS.is_empty());
        let mut ids: HashSet<&str> = HashSet::new();
        for rule in RULE_DEFINITIONS {
            assert!(!rule.id.is_empty(), "rule id must not be empty");
            assert!(!rule.name.is_empty(), "rule {} needs a name", rule.id);
            assert!(
                !rule.condition.is_empty(),
                "rule {} needs a condition",
                rule.id
            );
            assert!(
                !rule.severity_formula.is_empty(),
                "rule {} needs a severity formula",
                rule.id
            );
            assert!(ids.insert(rule.id), "duplicate rule id: {}", rule.id);
        }
    }
}
