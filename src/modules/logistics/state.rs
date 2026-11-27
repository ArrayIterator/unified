//! Logistics module - shipments, fleet, warehouses, delivery - Shared state.
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
use super::tracking::*;
use super::types::*;
use super::vehicles::*;
use super::warehouses::*;

use unified_core::hooks::HookBus;
use unified_core::json;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// In-memory stores for logistics. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration `2026-11-22-000061_logistics`;
/// until that migration is executed, logistics data does not
/// survive a restart.
#[derive(Clone)]
pub struct LogisticsState {
    pub shipments: Arc<DashMap<Uuid, Shipment>>,
    pub timeline: Arc<DashMap<Uuid, ShipmentTimelineEntry>>,
    pub vehicles: Arc<DashMap<Uuid, Vehicle>>,
    pub drivers: Arc<DashMap<Uuid, Driver>>,
    pub warehouses: Arc<DashMap<Uuid, Warehouse>>,
    pub stock_movements: Arc<DashMap<Uuid, StockMovement>>,
    pub routes: Arc<DashMap<Uuid, DeliveryRoute>>,
    pub route_stops: Arc<DashMap<Uuid, RouteStop>>,
    pub proofs: Arc<DashMap<Uuid, ProofOfDelivery>>,
    pub exceptions: Arc<DashMap<Uuid, DeliveryException>>,
    pub rate_tables: Arc<DashMap<Uuid, RateTable>>,
    /// Public tracking rate-limit buckets, keyed by
    /// `client address + tracking number`: (requests in window,
    /// window start).
    pub track_buckets: Arc<DashMap<String, (u64, DateTime<Utc>)>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for LogisticsState {
    fn default() -> Self {
        Self {
            shipments: Arc::new(DashMap::new()),
            timeline: Arc::new(DashMap::new()),
            vehicles: Arc::new(DashMap::new()),
            drivers: Arc::new(DashMap::new()),
            warehouses: Arc::new(DashMap::new()),
            stock_movements: Arc::new(DashMap::new()),
            routes: Arc::new(DashMap::new()),
            route_stops: Arc::new(DashMap::new()),
            proofs: Arc::new(DashMap::new()),
            exceptions: Arc::new(DashMap::new()),
            rate_tables: Arc::new(DashMap::new()),
            track_buckets: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl LogisticsState {
    pub(super) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(super) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Emit a `notification.requested` event in the notifications
    /// module's payload shape.
    pub(super) fn notify(
        &self,
        kind: &str,
        user_id: &str,
        title: &str,
        body: &str,
        link: Option<String>,
    ) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "logistics",
                "category": "logistics",
                "priority": "normal",
                "link": link,
            }),
        );
    }

    pub(super) fn add_timeline(
        &self,
        shipment_id: Uuid,
        kind: &str,
        from_status: Option<&str>,
        to_status: Option<&str>,
        actor: &str,
        location: Option<String>,
        note: Option<String>,
    ) {
        let now = Utc::now();
        let entry = ShipmentTimelineEntry {
            id: Uuid::now_v7(),
            shipment_id,
            kind: kind.to_string(),
            from_status: from_status.map(str::to_string),
            to_status: to_status.map(str::to_string),
            actor: actor.to_string(),
            location,
            note,
            created_at: now,
        };
        self.timeline.insert(entry.id, entry.clone());
        self.emit(
            "logistics.shipment.timeline_added",
            json!({
                "shipment_id": shipment_id.to_string(),
                "timeline_id": entry.id.to_string(),
                "kind": kind,
                "from_status": from_status,
                "to_status": to_status,
                "actor": actor,
            }),
        );
    }

    /// On-hand quantity for one (warehouse, asset) pair, derived
    /// from the recorded movements. `in` adds, `out` subtracts.
    pub(super) fn stock_on_hand(&self, warehouse_id: Uuid, asset_id: Uuid) -> i64 {
        self.stock_movements
            .iter()
            .filter(|entry| {
                entry.value().warehouse_id == warehouse_id && entry.value().asset_id == asset_id
            })
            .map(|entry| {
                if entry.value().direction == "in" {
                    entry.value().quantity
                } else {
                    -entry.value().quantity
                }
            })
            .sum()
    }
}
