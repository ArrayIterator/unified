//! payments - handlers split bills.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::reference_id;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{Utc};
use serde_json::{json, Value};
use std::sync::{Arc};
use std::time::Duration;
use uuid::Uuid;
use super::{events::*, card_validation::*, card_settings::*, limits::*, providers::*, types::*, reference_ids::*, credentials::*, records::*, adapter::*, dispatch::*, stripe::*, paypal::*, xendit::*, midtrans::*, hyperpay::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, hooks::*, module::*};



// ---------------------------------------------------------------------------
// Handlers: split bills.
// ---------------------------------------------------------------------------
/// Recompute the paid / remaining amounts of a split from its parts.
pub(crate) fn recompute_split_totals(split: &mut SplitBill) {
    split.paid_minor = split
        .parts
        .iter()
        .filter(|part| part.status == "paid")
        .map(|part| part.amount_minor)
        .sum();
    split.remaining_minor = (split.total_minor - split.paid_minor).max(0);
    split.updated_at = Utc::now();
}


pub(crate) fn find_split(id: Uuid) -> UnifiedResult<SplitBill> {
    splits_store()
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Split bill \"{id}\" not found")))
}


/// Validate a create-split request and build the record. The parts
/// must sum exactly to the total: a mismatch is a 422 whose metadata
/// carries the total, the parts sum, the difference, and the per-part
/// amounts, so the caller can see which part is wrong.
///
/// Note: the caller supplies `total_minor` (the order/invoice total
/// in minor units). This module cannot read the store or billing
/// totals directly — hook wiring only — so the integrator should
/// validate the total against the order/invoice when the durable
/// layer lands (see the coordinator report).
pub(crate) fn build_split_bill(input: CreateSplitRequest) -> UnifiedResult<SplitBill> {
    let customer_id = input.customer_id.trim().to_string();
    if customer_id.is_empty() {
        return Err(UnifiedError::bad_request("customer_id is required"));
    }
    if input.total_minor <= 0 {
        return Err(UnifiedError::bad_request(
            "Split total must be greater than zero (integer minor units)",
        ));
    }
    if input.total_minor > MAX_AMOUNT_MINOR {
        return Err(UnifiedError::bad_request(
            "Split total exceeds the maximum accepted for one charge",
        ));
    }
    let currency = validate_currency(&input.currency)?;
    let order_id = input
        .order_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let invoice_id = input
        .invoice_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if order_id.is_none() && invoice_id.is_none() {
        return Err(UnifiedError::bad_request(
            "A split needs order_id or invoice_id",
        ));
    }
    if input.parts.len() < 2 {
        return Err(UnifiedError::unprocessable(
            "A split needs at least two parts",
        ));
    }
    let split_id = Uuid::now_v7();
    let mut parts: Vec<SplitPart> = Vec::with_capacity(input.parts.len());
    let mut parts_sum: i64 = 0;
    for (index, part_input) in input.parts.iter().enumerate() {
        if part_input.amount_minor <= 0 {
            return Err(UnifiedError::bad_request(format!(
                "Split part {index} amount must be greater than zero"
            )));
        }
        let provider = part_input
            .provider
            .clone()
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        let kind = PaymentProviderKind::from_str(&provider).ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Split part {index} has unknown provider \"{provider}\""
            ))
        })?;
        let payer_id = part_input
            .payer_id
            .clone()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| customer_id.clone());
        parts_sum += part_input.amount_minor;
        parts.push(SplitPart {
            id: Uuid::now_v7(),
            split_id,
            index,
            amount_minor: part_input.amount_minor,
            currency: currency.clone(),
            provider: kind.as_str().to_string(),
            connection_id: part_input.connection_id,
            payer_id,
            due_date: part_input.due_date,
            status: "pending".to_string(),
            paid_transaction_id: None,
            paid_at: None,
        });
    }
    if parts_sum != input.total_minor {
        return Err(UnifiedError::unprocessable(format!(
            "Split parts sum to {parts_sum} {currency} but the total is {} {currency}",
            input.total_minor
        ))
        .with_metadata("total_minor", Value::from(input.total_minor))
        .with_metadata("parts_sum_minor", Value::from(parts_sum))
        .with_metadata(
            "difference_minor",
            Value::from(input.total_minor - parts_sum),
        )
        .with_metadata(
            "parts",
            Value::Array(
                parts
                    .iter()
                    .map(|part| {
                        json!({
                            "index": part.index,
                            "amount_minor": part.amount_minor,
                            "provider": part.provider.clone(),
                            "payer_id": part.payer_id.clone(),
                        })
                    })
                    .collect(),
            ),
        ));
    }
    let now = Utc::now();
    let mut split = SplitBill {
        id: split_id,
        order_id,
        invoice_id,
        customer_id,
        total_minor: input.total_minor,
        currency,
        status: "active".to_string(),
        parts,
        paid_minor: 0,
        remaining_minor: input.total_minor,
        created_at: now,
        updated_at: now,
    };
    recompute_split_totals(&mut split);
    Ok(split)
}


pub(crate) async fn create_split(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    req: HttpRequest,
    body: web::Json<CreateSplitRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SplitBill> = (|| {
        let _tenant = require_tenant(&req)?;
        let input = body.into_inner();
        // The caller must own the customer the split is created
        // for: anyone could otherwise create splits (and later
        // charges) against another customer's order.
        require_customer(&req, input.customer_id.trim(), "split")?;
        let split = build_split_bill(input)?;
        splits_store().insert(split.id, split.clone());
        state.emit(
            PAYMENT_SPLIT_CREATED,
            json!({
                "split_id": split.id,
                "order_id": split.order_id.clone(),
                "invoice_id": split.invoice_id.clone(),
                "customer_id": split.customer_id.clone(),
                "total_minor": split.total_minor,
                "currency": split.currency.clone(),
                "split": serde_json::to_value(&split).unwrap_or(Value::Null),
            }),
        );
        Ok(split)
    })();
    match result {
        Ok(split) => json::success_response(split, 201),
        Err(error) => json::error_response(&error, debug),
    }
}


pub(crate) async fn list_splits(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<SplitBill>> = (|| {
        // Lists are scoped to the caller: an admin sees every
        // customer's splits, anyone else sees only their own.
        let _tenant = require_tenant(&req)?;
        let scope = list_customer_scope(&ctx, &req)?;
        let pagination = params.normalized();
        let mut items: Vec<SplitBill> = splits_store()
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|split| {
                scope
                    .as_ref()
                    .is_none_or(|caller| split.customer_id == *caller)
                    && matches_search(
                        &format!(
                            "{} {} {}",
                            split.customer_id,
                            split.order_id.clone().unwrap_or_default(),
                            split.invoice_id.clone().unwrap_or_default()
                        ),
                        &params,
                    ) && params
                    .filters
                    .get("order_id")
                    .is_none_or(|value| split.order_id.as_deref() == Some(value.as_str()))
                    && params.filters.get("invoice_id").is_none_or(|value| {
                        split.invoice_id.as_deref() == Some(value.as_str())
                    })
                    && params.filters.get("customer_id").is_none_or(|value| {
                        split.customer_id == *value
                    })
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|value| split.status == *value)
                    && params.filters.get("provider").is_none_or(|value| {
                        split.parts.iter().any(|part| part.provider == *value)
                    })
                    && params.filters.get("currency").is_none_or(|value| {
                        split.currency.eq_ignore_ascii_case(value)
                    })
            })
            .collect();
        let sort = params.allowed_sort(&["created_at", "total_minor"], "created_at");
        match sort.as_str() {
            "total_minor" => items.sort_by_key(|split| split.total_minor),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if pagination.order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}


pub(crate) async fn get_split(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SplitBill> = (|| {
        let _tenant = require_tenant(&req)?;
        let split = find_split(id.into_inner())?;
        require_customer(&req, &split.customer_id, "split")?;
        Ok(split)
    })();
    json::respond(result, debug)
}


pub(crate) async fn pay_split_part(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
    body: web::Json<PaySplitPartRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let _tenant = require_tenant(&req)?;
        let (split_id, part_id) = path.into_inner();
        let request = body.into_inner();
        let split = find_split(split_id)?;
        if split.status != "active" {
            return Err(UnifiedError::conflict(format!(
                "Split is {} and cannot be paid",
                split.status
            )));
        }
        let part = split
            .parts
            .iter()
            .find(|part| part.id == part_id)
            .cloned()
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Split part \"{part_id}\" not found"))
            })?;
        if part.status == "paid" {
            return Err(UnifiedError::conflict("Split part is already paid"));
        }
        if part.status != "pending" {
            return Err(UnifiedError::unprocessable(format!(
                "Split part in status \"{}\" cannot be paid",
                part.status
            )));
        }
        // The caller must own this part (as its payer) or the
        // split itself. The request body's `customer_id` is never
        // trusted to pick whose stored method gets charged: without
        // this, supplying a victim's customer id plus their stored
        // method id would charge the victim.
        require_customer_any(
            &req,
            &[part.payer_id.as_str(), split.customer_id.as_str()],
            "split part",
        )?;
        if part.provider == "bank_transfer" {
            // No gateway charge: ask the bank-transfer module to open
            // a manual verification for this part. The module listens
            // to `bank_transfer.proof.requested` on the hook bus and
            // creates the verification; the administrator approval
            // then emits `payment.captured` with the split context,
            // which marks this part paid (see
            // `on_payment_captured_for_split`).
            state.emit(
                "bank_transfer.proof.requested",
                json!({
                    "split_id": split_id,
                    "part_id": part_id,
                    "order_id": split.order_id,
                    "invoice_id": split.invoice_id,
                    "customer_id": part.payer_id,
                    "amount_minor": part.amount_minor,
                    "currency": part.currency,
                    "provider": "bank_transfer",
                }),
            );
            return Ok(json!({
                "split_id": split_id,
                "part_id": part_id,
                "status": "pending",
                "provider": "bank_transfer",
                "next_step": "Transfer the amount to a configured bank account, then upload the payment proof with POST /api/v1/bank-transfer/verifications/{id}/proof. Find the verification with GET /api/v1/bank-transfer/verifications?split_id=<split_id>.",
            }));
        }
        let connection_id = request.connection_id.or(part.connection_id).ok_or_else(|| {
            UnifiedError::bad_request(
                "connection_id is required to pay this split part (pass it in the request or store it on the part)",
            )
        })?;
        let connection = find_connection(connection_id)?;
        // The payer of the part is the customer being charged.
        // A caller-supplied `customer_id` is ignored here: the
        // ownership check above already established who may pay
        // this part, and letting the body override the payer would
        // reopen the cross-customer charge.
        let customer_id = part.payer_id.clone();
        let key = idempotency_key(request.idempotency_key.clone());
        if let Some(existing) = find_by_idempotency(connection.id, &key) {
            let existing = replay_for_customer(existing, &customer_id)?;
            return Ok(json!({
                "split_id": split_id,
                "part_id": part_id,
                "transaction": existing,
                "status": "already_recorded",
            }));
        }
        let token = resolve_token(
            request.payment_method_token.as_deref(),
            request.payment_method_id,
            &customer_id,
        )?;
        // Batch 18 (S4): validate the card before any provider
        // call. The bank-transfer early return above stays before
        // the gate — no card is involved there. Raw details on the
        // request run in `full` mode; a stored method id runs in
        // `method` mode; an opaque token alone skips the gate.
        request_card_validation(&state, &req, request.card.as_ref(), request.payment_method_id, None, &connection, &customer_id, split.order_id.as_deref(), None)?;
        let adapter = adapter_for(&connection)?;
        // Claim the idempotency key BEFORE the provider call: a
        // concurrent duplicate payment of the same part with the
        // same key waits for this request's transaction instead
        // of charging twice.
        let claim = match claim_idempotency(connection.id, &key) {
            Some(sentinel) => sentinel,
            None => {
                if let Some(existing) = wait_for_idempotency(connection.id, &key).await {
                    let existing = replay_for_customer(existing, &customer_id)?;
                    return Ok(json!({
                        "split_id": split_id,
                        "part_id": part_id,
                        "transaction": existing,
                        "status": "already_recorded",
                    }));
                }
                return Err(UnifiedError::conflict(
                    "A payment with this idempotency key is already in progress",
                ));
            }
        };
        let charge = match adapter
            .charge(&ChargeInput {
                amount_minor: part.amount_minor,
                currency: part.currency.clone(),
                customer_id: customer_id.clone(),
                order_id: split.order_id.clone(),
                reference_id: None,
                invoice_id: split.invoice_id.clone(),
                description: request.description.clone(),
                payment_method_token: token,
                idempotency_key: key.clone(),
                three_ds: request.three_ds,
                billing: None,
            })
            .await
        {
            Ok(charge) => charge,
            Err(error) => {
                release_idempotency_claim(connection.id, &key, claim);
                return Err(error);
            }
        };
        let mut transaction = new_transaction(
            connection.id,
            connection.kind.as_str(),
            "split_part_charge",
            &customer_id,
            split.order_id.clone(),
            split.invoice_id.clone(),
            part.amount_minor,
            &part.currency,
            charge.status,
            charge.provider_transaction_id.clone(),
            key,
        );
        transaction.fee_minor = charge.fee_minor;
        let transaction = record_transaction(transaction);
        let payload = json!({
            "transaction_id": transaction.id,
            "provider_transaction_id": charge.provider_transaction_id,
            "provider": connection.kind.as_str(),
            "connection_id": connection.id,
            "customer_id": customer_id,
            "order_id": split.order_id,
            "invoice_id": split.invoice_id,
            "amount_minor": part.amount_minor,
            "currency": part.currency,
            "fee_minor": charge.fee_minor,
            "status": transaction.status,
            "split_id": split_id,
            "part_id": part_id,
        });
        match charge.status {
            PaymentStatus::Captured => state.emit(PAYMENT_CAPTURED, payload),
            PaymentStatus::Failed => {
                state.emit(PAYMENT_FAILED, payload);
                if let Some(mut entry) = splits_store().get_mut(&split_id) {
                    if let Some(failed) =
                        entry.parts.iter_mut().find(|part| part.id == part_id)
                    {
                        failed.status = "failed".to_string();
                    }
                    entry.updated_at = Utc::now();
                }
                notify(
                    &state,
                    &customer_id,
                    "payment_failed",
                    "Split payment failed",
                    &format!(
                        "Part {} of split {split_id} could not be paid.",
                        part.index + 1
                    ),
                );
                return Err(UnifiedError::bad_request(format!(
                    "Provider \"{}\" declined the payment for split part {part_id}",
                    connection.kind.as_str()
                ))
                .with_metadata(
                    "provider",
                    Value::String(connection.kind.as_str().to_string()),
                )
                .with_metadata(
                    "transaction_id",
                    Value::String(transaction.id.to_string()),
                ));
            }
            _ => {}
        }
        // The `payment.captured` listener marks the part paid
        // synchronously, so re-reading the split shows the new state.
        let updated = find_split(split_id)?;
        Ok(json!({ "split": updated, "transaction": transaction }))
    })()
    .await;
    json::respond(result, debug)
}


pub(crate) async fn cancel_split(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    req: HttpRequest,
    path: web::Path<Uuid>,
    body: Option<web::Json<CancelSplitRequest>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SplitBill> = (|| {
        let _tenant = require_tenant(&req)?;
        let id = path.into_inner();
        let reason = body.and_then(|body| body.into_inner().reason);
        let mut split = find_split(id)?;
        require_customer(&req, &split.customer_id, "split")?;
        if split.status != "active" {
            return Err(UnifiedError::conflict(format!(
                "Split is {} and cannot be cancelled",
                split.status
            )));
        }
        if split.parts.iter().any(|part| part.status == "paid") {
            return Err(UnifiedError::conflict(
                "Cannot cancel a split with paid parts",
            ));
        }
        split.status = "cancelled".to_string();
        for part in split.parts.iter_mut() {
            if part.status == "pending" || part.status == "failed" {
                part.status = "cancelled".to_string();
            }
        }
        recompute_split_totals(&mut split);
        splits_store().insert(id, split.clone());
        state.emit(
            PAYMENT_SPLIT_CANCELLED,
            json!({
                "split_id": id,
                "order_id": split.order_id.clone(),
                "invoice_id": split.invoice_id.clone(),
                "customer_id": split.customer_id.clone(),
                "reason": reason,
            }),
        );
        Ok(split)
    })();
    json::respond(result, debug)
}


pub(crate) async fn generate_schedule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    req: HttpRequest,
    body: web::Json<GenerateScheduleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let input = body.into_inner();
        if input.total_minor <= 0 {
            return Err(UnifiedError::bad_request(
                "Schedule total must be greater than zero (integer minor units)",
            ));
        }
        if input.total_minor > MAX_AMOUNT_MINOR {
            return Err(UnifiedError::bad_request(
                "Schedule total exceeds the maximum accepted for one charge",
            ));
        }
        let currency = validate_currency(&input.currency)?;
        let customer_id = input.customer_id.trim().to_string();
        if customer_id.is_empty() {
            return Err(UnifiedError::bad_request("customer_id is required"));
        }
        // The schedule may create a split bill for this customer
        // (see `create` below), so the caller must own the
        // customer named here, exactly like `create_split`.
        require_customer(&req, &customer_id, "customer")?;
        if input.parts_count < 2 || input.parts_count > 60 {
            return Err(UnifiedError::unprocessable(
                "parts_count must be between 2 and 60",
            ));
        }
        let interval_days = input.interval_days.unwrap_or(30);
        if interval_days == 0 || interval_days > 365 {
            return Err(UnifiedError::unprocessable(
                "interval_days must be between 1 and 365",
            ));
        }
        let count = input.parts_count as usize;
        let amounts: Vec<i64> = match &input.amounts {
            Some(custom) => {
                if custom.len() != count {
                    return Err(UnifiedError::unprocessable(format!(
                        "Custom amounts has {} entries but parts_count is {count}",
                        custom.len()
                    )));
                }
                if custom.iter().any(|amount| *amount <= 0) {
                    return Err(UnifiedError::bad_request(
                        "Every custom amount must be greater than zero",
                    ));
                }
                let sum: i64 = custom.iter().sum();
                if sum != input.total_minor {
                    return Err(UnifiedError::unprocessable(format!(
                        "Custom amounts sum to {sum} {currency} but the total is {} {currency}",
                        input.total_minor
                    ))
                    .with_metadata("total_minor", Value::from(input.total_minor))
                    .with_metadata("parts_sum_minor", Value::from(sum))
                    .with_metadata(
                        "difference_minor",
                        Value::from(input.total_minor - sum),
                    ));
                }
                custom.clone()
            }
            None => {
                // Equal parts in integer minor units: the remainder
                // (total % count) goes one minor unit at a time to the
                // earliest parts, so the parts always sum to the total.
                let base = input.total_minor / input.parts_count as i64;
                let remainder = (input.total_minor % input.parts_count as i64) as usize;
                (0..count)
                    .map(|index| if index < remainder { base + 1 } else { base })
                    .collect()
            }
        };
        let payer_id = input
            .payer_id
            .clone()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| customer_id.clone());
        let provider = input
            .provider
            .clone()
            .map(|value| value.trim().to_lowercase())
            .filter(|value| !value.is_empty());
        if let Some(provider) = &provider {
            PaymentProviderKind::from_str(provider).ok_or_else(|| {
                UnifiedError::bad_request(format!("Unknown provider \"{provider}\""))
            })?;
        }
        let parts: Vec<Value> = amounts
            .iter()
            .enumerate()
            .map(|(index, amount)| {
                let due = input.first_due_date
                    + chrono::Duration::days(index as i64 * interval_days as i64);
                json!({
                    "index": index,
                    "amount_minor": amount,
                    "currency": currency.clone(),
                    "provider": provider.clone(),
                    "payer_id": payer_id.clone(),
                    "due_date": due.to_rfc3339(),
                })
            })
            .collect();
        let schedule = json!({
            "total_minor": input.total_minor,
            "currency": currency.clone(),
            "parts_count": input.parts_count,
            "interval_days": interval_days,
            "parts": parts,
        });
        if input.create {
            let provider = provider.ok_or_else(|| {
                UnifiedError::bad_request("provider is required when create is true")
            })?;
            let create = CreateSplitRequest {
                order_id: input.order_id.clone(),
                invoice_id: input.invoice_id.clone(),
                customer_id: customer_id.clone(),
                total_minor: input.total_minor,
                currency: currency.clone(),
                parts: amounts
                    .iter()
                    .enumerate()
                    .map(|(index, amount)| SplitPartInput {
                        amount_minor: *amount,
                        provider: Some(provider.clone()),
                        connection_id: input.connection_id,
                        payer_id: Some(payer_id.clone()),
                        due_date: Some(
                            input.first_due_date
                                + chrono::Duration::days(
                                    index as i64 * interval_days as i64,
                                ),
                        ),
                    })
                    .collect(),
            };
            let split = build_split_bill(create)?;
            splits_store().insert(split.id, split.clone());
            state.emit(
                PAYMENT_SPLIT_CREATED,
                json!({
                    "split_id": split.id,
                    "order_id": split.order_id.clone(),
                    "invoice_id": split.invoice_id.clone(),
                    "customer_id": split.customer_id.clone(),
                    "total_minor": split.total_minor,
                    "currency": split.currency.clone(),
                    "split": serde_json::to_value(&split).unwrap_or(Value::Null),
                }),
            );
            return Ok(json!({ "schedule": schedule, "split": split }));
        }
        Ok(json!({ "schedule": schedule }))
    })();
    json::respond(result, debug)
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
    use unified_core::hooks::HookBus;
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

    fn state_data() -> web::Data<Arc<PaymentsState>> {
        web::Data::new(Arc::new(PaymentsState::new()))
    }

    fn ctx_data() -> web::Data<Arc<AppContext>> {
        web::Data::new(Arc::new(test_context()))
    }

    fn split_request(customer_id: &str, payer_id: &str) -> CreateSplitRequest {
        CreateSplitRequest {
            order_id: Some(format!("order-{}", Uuid::now_v7())),
            invoice_id: None,
            customer_id: customer_id.to_string(),
            total_minor: 2000,
            currency: "USD".to_string(),
            parts: vec![
                SplitPartInput {
                    amount_minor: 1000,
                    provider: Some("stripe".to_string()),
                    connection_id: None,
                    payer_id: Some(payer_id.to_string()),
                    due_date: None,
                },
                SplitPartInput {
                    amount_minor: 1000,
                    provider: Some("stripe".to_string()),
                    connection_id: None,
                    payer_id: Some(payer_id.to_string()),
                    due_date: None,
                },
            ],
        }
    }

    fn stored_split(customer_id: &str, payer_id: &str) -> SplitBill {
        let split = build_split_bill(split_request(customer_id, payer_id)).unwrap();
        splits_store().insert(split.id, split.clone());
        split
    }

    #[tokio::test]
    async fn split_create_get_cancel_require_the_split_owner() {
        // Creating a split for another customer is refused.
        let victim = test_user_uuid("cust-victim").to_string();
        let attacker = verified_request_without_roles("cust-attacker");
        let response = create_split(
            ctx_data(),
            state_data(),
            attacker,
            web::Json(split_request(&victim, &victim)),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        // The owner can create it.
        let owner = verified_request_without_roles("cust-victim");
        let response = create_split(
            ctx_data(),
            state_data(),
            owner,
            web::Json(split_request(&victim, &victim)),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = String::from_utf8(
            actix_web::body::to_bytes(response.into_body())
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        let created_id = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|value| value.get("data")?.get("id")?.as_str()?.to_string())
            .and_then(|id| Uuid::parse_str(&id).ok())
            .expect("created split response carries its id");
        splits_store().remove(&created_id);
        let split = stored_split(&victim, &victim);
        // Reading and cancelling it as someone else is refused.
        let attacker = verified_request_without_roles("cust-attacker");
        let response = get_split(ctx_data(), attacker, web::Path::from(split.id)).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let attacker = verified_request_without_roles("cust-attacker");
        let response = cancel_split(ctx_data(), state_data(), attacker, web::Path::from(split.id), None).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        // The owner can cancel it (no part is paid).
        let owner = verified_request_without_roles("cust-victim");
        let response = cancel_split(ctx_data(), state_data(), owner, web::Path::from(split.id), None).await;
        assert_eq!(response.status(), StatusCode::OK);
        splits_store().remove(&split.id);
    }

    #[tokio::test]
    async fn pay_split_part_rejects_a_caller_who_owns_neither_payer_nor_split() {
        let victim = test_user_uuid("cust-victim").to_string();
        let split = stored_split(&victim, &victim);
        let part_id = split.parts[0].id;
        // The attacker supplies the victim's customer id and a
        // stored method id: the ownership check must fire before
        // any token resolution or provider call.
        let attacker = verified_request_without_roles("cust-attacker");
        let body = web::Json(PaySplitPartRequest {
            connection_id: Some(Uuid::now_v7()),
            customer_id: Some(victim.clone()),
            payment_method_token: None,
            payment_method_id: Some(Uuid::now_v7()),
            idempotency_key: None,
            description: None,
            three_ds: false,
            card: None,
        });
        let response = pay_split_part(
            ctx_data(),
            state_data(),
            attacker,
            web::Path::from((split.id, part_id)),
            body,
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        splits_store().remove(&split.id);
    }
}
