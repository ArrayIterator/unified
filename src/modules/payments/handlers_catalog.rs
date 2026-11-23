//! payments - handlers catalog.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::mapref::entry::Entry;
use serde::{Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex, OnceLock};
use uuid::Uuid;
use super::{events::*, card_validation::*, card_settings::*, limits::*, providers::*, types::*, reference_ids::*, credentials::*, records::*, adapter::*, dispatch::*, stripe::*, paypal::*, xendit::*, midtrans::*, hyperpay::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};



// ---------------------------------------------------------------------------
// Handler helpers.
// ---------------------------------------------------------------------------
/// Paginate an already-sorted, already-filtered list.
pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}


pub(crate) fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        None => true,
        Some(needle) => haystack
            .to_lowercase()
            .contains(&needle.to_lowercase()),
    }
}


pub(crate) fn parse_date_filter(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}


/// Resolve a stored connection by id.
pub(crate) fn find_connection(id: Uuid) -> UnifiedResult<PaymentConnection> {
    connections_store()
        .get(&id)
        .map(|entry| entry.clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Payment connection \"{id}\" not found")))
}


/// Build the adapter for a connection, refusing disabled connections.
pub(crate) fn adapter_for(connection: &PaymentConnection) -> UnifiedResult<Arc<dyn PaymentProvider>> {
    if !connection.enabled {
        return Err(UnifiedError::bad_request(format!(
            "Payment connection \"{}\" is disabled",
            connection.name
        )));
    }
    payment_registry().build(connection)
}


/// Idempotency key: the caller-supplied one, or a generated one.
pub(crate) fn idempotency_key(provided: Option<String>) -> String {
    provided
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
        .unwrap_or_else(|| Uuid::now_v7().to_string())
}


/// A previous transaction for this idempotency key on this
/// connection, if the call was already made.
pub(crate) fn find_by_idempotency(connection_id: Uuid, key: &str) -> Option<PaymentTransaction> {
    idempotency_store()
        .get(&(connection_id, key.to_string()))
        .and_then(|entry| transactions_store().get(entry.value()).map(|t| t.clone()))
}

/// Return a replayed transaction only when it belongs to the
/// customer this request acts for. Idempotency keys are scoped to
/// (connection, key), not to a customer, so a replay lookup can
/// otherwise hand one customer's transaction — amount, provider
/// id, order — to another customer who guessed or reused the key.
/// A foreign replay reads as a generic conflict, never as the
/// other customer's record.
pub(crate) fn replay_for_customer(
    existing: PaymentTransaction,
    customer_id: &str,
) -> UnifiedResult<PaymentTransaction> {
    if existing.customer_id == customer_id {
        Ok(existing)
    } else {
        Err(UnifiedError::conflict(
            "A request with this idempotency key is already in progress",
        ))
    }
}


/// Atomically claim an idempotency key BEFORE the provider call.
///
/// A plain `find_by_idempotency` check followed by a charge is
/// check-then-act: two concurrent requests with the same key both
/// pass the check and both charge. The claim inserts a sentinel
/// transaction id for (connection, key) in one atomic map step
/// (DashMap's entry API); exactly one caller wins. The winner's
/// `record_transaction` overwrites the sentinel with the real
/// transaction id; a loser waits briefly for that record
/// (`wait_for_idempotency`) instead of charging. `find_by_idempotency`
/// ignores the sentinel — it points at no transaction row — so a
/// replay lookup never returns a half-finished claim.
///
/// PER-PROCESS ONLY: the claim lives in the in-memory DashMap, so
/// two instances of this service can still both claim the same key.
/// Multi-instance deployments must enforce the claim in Postgres
/// instead — see migration 2026-11-22-000112
/// (`payment_idempotency_claims`, primary key on
/// (connection_id, idempotency_key)): insert the claim row first
/// and treat a unique-violation as "already claimed".
pub(crate) fn claim_idempotency(connection_id: Uuid, key: &str) -> Option<Uuid> {
    match idempotency_store().entry((connection_id, key.to_string())) {
        Entry::Vacant(entry) => {
            let sentinel = Uuid::now_v7();
            entry.insert(sentinel);
            Some(sentinel)
        }
        Entry::Occupied(_) => None,
    }
}


/// Release a claim, but only when the entry still holds the
/// expected id — never remove a completed record. Call this on
/// every path where the provider call failed before a transaction
/// was recorded; a recorded transaction overwrites the sentinel
/// and must be left in place.
pub(crate) fn release_idempotency_claim(connection_id: Uuid, key: &str, expected: Uuid) {
    idempotency_store().remove_if(&(connection_id, key.to_string()), |_, value| {
        *value == expected
    });
}


/// Wait briefly for the winner of an idempotency claim to record
/// its transaction, then return it. A duplicate that arrives while
/// the first request is still at the provider gets the original
/// result instead of a second charge. Returns `None` when nothing
/// is recorded within the wait (the caller then reports a
/// conflict).
pub(crate) async fn wait_for_idempotency(
    connection_id: Uuid,
    key: &str,
) -> Option<PaymentTransaction> {
    for _ in 0..50 {
        if let Some(transaction) = find_by_idempotency(connection_id, key) {
            return Some(transaction);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    find_by_idempotency(connection_id, key)
}


/// Serializes refund admission for one charge: the remaining-amount
/// check and the reservation of a pending refund row happen under
/// this lock, so two concurrent refunds cannot both spend the same
/// remainder. Held only for synchronous store reads/writes, never
/// across a provider call.
pub(crate) static REFUND_ADMISSION: OnceLock<Mutex<()>> = OnceLock::new();


pub(crate) fn refund_admission_lock() -> &'static Mutex<()> {
    REFUND_ADMISSION.get_or_init(|| Mutex::new(()))
}


/// A webhook refund whose event carried no usable amount is
/// recorded as a pending row with amount 0 (unquantified). Its
/// true size is unknown, so no further API refund may be admitted
/// against the same charge until that row is resolved (completed
/// with a real amount, or failed).
pub(crate) fn has_unquantified_pending_refund(original_id: Uuid) -> bool {
    transactions_store().iter().any(|entry| {
        let transaction = entry.value();
        transaction.kind == "refund"
            && transaction.original_transaction_id == Some(original_id)
            && transaction.status == "pending"
            && transaction.amount_minor == 0
    })
}


/// Record a transaction and index its idempotency key.
pub(crate) fn record_transaction(mut transaction: PaymentTransaction) -> PaymentTransaction {
    transaction.id = Uuid::now_v7();
    transaction.created_at = Utc::now();
    idempotency_store().insert(
        (transaction.connection_id, transaction.idempotency_key.clone()),
        transaction.id,
    );
    transactions_store().insert(transaction.id, transaction.clone());
    transaction
}


pub(crate) fn new_transaction(
    connection_id: Uuid,
    provider: &str,
    kind: &str,
    customer_id: &str,
    order_id: Option<String>,
    invoice_id: Option<String>,
    amount_minor: i64,
    currency: &str,
    status: PaymentStatus,
    provider_transaction_id: String,
    idempotency_key: String,
) -> PaymentTransaction {
    PaymentTransaction {
        id: Uuid::now_v7(),
        intent_id: None,
        connection_id,
        provider: provider.to_string(),
        kind: kind.to_string(),
        customer_id: customer_id.to_string(),
        order_id,
        invoice_id,
        subscription_id: None,
        amount_minor,
        currency: currency.to_string(),
        fee_minor: None,
        status: status.as_str().to_string(),
        provider_transaction_id,
        provider_refund_id: None,
        original_transaction_id: None,
        operator_id: None,
        idempotency_key,
        created_at: Utc::now(),
    }
}


/// Money already refunded (or reserved by a pending refund) against
/// one charge, summed over the refund records linked through
/// `original_transaction_id`. Completed refunds
/// (`refunded`/`partially_refunded`) and in-flight ones (`pending`)
/// both count here: a pending refund that later completes must not
/// be double-spent by a second refund in the meantime.
pub(crate) fn refunded_amount_minor(original_id: Uuid) -> i64 {
    transactions_store()
        .iter()
        .filter(|entry| {
            let transaction = entry.value();
            transaction.kind == "refund"
                && transaction.original_transaction_id == Some(original_id)
                && matches!(
                    transaction.status.as_str(),
                    "refunded" | "partially_refunded" | "pending"
                )
        })
        .map(|entry| entry.value().amount_minor)
        .sum()
}


/// Money whose refund has actually completed against one charge.
/// Drives the charge's status after a refund completes: at or over
/// the charge amount the charge is `refunded`, below it
/// `partially_refunded`.
pub(crate) fn completed_refunded_amount_minor(original_id: Uuid) -> i64 {
    transactions_store()
        .iter()
        .filter(|entry| {
            let transaction = entry.value();
            transaction.kind == "refund"
                && transaction.original_transaction_id == Some(original_id)
                && matches!(
                    transaction.status.as_str(),
                    "refunded" | "partially_refunded"
                )
        })
        .map(|entry| entry.value().amount_minor)
        .sum()
}


/// Resolve a payment method token: an explicit token wins, then a
/// stored payment method id, then `None`.
pub(crate) fn resolve_token(
    explicit_token: Option<&str>,
    method_id: Option<Uuid>,
    customer_id: &str,
) -> UnifiedResult<Option<String>> {
    if let Some(token) = explicit_token.map(str::trim).filter(|t| !t.is_empty()) {
        return Ok(Some(validate_token(Some(token), "request")?));
    }
    if let Some(method_id) = method_id {
        let method = methods_store()
            .get(&method_id)
            .map(|entry| entry.clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Payment method \"{method_id}\" not found"))
            })?;
        if method.customer_id != customer_id {
            return Err(UnifiedError::forbidden(
                "Payment method belongs to a different customer",
            ));
        }
        return Ok(Some(validate_token(Some(&method.provider_token), "stored method")?));
    }
    Ok(None)
}


/// Ask the scheduler to run a renewal for a subscription. The
/// scheduler only accepts known job kinds today, so the result is
/// reported back instead of assumed: `queued` is false until the
/// scheduler learns the `payment.subscription.renew` kind.
pub(crate) fn enqueue_renewal(state: &PaymentsState, subscription_id: Uuid, renew_at: DateTime<Utc>) -> bool {
    let context = state.bus().emit(
        "scheduler.enqueue",
        json!({
            "kind": "payment.subscription.renew",
            "event": "payment.subscription.renew",
            "payload": {
                "subscription_id": subscription_id.to_string(),
                "renew_at": renew_at.to_rfc3339(),
            },
        }),
    );
    context.results.iter().any(|result| {
        result
            .get("queued")
            .and_then(|queued| queued.as_bool())
            .unwrap_or(false)
    })
}


/// Spawn renewal work the way the scheduler-facing modules do: on the
/// current Tokio runtime when there is one, otherwise on a dedicated
/// thread with its own runtime (hook handlers are synchronous, and the
/// renewal charge needs async I/O).
pub(crate) fn spawn_renewal_worker<F>(future: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(future);
    } else {
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(future);
        });
    }
}


/// Request an in-app notification through the hook bus (the
/// notifications module listens to `notification.requested`).
pub(crate) fn notify(state: &PaymentsState, user_id: &str, kind: &str, title: &str, body: &str) {
    state.emit(
        "notification.requested",
        json!({
            "user_id": user_id,
            "kind": kind,
            "title": title,
            "body": body,
        }),
    );
}


/// Catalog view for one provider kind.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProviderCatalogView {
    pub(crate) kind: String,
    pub(crate) display_name: String,
    pub(crate) capabilities: PaymentCapabilities,
    pub(crate) currencies: Vec<String>,
    pub(crate) settings_fields: Vec<SettingField>,
    pub(crate) needs_delegate: bool,
    pub(crate) needs_endpoint: bool,
    pub(crate) has_fixed_hosts: bool,
}


pub(crate) fn catalog_view(kind: PaymentProviderKind) -> ProviderCatalogView {
    ProviderCatalogView {
        kind: kind.as_str().to_string(),
        display_name: kind.display_name().to_string(),
        capabilities: capabilities_for(kind),
        currencies: currencies_for(kind)
            .iter()
            .map(|currency| currency.to_string())
            .collect(),
        settings_fields: settings_fields_for(kind),
        needs_delegate: kind.needs_delegate(),
        needs_endpoint: kind.needs_endpoint(),
        has_fixed_hosts: kind.has_fixed_hosts(),
    }
}



// ---------------------------------------------------------------------------
// Handlers: catalog.
// ---------------------------------------------------------------------------
pub(crate) async fn list_providers(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vec<ProviderCatalogView>> =
        Ok(PaymentProviderKind::all().iter().map(|kind| catalog_view(*kind)).collect());
    json::respond(result, debug)
}


pub(crate) async fn get_provider(
    ctx: web::Data<Arc<AppContext>>,
    kind: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProviderCatalogView> = (|| {
        let kind = PaymentProviderKind::from_str(&kind).ok_or_else(|| {
            UnifiedError::not_found(format!("Unknown payment provider \"{kind}\""))
        })?;
        Ok(catalog_view(kind))
    })();
    json::respond(result, debug)
}


pub(crate) async fn get_provider_settings(
    ctx: web::Data<Arc<AppContext>>,
    kind: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let kind = PaymentProviderKind::from_str(&kind).ok_or_else(|| {
            UnifiedError::not_found(format!("Unknown payment provider \"{kind}\""))
        })?;
        Ok(json!({
            "kind": kind.as_str(),
            "display_name": kind.display_name(),
            "fields": settings_fields_for(kind),
            "currencies": currencies_for(kind),
            "capabilities": capabilities_for(kind),
            "needs_delegate": kind.needs_delegate(),
            "needs_endpoint": kind.needs_endpoint(),
        }))
    })();
    json::respond(result, debug)
}



// ---------------------------------------------------------------------------
// Handlers: connections.
// ---------------------------------------------------------------------------
pub(crate) async fn list_connections(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<PaymentConnectionView>> = (|| {
        let _tenant = require_tenant(&req)?;
        require_payments_admin(&ctx, &req)?;
        let pagination = params.normalized();
        let mut views: Vec<PaymentConnectionView> = connections_store()
            .iter()
            .map(|entry| entry.value().view())
            .filter(|view| {
                matches_search(&format!("{} {} {}", view.name, view.kind, view.kind_display), &params)
                    && params
                        .filters
                        .get("kind")
                        .is_none_or(|kind| view.kind == *kind)
                    && params
                        .filters
                        .get("mode")
                        .is_none_or(|mode| view.mode == *mode)
                    && params.filters.get("enabled").is_none_or(|enabled| {
                        view.enabled == (*enabled == "true" || *enabled == "1")
                    })
            })
            .collect();
        views.sort_by(|a, b| a.name.cmp(&b.name));
        if pagination.order == "desc" {
            views.reverse();
        }
        Ok(paginate(views, &params))
    })();
    json::respond(result, debug)
}


pub(crate) async fn create_connection(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateConnectionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentConnectionView> = (|| {
        let _tenant = require_tenant(&req)?;
        require_payments_admin(&ctx, &req)?;
        let input = body.into_inner();
        let kind = validate_connection(&input)?;
        let mode = match &input.mode {
            None => GatewayMode::Sandbox,
            Some(mode) => GatewayMode::from_str(mode).ok_or_else(|| {
                UnifiedError::bad_request(format!("Unknown gateway mode \"{mode}\""))
            })?,
        };
        let now = Utc::now();
        let connection = PaymentConnection {
            id: Uuid::now_v7(),
            name: input.name.trim().to_string(),
            kind,
            mode,
            endpoint: input.endpoint.map(|endpoint| endpoint.trim().to_string()),
            credentials: input.credentials,
            custom_config: input.custom_config,
            delegate_connection_id: input.delegate_connection_id,
            enabled: input.enabled.unwrap_or(true),
            last_tested_at: None,
            last_status: None,
            created_at: now,
            updated_at: now,
        };
        connections_store().insert(connection.id, connection.clone());
        Ok(connection.view())
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}


pub(crate) async fn get_connection(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentConnectionView> = (|| {
        let _tenant = require_tenant(&req)?;
        require_payments_admin(&ctx, &req)?;
        find_connection(id.into_inner()).map(|connection| connection.view())
    })();
    json::respond(result, debug)
}


pub(crate) async fn update_connection(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<UpdateConnectionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentConnectionView> = (|| {
        let _tenant = require_tenant(&req)?;
        require_payments_admin(&ctx, &req)?;
        let id = id.into_inner();
        let update = body.into_inner();
        let mut connection = find_connection(id)?;
        // Re-validate the merged connection through the same rules as
        // creation, so an update can never leave a broken connection
        // behind.
        let merged = CreateConnectionRequest {
            name: update.name.clone().unwrap_or_else(|| connection.name.clone()),
            kind: connection.kind.as_str().to_string(),
            mode: update
                .mode
                .clone()
                .map(|mode| mode)
                .or_else(|| Some(connection.mode.as_str().to_string())),
            endpoint: update.endpoint.clone().or_else(|| connection.endpoint.clone()),
            credentials: {
                let mut creds = connection.credentials.clone();
                if let Some(patch) = update.credentials.clone() {
                    creds.merge(patch);
                }
                creds
            },
            custom_config: update
                .custom_config
                .clone()
                .or_else(|| connection.custom_config.clone()),
            delegate_connection_id: update
                .delegate_connection_id
                .or(connection.delegate_connection_id),
            enabled: update.enabled.or(Some(connection.enabled)),
        };
        validate_connection(&merged)?;
        if let Some(name) = update.name {
            connection.name = name.trim().to_string();
        }
        if let Some(mode) = update.mode {
            connection.mode = GatewayMode::from_str(&mode).ok_or_else(|| {
                UnifiedError::bad_request(format!("Unknown gateway mode \"{mode}\""))
            })?;
        }
        if update.endpoint.is_some() {
            connection.endpoint = update.endpoint.map(|endpoint| endpoint.trim().to_string());
        }
        if let Some(patch) = update.credentials {
            connection.credentials.merge(patch);
        }
        if update.custom_config.is_some() {
            connection.custom_config = update.custom_config;
        }
        if update.delegate_connection_id.is_some() {
            connection.delegate_connection_id = update.delegate_connection_id;
        }
        if let Some(enabled) = update.enabled {
            connection.enabled = enabled;
        }
        connection.updated_at = Utc::now();
        connections_store().insert(id, connection.clone());
        Ok(connection.view())
    })();
    json::respond(result, debug)
}


pub(crate) async fn delete_connection(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        require_payments_admin(&ctx, &req)?;
        let id = id.into_inner();
        let _connection = find_connection(id)?;
        // Refuse to delete a connection another connection delegates
        // to, or an active subscription charges through.
        let in_use_as_delegate = connections_store().iter().any(|entry| {
            entry.value().delegate_connection_id == Some(id)
        });
        if in_use_as_delegate {
            return Err(UnifiedError::conflict(
                "Connection is the delegate of another wallet connection; reassign the delegate first",
            ));
        }
        let in_use_by_subscription = subscriptions_store()
            .iter()
            .any(|entry| entry.value().connection_id == id && entry.value().status == "active");
        if in_use_by_subscription {
            return Err(UnifiedError::conflict(
                "Connection has active subscriptions; cancel them first",
            ));
        }
        // Refuse to delete a connection with recorded history:
        // later refund / void / capture calls on those intents and
        // transactions resolve the connection through
        // `find_connection`, so deleting it would orphan them.
        // Disable the connection instead of deleting it.
        let in_use_by_intent = intents_store()
            .iter()
            .any(|entry| entry.value().connection_id == id);
        if in_use_by_intent {
            return Err(UnifiedError::conflict(
                "Connection has payment intents; disable it instead of deleting it",
            ));
        }
        let in_use_by_transaction = transactions_store()
            .iter()
            .any(|entry| entry.value().connection_id == id);
        if in_use_by_transaction {
            return Err(UnifiedError::conflict(
                "Connection has recorded transactions; disable it instead of deleting it",
            ));
        }
        connections_store().remove(&id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}


pub(crate) async fn test_connection(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ConnectionTestResult> = (|| async {
        let _tenant = require_tenant(&req)?;
        require_payments_admin(&ctx, &req)?;
        let id = id.into_inner();
        let connection = find_connection(id)?;
        let adapter = payment_registry().build(&connection)?;
        let test = adapter.test_connection().await?;
        if let Some(mut stored) = connections_store().get_mut(&id) {
            stored.last_tested_at = Some(Utc::now());
            stored.last_status = Some("ok".to_string());
            stored.updated_at = Utc::now();
        }
        Ok(test)
    })()
    .await;
    json::respond(result, debug)
}


pub(crate) async fn switch_mode(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<SwitchModeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PaymentConnectionView> = (|| {
        let _tenant = require_tenant(&req)?;
        require_payments_admin(&ctx, &req)?;
        let id = id.into_inner();
        let mode = GatewayMode::from_str(&body.mode).ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Unknown gateway mode \"{}\" (expected sandbox or live)",
                body.mode
            ))
        })?;
        let mut connection = find_connection(id)?;
        if connection.mode == mode {
            return Err(UnifiedError::bad_request(format!(
                "Connection is already in {} mode",
                mode.as_str()
            )));
        }
        // The switch only changes the base URL; the operator must have
        // stored the matching mode's credentials on the connection.
        // A test right after the switch is the way to confirm them.
        connection.mode = mode;
        connection.updated_at = Utc::now();
        connection.last_tested_at = None;
        connection.last_status = Some(format!("switched to {}", mode.as_str()));
        connections_store().insert(id, connection.clone());
        Ok(connection.view())
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

    /// A context whose `role.check` handler allows every permission.
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

    fn stored_connection() -> PaymentConnection {
        let now = Utc::now();
        let connection = PaymentConnection {
            id: Uuid::now_v7(),
            name: "test connection".to_string(),
            kind: PaymentProviderKind::Stripe,
            mode: GatewayMode::Sandbox,
            endpoint: None,
            credentials: PaymentCredentials::default(),
            custom_config: None,
            delegate_connection_id: None,
            enabled: true,
            last_tested_at: None,
            last_status: None,
            created_at: now,
            updated_at: now,
        };
        connections_store().insert(connection.id, connection.clone());
        connection
    }

    #[tokio::test]
    async fn connection_admin_endpoints_reject_unauthenticated_callers() {
        // No identity headers at all: refused before any work.
        let response = list_connections(
            data(test_context()),
            TestRequest::default().to_http_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        // Identity headers present, but no `role.check` handler
        // answers: the admin gate fails closed.
        let response = list_connections(
            data(test_context()),
            authed_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        // An allowing `role.check` answer admits the operator.
        let response = list_connections(
            data(allowing_context()),
            authed_request(),
            web::Query(PaginationParams::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn delete_connection_refuses_when_history_references_it() {
        let connection = stored_connection();
        let transaction = record_transaction(new_transaction(
            connection.id,
            "stripe",
            "charge",
            "cust-p14",
            None,
            None,
            1000,
            "USD",
            PaymentStatus::Captured,
            "prov-p14".to_string(),
            format!("p14-{}", Uuid::now_v7()),
        ));
        // An unauthenticated caller is refused first.
        let response = delete_connection(
            data(test_context()),
            TestRequest::default().to_http_request(),
            web::Path::from(connection.id),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        // An admin still cannot delete it while a transaction
        // references the connection: a later refund on that
        // transaction would fail at `find_connection`.
        let response = delete_connection(
            data(allowing_context()),
            authed_request(),
            web::Path::from(connection.id),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert!(connections_store().contains_key(&connection.id));
        // With the history gone, the delete succeeds.
        transactions_store().remove(&transaction.id);
        let response = delete_connection(
            data(allowing_context()),
            authed_request(),
            web::Path::from(connection.id),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!connections_store().contains_key(&connection.id));
    }

    #[test]
    fn idempotency_claim_is_atomic_and_releasable() {
        let connection_id = Uuid::now_v7();
        let key = format!("claim-test-{}", Uuid::now_v7());
        // The first claim wins; a second claim for the same key
        // loses, so a concurrent duplicate cannot charge twice.
        let sentinel = claim_idempotency(connection_id, &key).expect("first claim wins");
        assert!(claim_idempotency(connection_id, &key).is_none());
        // A different key on the same connection is unaffected.
        let other_key = format!("{key}-other");
        assert!(claim_idempotency(connection_id, &other_key).is_some());
        // Releasing with the wrong id does not drop the claim.
        release_idempotency_claim(connection_id, &key, Uuid::now_v7());
        assert!(claim_idempotency(connection_id, &key).is_none());
        // Releasing with the sentinel frees the key.
        release_idempotency_claim(connection_id, &key, sentinel);
        assert!(claim_idempotency(connection_id, &key).is_some());
        idempotency_store().remove(&(connection_id, key));
        idempotency_store().remove(&(connection_id, other_key));
    }

    #[tokio::test]
    async fn wait_for_idempotency_returns_the_recorded_transaction() {
        let connection_id = Uuid::now_v7();
        let key = format!("wait-test-{}", Uuid::now_v7());
        let transaction = record_transaction(new_transaction(
            connection_id,
            "stripe",
            "charge",
            "cust-wait",
            None,
            None,
            1000,
            "USD",
            PaymentStatus::Captured,
            "prov-wait".to_string(),
            key.clone(),
        ));
        let found = wait_for_idempotency(connection_id, &key).await;
        assert_eq!(found.map(|found| found.id), Some(transaction.id));
        transactions_store().remove(&transaction.id);
        idempotency_store().remove(&(connection_id, key));
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let req = attach_tenant_context(TestRequest::default().to_http_request());
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
    }

    #[test]
    fn replay_for_customer_returns_only_the_own_transaction() {
        let own = new_transaction(
            Uuid::now_v7(),
            "stripe",
            "charge",
            "cust-a",
            None,
            None,
            1000,
            "USD",
            PaymentStatus::Captured,
            "prov-own".to_string(),
            format!("replay-{}", Uuid::now_v7()),
        );
        assert_eq!(
            replay_for_customer(own.clone(), "cust-a").unwrap().id,
            own.id
        );
        // Another customer's replay of the same key must not hand
        // over the first customer's transaction.
        assert!(replay_for_customer(own, "cust-b").is_err());
    }
}
