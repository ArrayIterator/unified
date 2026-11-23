//! payments - handlers subscriptions.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::reference_id;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::sync::{Arc};
use uuid::Uuid;
use super::{events::*, card_validation::*, card_settings::*, limits::*, providers::*, types::*, reference_ids::*, credentials::*, records::*, adapter::*, dispatch::*, stripe::*, paypal::*, xendit::*, midtrans::*, hyperpay::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};



// ---------------------------------------------------------------------------
// Handlers: subscriptions.
// ---------------------------------------------------------------------------
/// What one renewal charge means for the subscription. Only a
/// captured charge advances the period: a pending charge has moved
/// no money, so the period stays put and no captured event is
/// emitted; anything else is a failed renewal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RenewalOutcome {
    Captured,
    Pending,
    Failed,
}


pub(crate) fn renewal_outcome(status: PaymentStatus) -> RenewalOutcome {
    match status {
        PaymentStatus::Captured => RenewalOutcome::Captured,
        PaymentStatus::Pending => RenewalOutcome::Pending,
        _ => RenewalOutcome::Failed,
    }
}


/// Deterministic idempotency key for one subscription's renewal of
/// one period, keyed by the period's due time. A retry after a
/// timeout or provider ambiguity replays the original charge
/// instead of charging the same period twice.
pub(crate) fn renewal_idempotency_key(
    subscription_id: Uuid,
    period_marker: DateTime<Utc>,
) -> String {
    format!("renew:{subscription_id}:{}", period_marker.timestamp())
}


/// One renewal attempt for a subscription: charge the stored token for
/// one period, advance the period on success, mark past-due and emit
/// `payment.failed` on failure. A failed renewal counts toward the
/// stored dunning settings (`renewal_failed`); once they are
/// exhausted the subscription is never charged again. Shared by the
/// renew endpoint and the renew-due sweep.
pub(crate) async fn renew_one(
    state: &PaymentsState,
    subscription_id: Uuid,
) -> UnifiedResult<Uuid> {
    let subscription = subscriptions_store()
        .get(&subscription_id)
        .map(|entry| entry.clone())
        .ok_or_else(|| UnifiedError::not_found("Subscription not found"))?;
    if subscription.status != "active" && subscription.status != "past_due" {
        return Err(UnifiedError::conflict(format!(
            "Subscription in status \"{}\" cannot renew",
            subscription.status
        )));
    }
    // Dunning already exhausted: never charge again. A subscription
    // inside its grace period waits it out; one past the grace
    // period expires here (the sweep reports it under "expired").
    if subscription.dunning_exhausted {
        let now = Utc::now();
        if subscription
            .grace_ends_at
            .is_none_or(|ends_at| now >= ends_at)
        {
            expire_dunning_exhausted(state, &subscription);
            return Err(UnifiedError::conflict(
                "Subscription renewal failed: dunning retries exhausted and the grace period has ended",
            ));
        }
        return Err(UnifiedError::conflict(
            "Subscription renewal is suspended: dunning retries exhausted, grace period running",
        ));
    }
    let token = match subscription.payment_method_id {
        Some(method_id) => {
            let method = methods_store()
                .get(&method_id)
                .map(|entry| entry.clone())
                .ok_or_else(|| {
                    UnifiedError::not_found(format!(
                        "Stored payment method \"{method_id}\" for the subscription no longer exists"
                    ))
                })?;
            validate_token(Some(&method.provider_token), "stored method")?
        }
        None => {
            return Err(UnifiedError::bad_request(
                "Subscription has no stored payment method to renew with",
            ));
        }
    };
    let connection = find_connection(subscription.connection_id)?;
    let adapter = adapter_for(&connection)?;
    // The period being renewed is the one whose renewal time has
    // arrived. The key is deterministic per subscription + period,
    // so a retry replays instead of double-charging.
    let period_marker = subscription
        .next_renewal_at
        .or(subscription.current_period_end)
        .unwrap_or_else(Utc::now);
    let key = renewal_idempotency_key(subscription.id, period_marker);
    // A previous attempt that already captured this period is
    // replayed, not charged again. If that attempt recorded the
    // capture but stopped before advancing the period, finish the
    // bookkeeping now (without re-emitting the capture event).
    // A merely pending earlier attempt does not block a fresh
    // charge: no money moved, so there is nothing to replay.
    if let Some(existing) = find_by_idempotency(connection.id, &key) {
        if existing.status == "captured" {
            if subscription.next_renewal_at == Some(period_marker)
                || subscription.current_period_end == Some(period_marker)
            {
                // Anchor to the previous period end, never to now:
                // a late replay must not grant free days.
                let anchor = subscription
                    .current_period_end
                    .map(|end| end.max(Utc::now()))
                    .unwrap_or_else(Utc::now);
                let next_end = period_end_from(
                    anchor,
                    &subscription.interval,
                    subscription.interval_count,
                    0,
                );
                if let Some(mut stored) = subscriptions_store().get_mut(&subscription_id) {
                    stored.status = "active".to_string();
                    stored.current_period_end = Some(next_end);
                    stored.next_renewal_at = Some(next_end);
                    stored.consecutive_failures = 0;
                    stored.dunning_exhausted = false;
                    stored.grace_ends_at = None;
                    stored.updated_at = Utc::now();
                }
            }
            return Ok(existing.id);
        }
        // A merely pending earlier attempt does not replay, but it
        // does block a fresh charge for the same period: the money
        // may still move, so charging again could double-charge.
        // The sweep retries later; a capture webhook flips the
        // pending transaction to captured, and the next attempt
        // replays it above.
        return Ok(existing.id);
    }
    // Claim the renewal key BEFORE the provider call: a concurrent
    // renewal of the same period (sweep + manual renew, or two
    // sweeps) waits for this request's charge instead of charging
    // twice. The claim is released on every path that records no
    // transaction; a recorded transaction overwrites it.
    let claim = match claim_idempotency(connection.id, &key) {
        Some(sentinel) => sentinel,
        None => {
            if let Some(existing) = wait_for_idempotency(connection.id, &key).await {
                return Ok(existing.id);
            }
            return Err(UnifiedError::conflict(
                "A renewal with this idempotency key is already in progress",
            ));
        }
    };
    let result = adapter
        .charge(&ChargeInput {
            amount_minor: subscription.amount_minor,
            currency: subscription.currency.clone(),
            customer_id: subscription.customer_id.clone(),
            order_id: subscription.order_id.clone(),
            reference_id: None,
            invoice_id: None,
            description: Some(format!(
                "Subscription renewal (plan {})",
                subscription.plan_id.clone().unwrap_or_default()
            )),
            payment_method_token: Some(token),
            idempotency_key: key.clone(),
            three_ds: false,
            billing: None,
        })
        .await;
    match result {
        Ok(charge) => {
            let outcome = renewal_outcome(charge.status);
            if outcome == RenewalOutcome::Failed {
                // A declined renewal records no transaction, so
                // the claim must be released: the next retry of
                // this period needs a fresh claim.
                release_idempotency_claim(connection.id, &key, claim);
                return Err(renewal_failed(state, &subscription, "declined by provider"));
            }
            let mut transaction = new_transaction(
                connection.id,
                &subscription.provider,
                "subscription_charge",
                &subscription.customer_id,
                subscription.order_id.clone(),
                None,
                subscription.amount_minor,
                &subscription.currency,
                charge.status,
                charge.provider_transaction_id.clone(),
                key,
            );
            transaction.subscription_id = Some(subscription.id);
            transaction.fee_minor = charge.fee_minor;
            let transaction = record_transaction(transaction);
            if outcome == RenewalOutcome::Pending {
                // No money was captured: the period does not
                // advance, the subscription waits in past_due until
                // a later renewal captures the payment, and no
                // `payment.captured` event is emitted.
                if let Some(mut stored) = subscriptions_store().get_mut(&subscription_id) {
                    stored.status = "past_due".to_string();
                    stored.updated_at = Utc::now();
                }
                return Ok(transaction.id);
            }
            // Anchor to the previous period end, never to now: a
            // late sweep must not grant free days.
            let anchor = subscription
                .current_period_end
                .map(|end| end.max(Utc::now()))
                .unwrap_or_else(Utc::now);
            let next_end = period_end_from(
                anchor,
                &subscription.interval,
                subscription.interval_count,
                0,
            );
            if let Some(mut stored) = subscriptions_store().get_mut(&subscription_id) {
                stored.status = "active".to_string();
                stored.current_period_end = Some(next_end);
                stored.next_renewal_at = Some(next_end);
                stored.consecutive_failures = 0;
                stored.dunning_exhausted = false;
                stored.grace_ends_at = None;
                stored.updated_at = Utc::now();
            }
            state.emit(
                PAYMENT_SUBSCRIPTION_RENEWED,
                json!({
                    "subscription_id": subscription.id,
                    "transaction_id": transaction.id,
                    "provider_subscription_id": subscription.provider_subscription_id,
                    "provider": subscription.provider,
                    "connection_id": connection.id,
                    "customer_id": subscription.customer_id,
                    "order_id": subscription.order_id,
                    "amount_minor": subscription.amount_minor,
                    "currency": subscription.currency,
                    "fee_minor": charge.fee_minor,
                    "current_period_end": next_end.to_rfc3339(),
                }),
            );
            state.emit(
                PAYMENT_CAPTURED,
                json!({
                    "transaction_id": transaction.id,
                    "provider_transaction_id": charge.provider_transaction_id,
                    "provider": subscription.provider,
                    "connection_id": connection.id,
                    "customer_id": subscription.customer_id,
                    "order_id": subscription.order_id,
                    "subscription_id": subscription.id,
                    "amount_minor": subscription.amount_minor,
                    "currency": subscription.currency,
                    "fee_minor": charge.fee_minor,
                    "status": transaction.status,
                }),
            );
            Ok(transaction.id)
        }
        Err(error) => {
            release_idempotency_claim(connection.id, &key, claim);
            Err(renewal_failed(state, &subscription, &error.message))
        }
    }
}


/// Expire a subscription whose dunning is exhausted and whose grace
/// period has ended: status `ended`,
/// `payment.subscription.cancelled` emitted with reason
/// `dunning_exhausted`. Shared by `renew_one` and the renew-due
/// sweep.
pub(crate) fn expire_dunning_exhausted(state: &PaymentsState, subscription: &GatewaySubscription) {
    let now = Utc::now();
    if let Some(mut stored) = subscriptions_store().get_mut(&subscription.id) {
        stored.status = "ended".to_string();
        stored.cancelled_at = Some(now);
        stored.updated_at = now;
    }
    state.emit(
        PAYMENT_SUBSCRIPTION_CANCELLED,
        json!({
            "subscription_id": subscription.id,
            "provider_subscription_id": subscription.provider_subscription_id,
            "provider": subscription.provider,
            "customer_id": subscription.customer_id,
            "order_id": subscription.order_id,
            "reason": "dunning_exhausted",
        }),
    );
}


/// Mark a subscription past-due, count the failure, and enforce
/// the stored dunning settings. Returns the error the renew
/// endpoint reports.
///
/// Dunning: each failed renewal increments `consecutive_failures`
/// and schedules the next attempt `dunning_retry_interval_days`
/// later, while the subscription stays `past_due`. Once the count
/// reaches `dunning_max_retries` no further charge is attempted:
/// with a grace period the subscription waits it out in
/// `past_due` (`dunning_exhausted`, `grace_ends_at` set) and
/// expires when the grace period ends; without one it expires
/// immediately (`ended`, `payment.subscription.cancelled`
/// emitted with reason `dunning_exhausted`). A later captured
/// renewal resets the count.
pub(crate) fn renewal_failed(
    state: &PaymentsState,
    subscription: &GatewaySubscription,
    reason: &str,
) -> UnifiedError {
    let now = Utc::now();
    let failures = subscription.consecutive_failures.saturating_add(1);
    let max_retries = subscription.dunning_max_retries;
    let exhausted = failures >= max_retries;
    let mut expired = false;
    if let Some(mut stored) = subscriptions_store().get_mut(&subscription.id) {
        stored.consecutive_failures = failures;
        stored.updated_at = now;
        if exhausted {
            stored.dunning_exhausted = true;
            if subscription.grace_period_days == 0 {
                stored.status = "ended".to_string();
                stored.cancelled_at = Some(now);
                expired = true;
            } else {
                stored.status = "past_due".to_string();
                let grace_ends_at =
                    now + chrono::Duration::days(i64::from(subscription.grace_period_days));
                stored.grace_ends_at = Some(grace_ends_at);
                stored.next_renewal_at = Some(grace_ends_at);
            }
        } else {
            stored.status = "past_due".to_string();
            stored.next_renewal_at = Some(
                now + chrono::Duration::days(i64::from(
                    subscription.dunning_retry_interval_days,
                )),
            );
        }
    }
    state.emit(
        PAYMENT_FAILED,
        json!({
            "subscription_id": subscription.id,
            "provider_subscription_id": subscription.provider_subscription_id,
            "provider": subscription.provider,
            "customer_id": subscription.customer_id,
            "order_id": subscription.order_id,
            "amount_minor": subscription.amount_minor,
            "currency": subscription.currency,
            "reason": reason,
            "consecutive_failures": failures,
            "dunning_max_retries": max_retries,
            "dunning_exhausted": exhausted,
        }),
    );
    if expired {
        state.emit(
            PAYMENT_SUBSCRIPTION_CANCELLED,
            json!({
                "subscription_id": subscription.id,
                "provider_subscription_id": subscription.provider_subscription_id,
                "provider": subscription.provider,
                "customer_id": subscription.customer_id,
                "order_id": subscription.order_id,
                "reason": "dunning_exhausted",
            }),
        );
    }
    notify(
        state,
        &subscription.customer_id,
        "payment_failed",
        "Subscription renewal failed",
        &format!(
            "The renewal of your subscription ({} {}) failed: {reason}. Please update your payment method.",
            subscription.amount_minor, subscription.currency
        ),
    );
    UnifiedError::bad_request(format!("Subscription renewal failed: {reason}"))
        .with_metadata("provider", Value::String(subscription.provider.clone()))
        .with_metadata(
            "subscription_id",
            Value::String(subscription.id.to_string()),
        )
}


pub(crate) async fn create_subscription(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    req: HttpRequest,
    body: web::Json<CreateSubscriptionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let _tenant = require_tenant(&req)?;
        let input = body.into_inner();
        if input.customer_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("customer_id is required"));
        }
        // The caller must own the customer they subscribe: without
        // this, anyone could charge another customer's stored
        // payment method through `resolve_token` below.
        require_customer(&req, &input.customer_id, "customer")?;
        validate_money(input.amount_minor, &input.currency)?;
        if !matches!(input.interval.as_str(), "day" | "week" | "month" | "year") {
            return Err(UnifiedError::bad_request(
                "Subscription interval must be day, week, month, or year",
            ));
        }
        if input.interval_count == 0 {
            return Err(UnifiedError::bad_request(
                "interval_count must be at least 1",
            ));
        }
        // Dunning settings come from the product/plan when the
        // caller passes them, else the product defaults (grace 0,
        // 3 retries, 3 days apart). Bounds mirror the products
        // module's MAX_GRACE_PERIOD_DAYS / MAX_DUNNING_RETRIES /
        // MAX_DUNNING_RETRY_INTERVAL_DAYS (90 / 10 / 30). The
        // renewal code reads the values stored here.
        let grace_period_days = input
            .grace_period_days
            .unwrap_or_else(default_grace_period_days);
        let dunning_max_retries = input
            .dunning_max_retries
            .unwrap_or_else(default_dunning_max_retries);
        let dunning_retry_interval_days = input
            .dunning_retry_interval_days
            .unwrap_or_else(default_dunning_retry_interval_days);
        if grace_period_days > 90 {
            return Err(UnifiedError::bad_request(
                "grace_period_days must be at most 90",
            ));
        }
        if dunning_max_retries > 10 {
            return Err(UnifiedError::bad_request(
                "dunning_max_retries must be at most 10",
            ));
        }
        if dunning_retry_interval_days == 0 || dunning_retry_interval_days > 30 {
            return Err(UnifiedError::bad_request(
                "dunning_retry_interval_days must be between 1 and 30",
            ));
        }
        let connection = find_connection(input.connection_id)?;
        let token = resolve_token(
            input.payment_method_token.as_deref(),
            input.payment_method_id,
            &input.customer_id,
        )?;
        // Batch 18 (S6): validate the card before the provider
        // call — `subscription_create` can charge immediately.
        // Raw details run in `full` mode; a stored method id runs
        // in `method` mode; an opaque token alone skips the gate.
        request_card_validation(&state, &req, input.card.as_ref(), input.payment_method_id, None, &connection, &input.customer_id, input.order_id.as_deref(), None)?;
        let adapter = adapter_for(&connection)?;
        let key = idempotency_key(input.idempotency_key.clone());
        let created = adapter
            .subscription_create(&SubscriptionInput {
                customer_id: input.customer_id.clone(),
                plan_id: input.plan_id.clone(),
                amount_minor: input.amount_minor,
                currency: input.currency.clone(),
                interval: input.interval.clone(),
                interval_count: input.interval_count,
                trial_days: input.trial_days,
                payment_method_token: token,
                order_id: input.order_id.clone(),
                idempotency_key: key,
            })
            .await?;
        let period_end_at = created
            .current_period_end
            .unwrap_or_else(|| period_end(&input.interval, input.interval_count, input.trial_days));
        // A provider status that is not yet active (PayPal
        // APPROVAL_PENDING, for example) is recorded as pending, not
        // active: the subscription only renews once it is active.
        let status = match created.status.to_lowercase().as_str() {
            "active" | "trialing" | "succeeded" => "active".to_string(),
            other => other.to_string(),
        };
        let now = Utc::now();
        let subscription = GatewaySubscription {
            id: Uuid::now_v7(),
            connection_id: connection.id,
            provider: connection.kind.as_str().to_string(),
            customer_id: input.customer_id.clone(),
            order_id: input.order_id.clone(),
            plan_id: input.plan_id.clone(),
            amount_minor: input.amount_minor,
            currency: input.currency.clone(),
            interval: input.interval.clone(),
            interval_count: input.interval_count,
            status: status.clone(),
            provider_subscription_id: created.provider_subscription_id.clone(),
            payment_method_id: input.payment_method_id,
            current_period_end: Some(period_end_at),
            next_renewal_at: Some(period_end_at),
            cancelled_at: None,
            dunning_max_retries,
            dunning_retry_interval_days,
            grace_period_days,
            consecutive_failures: 0,
            dunning_exhausted: false,
            grace_ends_at: None,
            created_at: now,
            updated_at: now,
        };
        subscriptions_store().insert(subscription.id, subscription.clone());
        state.emit(
            PAYMENT_SUBSCRIPTION_CREATED,
            json!({
                "subscription_id": subscription.id,
                "provider_subscription_id": created.provider_subscription_id,
                "provider": subscription.provider,
                "connection_id": connection.id,
                "customer_id": input.customer_id,
                "order_id": input.order_id,
                "plan_id": input.plan_id,
                "amount_minor": input.amount_minor,
                "currency": input.currency,
                "interval": input.interval,
                "status": subscription.status,
                "current_period_end": period_end_at.to_rfc3339(),
            }),
        );
        // Ask the scheduler to run the renewal at the period end.
        // The scheduler rejects the kind until it learns it, so the
        // outcome is reported honestly in the response.
        let renewal_enqueued = enqueue_renewal(&state, subscription.id, period_end_at);
        Ok(json!({
            "subscription": subscription,
            "renewal_enqueued": renewal_enqueued,
            "renewal_note": if renewal_enqueued {
                Value::Null
            } else {
                Value::String("Scheduler does not know the payment.subscription.renew job kind yet; renewals can be triggered with POST /api/v1/payments/subscriptions/renew-due until the scheduler learns it".to_string())
            },
        }))
    })()
    .await;
    match result {
        Ok(value) => json::success_response(value, 201),
        Err(error) => json::error_response(&error, debug),
    }
}


pub(crate) async fn list_subscriptions(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<GatewaySubscription>> = (|| {
        // Lists are scoped to the caller: an admin sees every
        // customer's subscriptions, anyone else sees only their own.
        let _tenant = require_tenant(&req)?;
        let scope = list_customer_scope(&ctx, &req)?;
        let mut items: Vec<GatewaySubscription> = subscriptions_store()
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|subscription| {
                scope
                    .as_ref()
                    .is_none_or(|caller| subscription.customer_id == *caller)
                    && matches_search(
                        &format!(
                            "{} {} {}",
                            subscription.provider_subscription_id,
                            subscription.provider,
                            subscription.customer_id
                        ),
                        &params,
                    ) && params
                    .filters
                    .get("provider")
                    .is_none_or(|provider| subscription.provider == *provider)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| subscription.status == *status)
                    && params
                        .filters
                        .get("customer_id")
                        .is_none_or(|customer| subscription.customer_id == *customer)
                    && params
                        .filters
                        .get("customer")
                        .is_none_or(|customer| subscription.customer_id == *customer)
                    && params
                        .filters
                        .get("order_id")
                        .is_none_or(|order_id| subscription.order_id.as_deref() == Some(order_id.as_str()))
            })
            .collect();
        items.sort_by_key(|subscription| subscription.created_at);
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}


pub(crate) async fn get_subscription(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GatewaySubscription> = (|| {
        let _tenant = require_tenant(&req)?;
        let subscription = subscriptions_store()
            .get(&id.into_inner())
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::not_found("Subscription not found"))?;
        require_customer(&req, &subscription.customer_id, "subscription")?;
        Ok(subscription)
    })();
    json::respond(result, debug)
}


pub(crate) async fn cancel_subscription(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<CancelSubscriptionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<GatewaySubscription> = (|| async {
        let _tenant = require_tenant(&req)?;
        let id = id.into_inner();
        let request = body.into_inner();
        let mut subscription = subscriptions_store()
            .get(&id)
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::not_found("Subscription not found"))?;
        require_customer(&req, &subscription.customer_id, "subscription")?;
        if subscription.status == "cancelled" || subscription.status == "ended" {
            return Err(UnifiedError::conflict(format!(
                "Subscription is already {}",
                subscription.status
            )));
        }
        let connection = find_connection(subscription.connection_id)?;
        let adapter = adapter_for(&connection)?;
        adapter
            .subscription_cancel(&subscription.provider_subscription_id)
            .await?;
        subscription.status = "cancelled".to_string();
        subscription.cancelled_at = Some(Utc::now());
        subscription.updated_at = Utc::now();
        subscriptions_store().insert(id, subscription.clone());
        state.emit(
            PAYMENT_SUBSCRIPTION_CANCELLED,
            json!({
                "subscription_id": subscription.id,
                "provider_subscription_id": subscription.provider_subscription_id,
                "provider": subscription.provider,
                "customer_id": subscription.customer_id,
                "order_id": subscription.order_id,
                "reason": request.reason,
            }),
        );
        Ok(subscription)
    })()
    .await;
    json::respond(result, debug)
}


pub(crate) async fn renew_subscription(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let _tenant = require_tenant(&req)?;
        let id = id.into_inner();
        let subscription = subscriptions_store()
            .get(&id)
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::not_found("Subscription not found"))?;
        require_customer(&req, &subscription.customer_id, "subscription")?;
        let transaction_id = renew_one(&state, id).await?;
        let transaction = transactions_store()
            .get(&transaction_id)
            .map(|entry| entry.clone())
            .ok_or_else(|| UnifiedError::internal("Renewal transaction vanished after charge"))?;
        Ok(json!({ "transaction": transaction }))
    })()
    .await;
    json::respond(result, debug)
}


/// Renew every subscription whose renewal time has arrived. This is
/// the endpoint a scheduler job or an operator cron calls until the
/// scheduler learns the `payment.subscription.renew` job kind.
pub(crate) async fn renew_due_subscriptions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PaymentsState>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let _tenant = require_tenant(&req)?;
        // A batch sweep that charges every due subscription: an
        // operator endpoint (scheduler / cron), never a customer
        // endpoint, so it needs the payments admin capability.
        require_payments_admin(&ctx, &req)?;
        let now = Utc::now();
        let due: Vec<Uuid> = subscriptions_store()
            .iter()
            .filter(|entry| {
                (entry.value().status == "active" || entry.value().status == "past_due")
                    && entry
                        .value()
                        .next_renewal_at
                        .is_some_and(|renew_at| renew_at <= now)
            })
            .map(|entry| *entry.key())
            .collect();
        let mut renewed: Vec<String> = Vec::new();
        let mut failed: Vec<Value> = Vec::new();
        let mut expired: Vec<String> = Vec::new();
        for subscription_id in due {
            // Dunning exhausted and past the grace period expires
            // here instead of charging: the sweep reports it under
            // "expired", not as a failed renewal.
            let dunning_expired = subscriptions_store()
                .get(&subscription_id)
                .map(|entry| {
                    let subscription = entry.value();
                    subscription.dunning_exhausted
                        && subscription
                            .grace_ends_at
                            .is_none_or(|ends_at| now >= ends_at)
                })
                .unwrap_or(false);
            if dunning_expired {
                if let Some(subscription) = subscriptions_store()
                    .get(&subscription_id)
                    .map(|entry| entry.clone())
                {
                    expire_dunning_exhausted(&state, &subscription);
                }
                expired.push(subscription_id.to_string());
                continue;
            }
            match renew_one(&state, subscription_id).await {
                Ok(transaction_id) => renewed.push(transaction_id.to_string()),
                Err(error) => failed.push(json!({
                    "subscription_id": subscription_id,
                    "error": error.message,
                })),
            }
        }
        Ok(json!({
            "renewed": renewed,
            "failed": failed,
            "expired": expired,
            "checked_at": now.to_rfc3339(),
        }))
    })()
    .await;
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

    fn subscription_body(customer_id: &str) -> web::Json<CreateSubscriptionRequest> {
        web::Json(CreateSubscriptionRequest {
            connection_id: Uuid::now_v7(),
            customer_id: customer_id.to_string(),
            plan_id: None,
            amount_minor: 1000,
            currency: "USD".to_string(),
            interval: "month".to_string(),
            interval_count: 1,
            trial_days: 0,
            payment_method_id: None,
            payment_method_token: None,
            order_id: None,
            idempotency_key: None,
            grace_period_days: None,
            dunning_max_retries: None,
            dunning_retry_interval_days: None,
            card: None,
        })
    }

    fn stored_subscription(
        failures: u32,
        grace: u32,
        max_retries: u32,
        interval: u32,
    ) -> GatewaySubscription {
        let now = Utc::now();
        GatewaySubscription {
            id: Uuid::now_v7(),
            connection_id: Uuid::now_v7(),
            provider: "stripe".to_string(),
            customer_id: "cust-dunning".to_string(),
            order_id: None,
            plan_id: None,
            amount_minor: 1000,
            currency: "USD".to_string(),
            interval: "month".to_string(),
            interval_count: 1,
            status: "past_due".to_string(),
            provider_subscription_id: "sub-dunning".to_string(),
            payment_method_id: None,
            current_period_end: Some(now),
            next_renewal_at: Some(now),
            cancelled_at: None,
            dunning_max_retries: max_retries,
            dunning_retry_interval_days: interval,
            grace_period_days: grace,
            consecutive_failures: failures,
            dunning_exhausted: false,
            grace_ends_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn get_subscription_is_gated_and_owner_scoped() {
        // The owner id is the test-user UUID for the name, exactly
        // like the other ownership tests in this module.
        let mut subscription = stored_subscription(0, 0, 3, 3);
        subscription.customer_id = test_user_uuid("cust-dunning").to_string();
        subscriptions_store().insert(subscription.id, subscription.clone());
        // No tenant context at all: refused before any record is read.
        let response = get_subscription(
            web::Data::new(Arc::new(test_context())),
            TestRequest::default().to_http_request(),
            web::Path::from(subscription.id),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        // Another customer: the subscription reads as forbidden,
        // never as the owner's record.
        let response = get_subscription(
            web::Data::new(Arc::new(test_context())),
            verified_request_without_roles("cust-attacker"),
            web::Path::from(subscription.id),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        // The owning customer reads it.
        let response = get_subscription(
            web::Data::new(Arc::new(test_context())),
            verified_request_without_roles("cust-dunning"),
            web::Path::from(subscription.id),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        subscriptions_store().remove(&subscription.id);
    }

    #[tokio::test]
    async fn create_subscription_rejects_a_caller_charging_another_customer() {
        let state = web::Data::new(Arc::new(PaymentsState::new()));
        // The caller presents as `cust-attacker` but subscribes
        // `cust-victim`: refused before any provider is touched.
        let request = verified_request_without_roles("cust-attacker");
        let response = create_subscription(
            web::Data::new(Arc::new(test_context())),
            state,
            request,
            subscription_body("cust-victim"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn renewal_outcome_only_captured_advances_the_period() {
        assert_eq!(
            renewal_outcome(PaymentStatus::Captured),
            RenewalOutcome::Captured
        );
        assert_eq!(
            renewal_outcome(PaymentStatus::Pending),
            RenewalOutcome::Pending
        );
        // Every other status is a failed renewal, never a success.
        for status in [
            PaymentStatus::Failed,
            PaymentStatus::Refunded,
            PaymentStatus::Voided,
            PaymentStatus::Chargeback,
            PaymentStatus::Authorized,
            PaymentStatus::RequiresConfirmation,
            PaymentStatus::Cancelled,
            PaymentStatus::PartiallyRefunded,
        ] {
            assert_eq!(renewal_outcome(status), RenewalOutcome::Failed);
        }
    }

    #[test]
    fn renewal_failed_counts_failures_and_enforces_dunning() {
        let state = PaymentsState::new();
        // Below the cap: past_due, failure counted, next retry
        // spaced by the retry interval — not retried on every
        // sweep.
        let subscription = stored_subscription(0, 0, 3, 3);
        subscriptions_store().insert(subscription.id, subscription.clone());
        let error = renewal_failed(&state, &subscription, "declined");
        assert!(error.message.contains("declined"));
        let stored = subscriptions_store()
            .get(&subscription.id)
            .map(|entry| entry.clone())
            .unwrap();
        assert_eq!(stored.status, "past_due");
        assert_eq!(stored.consecutive_failures, 1);
        assert!(!stored.dunning_exhausted);
        assert!(stored.next_renewal_at > Some(Utc::now()));
        subscriptions_store().remove(&subscription.id);

        // At the cap with no grace period: the subscription
        // expires at once and is never charged again.
        let subscription = stored_subscription(2, 0, 3, 3);
        subscriptions_store().insert(subscription.id, subscription.clone());
        renewal_failed(&state, &subscription, "declined");
        let stored = subscriptions_store()
            .get(&subscription.id)
            .map(|entry| entry.clone())
            .unwrap();
        assert_eq!(stored.status, "ended");
        assert_eq!(stored.consecutive_failures, 3);
        assert!(stored.dunning_exhausted);
        assert!(stored.cancelled_at.is_some());
        subscriptions_store().remove(&subscription.id);

        // At the cap with a grace period: past_due through the
        // grace period, expiring after it — never charged again.
        let subscription = stored_subscription(2, 5, 3, 3);
        subscriptions_store().insert(subscription.id, subscription.clone());
        renewal_failed(&state, &subscription, "declined");
        let stored = subscriptions_store()
            .get(&subscription.id)
            .map(|entry| entry.clone())
            .unwrap();
        assert_eq!(stored.status, "past_due");
        assert!(stored.dunning_exhausted);
        assert!(stored.grace_ends_at.is_some());
        assert_eq!(stored.next_renewal_at, stored.grace_ends_at);
        subscriptions_store().remove(&subscription.id);
    }

    #[test]
    fn renewal_idempotency_key_is_deterministic_per_subscription_and_period() {
        let subscription_id = Uuid::now_v7();
        let period = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let first = renewal_idempotency_key(subscription_id, period);
        assert_eq!(first, renewal_idempotency_key(subscription_id, period));
        assert!(first.starts_with("renew:"));
        // A different period is a different charge.
        let later = period + chrono::Duration::days(30);
        assert_ne!(first, renewal_idempotency_key(subscription_id, later));
        // A different subscription is a different charge.
        assert_ne!(
            first,
            renewal_idempotency_key(Uuid::now_v7(), period)
        );
    }
}
