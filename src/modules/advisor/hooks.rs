//! advisor - hooks.

use unified_core::hooks::HookContext;
use chrono::{ DateTime, Utc };
use serde_json::json;
use std::sync::Arc;
use super::{rules::*, types::*, state::*, helpers::*, handlers_suggestions::*, handlers_rules::*, module::*};



// ---------------------------------------------------------------------------
// Hook handlers: ingest source events into the local fact cache
// ---------------------------------------------------------------------------
/// Cache the invoice carried by `invoice.created`, `invoice.paid`,
/// or `invoice.voided` (all three emit the full invoice JSON).
pub(crate) fn on_invoice_event(state: Arc<AdvisorState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(id) = data.get("id").and_then(|v| v.as_str()) else {
        return;
    };
    let currency = data
        .get("currency")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let fact = InvoiceFact {
        id: id.to_string(),
        company_id: data
            .get("company_id")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        user_id: data
            .get("user_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        number: data
            .get("number")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        // The billing payload carries major units; convert to minor
        // units at this boundary (currency-aware, so zero-decimal
        // currencies convert without dividing by 100).
        total: major_to_minor(
            data.get("total").and_then(|v| v.as_f64()).unwrap_or(0.0),
            &currency,
        ),
        balance: major_to_minor(
            data.get("balance").and_then(|v| v.as_f64()).unwrap_or(0.0),
            &currency,
        ),
        currency,
        status: data
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        overdue: data
            .get("overdue")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        due_date: data
            .get("due_date")
            .and_then(|v| v.as_str())
            .and_then(|value| value.parse::<DateTime<Utc>>().ok()),
        updated_at: Utc::now(),
    };
    state.invoices.insert(fact.id.clone(), fact);
}


/// Cache one payment from `invoice.payment.recorded` and refresh
/// the invoice balance when the payload carries it. The company is
/// resolved through the cached invoice when the payload does not
/// name one, so collections stay attributed to the right company.
pub(crate) fn on_payment_recorded(state: Arc<AdvisorState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let invoice_id = data
        .get("invoice_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let mut company_id = data
        .get("company_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    // The payment payload carries no currency: resolve it from the
    // cached invoice (the billing payload itself carries major
    // units). Without a cached invoice, fall back to a two-decimal
    // currency, the same assumption `serde_major` makes.
    let invoice_currency = invoice_id.as_ref().and_then(|invoice_id| {
        state
            .invoices
            .get(invoice_id)
            .map(|invoice| invoice.currency.clone())
    });
    if company_id.is_none() {
        if let Some(invoice_id) = &invoice_id {
            company_id = state
                .invoices
                .get(invoice_id)
                .and_then(|invoice| invoice.company_id.clone());
        }
    }
    let amount = major_to_minor(
        data.get("amount").and_then(|v| v.as_f64()).unwrap_or(0.0),
        invoice_currency.as_deref().unwrap_or("USD"),
    );
    if amount <= 0 {
        return;
    }
    let paid_at = data
        .get("paid_at")
        .or_else(|| data.get("created_at"))
        .and_then(|v| v.as_str())
        .and_then(|value| value.parse::<DateTime<Utc>>().ok())
        .unwrap_or_else(Utc::now);
    let id = data
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(new_id);
    state.payments.insert(
        id.clone(),
        PaymentFact {
            id,
            company_id,
            invoice_id: invoice_id.clone(),
            amount,
            paid_at,
        },
    );
    if let Some(invoice_id) = invoice_id {
        if let Some(mut invoice) = state.invoices.get_mut(&invoice_id) {
            if let Some(balance) = data.get("balance").and_then(|v| v.as_f64()) {
                invoice.balance = major_to_minor(balance, &invoice.currency);
            }
            if let Some(status) = data.get("status").and_then(|v| v.as_str()) {
                invoice.status = status.to_string();
            }
            invoice.updated_at = Utc::now();
        }
    }
}


/// Cache the license expiry fact from `license.expiring` and
/// evaluate the expiring-license rule immediately, so the
/// suggestion exists without waiting for the next scheduled run.
pub(crate) fn on_license_expiring(state: Arc<AdvisorState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(license_id) = data.get("license_id").and_then(|v| v.as_str()) else {
        return;
    };
    let fact = LicenseFact {
        license_id: license_id.to_string(),
        license_key: data
            .get("license_key")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        days_remaining: data
            .get("days_remaining")
            .and_then(|v| v.as_i64())
            .unwrap_or(0),
        expired: data
            .get("expired")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        expires_at: data
            .get("expires_at")
            .and_then(|v| v.as_str())
            .and_then(|value| value.parse::<DateTime<Utc>>().ok()),
        seen_at: Utc::now(),
    };
    state.licenses.insert(fact.license_id.clone(), fact);
    state.evaluate(None, &["licensing.expiring_license".to_string()]);
}


/// Answer `advisor.evaluate_requested` by running the evaluation
/// for the requested company (or all companies) and reporting the
/// outcome as the hook result. Snapshots in the payload
/// (`crm_activities`, `subscriptions`) are stored first, so the
/// churn and underused-subscription rules can use them.
pub(crate) fn on_evaluate_requested(state: Arc<AdvisorState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let company_id = data
        .get("company_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|value| !value.is_empty());
    if let Some(activities) = data.get("crm_activities").and_then(|v| v.as_array()) {
        for raw in activities {
            if let Ok(activity) = serde_json::from_value::<CrmActivitySnapshot>(raw.clone()) {
                if !activity.contact_id.trim().is_empty() {
                    state
                        .crm_activities
                        .insert(activity.contact_id.clone(), activity);
                }
            }
        }
    }
    if let Some(subscriptions) = data.get("subscriptions").and_then(|v| v.as_array()) {
        for raw in subscriptions {
            if let Ok(subscription) = serde_json::from_value::<SubscriptionSnapshot>(raw.clone()) {
                if !subscription.subscription_id.trim().is_empty()
                    && subscription.seats_used <= subscription.seats_purchased
                    && subscription.amount >= 0
                {
                    state
                        .subscriptions
                        .insert(subscription.subscription_id.clone(), subscription);
                }
            }
        }
    }
    let rules: Vec<String> = data
        .get("rules")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .filter(|rule_id| RULE_DEFINITIONS.iter().any(|rule| rule.id == rule_id))
                .collect()
        })
        .unwrap_or_default();
    let (created, updated, evaluated, gaps) = state.evaluate(company_id.as_deref(), &rules);
    ctx.add_result(json!({
        "company_id": company_id,
        "rules_evaluated": evaluated,
        "suggestions_created": created,
        "suggestions_updated": updated,
        "data_gaps": gaps,
    }));
}
