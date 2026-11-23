//! billing - reference.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::reference_id::{self, ReferenceIdSettings};
use actix_web::{web, HttpRequest, HttpResponse};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use super::state::*;
use super::helpers::*;
use super::templates::*;

/// Capability needed to read the reference settings.
pub(crate) const PERM_REFERENCE_READ: &str = "billing.reference:read";

/// Capability needed to change the reference settings.
pub(crate) const PERM_REFERENCE_MANAGE: &str = "billing.reference:manage";

/// Snapshot of the current reference settings.
pub(crate) fn reference_settings(state: &BillingState) -> ReferenceIdSettings {
    state
        .reference_settings
        .read()
        .expect("reference settings lock poisoned")
        .clone()
}

/// A fresh reference for an invoice with no source order, issued
/// with the configured default prefix.
pub(crate) fn fresh_invoice_reference(state: &BillingState) -> String {
    reference_id::generate_with_prefix(&reference_settings(state).default_prefix)
}

/// The reference for an invoice built from an order payload: the
/// order's own reference when the payload carries a valid one,
/// otherwise a fresh reference (orders issued before references
/// existed carry none).
pub(crate) fn invoice_reference_from_order(state: &BillingState, order: &Value) -> String {
    match order
        .get("reference")
        .and_then(|value| value.as_str())
        .filter(|reference| reference_id::is_valid(reference))
    {
        Some(reference) => reference.to_string(),
        None => fresh_invoice_reference(state),
    }
}

/// The reference for a manually created invoice: the seen order's
/// reference when an order id is given and this module has seen that
/// order's payload, otherwise a fresh reference.
pub(crate) fn invoice_reference_for_create(
    state: &BillingState,
    order_id: Option<&str>,
) -> String {
    match order_id.and_then(|id| state.seen_orders.get(id)) {
        Some(order) => invoice_reference_from_order(state, order.value()),
        None => fresh_invoice_reference(state),
    }
}

/// Admin input for `PUT /api/v1/billing/reference-settings`. All
/// fields are optional and applied together: `default_prefix`
/// replaces the default prefix, `product_prefixes` replaces the
/// whole per-product map, and `remove_product_prefixes` deletes
/// individual overrides. Every prefix is normalized with
/// `format_prefix` before it is stored.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateReferenceSettings {
    pub default_prefix: Option<String>,
    pub product_prefixes: Option<HashMap<String, String>>,
    pub remove_product_prefixes: Option<Vec<String>>,
}

/// Read the reference settings (capability `billing.reference:read`).
pub(crate) async fn get_reference_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
) -> HttpResponse {
    let result: UnifiedResult<ReferenceIdSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_REFERENCE_READ)?;
        Ok(reference_settings(&state))
    })();
    json::respond(result, ctx.is_debug())
}

/// Update the reference settings (capability
/// `billing.reference:manage`). Already-issued references keep
/// validating: changing a prefix never rewrites an issued id.
pub(crate) async fn update_reference_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<BillingState>>,
    req: HttpRequest,
    body: web::Json<UpdateReferenceSettings>,
) -> HttpResponse {
    let result: UnifiedResult<ReferenceIdSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_REFERENCE_MANAGE)?;
        let input = body.into_inner();
        let mut candidate = reference_settings(&state);
        if let Some(prefix) = input.default_prefix {
            candidate.default_prefix = reference_id::format_prefix(&prefix);
        }
        if let Some(prefixes) = input.product_prefixes {
            let mut next = ReferenceIdSettings::default();
            next.default_prefix = candidate.default_prefix.clone();
            for (key, prefix) in prefixes {
                next.set_product_prefix(&key, &prefix).map_err(|problem| {
                    UnifiedError::bad_request(problem)
                })?;
            }
            candidate = next;
        }
        if let Some(keys) = input.remove_product_prefixes {
            for key in keys {
                candidate.remove_product_prefix(&key);
            }
        }
        let problems = candidate.validate();
        if !problems.is_empty() {
            return Err(UnifiedError::unprocessable(format!(
                "Reference settings are invalid: {}",
                problems.join("; ")
            )));
        }
        {
            let mut settings = state
                .reference_settings
                .write()
                .expect("reference settings lock poisoned");
            *settings = candidate.clone();
        }
        state.emit(
            reference_id::EVENT_REFERENCE_SETTINGS_CHANGED,
            serde_json::to_value(&candidate).unwrap_or(Value::Null),
        );
        Ok(candidate)
    })();
    json::respond(result, ctx.is_debug())
}
