//! payments - handlers charges.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::reference_id;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::{Arc};
use uuid::Uuid;
use super::{events::*, card_validation::*, card_settings::*, limits::*, providers::*, types::*, reference_ids::*, credentials::*, records::*, adapter::*, dispatch::*, stripe::*, paypal::*, xendit::*, midtrans::*, hyperpay::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};



// ---------------------------------------------------------------------------
// Handlers: charges, transactions, refunds, voids, payouts.
// ---------------------------------------------------------------------------
/// Direct charge (no intent): create the charge at the provider,
/// record it, and emit the matching event. Idempotent on
/// (connection, idempotency key).
pub(crate) async fn create_charge(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    req: HttpRequest,
    body: web::Json<CreateChargeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentTransaction> = (|| async {
        let _tenant = require_tenant(&req)?;
        let input = body.into_inner();
        if input.customer_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("customer_id is required"));
        }
        require_customer(&req, &input.customer_id, "customer")?;
        validate_money(input.amount_minor, &input.currency)?;
        check_pending_checkout_amount(
            input.order_id.as_deref(),
            input.amount_minor,
            &input.currency,
        )?;
        let connection = find_connection(input.connection_id)?;
        let key = idempotency_key(input.idempotency_key.clone());
        if let Some(existing) = find_by_idempotency(connection.id, &key) {
            return Ok(replay_for_customer(existing, &input.customer_id)?);
        }
        if !connection.kind.needs_delegate()
            && !currencies_for(connection.kind).contains(&input.currency.as_str())
        {
            return Err(UnifiedError::bad_request(format!(
                "Currency \"{}\" is not supported by provider \"{}\"",
                input.currency,
                connection.kind.as_str()
            )));
        }
        let token = resolve_token(
            input.payment_method_token.as_deref(),
            input.payment_method_id,
            &input.customer_id,
        )?;
        // Batch 18 (S2): validate the card before any provider
        // call. Raw details on the request run in `full` mode; a
        // stored method id runs in `method` mode; an opaque token
        // alone skips the gate.
        request_card_validation(&state, &req, input.card.as_ref(), input.payment_method_id, None, &connection, &input.customer_id, input.order_id.as_deref(), None)?;
        let adapter = adapter_for(&connection)?;
        // Claim the idempotency key BEFORE the provider call: a
        // concurrent duplicate with the same key waits for this
        // request's transaction instead of charging twice. The
        // claim is released only when the provider call fails
        // before any transaction is recorded; a recorded
        // transaction overwrites the claim.
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
        let charge_input = ChargeInput {
            amount_minor: input.amount_minor,
            currency: input.currency.clone(),
            customer_id: input.customer_id.clone(),
            order_id: input.order_id.clone(),
            reference_id: pending_checkout_reference(input.order_id.as_deref()),
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
        };
        let result = match adapter.charge(&charge_input).await {
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
            input.order_id.clone(),
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
            "order_id": input.order_id,
            "invoice_id": input.invoice_id,
            "amount_minor": input.amount_minor,
            "currency": input.currency,
            "fee_minor": result.fee_minor,
            "status": transaction.status,
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
                    &format!(
                        "Your payment of {} {} could not be completed.",
                        input.amount_minor, input.currency
                    ),
                );
            }
            _ => {}
        }
        if result.status == PaymentStatus::Failed {
            return Err(UnifiedError::bad_request(format!(
                "Provider \"{}\" declined the payment",
                connection.kind.as_str()
            ))
            .with_metadata("provider", Value::String(connection.kind.as_str().to_string()))
            .with_metadata("transaction_id", Value::String(transaction.id.to_string())));
        }
        Ok(transaction)
    })()
    .await;
    match result {
        Ok(transaction) => {
            let status = charge_success_status(&transaction.status);
            json::success_response(transaction, status)
        }
        Err(error) => json::error_response(&error, debug),
    }
}


pub(crate) async fn list_transactions(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<PaymentTransaction>> = (|| {
        // Lists are scoped to the caller: an admin sees every
        // customer's transactions, anyone else sees only their own.
        let _tenant = require_tenant(&req)?;
        let scope = list_customer_scope(&ctx, &req)?;
        let from = params
            .filters
            .get("from")
            .and_then(|value| parse_date_filter(value));
        let to = params
            .filters
            .get("to")
            .and_then(|value| parse_date_filter(value));
        let mut items: Vec<PaymentTransaction> = transactions_store()
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|transaction| {
                scope
                    .as_ref()
                    .is_none_or(|caller| transaction.customer_id == *caller)
                    && matches_search(
                        &format!(
                            "{} {} {} {}",
                            transaction.provider_transaction_id,
                            transaction.provider,
                            transaction.customer_id,
                            transaction.order_id.clone().unwrap_or_default()
                        ),
                        &params,
                    ) && params
                    .filters
                    .get("provider")
                    .is_none_or(|provider| transaction.provider == *provider)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| transaction.status == *status)
                    && params
                        .filters
                        .get("currency")
                        .is_none_or(|currency| transaction.currency == *currency)
                    && params
                        .filters
                        .get("customer")
                        .is_none_or(|customer| transaction.customer_id == *customer)
                    && params
                        .filters
                        .get("customer_id")
                        .is_none_or(|customer| transaction.customer_id == *customer)
                    && params
                        .filters
                        .get("order_id")
                        .is_none_or(|order_id| transaction.order_id.as_deref() == Some(order_id.as_str()))
                    && params
                        .filters
                        .get("invoice_id")
                        .is_none_or(|invoice_id| transaction.invoice_id.as_deref() == Some(invoice_id.as_str()))
                    && params
                        .filters
                        .get("kind")
                        .is_none_or(|kind| transaction.kind == *kind)
                    && from.is_none_or(|from| transaction.created_at >= from)
                    && to.is_none_or(|to| transaction.created_at <= to)
            })
            .collect();
        let sort = params.allowed_sort(&["created_at", "amount_minor"], "created_at");
        match sort.as_str() {
            "amount_minor" => items.sort_by_key(|transaction| transaction.amount_minor),
            _ => items.sort_by_key(|transaction| transaction.created_at),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}


pub(crate) async fn get_transaction(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentTransaction> = (|| {
        let _tenant = require_tenant(&req)?;
        let transaction = transactions_store()
            .get(&id.into_inner())
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::not_found("Payment transaction not found"))?;
        require_customer(&req, &transaction.customer_id, "transaction")?;
        Ok(transaction)
    })();
    json::respond(result, debug)
}


/// Wait briefly for an in-flight (pending) refund row to settle,
/// then return its latest state. A duplicate that arrives while
/// the first refund is still at the provider gets the settled
/// result instead of refunding twice. Returns `None` when the row
/// was discarded — the provider call failed, so the caller may
/// attempt the refund itself.
async fn wait_for_refund_settled(id: Uuid) -> Option<PaymentTransaction> {
    for _ in 0..50 {
        match transactions_store().get(&id).map(|entry| entry.clone()) {
            Some(transaction) if transaction.status != "pending" => return Some(transaction),
            Some(_) => {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            None => return None,
        }
    }
    transactions_store()
        .get(&id)
        .map(|entry| entry.clone())
        .filter(|transaction| transaction.status != "pending")
}


/// Drop a pending refund reservation after the provider call
/// failed: the row never happened, so it must stop reserving the
/// amount and stop answering replays for its key. The idempotency
/// entry is removed only while it still points at this row, so a
/// completed record is never deleted.
fn discard_pending_refund(connection_id: Uuid, key: &str, row_id: Uuid) {
    transactions_store().remove(&row_id);
    release_idempotency_claim(connection_id, key, row_id);
}


/// Refund a captured charge (or a charge waiting in
/// `refund_pending` for an earlier refund to finish).
///
/// The refund is only final when the provider says it is. A
/// `Refunded` / `PartiallyRefunded` result records the refund,
/// moves the original to `refunded` / `partially_refunded` from
/// the cumulative completed refunds, and emits `payment.refunded`
/// with an explicit `status` so the store and billing listeners
/// can gate on it. A `Pending` result (bank-transfer refunds)
/// records the refund with status `pending`, moves the original
/// to `refund_pending`, and emits NOTHING — the order and invoice
/// stay un-refunded until `complete_refund` closes it. Any other
/// result (failed, ...) records nothing and returns an error, so
/// a failed refund can never mark an order refunded.
pub(crate) async fn refund_transaction(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<CreateRefundRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentTransaction> = (|| async {
        let _tenant = require_tenant(&req)?;
        let id = id.into_inner();
        let request = body.into_inner();
        let original = transactions_store()
            .get(&id)
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::not_found("Payment transaction not found"))?;
        require_customer(&req, &original.customer_id, "transaction")?;
        if original.kind != "charge" && original.kind != "capture" {
            return Err(UnifiedError::unprocessable(format!(
                "Only a charge or capture can be refunded (transaction kind is \"{}\")",
                original.kind
            )));
        }
        if original.status != "captured"
            && original.status != "partially_refunded"
            && original.status != "refund_pending"
        {
            return Err(UnifiedError::unprocessable(format!(
                "Only a captured transaction can be refunded (status is \"{}\")",
                original.status
            )));
        }
        // Over-refund guard (preliminary fast fail; the
        // authoritative check runs under the admission lock below,
        // together with the reservation, so two concurrent refunds
        // cannot both spend the same remainder). An absent amount
        // refunds the remainder, never the full original amount.
        let already_refunded = refunded_amount_minor(original.id);
        let remaining = original.amount_minor - already_refunded;
        if remaining <= 0 {
            return Err(UnifiedError::unprocessable(
                "This transaction has already been fully refunded",
            ));
        }
        let refund_amount = request.amount_minor.unwrap_or(remaining);
        if refund_amount <= 0 || refund_amount > remaining {
            return Err(UnifiedError::bad_request(
                "Refund amount must be greater than zero and no more than the remaining refundable amount",
            ));
        }
        if has_unquantified_pending_refund(original.id) {
            return Err(UnifiedError::conflict(
                "A webhook refund with an unknown amount is still pending for this transaction; resolve it before refunding again",
            ));
        }
        let connection = find_connection(original.connection_id)?;
        let key = idempotency_key(request.idempotency_key.clone());
        if let Some(existing) = find_by_idempotency(connection.id, &key) {
            let existing = replay_for_customer(existing, &original.customer_id)?;
            // An in-flight refund with this key is still pending:
            // wait for it to settle instead of refunding twice.
            // A discarded row means the first attempt failed at
            // the provider, so this request may attempt the
            // refund itself.
            if existing.status == "pending" {
                if let Some(settled) = wait_for_refund_settled(existing.id).await {
                    return Ok(settled);
                }
            } else {
                return Ok(existing);
            }
        }
        // Claim the key BEFORE the provider call, so a concurrent
        // duplicate with the same key waits for this request's
        // refund instead of refunding twice.
        let claim = match claim_idempotency(connection.id, &key) {
            Some(sentinel) => sentinel,
            None => {
                if let Some(existing) = wait_for_idempotency(connection.id, &key).await {
                    let existing = replay_for_customer(existing, &original.customer_id)?;
                    if existing.status == "pending" {
                        if let Some(settled) = wait_for_refund_settled(existing.id).await {
                            return Ok(settled);
                        }
                    } else {
                        return Ok(existing);
                    }
                }
                return Err(UnifiedError::conflict(
                    "A refund with this idempotency key is already in progress",
                ));
            }
        };
        // Atomic over-refund guard + reservation, under one lock:
        // the remaining amount is recomputed and the pending refund
        // row is recorded together, so two concurrent refunds
        // cannot both spend the same remainder. The pending row is
        // the reservation: `refunded_amount_minor` counts pending
        // rows, so later refunds see the reserved amount. The row
        // is settled (or discarded) once the provider answers.
        // The adapter is resolved BEFORE the reservation: a
        // disabled connection must fail while nothing is reserved
        // yet, not leave a pending row and a held claim behind.
        let adapter = match adapter_for(&connection) {
            Ok(adapter) => adapter,
            Err(error) => {
                release_idempotency_claim(connection.id, &key, claim);
                return Err(error);
            }
        };
        let reservation = {
            let _admission = refund_admission_lock()
                .lock()
                .expect("refund admission lock poisoned");
            if has_unquantified_pending_refund(original.id) {
                release_idempotency_claim(connection.id, &key, claim);
                return Err(UnifiedError::conflict(
                    "A webhook refund with an unknown amount is still pending for this transaction; resolve it before refunding again",
                ));
            }
            let remaining = original.amount_minor - refunded_amount_minor(original.id);
            if remaining <= 0 {
                release_idempotency_claim(connection.id, &key, claim);
                return Err(UnifiedError::unprocessable(
                    "This transaction has already been fully refunded",
                ));
            }
            if refund_amount <= 0 || refund_amount > remaining {
                release_idempotency_claim(connection.id, &key, claim);
                return Err(UnifiedError::bad_request(
                    "Refund amount must be greater than zero and no more than the remaining refundable amount",
                ));
            }
            let mut reservation = new_transaction(
                connection.id,
                &original.provider,
                "refund",
                &original.customer_id,
                original.order_id.clone(),
                original.invoice_id.clone(),
                refund_amount,
                &original.currency,
                PaymentStatus::Pending,
                format!("pending-refund:{key}"),
                key.clone(),
            );
            reservation.subscription_id = original.subscription_id;
            reservation.original_transaction_id = Some(original.id);
            record_transaction(reservation)
        };
        let refund = match adapter
            .refund(&RefundInput {
                provider_transaction_id: original.provider_transaction_id.clone(),
                amount_minor: Some(refund_amount),
                currency: original.currency.clone(),
                reason: request.reason.clone(),
                idempotency_key: key.clone(),
            })
            .await
        {
            Ok(refund) => refund,
            Err(error) => {
                discard_pending_refund(connection.id, &key, reservation.id);
                return Err(error);
            }
        };
        let refund_status = refund.status;
        match refund_status {
            PaymentStatus::Refunded | PaymentStatus::PartiallyRefunded => {
                // Settle the reservation row instead of recording
                // a second row: it already reserves the amount and
                // holds the idempotency key.
                let refund_transaction = match transactions_store().get_mut(&reservation.id) {
                    Some(mut stored) => {
                        stored.status = refund_status.as_str().to_string();
                        stored.provider_transaction_id = refund.provider_refund_id.clone();
                        stored.provider_refund_id = Some(refund.provider_refund_id.clone());
                        stored.clone()
                    }
                    None => {
                        return Err(UnifiedError::internal(
                            "Refund reservation vanished before the provider answered",
                        ));
                    }
                };
                let completed = completed_refunded_amount_minor(original.id);
                let fully_refunded = completed >= original.amount_minor;
                if let Some(mut stored) = transactions_store().get_mut(&id) {
                    stored.status = if fully_refunded {
                        "refunded".to_string()
                    } else {
                        "partially_refunded".to_string()
                    };
                }
                state.emit(
                    PAYMENT_REFUNDED,
                    json!({
                        "transaction_id": refund_transaction.id,
                        "original_transaction_id": original.id,
                        "provider_transaction_id": original.provider_transaction_id,
                        "provider_refund_id": refund.provider_refund_id,
                        "provider": original.provider,
                        "connection_id": connection.id,
                        "customer_id": original.customer_id,
                        "order_id": original.order_id,
                        "invoice_id": original.invoice_id,
                        "amount_minor": refund_amount,
                        "currency": original.currency,
                        "status": if fully_refunded { "refunded" } else { "partially_refunded" },
                        "fully_refunded": fully_refunded,
                        "reason": request.reason,
                    }),
                );
                Ok(refund_transaction)
            }
            PaymentStatus::Pending => {
                // The reservation row already is the pending
                // refund: only attach the provider's refund id.
                let refund_transaction = match transactions_store().get_mut(&reservation.id) {
                    Some(mut stored) => {
                        stored.provider_transaction_id = refund.provider_refund_id.clone();
                        stored.provider_refund_id = Some(refund.provider_refund_id.clone());
                        stored.clone()
                    }
                    None => {
                        return Err(UnifiedError::internal(
                            "Refund reservation vanished before the provider answered",
                        ));
                    }
                };
                if let Some(mut stored) = transactions_store().get_mut(&id) {
                    stored.status = "refund_pending".to_string();
                }
                Ok(refund_transaction)
            }
            other => {
                discard_pending_refund(connection.id, &key, reservation.id);
                Err(UnifiedError::unprocessable(format!(
                    "Provider \"{}\" did not complete the refund (status is \"{}\"); nothing was recorded",
                    original.provider,
                    other.as_str()
                ))
                .with_metadata("provider", Value::String(original.provider.clone())))
            }
        }
    })()
    .await;
    match result {
        Ok(transaction) => json::success_response(transaction, 201),
        Err(error) => json::error_response(&error, debug),
    }
}


/// Complete a pending refund: the refund-completion path.
///
/// Takes the REFUND transaction id (kind `refund`, status
/// `pending`). `status` defaults to `refunded` and accepts
/// `refunded` or `failed`. On `refunded` the refund is marked
/// completed, the original charge's status is recomputed from the
/// cumulative completed refunds, and `payment.refunded` is emitted
/// with an explicit `status` so the store and billing listeners
/// move the order and invoice. On `failed` the refund is marked
/// failed and an original waiting in `refund_pending` returns to
/// `captured` (or `partially_refunded` when other refunds already
/// completed), with no refund event emitted. Refunds recorded by
/// the current refund path always carry their original link, so a
/// completed refund without one emits nothing.
pub(crate) async fn complete_refund(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<CompleteRefundRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentTransaction> = (|| {
        let _tenant = require_tenant(&req)?;
        let id = id.into_inner();
        let request = body.into_inner();
        let outcome = request
            .status
            .clone()
            .unwrap_or_else(|| "refunded".to_string());
        if outcome != "refunded" && outcome != "failed" {
            return Err(UnifiedError::bad_request(
                "status must be \"refunded\" or \"failed\"",
            ));
        }
        let refund_transaction = transactions_store()
            .get(&id)
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::not_found("Refund transaction not found"))?;
        require_customer(&req, &refund_transaction.customer_id, "refund")?;
        if refund_transaction.kind != "refund" {
            return Err(UnifiedError::unprocessable(format!(
                "Only a refund can be completed (transaction kind is \"{}\")",
                refund_transaction.kind
            )));
        }
        if refund_transaction.status != "pending" {
            return Err(UnifiedError::conflict(format!(
                "Only a pending refund can be completed (status is \"{}\")",
                refund_transaction.status
            )));
        }
        let original_id = refund_transaction.original_transaction_id;
        if outcome == "refunded" {
            if let Some(mut stored) = transactions_store().get_mut(&id) {
                stored.status = "refunded".to_string();
            }
            if let Some(original_id) = original_id {
                let original = transactions_store()
                    .get(&original_id)
                    .map(|entry| entry.clone());
                if let Some(original) = original {
                    let completed = completed_refunded_amount_minor(original_id);
                    let fully_refunded = completed >= original.amount_minor;
                    let new_status = if fully_refunded {
                        "refunded"
                    } else {
                        "partially_refunded"
                    };
                    if let Some(mut stored) = transactions_store().get_mut(&original_id) {
                        stored.status = new_status.to_string();
                    }
                    state.emit(
                        PAYMENT_REFUNDED,
                        json!({
                            "transaction_id": refund_transaction.id,
                            "original_transaction_id": original.id,
                            "provider_transaction_id": original.provider_transaction_id,
                            "provider_refund_id": refund_transaction.provider_refund_id,
                            "provider": refund_transaction.provider,
                            "connection_id": refund_transaction.connection_id,
                            "customer_id": refund_transaction.customer_id,
                            "order_id": refund_transaction.order_id,
                            "invoice_id": refund_transaction.invoice_id,
                            "amount_minor": refund_transaction.amount_minor,
                            "currency": refund_transaction.currency,
                            "status": if fully_refunded { "refunded" } else { "partially_refunded" },
                            "fully_refunded": fully_refunded,
                            "reason": request.reason,
                        }),
                    );
                }
            }
        } else {
            if let Some(mut stored) = transactions_store().get_mut(&id) {
                stored.status = "failed".to_string();
            }
            if let Some(original_id) = original_id {
                let was_waiting = transactions_store()
                    .get(&original_id)
                    .map(|entry| entry.value().status == "refund_pending")
                    .unwrap_or(false);
                if was_waiting {
                    let completed = completed_refunded_amount_minor(original_id);
                    if let Some(mut stored) = transactions_store().get_mut(&original_id) {
                        stored.status = if completed > 0 {
                            "partially_refunded".to_string()
                        } else {
                            "captured".to_string()
                        };
                    }
                }
            }
        }
        Ok(transactions_store()
            .get(&id)
            .map(|entry| entry.clone())
            .unwrap_or(refund_transaction))
    })();
    json::respond(result, debug)
}


/// Void an authorization (or an uncaptured charge) at the provider.
pub(crate) async fn void_transaction(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentTransaction> = (|| async {
        let _tenant = require_tenant(&req)?;
        let id = id.into_inner();
        let original = transactions_store()
            .get(&id)
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::not_found("Payment transaction not found"))?;
        require_customer(&req, &original.customer_id, "transaction")?;
        if original.kind != "authorize" && original.kind != "charge" {
            return Err(UnifiedError::unprocessable(format!(
                "Only an authorization or uncaptured charge can be voided (transaction kind is \"{}\")",
                original.kind
            )));
        }
        if original.status != "authorized" && original.status != "requires_confirmation" {
            return Err(UnifiedError::unprocessable(format!(
                "Only an authorized transaction can be voided (status is \"{}\")",
                original.status
            )));
        }
        let connection = find_connection(original.connection_id)?;
        let adapter = adapter_for(&connection)?;
        adapter.void(&original.provider_transaction_id).await?;
        let void_transaction = record_transaction(new_transaction(
            connection.id,
            &original.provider,
            "void",
            &original.customer_id,
            original.order_id.clone(),
            original.invoice_id.clone(),
            original.amount_minor,
            &original.currency,
            PaymentStatus::Voided,
            original.provider_transaction_id.clone(),
            Uuid::now_v7().to_string(),
        ));
        if let Some(mut stored) = transactions_store().get_mut(&id) {
            stored.status = "voided".to_string();
        }
        Ok(void_transaction)
    })()
    .await;
    match result {
        Ok(transaction) => json::success_response(transaction, 201),
        Err(error) => json::error_response(&error, debug),
    }
}


/// Pay money out through a provider that supports payouts.
/// Admin-only: moving money out to an arbitrary destination must
/// never be reachable by an unauthenticated caller. The operator
/// identity is recorded on the payout transaction.
pub(crate) async fn create_payout(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreatePayoutRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentTransaction> = (|| async {
        let _tenant = require_tenant(&req)?;
        let operator = require_payments_admin(&ctx, &req)?;
        let input = body.into_inner();
        validate_money(input.amount_minor, &input.currency)?;
        let connection = find_connection(input.connection_id)?;
        let key = idempotency_key(input.idempotency_key.clone());
        if let Some(existing) = find_by_idempotency(connection.id, &key) {
            return Ok(existing);
        }
        let adapter = adapter_for(&connection)?;
        // Claim the idempotency key BEFORE the provider call: a
        // concurrent duplicate with the same key waits for this
        // request's payout instead of paying out twice.
        let claim = match claim_idempotency(connection.id, &key) {
            Some(sentinel) => sentinel,
            None => {
                if let Some(existing) = wait_for_idempotency(connection.id, &key).await {
                    return Ok(existing);
                }
                return Err(UnifiedError::conflict(
                    "A request with this idempotency key is already in progress",
                ));
            }
        };
        let payout = match adapter
            .payout(&PayoutInput {
                amount_minor: input.amount_minor,
                currency: input.currency.clone(),
                destination_account: input.destination_account.clone(),
                destination_type: input.destination_type.clone(),
                description: input.description.clone(),
                idempotency_key: key.clone(),
            })
            .await
        {
            Ok(payout) => payout,
            Err(error) => {
                release_idempotency_claim(connection.id, &key, claim);
                return Err(error);
            }
        };
        let mut transaction = new_transaction(
            connection.id,
            connection.kind.as_str(),
            "payout",
            &input.destination_account,
            None,
            None,
            input.amount_minor,
            &input.currency,
            payout.status,
            payout.provider_payout_id.clone(),
            key,
        );
        transaction.operator_id = Some(operator);
        let transaction = record_transaction(transaction);
        // A payout is an outgoing movement; there is no order or
        // invoice to notify about, so no payment event is emitted.
        Ok(transaction)
    })()
    .await;
    match result {
        Ok(transaction) => json::success_response(transaction, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

#[cfg(test)]
mod tests {

    /// Attach the middleware-resolved tenant context the
    /// tenant-resolution middleware would attach, so the
    /// fail-closed tenant gate (`require_tenant`) passes and the
    /// test reaches the handler's own auth checks.
    fn attach_tenant_context(req: HttpRequest) -> HttpRequest {
        use actix_web::HttpMessage;
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: String::new(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req
    }

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        let req = attach_tenant_context(req);
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        let req = attach_tenant_context(req);
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    use super::*;
    use unified_core::config::AppConfig;
    use unified_core::hooks::{HookBus, HookContext};
    use unified_core::modules::ModuleRegistry;
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    fn allowing_context() -> AppContext {
        let ctx = test_context();
        ctx.hooks.register(
            "role.check",
            "test",
            Arc::new(|context: &mut HookContext| {
                context.set("allowed", Value::Bool(true));
            }),
        );
        ctx
    }

    fn authed_request() -> HttpRequest {
        verified_request("op-1", "admin")
    }

    fn data(ctx: AppContext) -> web::Data<Arc<AppContext>> {
        web::Data::new(Arc::new(ctx))
    }

    fn payout_body() -> web::Json<CreatePayoutRequest> {
        web::Json(CreatePayoutRequest {
            connection_id: Uuid::now_v7(),
            amount_minor: 1000,
            currency: "USD".to_string(),
            destination_account: "acct-victim".to_string(),
            destination_type: None,
            description: None,
            idempotency_key: None,
        })
    }

    #[tokio::test]
    async fn create_payout_rejects_callers_without_the_admin_capability() {
        // No identity at all: refused before any provider is touched.
        let response = create_payout(
            data(test_context()),
            TestRequest::default().to_http_request(),
            payout_body(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        // Identity headers present, but no `role.check` handler
        // answers: the payout gate fails closed.
        let response = create_payout(data(test_context()), authed_request(), payout_body()).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        // An admin passes the gate: the unknown connection id is now
        // what fails (404), not the missing capability.
        let response = create_payout(data(allowing_context()), authed_request(), payout_body()).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn list_transactions_is_scoped_to_the_caller() {
        let customer_a = format!("p4-a-{}", Uuid::now_v7());
        let customer_b = format!("p4-b-{}", Uuid::now_v7());
        let connection_id = Uuid::now_v7();
        let transaction_a = record_transaction(new_transaction(
            connection_id,
            "stripe",
            "charge",
            &customer_a,
            None,
            None,
            1000,
            "USD",
            PaymentStatus::Captured,
            "prov-p4-a".to_string(),
            format!("p4-a-{}", Uuid::now_v7()),
        ));
        let transaction_b = record_transaction(new_transaction(
            connection_id,
            "stripe",
            "charge",
            &customer_b,
            None,
            None,
            2000,
            "USD",
            PaymentStatus::Captured,
            "prov-p4-b".to_string(),
            format!("p4-b-{}", Uuid::now_v7()),
        ));
        // No identity: the list is refused outright.
        let response = list_transactions(
            data(test_context()),
            TestRequest::default().to_http_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        // Customer A sees only their own record, never B's.
        let request = verified_request_without_roles(customer_a.clone());
        let response = list_transactions(
            data(test_context()),
            request,
            web::Query(PaginationParams {
                per_page: Some(100),
                ..Default::default()
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = String::from_utf8(
            actix_web::body::to_bytes(response.into_body())
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(body.contains(&customer_a));
        assert!(!body.contains(&customer_b));
        // An admin sees both.
        let response = list_transactions(
            data(allowing_context()),
            authed_request(),
            web::Query(PaginationParams {
                per_page: Some(100),
                ..Default::default()
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = String::from_utf8(
            actix_web::body::to_bytes(response.into_body())
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(body.contains(&customer_a));
        assert!(body.contains(&customer_b));
        transactions_store().remove(&transaction_a.id);
        transactions_store().remove(&transaction_b.id);
    }
}
