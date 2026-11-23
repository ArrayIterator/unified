// procurement -- Shared in-memory state.

use unified_core::hooks::HookBus;
use chrono::{Datelike, Utc};
use dashmap::DashMap;
use serde_json::{Value, json};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::types::*;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for procurement. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration
/// `2026-11-22-000049_procurement`; until that migration is
/// executed, procurement data does not survive a restart.
#[derive(Clone)]
pub struct ProcurementState {
    pub vendors: Arc<DashMap<Uuid, Vendor>>,
    pub purchase_orders: Arc<DashMap<Uuid, PurchaseOrder>>,
    pub lines: Arc<DashMap<Uuid, PurchaseOrderLine>>,
    pub approvals: Arc<DashMap<Uuid, PurchaseOrderApproval>>,
    pub receipts: Arc<DashMap<Uuid, GoodsReceipt>>,
    pub budgets: Arc<DashMap<Uuid, ProcurementBudget>>,
    pub po_counter: Arc<RwLock<u64>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for ProcurementState {
    fn default() -> Self {
        Self {
            vendors: Arc::new(DashMap::new()),
            purchase_orders: Arc::new(DashMap::new()),
            lines: Arc::new(DashMap::new()),
            approvals: Arc::new(DashMap::new()),
            receipts: Arc::new(DashMap::new()),
            budgets: Arc::new(DashMap::new()),
            po_counter: Arc::new(RwLock::new(0)),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl ProcurementState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Emit a `notification.requested` event in the notifications
    /// module's payload shape.
    pub(crate) fn notify(
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
                "module": "procurement",
                "category": "procurement",
                "priority": "normal",
                "link": link,
            }),
        );
    }

    /// Next unique purchase order number (`PO-YYYY-NNNNNN`).
    pub(crate) fn next_po_number(&self) -> String {
        let mut counter = self.po_counter.write().expect("po counter lock poisoned");
        *counter += 1;
        format!("PO-{}-{:06}", Utc::now().year(), *counter)
    }
}
