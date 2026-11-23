//! Logistics module — shipments, fleet, warehouses, delivery — Public tracking (no auth — token-gated, rate-limited; see header).
//!
//! Split from `src/modules/logistics.rs`; behaviour unchanged.

use super::constants::*;
use super::delivery_routes::*;
use super::drivers::*;
use super::helpers::*;
use super::hooks::*;
use super::module::*;
use super::rates::*;
use super::requests::*;
use super::shipments::*;
use super::state::*;
use super::types::*;
use super::vehicles::*;
use super::warehouses::*;

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;

pub(super) async fn track_shipment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<TrackQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tracking_number = path.into_inner();
        let token = query
            .token
            .clone()
            .unwrap_or_default();
        if token.trim().is_empty() {
            return Err(UnifiedError::unauthorized(
                "A verification token is required (?token=...)",
            ));
        }
        // Rate limit before any token check, so a wrong token cannot
        // be probed without bound. The bucket key binds the client
        // address to the tracking number. The address trusts
        // `X-Forwarded-For` only from a configured trusted proxy
        // (see `client_address`), so a direct client cannot rotate
        // the header to evade this limit.
        let settings = ctx
            .config
            .read()
            .expect("config lock poisoned")
            .security
            .clone();
        let address = client_address(&req, &settings);
        let bucket_key = format!("{address}|{tracking_number}");
        let now = Utc::now();
        let mut allowed = true;
        if let Some(mut bucket) = state.track_buckets.get_mut(&bucket_key) {
            let (count, window_start) = bucket.value_mut();
            if now.signed_duration_since(*window_start).num_seconds() >= TRACK_RATE_WINDOW_SECS {
                *count = 1;
                *window_start = now;
            } else if *count >= TRACK_RATE_LIMIT {
                allowed = false;
            } else {
                *count += 1;
            }
        } else {
            state
                .track_buckets
                .insert(bucket_key, (1, now));
        }
        if !allowed {
            return Err(UnifiedError::new(
                format!(
                    "Too many tracking lookups for this shipment; try again in {TRACK_RATE_WINDOW_SECS} seconds"
                ),
                429,
            ));
        }
        let shipment = state
            .shipments
            .iter()
            .map(|entry| entry.value().clone())
            .find(|shipment| shipment.tracking_number == tracking_number)
            .ok_or_else(|| {
                UnifiedError::not_found(format!("No shipment with tracking number {tracking_number}"))
            })?;
        if sha256_hex(token.trim()) != shipment.tracking_token_hash {
            return Err(UnifiedError::unauthorized(
                "The verification token does not match this tracking number",
            ));
        }
        // Public view: status and location history only. No
        // recipient name, addresses, phone data, cost, order id,
        // driver, or vehicle is exposed here.
        let mut timeline: Vec<ShipmentTimelineEntry> = state
            .timeline
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.shipment_id == shipment.id)
            .collect();
        timeline.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        let locations: Vec<Value> = timeline
            .iter()
            .map(|entry| {
                json!({
                    "at": entry.created_at.to_rfc3339(),
                    "status": entry.to_status,
                    "location": entry.location,
                })
            })
            .collect();
        Ok(json!({
            "tracking_number": shipment.tracking_number,
            "status": shipment.status.as_str(),
            "estimated_delivery_at": shipment.estimated_delivery_at.map(|value| value.to_rfc3339()),
            "scheduled_at": shipment.scheduled_at.map(|value| value.to_rfc3339()),
            "locations": locations,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
