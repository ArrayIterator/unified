//! payments - handlers methods.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{Datelike, Utc};
use serde_json::{json, Value};
use std::sync::{Arc};
use uuid::Uuid;
use super::{events::*, card_validation::*, card_settings::*, limits::*, providers::*, types::*, reference_ids::*, credentials::*, records::*, adapter::*, dispatch::*, stripe::*, paypal::*, xendit::*, midtrans::*, hyperpay::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};



// ---------------------------------------------------------------------------
// Handlers: payment methods (tokenized only).
// ---------------------------------------------------------------------------
pub(crate) async fn create_payment_method(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    req: HttpRequest,
    body: web::Json<CreatePaymentMethodRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentMethod> = (|| {
        let _tenant = require_tenant(&req)?;
        let input = body.into_inner();
        if input.customer_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("customer_id is required"));
        }
        require_customer(&req, &input.customer_id, "customer")?;
        let token = validate_token(Some(&input.provider_token), "payment method")?;
        if input.brand.trim().is_empty() {
            return Err(UnifiedError::bad_request("Card brand is required"));
        }
        if input.last4.len() != 4 || !input.last4.chars().all(|c| c.is_ascii_digit()) {
            return Err(UnifiedError::bad_request(
                "last4 must be exactly four digits",
            ));
        }
        if !(1..=12).contains(&input.exp_month) {
            return Err(UnifiedError::bad_request("exp_month must be 1-12"));
        }
        // Coarse shape guard only: the year must be a plausible
        // four-digit year no tenant setting could ever accept
        // beyond (current year + the cards module's 50-year
        // maximum). Past-ness and the real per-tenant bound stay
        // the validator's `invalid_expiry` — the gate below runs
        // it. See `CARDS_EXPIRY_YEAR_GUARD_MAX_AHEAD`.
        let latest_expiry_year =
            Utc::now().year() + CARDS_EXPIRY_YEAR_GUARD_MAX_AHEAD as i32;
        if input.exp_year < 2000 || input.exp_year > latest_expiry_year as u32 {
            return Err(UnifiedError::bad_request(
                "exp_year must be a four-digit year no more than 50 years ahead",
            ));
        }
        let connection = find_connection(input.connection_id)?;
        // Batch 18 (S5): validate the claimed brand / last4 /
        // expiry before storing the method (`claim` mode when no
        // raw card details were supplied, `full` mode when they
        // were). The stored brand comes from the validator's
        // answer — authoritative — falling back to the claimed
        // brand when the answer carried none.
        let outcome = request_card_validation(
            &state,
            &req,
            input.card.as_ref(),
            None,
            Some(CardMethodRef {
                brand: input.brand.trim().to_string(),
                last4: input.last4.clone(),
                exp_month: input.exp_month,
                exp_year: input.exp_year,
            }),
            &connection,
            &input.customer_id,
            None,
            None,
        )?;
        let brand = outcome
            .as_ref()
            .and_then(|outcome| outcome.brand.clone())
            .unwrap_or_else(|| input.brand.trim().to_string());
        let method = PaymentMethod {
            id: Uuid::now_v7(),
            customer_id: input.customer_id.clone(),
            connection_id: connection.id,
            provider: connection.kind.as_str().to_string(),
            provider_token: token,
            brand,
            last4: input.last4,
            exp_month: input.exp_month,
            exp_year: input.exp_year,
            is_default: input.is_default,
            created_at: Utc::now(),
        };
        if method.is_default {
            for mut entry in methods_store().iter_mut() {
                if entry.value().customer_id == method.customer_id && entry.value().is_default {
                    entry.value_mut().is_default = false;
                }
            }
        }
        methods_store().insert(method.id, method.clone());
        Ok(method)
    })();
    match result {
        Ok(method) => json::success_response(method, 201),
        Err(error) => json::error_response(&error, debug),
    }
}


pub(crate) async fn list_payment_methods(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<PaymentMethod>> = (|| {
        // Lists are scoped to the caller: an admin sees every
        // customer's methods, anyone else sees only their own.
        let _tenant = require_tenant(&req)?;
        let scope = list_customer_scope(&ctx, &req)?;
        let mut items: Vec<PaymentMethod> = methods_store()
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|method| {
                scope
                    .as_ref()
                    .is_none_or(|caller| method.customer_id == *caller)
                    && matches_search(&format!("{} {} {}", method.brand, method.last4, method.provider), &params)
                    && params
                        .filters
                        .get("customer_id")
                        .is_none_or(|customer| method.customer_id == *customer)
                    && params
                        .filters
                        .get("customer")
                        .is_none_or(|customer| method.customer_id == *customer)
                    && params
                        .filters
                        .get("provider")
                        .is_none_or(|provider| method.provider == *provider)
                    && params
                        .filters
                        .get("connection_id")
                        .is_none_or(|id| method.connection_id.to_string() == *id)
            })
            .collect();
        items.sort_by_key(|method| method.created_at);
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}


pub(crate) async fn get_payment_method(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentMethod> = (|| {
        let _tenant = require_tenant(&req)?;
        let method = methods_store()
            .get(&id.into_inner())
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::not_found("Payment method not found"))?;
        require_customer(&req, &method.customer_id, "payment method")?;
        Ok(method)
    })();
    json::respond(result, debug)
}


pub(crate) async fn delete_payment_method(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let id = id.into_inner();
        let method = methods_store()
            .get(&id)
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::not_found("Payment method not found"))?;
        require_customer(&req, &method.customer_id, "payment method")?;
        let in_use = subscriptions_store().iter().any(|entry| {
            entry.value().payment_method_id == Some(id) && entry.value().status == "active"
        });
        if in_use {
            return Err(UnifiedError::conflict(
                "Payment method is used by an active subscription; cancel the subscription first",
            ));
        }
        methods_store().remove(&id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}


pub(crate) async fn set_default_payment_method(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentMethod> = (|| {
        let _tenant = require_tenant(&req)?;
        let id = id.into_inner();
        let method = methods_store()
            .get(&id)
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::not_found("Payment method not found"))?;
        require_customer(&req, &method.customer_id, "payment method")?;
        for mut entry in methods_store().iter_mut() {
            if entry.value().customer_id == method.customer_id {
                entry.value_mut().is_default = entry.key() == &id;
            }
        }
        // Return the stored record, not the pre-update clone, so
        // `is_default` reflects the change.
        methods_store()
            .get(&id)
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::not_found("Payment method not found"))
    })();
    json::respond(result, debug)
}
