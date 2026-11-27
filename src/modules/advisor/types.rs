//! advisor - types.

use chrono::{ DateTime, Utc };
use serde::{Deserialize, Serialize};
use serde_json::Value;



#[derive(Debug, Clone, Serialize, Deserialize)]
// Types
/// A billing invoice fact cached from observed invoice events. Only
/// the fields the rules read are kept; the full event payload is
/// never stored.
pub struct InvoiceFact {
    pub id: String,
    pub company_id: Option<String>,
    pub user_id: String,
    pub number: String,
    /// Invoice total in minor units. JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub total: i64,
    /// Outstanding balance in minor units. JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub balance: i64,
    pub currency: String,
    pub status: String,
    pub overdue: bool,
    pub due_date: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}


impl InvoiceFact {
    /// True when the invoice is past its due date with money still
    /// outstanding and not in a terminal state. An invoice with no
    /// due date is never overdue.
    pub(crate) fn is_overdue(&self, now: DateTime<Utc>) -> bool {
        if matches!(self.status.as_str(), "paid" | "void" | "credited") {
            return false;
        }
        if self.balance <= 0 {
            return false;
        }
        if self.overdue {
            return true;
        }
        match self.due_date {
            Some(due) => due < now,
            None => false,
        }
    }

    /// Whole days past the due date; 0 when not overdue or when no
    /// due date is known.
    pub(crate) fn days_overdue(&self, now: DateTime<Utc>) -> i64 {
        match self.due_date {
            Some(due) if due < now => (now - due).num_days(),
            _ => 0,
        }
    }
}


/// One recorded payment, cached from `invoice.payment.recorded`
/// for the collections-trend rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentFact {
    pub id: String,
    pub company_id: Option<String>,
    pub invoice_id: Option<String>,
    /// Amount paid in minor units. JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub amount: i64,
    pub paid_at: DateTime<Utc>,
}


/// A license expiry fact cached from `license.expiring` events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LicenseFact {
    pub license_id: String,
    pub license_key: String,
    pub days_remaining: i64,
    pub expired: bool,
    pub expires_at: Option<DateTime<Utc>>,
    pub seen_at: DateTime<Utc>,
}


/// One CRM activity snapshot supplied with a re-evaluate request.
/// The advisor stores the latest snapshot per contact so later
/// evaluations can reuse it without a fresh payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrmActivitySnapshot {
    pub contact_id: String,
    pub company_id: Option<String>,
    pub contact_name: Option<String>,
    pub last_activity_at: Option<DateTime<Utc>>,
}


/// One subscription snapshot supplied with a re-evaluate request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionSnapshot {
    pub subscription_id: String,
    pub company_id: Option<String>,
    pub seats_purchased: u32,
    pub seats_used: u32,
    /// Amount paid per billing period in minor units, in the
    /// subscription currency. JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub amount: i64,
    pub currency: Option<String>,
}


/// One recommendation. `figures` carries the exact numbers the rule
/// used (amounts, counts, dates, ids) and `severity_reason` spells
/// out which branch of the severity formula applied, so no severity
/// is ever presented without its data cited.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Suggestion {
    pub id: String,
    pub company_id: Option<String>,
    pub rule_id: String,
    pub title: String,
    pub body: String,
    pub recommended_action: String,
    pub severity: String,
    pub severity_reason: String,
    pub status: String,
    pub source_type: String,
    pub source_id: String,
    #[serde(default)]
    pub source_ids: Vec<String>,
    pub figures: Value,
    pub expires_at: Option<DateTime<Utc>>,
    pub snoozed_until: Option<DateTime<Utc>>,
    pub acknowledged_by: Option<String>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub dismissed_by: Option<String>,
    pub dismissed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}


/// One rule evaluation run, kept as the audit trail of what the
/// engine saw and produced.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleRun {
    pub id: String,
    pub company_id: Option<String>,
    pub rule_id: String,
    pub ran_at: DateTime<Utc>,
    pub suggestions_created: u32,
    pub suggestions_updated: u32,
    pub inputs_summary: Value,
}


#[derive(Debug, Clone, Deserialize)]
pub struct EvaluateRequest {
    pub company_id: Option<String>,
    #[serde(default)]
    pub crm_activities: Vec<CrmActivitySnapshot>,
    #[serde(default)]
    pub subscriptions: Vec<SubscriptionSnapshot>,
    #[serde(default)]
    pub rules: Vec<String>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct SnoozeRequest {
    pub snoozed_until: Option<DateTime<Utc>>,
    pub snooze_days: Option<u32>,
}


/// Actor identity once taken from query parameters (`viewer_id` /
/// `viewer_role`). Retained for compatibility only: callers are now
/// resolved from the `X-User-Id` / `X-Role` request headers by
/// `caller()` in `helpers.rs`, and no authorization decision reads
/// this type. Query parameters must never be trusted for a role.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ActorQuery {
    pub viewer_id: Option<String>,
    pub viewer_role: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invoice(status: &str, balance: i64, due: Option<DateTime<Utc>>) -> InvoiceFact {
        InvoiceFact {
            id: "inv-1".to_string(),
            company_id: None,
            user_id: "user-1".to_string(),
            number: "INV-1".to_string(),
            total: 10000,
            balance,
            currency: "USD".to_string(),
            status: status.to_string(),
            overdue: false,
            due_date: due,
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn is_overdue_requires_open_balance_past_due() {
        let now = parse_due("2026-11-20T00:00:00Z");
        let past = Some(parse_due("2026-11-10T00:00:00Z"));
        assert!(invoice("sent", 5000, past).is_overdue(now));
        assert!(!invoice("paid", 5000, past).is_overdue(now));
        assert!(!invoice("void", 5000, past).is_overdue(now));
        assert!(!invoice("sent", 0, past).is_overdue(now));
        assert!(!invoice("sent", 5000, None).is_overdue(now));
        let future = Some(parse_due("2026-11-25T00:00:00Z"));
        assert!(!invoice("sent", 5000, future).is_overdue(now));
    }

    fn parse_due(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("fixed test date")
            .with_timezone(&Utc)
    }

    #[test]
    fn days_overdue_counts_whole_days_past_due() {
        let now = parse_due("2026-11-20T12:00:00Z");
        let past = Some(parse_due("2026-11-10T00:00:00Z"));
        assert_eq!(invoice("sent", 5000, past).days_overdue(now), 10);
        assert_eq!(invoice("sent", 5000, None).days_overdue(now), 0);
        let future = Some(parse_due("2026-11-25T00:00:00Z"));
        assert_eq!(invoice("sent", 5000, future).days_overdue(now), 0);
    }

    #[test]
    fn overdue_flag_marks_invoice_overdue_without_due_date() {
        let now = parse_due("2026-11-20T00:00:00Z");
        let mut fact = invoice("sent", 5000, None);
        fact.overdue = true;
        assert!(fact.is_overdue(now));
    }
}
