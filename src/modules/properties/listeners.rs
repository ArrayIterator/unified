// Properties — Hook listeners (payment / order events).

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use uuid::Uuid;

use super::*;


// Module notes moved here from the original file header
// (kept verbatim so no documentation is lost):

// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory property, plan, block, history, and purchase
// management; GeoJSON geometry validated server-side (only
// Polygon / MultiPolygon / LineString, bounded vertices, closed
// rings, coordinates in range) with area and length computed
// server-side from an equirectangular projection (an approximation
// suited to site plans, not a survey-grade geodesic measurement);
// a block status machine (available -> reserved -> sold / assigned,
// reserved -> available, blocked admin-only, sold terminal except
// audited admin correction) with a history row per transition;
// server-generated SVG from stored geometry (status colours,
// labels, viewBox fitted to the plan bounds) and a print page
// (HTML + inline SVG + per-block table + print stylesheet); a
// pluggable geocoder provider interface with one built-in HTTP
// adapter for a Nominatim-compatible endpoint plus a `none`
// provider; and a buy flow that emits an event for the store
// module and reacts to the payment events (see the buy-flow
// contract below). What is NOT built here: TRUE 3D VISUALIZATION
// — this is a 2D SVG site-plan editor only; a 3D view (extruded
// plots, terrain, a WebGL viewer) is a future item, recorded in
// the builder report, and nothing in this module pretends it
// exists. There is also no built-in geocoding database: location
// search needs a configured provider (see the location note).
// No payment collection lives here (payments, store, and billing
// own it — this module only reacts to their hook events), and no
// image bytes live here (media is referenced through the storage
// module by target + key, exactly like the events module's cover
// image). The map picker itself is client-side: the server
// accepts and validates coordinates and serves stored geometry.
//
// Location note: the geo module in this codebase resolves IP
// addresses (MaxMind database / settings overrides) and has NO
// address geocoder, so this module does not pretend one exists.
// `LocationProvider` is the pluggable interface
// (`search(query) -> Vec<PlaceCandidate>`,
// `reverse_geocode(lat, lng) -> Option<PlaceCandidate>`). The
// built-in `NominatimProvider` calls a Nominatim-compatible HTTP
// endpoint whose base URL is admin-configured; the host must match
// the configured allowlisted host exactly, the scheme must be
// HTTPS, requests carry a fixed User-Agent and a bounded timeout,
// and no API key is ever sent or logged (Nominatim needs none).
// The `none` provider (the default) accepts manual coordinates
// only and answers search / reverse with a clear "not configured"
// error instead of a fake result.
//
// Buy-flow contract (the store module owns checkout):
//   1. `POST .../blocks/{block_id}/purchase` requires the block
//      to be `available`. It reserves the block (status `reserved`,
//      buyer recorded), stores a purchase record, and emits
//      `properties.block.purchase_requested` with
//      `{purchase_id, property_id, plan_id, block_id, buyer_id,
//      price, currency}`. It does NOT create an order and does NOT
//      charge anything.
//   2. The store side (see the integrator snippet) listens to
//      `properties.block.purchase_requested`, creates the order,
//      and emits `properties.order.created` with
//      `{purchase_id, order_id, block_id, property_id, buyer_id,
//      total, currency}`. This module listens to that event and
//      links the order id to the purchase record.
//   3. On `payment.captured` for that order (gateway captures must
//      cover the block price; bank-transfer captures skip the
//      amount check, mirroring the store module), the block moves
//      to `sold` and `properties.block.sold` is emitted.
//   4. On `bank_transfer.verification.created` for that order the
//      purchase moves to `pending_verification` and
//      `properties.block.pending_verification` is emitted (the
//      block stays `reserved` while verification is pending).
//   5. On `bank_transfer.rejected` for that order the block is
//      released back to `available` and
//      `properties.block.released` is emitted.
//   A purchase whose order is never created leaves the block
//   `reserved`; releasing such a stale reservation is a manual
//   admin action today (a scheduler sweep is a future item).
//
// Public-access gate: exactly like the events module, the API
// Manager module enforces public access through its
// admin-managed `ApiAccessRule` records (default deny). The
// public routes below rely on that gate: an administrator must
// create the matching `ApiAccessRule` rows for
// `GET /api/v1/properties/public/*`. Do NOT treat the absence of
// a capability check on a public route as an open door: without
// the rule the API Manager denies the call. Public responses
// only ever contain `published` properties and never expose the
// buyer id or order id of a block.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the events module. Capability checks use
// the `role.check` hook and fail closed (no answer means not
// allowed).
//
// Events emitted on the hook bus:
//   - `properties.created`, `properties.updated`,
//     `properties.deleted`, `properties.published`,
//     `properties.archived`, `properties.sold_out`
//   - `properties.plan.created`, `properties.plan.updated`,
//     `properties.plan.deleted`
//   - `properties.block.created`, `properties.block.updated`,
//     `properties.block.deleted`
//   - `properties.block.reserved`, `properties.block.released`,
//     `properties.block.sold`, `properties.block.assigned`,
//     `properties.block.blocked`, `properties.block.unblocked`,
//     `properties.block.status_corrected`,
//     `properties.block.pending_verification`
//   - `properties.block.purchase_requested`
//   - `properties.location.settings_updated`
//   - `notification.requested` (reservation, sale, assignment)
//     with the notifications module's payload shape.
// Events this module listens to:
//   - `properties.order.created` — links a store order id to a
//     purchase record (emitted by the store-side purchase
//     handler; see the buy-flow contract above).
//   - `payment.captured` — marks the purchased block `sold`
//     (gateway captures must cover the block price; bank-transfer
//     captures skip the amount check; only purchases in
//     `requested` / `order_created` / `pending_verification` may
//     move to sold; already-sold is a safe no-op).
//   - `bank_transfer.verification.created` — moves the purchase
//     to `pending_verification` (the block stays `reserved`).
//   - `bank_transfer.rejected` — releases the block back to
//     `available`.

// ---------------------------------------------------------------------------
// Hook listeners (payment / order events)
// ---------------------------------------------------------------------------

/// Link a store order id to a purchase record. The store side
/// emits `properties.order.created` after creating the order for
/// a `properties.block.purchase_requested` (see the buy-flow
/// contract in the module header). Payload:
/// `{purchase_id, order_id, block_id, property_id, buyer_id,
/// total, currency}`.
pub(crate) fn on_properties_order_created(state: Arc<PropertiesState>, ctx: &mut HookContext) {
    let purchase_id = ctx
        .get("purchase_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let order_id = ctx
        .get("order_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let (Some(purchase_id), Some(order_id)) = (purchase_id, order_id) else {
        ctx.add_result(json!({
            "properties": "skipped",
            "reason": "properties.order.created carries no purchase_id or order_id",
        }));
        return;
    };
    let Some(mut purchase) = state
        .purchases
        .get_mut(&purchase_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({
            "properties": "skipped",
            "purchase_id": purchase_id.to_string(),
            "reason": "purchase not found",
        }));
        return;
    };
    if purchase.status != "requested" {
        ctx.add_result(json!({
            "properties": "already_linked",
            "purchase_id": purchase_id.to_string(),
            "status": purchase.status,
        }));
        return;
    }
    // The payload must describe this purchase: a block_id or
    // buyer_id that is present but belongs to another purchase is
    // a crossed event, not this purchase's order.
    if let Some(raw_block_id) = ctx.get("block_id").and_then(|value| value.as_str()) {
        let matches_purchase = Uuid::parse_str(raw_block_id)
            .map(|block_id| block_id == purchase.block_id)
            .unwrap_or(false);
        if !matches_purchase {
            ctx.add_result(json!({
                "properties": "skipped",
                "purchase_id": purchase_id.to_string(),
                "reason": "properties.order.created block_id does not match the purchase",
            }));
            return;
        }
    }
    if let Some(buyer_id) = ctx.get("buyer_id").and_then(|value| value.as_str()) {
        if buyer_id != purchase.buyer_id {
            ctx.add_result(json!({
                "properties": "skipped",
                "purchase_id": purchase_id.to_string(),
                "reason": "properties.order.created buyer_id does not match the purchase",
            }));
            return;
        }
    }
    purchase.order_id = Some(order_id.clone());
    purchase.status = "order_created".to_string();
    purchase.updated_at = Utc::now();
    state.purchases.insert(purchase.id, purchase);
    ctx.add_result(json!({
        "properties": "order_linked",
        "purchase_id": purchase_id.to_string(),
        "order_id": order_id,
    }));
}

/// Find the purchase for a payment event: by `order_id` first
/// (top-level, or nested under `order.id`), then by a top-level
/// `purchase_id` when the emitter forwards it.
pub(crate) fn find_purchase_for_payment(state: &PropertiesState, ctx: &HookContext) -> Option<BlockPurchase> {
    let order_id = ctx
        .get("order_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("order")
                .and_then(|order| order.get("id"))
                .and_then(|id| id.as_str())
                .map(str::to_string)
        });
    if let Some(order_id) = order_id {
        if let Some(purchase) = state
            .purchases
            .iter()
            .map(|entry| entry.value().clone())
            .find(|purchase| purchase.order_id.as_deref() == Some(order_id.as_str()))
        {
            return Some(purchase);
        }
    }
    let purchase_id = ctx
        .get("purchase_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    if let Some(purchase_id) = purchase_id {
        if let Some(purchase) = state
            .purchases
            .get(&purchase_id)
            .map(|entry| entry.value().clone())
        {
            return Some(purchase);
        }
    }
    None
}

/// Zero-decimal currencies (ISO 4217): the minor unit is the
/// major unit, so no division by 100 applies.
///
/// KNOWN DEVIATION, kept deliberately: this list also treats MGA as
/// zero-decimal, but ISO 4217 lists the Malagasy ariary with 2 minor
/// digits, and the central catalogue
/// (`unified_core::reference_data::currencies`) agrees. The local list
/// stays until the coordinator confirms the MGA behaviour change for
/// property purchases -- do not "fix" it by delegating to the central
/// `is_zero_decimal_currency` without that confirmation, and do not
/// copy this list anywhere else.
pub(crate) fn is_zero_decimal_currency(currency: &str) -> bool {
    matches!(
        currency.to_uppercase().as_str(),
        "BIF" | "CLP" | "DJF" | "GNF" | "ISK" | "JPY" | "KMF" | "KRW" | "MGA" | "PYG"
            | "RWF" | "UGX" | "VND" | "VUV" | "XAF" | "XOF" | "XPF"
    )
}

pub(crate) fn minor_to_major(amount_minor: i64, currency: &str) -> f64 {
    if is_zero_decimal_currency(currency) {
        amount_minor as f64
    } else {
        amount_minor as f64 / 100.0
    }
}

/// An approved payment (`payment.captured`, gateway or bank
/// transfer) moves the purchased block to `sold`. Only purchases
/// in `requested` / `order_created` / `pending_verification` may
/// move to sold — never `rejected` or `cancelled` — and a block
/// that is already sold is left alone, so redelivered captures
/// are safe. Gateway captures must cover the block price;
/// bank-transfer captures skip the amount check (the
/// administrator already verified the amount when approving the
/// transfer, mirroring the store module).
pub(crate) fn on_payment_captured(state: Arc<PropertiesState>, ctx: &mut HookContext) {
    if let Some(status) = ctx.get("status").and_then(|value| value.as_str()) {
        if status != "captured" {
            ctx.add_result(json!({
                "properties": "skipped",
                "reason": format!("payment.captured carries status \"{status}\", not \"captured\""),
            }));
            return;
        }
    }
    let Some(purchase) = find_purchase_for_payment(&state, ctx) else {
        ctx.add_result(json!({
            "properties": "skipped",
            "reason": "payment.captured carries no order id matching a block purchase",
        }));
        return;
    };
    if purchase.status == "paid" {
        ctx.add_result(json!({
            "properties": "already_paid",
            "purchase_id": purchase.id.to_string(),
        }));
        return;
    }
    if !matches!(
        purchase.status.as_str(),
        "requested" | "order_created" | "pending_verification"
    ) {
        ctx.add_result(json!({
            "properties": "skipped",
            "purchase_id": purchase.id.to_string(),
            "reason": format!("purchase in status {} can never move to paid", purchase.status),
        }));
        return;
    }
    let provider = ctx
        .get("provider")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    if provider.as_deref() != Some("bank_transfer") {
        let (Some(amount_minor), Some(event_currency)) = (
            ctx.get("amount_minor").and_then(|value| value.as_i64()),
            ctx.get("currency").and_then(|value| value.as_str()),
        ) else {
            ctx.add_result(json!({
                "properties": "skipped",
                "purchase_id": purchase.id.to_string(),
                "reason": "captured amount missing",
            }));
            return;
        };
        if !event_currency.eq_ignore_ascii_case(&purchase.currency) {
            ctx.add_result(json!({
                "properties": "skipped",
                "purchase_id": purchase.id.to_string(),
                "reason": format!(
                    "captured currency {event_currency} does not match purchase currency {}",
                    purchase.currency
                ),
            }));
            return;
        }
        if amount_minor < purchase.price {
            ctx.add_result(json!({
                "properties": "skipped",
                "purchase_id": purchase.id.to_string(),
                "reason": "captured amount does not cover the block price",
            }));
            return;
        }
    }
    let Some(mut block) = state.blocks.get_mut(&purchase.block_id).map(|entry| entry.value().clone()) else {
        ctx.add_result(json!({
            "properties": "skipped",
            "purchase_id": purchase.id.to_string(),
            "reason": "purchased block not found",
        }));
        return;
    };
    if block.status == BlockStatus::Sold {
        ctx.add_result(json!({
            "properties": "already_sold",
            "block_id": block.id.to_string(),
        }));
        return;
    }
    if block.status != BlockStatus::Reserved && block.status != BlockStatus::Assigned {
        ctx.add_result(json!({
            "properties": "skipped",
            "block_id": block.id.to_string(),
            "reason": format!("block in status {} cannot move to sold", block.status.as_str()),
        }));
        return;
    }
    let mut purchase = purchase;
    purchase.status = "paid".to_string();
    purchase.updated_at = Utc::now();
    state.purchases.insert(purchase.id, purchase.clone());
    block.order_id = purchase.order_id.clone();
    block.assigned_customer_id = Some(purchase.buyer_id.clone());
    move_block_status(
        &state,
        &mut block,
        BlockStatus::Sold,
        "system:payment",
        Some("payment captured".to_string()),
        purchase.order_id.clone(),
        "properties.block.sold",
    );
    state.blocks.insert(block.id, block.clone());
    state.notify(
        "properties.block.sold",
        &purchase.buyer_id,
        &format!("Block \"{}\" sold", block.title),
        &format!(
            "Your purchase of block \"{}\" is confirmed. Price: {} {}.",
            block.title,
            format_money(block.price, &block.currency),
            block.currency
        ),
        Some(format!("/api/v1/properties/{}", block.property_id)),
    );
    ctx.add_result(json!({
        "properties": "sold",
        "purchase_id": purchase.id.to_string(),
        "block_id": block.id.to_string(),
    }));
}

/// A bank-transfer verification moves the purchase to
/// `pending_verification`; the block stays `reserved` while the
/// administrator verifies the transfer.
pub(crate) fn on_bank_transfer_verification_created(state: Arc<PropertiesState>, ctx: &mut HookContext) {
    let Some(purchase) = find_purchase_for_payment(&state, ctx) else {
        ctx.add_result(json!({
            "properties": "skipped",
            "reason": "bank_transfer.verification.created carries no order id matching a block purchase",
        }));
        return;
    };
    if !matches!(purchase.status.as_str(), "requested" | "order_created") {
        ctx.add_result(json!({
            "properties": "skipped",
            "purchase_id": purchase.id.to_string(),
            "reason": format!("purchase in status {} cannot move to pending_verification", purchase.status),
        }));
        return;
    }
    let mut purchase = purchase;
    purchase.status = "pending_verification".to_string();
    purchase.updated_at = Utc::now();
    state.purchases.insert(purchase.id, purchase.clone());
    state.emit(
        "properties.block.pending_verification",
        json!({
            "property_id": purchase.property_id.to_string(),
            "plan_id": purchase.plan_id.to_string(),
            "block_id": purchase.block_id.to_string(),
            "purchase_id": purchase.id.to_string(),
            "order_id": purchase.order_id,
            "buyer_id": purchase.buyer_id,
        }),
    );
    ctx.add_result(json!({
        "properties": "pending_verification",
        "purchase_id": purchase.id.to_string(),
    }));
}

/// A rejected bank transfer releases the block back to
/// `available` so it can be sold again. This applies to both
/// `reserved` blocks (payment in flight) and `assigned` blocks
/// (manager-assigned while the transfer was pending).
pub(crate) fn on_bank_transfer_rejected(state: Arc<PropertiesState>, ctx: &mut HookContext) {
    let Some(purchase) = find_purchase_for_payment(&state, ctx) else {
        ctx.add_result(json!({
            "properties": "skipped",
            "reason": "bank_transfer.rejected carries no order id matching a block purchase",
        }));
        return;
    };
    if !matches!(
        purchase.status.as_str(),
        "requested" | "order_created" | "pending_verification"
    ) {
        ctx.add_result(json!({
            "properties": "skipped",
            "purchase_id": purchase.id.to_string(),
            "reason": format!("purchase in status {} cannot be released", purchase.status),
        }));
        return;
    }
    let Some(mut block) = state.blocks.get_mut(&purchase.block_id).map(|entry| entry.value().clone()) else {
        ctx.add_result(json!({
            "properties": "skipped",
            "purchase_id": purchase.id.to_string(),
            "reason": "purchased block not found",
        }));
        return;
    };
    if block.status == BlockStatus::Reserved || block.status == BlockStatus::Assigned {
        let mut purchase = purchase;
        purchase.status = "rejected".to_string();
        purchase.updated_at = Utc::now();
        state.purchases.insert(purchase.id, purchase.clone());
        block.assigned_customer_id = None;
        block.order_id = None;
        move_block_status(
            &state,
            &mut block,
            BlockStatus::Available,
            "system:bank_transfer",
            Some("bank transfer rejected".to_string()),
            purchase.order_id.clone(),
            "properties.block.released",
        );
        state.blocks.insert(block.id, block);
        ctx.add_result(json!({
            "properties": "released",
            "purchase_id": purchase.id.to_string(),
        }));
    } else {
        ctx.add_result(json!({
            "properties": "skipped",
            "purchase_id": purchase.id.to_string(),
            "reason": format!("block in status {} cannot be released", block.status.as_str()),
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_block(state: &PropertiesState, status: BlockStatus, price: i64) -> PropertyBlock {
        let now = Utc::now();
        let block = PropertyBlock {
            id: Uuid::now_v7(),
            plan_id: Uuid::now_v7(),
            property_id: Uuid::now_v7(),
            title: "Block A-1".to_string(),
            notes: None,
            geometry: serde_json::json!({"type": "Polygon", "coordinates": []}),
            geometry_hash: String::new(),
            area_sqm: None,
            length_m: None,
            price,
            currency: "USD".to_string(),
            status,
            assigned_customer_id: Some("buyer-1".to_string()),
            click_behavior: ClickBehavior::default(),
            click_link_kind: None,
            click_link_target: None,
            order_id: None,
            created_at: now,
            updated_at: now,
        };
        state.blocks.insert(block.id, block.clone());
        block
    }

    fn seed_purchase(
        state: &PropertiesState,
        block: &PropertyBlock,
        status: &str,
        order_id: Option<&str>,
    ) -> BlockPurchase {
        let now = Utc::now();
        let purchase = BlockPurchase {
            id: Uuid::now_v7(),
            property_id: block.property_id,
            plan_id: block.plan_id,
            block_id: block.id,
            buyer_id: "buyer-1".to_string(),
            price: block.price,
            currency: block.currency.clone(),
            order_id: order_id.map(str::to_string),
            status: status.to_string(),
            created_at: now,
            updated_at: now,
        };
        state.purchases.insert(purchase.id, purchase.clone());
        purchase
    }

    fn result_reason(ctx: &HookContext, reason: &str) -> bool {
        ctx.results
            .iter()
            .any(|result| result.get("reason").and_then(|value| value.as_str()) == Some(reason))
    }

    #[test]
    fn minor_to_major_converts_two_decimal_currencies() {
        assert_eq!(minor_to_major(10050, "USD"), 100.5);
    }

    #[test]
    fn minor_to_major_keeps_zero_decimal_currencies_whole() {
        assert_eq!(minor_to_major(10050, "JPY"), 10050.0);
        assert_eq!(minor_to_major(10050, "jpy"), 10050.0);
        assert_eq!(minor_to_major(10050, "BIF"), 10050.0);
        assert_eq!(minor_to_major(10050, "VUV"), 10050.0);
    }

    #[test]
    fn zero_decimal_currency_list() {
        // Full ISO 4217 zero-decimal set.
        for currency in [
            "BIF", "CLP", "DJF", "GNF", "ISK", "JPY", "KMF", "KRW", "MGA", "PYG", "RWF",
            "UGX", "VND", "VUV", "XAF", "XOF", "XPF",
        ] {
            assert!(is_zero_decimal_currency(currency), "{currency} must be zero-decimal");
            assert!(
                is_zero_decimal_currency(&currency.to_lowercase()),
                "{currency} must match case-insensitively"
            );
        }
        assert!(!is_zero_decimal_currency("USD"));
        assert!(!is_zero_decimal_currency("IDR"));
        assert!(!is_zero_decimal_currency("EUR"));
        assert!(!is_zero_decimal_currency("GBP"));
    }

    #[test]
    fn payment_captured_without_amount_is_skipped() {
        let state = Arc::new(PropertiesState::default());
        let block = seed_block(&state, BlockStatus::Reserved, 10000);
        let purchase = seed_purchase(&state, &block, "order_created", Some("ord-2"));
        let mut ctx = HookContext::new(
            "payment.captured",
            serde_json::json!({
                "status": "captured",
                "provider": "stripe",
                "order_id": "ord-2",
            }),
        );
        on_payment_captured(Arc::clone(&state), &mut ctx);
        assert!(result_reason(&ctx, "captured amount missing"));
        assert_eq!(
            state.purchases.get(&purchase.id).unwrap().status,
            "order_created"
        );
        assert_eq!(
            state.blocks.get(&block.id).unwrap().status,
            BlockStatus::Reserved
        );
    }

    #[test]
    fn payment_captured_with_covering_amount_still_sells() {
        let state = Arc::new(PropertiesState::default());
        let block = seed_block(&state, BlockStatus::Reserved, 10000);
        let purchase = seed_purchase(&state, &block, "order_created", Some("ord-2b"));
        let mut ctx = HookContext::new(
            "payment.captured",
            serde_json::json!({
                "status": "captured",
                "provider": "stripe",
                "order_id": "ord-2b",
                "amount_minor": 1_000_000,
                "currency": "USD",
            }),
        );
        on_payment_captured(Arc::clone(&state), &mut ctx);
        assert_eq!(state.purchases.get(&purchase.id).unwrap().status, "paid");
        assert_eq!(
            state.blocks.get(&block.id).unwrap().status,
            BlockStatus::Sold
        );
    }

    #[test]
    fn order_created_with_mismatched_block_id_is_skipped() {
        let state = Arc::new(PropertiesState::default());
        let block = seed_block(&state, BlockStatus::Reserved, 10000);
        let purchase = seed_purchase(&state, &block, "requested", None);
        let mut ctx = HookContext::new(
            "properties.order.created",
            serde_json::json!({
                "purchase_id": purchase.id.to_string(),
                "order_id": "ord-9",
                "block_id": Uuid::now_v7().to_string(),
                "buyer_id": "buyer-1",
            }),
        );
        on_properties_order_created(Arc::clone(&state), &mut ctx);
        assert!(result_reason(
            &ctx,
            "properties.order.created block_id does not match the purchase"
        ));
        let stored = state.purchases.get(&purchase.id).unwrap();
        assert_eq!(stored.status, "requested");
        assert!(stored.order_id.is_none());
    }

    #[test]
    fn order_created_with_mismatched_buyer_id_is_skipped() {
        let state = Arc::new(PropertiesState::default());
        let block = seed_block(&state, BlockStatus::Reserved, 10000);
        let purchase = seed_purchase(&state, &block, "requested", None);
        let mut ctx = HookContext::new(
            "properties.order.created",
            serde_json::json!({
                "purchase_id": purchase.id.to_string(),
                "order_id": "ord-9",
                "block_id": block.id.to_string(),
                "buyer_id": "someone-else",
            }),
        );
        on_properties_order_created(Arc::clone(&state), &mut ctx);
        assert!(result_reason(
            &ctx,
            "properties.order.created buyer_id does not match the purchase"
        ));
        let stored = state.purchases.get(&purchase.id).unwrap();
        assert_eq!(stored.status, "requested");
        assert!(stored.order_id.is_none());
    }

    #[test]
    fn bank_transfer_rejected_releases_assigned_block() {
        let state = Arc::new(PropertiesState::default());
        let block = seed_block(&state, BlockStatus::Assigned, 10000);
        let purchase = seed_purchase(&state, &block, "order_created", Some("ord-10"));
        let mut ctx = HookContext::new(
            "bank_transfer.rejected",
            serde_json::json!({ "order_id": "ord-10" }),
        );
        on_bank_transfer_rejected(Arc::clone(&state), &mut ctx);
        assert_eq!(state.purchases.get(&purchase.id).unwrap().status, "rejected");
        let stored = state.blocks.get(&block.id).unwrap();
        assert_eq!(stored.status, BlockStatus::Available);
        assert!(stored.assigned_customer_id.is_none());
        assert!(stored.order_id.is_none());
    }

    #[test]
    fn bank_transfer_rejected_still_releases_reserved_block() {
        let state = Arc::new(PropertiesState::default());
        let block = seed_block(&state, BlockStatus::Reserved, 10000);
        let purchase = seed_purchase(&state, &block, "pending_verification", Some("ord-10b"));
        let mut ctx = HookContext::new(
            "bank_transfer.rejected",
            serde_json::json!({ "order_id": "ord-10b" }),
        );
        on_bank_transfer_rejected(Arc::clone(&state), &mut ctx);
        assert_eq!(state.purchases.get(&purchase.id).unwrap().status, "rejected");
        assert_eq!(
            state.blocks.get(&block.id).unwrap().status,
            BlockStatus::Available
        );
    }
}
