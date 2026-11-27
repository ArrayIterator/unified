//! payments - handlers checkout.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::reference_id;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::{Arc};
use super::{events::*, card_validation::*, card_settings::*, limits::*, providers::*, types::*, reference_ids::*, credentials::*, records::*, adapter::*, dispatch::*, stripe::*, paypal::*, xendit::*, midtrans::*, hyperswitch::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};



// Handlers: store checkout through the gateway.
/// Pay for a store order through the gateway: charge once, record the
/// transaction, and emit `payment.captured` so the store can mark the
/// order paid / completed and billing can record the payment. The
/// store itself owns the order; this endpoint takes the amount the
/// caller computed for the order (the store's checkout total).
pub(crate) async fn pay_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    req: HttpRequest,
    order_id: web::Path<String>,
    body: web::Json<PayOrderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentTransaction> = (|| async {
        let _tenant = require_tenant(&req)?;
        let order_id = order_id.into_inner();
        if order_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("order_id is required"));
        }
        let input = body.into_inner();
        if input.customer_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("customer_id is required"));
        }
        require_customer(&req, &input.customer_id, "customer")?;
        validate_money(input.amount_minor, &input.currency)?;
        check_pending_checkout_amount(
            Some(order_id.as_str()),
            input.amount_minor,
            &input.currency,
        )?;
        let connection = find_connection(input.connection_id)?;
        let key = idempotency_key(input.idempotency_key.clone());
        if let Some(existing) = find_by_idempotency(connection.id, &key) {
            return Ok(replay_for_customer(existing, &input.customer_id)?);
        }
        let token = resolve_token(
            input.payment_method_token.as_deref(),
            input.payment_method_id,
            &input.customer_id,
        )?;
        // Batch 18 (S3): validate the card before any provider
        // call. Raw details on the request run in `full` mode; a
        // stored method id runs in `method` mode; an opaque token
        // alone skips the gate.
        request_card_validation(&state, &req, input.card.as_ref(), input.payment_method_id, None, &connection, &input.customer_id, Some(order_id.as_str()), None)?;
        let adapter = adapter_for(&connection)?;
        // Claim the idempotency key BEFORE the provider call: a
        // concurrent duplicate with the same key waits for this
        // request's transaction instead of charging twice.
        let claim = match claim_idempotency(connection.id, &key) {
            Some(sentinel) => sentinel,
            None => {
                if let Some(existing) = wait_for_idempotency(connection.id, &key).await {
                    return Ok(replay_for_customer(existing, &input.customer_id)?);
                }
                return Err(UnifiedError::conflict(
                    "A request with this idempotency key is already in progress",
                ));
            }
        };
        let result = match adapter
            .charge(&ChargeInput {
                amount_minor: input.amount_minor,
                currency: input.currency.clone(),
                customer_id: input.customer_id.clone(),
                order_id: Some(order_id.clone()),
                reference_id: pending_checkout_reference(Some(order_id.as_str())),
                invoice_id: input.invoice_id.clone(),
                description: input.description.clone(),
                payment_method_token: token,
                idempotency_key: key.clone(),
                three_ds: input.three_ds,
                billing: input.billing.map(|billing| BillingDetails {
                    name: billing.name,
                    email: billing.email,
                    country: billing.country,
                    city: billing.city,
                    address: billing.address,
                }),
            })
            .await
        {
            Ok(result) => result,
            Err(error) => {
                release_idempotency_claim(connection.id, &key, claim);
                return Err(error);
            }
        };
        let mut transaction = new_transaction(
            connection.id,
            connection.kind.as_str(),
            "charge",
            &input.customer_id,
            Some(order_id.clone()),
            input.invoice_id.clone(),
            input.amount_minor,
            &input.currency,
            result.status,
            result.provider_transaction_id.clone(),
            key,
        );
        transaction.fee_minor = result.fee_minor;
        let transaction = record_transaction(transaction);
        let payload = json!({
            "transaction_id": transaction.id,
            "provider_transaction_id": result.provider_transaction_id,
            "provider": connection.kind.as_str(),
            "connection_id": connection.id,
            "customer_id": input.customer_id,
            "order_id": order_id,
            "invoice_id": input.invoice_id,
            "amount_minor": input.amount_minor,
            "currency": input.currency,
            "fee_minor": result.fee_minor,
            "status": transaction.status,
            "pending_checkout": pending_checkouts_store().contains_key(&order_id),
        });
        match result.status {
            PaymentStatus::Captured => state.emit(PAYMENT_CAPTURED, payload),
            PaymentStatus::Failed => {
                state.emit(PAYMENT_FAILED, payload);
                notify(
                    &state,
                    &input.customer_id,
                    "payment_failed",
                    "Payment failed",
                    &format!("Your payment for order {order_id} could not be completed."),
                );
            }
            _ => {}
        }
        if result.status == PaymentStatus::Failed {
            return Err(UnifiedError::bad_request(format!(
                "Provider \"{}\" declined the payment for order {order_id}",
                connection.kind.as_str()
            ))
            .with_metadata("provider", Value::String(connection.kind.as_str().to_string()))
            .with_metadata("transaction_id", Value::String(transaction.id.to_string())));
        }
        Ok(transaction)
    })()
    .await;
    match result {
        // Captured payments are 201; a recorded but not yet
        // captured outcome (pending, authorized) is 202, so
        // callers can tell "paid" from "pending" by status code.
        Ok(transaction) => {
            let status = charge_success_status(&transaction.status);
            json::success_response(transaction, status)
        }
        Err(error) => json::error_response(&error, debug),
    }
}
