// Tax module - hook handlers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use base64::Engine as _;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

// Hook handlers

/// Handle a billing source event (`invoice.created` / `order.created`
/// and their `billing.*` alias spellings): compute the tax for the
/// source document and store the computation. The source document is
/// never mutated. The event is ignored when no company can be
/// resolved for it, because tax records are company-scoped.
pub(crate) fn on_source_created(state: Arc<TaxState>, ctx: &mut HookContext, source_type: &str) {
    let data = ctx.data.clone();
    let Some(source_id) = data.get("id").and_then(|v| v.as_str()) else {
        return;
    };
    let source_id = source_id.to_string();
    if state.computations.iter().any(|entry| {
        entry.value().source_type == source_type && entry.value().source_id == source_id
    }) {
        return;
    }
    let company_id = data
        .get("company_id")
        .and_then(|v| v.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok())
        .or_else(|| {
            let user_id = data.get("user_id").and_then(|v| v.as_str())?;
            let answer = state
                .bus()
                .emit("user.company_requested", json!({ "user_id": user_id }));
            answer
                .get("company_id")
                .and_then(|v| v.as_str())
                .and_then(|raw| Uuid::parse_str(raw).ok())
        });
    let Some(company_id) = company_id else {
        return;
    };
    let currency = data
        .get("currency")
        .and_then(|v| v.as_str())
        .unwrap_or("USD")
        .to_string();
    // Source events carry major units; tax works in integer minor
    // units, converted currency-aware at this boundary
    // (zero-decimal currencies keep whole units).
    let amount = data
        .get("subtotal")
        .and_then(|v| v.as_f64())
        .map(|value| major_to_minor(value, &currency))
        .or_else(|| {
            data.get("total")
                .and_then(|v| v.as_f64())
                .map(|value| major_to_minor(value, &currency))
        })
        .or_else(|| {
            data.get("items").and_then(|v| v.as_array()).map(|items| {
                items
                    .iter()
                    .map(|item| {
                        let price = item
                            .get("unit_price")
                            .and_then(|v| v.as_f64())
                            .or_else(|| item.get("amount").and_then(|v| v.as_f64()))
                            .unwrap_or(0.0);
                        let quantity =
                            item.get("quantity").and_then(|v| v.as_u64()).unwrap_or(1) as i64;
                        major_to_minor(price, &currency) * quantity
                    })
                    .sum::<i64>()
            })
        })
        .unwrap_or(0);
    let country_code = data
        .get("country_code")
        .and_then(|v| v.as_str())
        .or_else(|| data.get("country").and_then(|v| v.as_str()))
        .or_else(|| data.get("billing_country").and_then(|v| v.as_str()))
        .map(str::to_string)
        .or_else(|| state.settings_for(company_id).default_country_code.clone());
    let region_code = data
        .get("region_code")
        .and_then(|v| v.as_str())
        .or_else(|| data.get("region").and_then(|v| v.as_str()))
        .or_else(|| data.get("billing_region").and_then(|v| v.as_str()))
        .map(str::to_string)
        .or_else(|| state.settings_for(company_id).default_region_code.clone());
    let on = data
        .get("issue_date")
        .and_then(|v| v.as_str())
        .and_then(|raw| raw.get(..10))
        .and_then(|raw| NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok())
        .unwrap_or_else(today);
    // An inclusive invoice already carries its tax inside the total:
    // compute over the total as the gross amount. An exclusive
    // invoice computes over the net subtotal.
    let tax_inclusive = data
        .get("tax_inclusive")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let base = if tax_inclusive {
        data.get("total")
            .and_then(|v| v.as_f64())
            .map(|value| major_to_minor(value, &currency))
            .unwrap_or(amount)
    } else {
        amount
    };
    compute_and_store(
        &state,
        company_id,
        source_type,
        &source_id,
        base,
        &currency,
        country_code.as_deref(),
        region_code.as_deref(),
        on,
    );
}

/// Run the filing-reminder check for every company that has tax
/// settings stored, answering `tax.filing.check_requested`.
pub(crate) fn on_filing_check_requested(state: Arc<TaxState>, ctx: &mut HookContext) {
    let company_ids: Vec<Uuid> = if let Some(company_id) = ctx
        .get("company_id")
        .and_then(|v| v.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok())
    {
        vec![company_id]
    } else {
        state.settings.iter().map(|entry| *entry.key()).collect()
    };
    let mut checked = 0usize;
    for company_id in company_ids {
        checked += run_filing_check(&state, company_id).len();
    }
    ctx.add_result(json!({ "checked_filings": checked }));
}
