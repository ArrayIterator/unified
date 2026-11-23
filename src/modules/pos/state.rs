// Pos module — shared state.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, RwLock,
};
use uuid::Uuid;
use super::*;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for POS. The hook bus is attached in `init`
/// (the bus lives in `AppContext`); handlers tolerate a missing bus
/// by skipping event emission. Durable rows for these types are
/// defined by migration `2026-11-22-000074_pos`; until that
/// migration is executed, POS records do not survive a restart.
#[derive(Clone)]
pub struct PosState {
    pub registers: Arc<DashMap<String, PosRegister>>,
    pub shifts: Arc<DashMap<String, PosShift>>,
    pub sales: Arc<DashMap<String, PosSale>>,
    pub payments: Arc<DashMap<String, PosPayment>>,
    pub receipts: Arc<DashMap<String, PosReceipt>>,
    pub refunds: Arc<DashMap<String, PosRefund>>,
    pub payment_methods: Arc<DashMap<String, PosPaymentMethod>>,
    pub settings: Arc<RwLock<PosSettings>>,
    pub sale_seq: Arc<AtomicU64>,
    pub receipt_seq: Arc<AtomicU64>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for PosState {
    fn default() -> Self {
        Self {
            registers: Arc::new(DashMap::new()),
            shifts: Arc::new(DashMap::new()),
            sales: Arc::new(DashMap::new()),
            payments: Arc::new(DashMap::new()),
            receipts: Arc::new(DashMap::new()),
            refunds: Arc::new(DashMap::new()),
            payment_methods: Arc::new(DashMap::new()),
            settings: Arc::new(RwLock::new(PosSettings::default())),
            sale_seq: Arc::new(AtomicU64::new(1)),
            receipt_seq: Arc::new(AtomicU64::new(1)),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl PosState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    pub(crate) fn settings(&self) -> PosSettings {
        self.settings
            .read()
            .expect("pos settings lock poisoned")
            .clone()
    }

    pub(crate) fn next_sale_number(&self) -> String {
        let seq = self.sale_seq.fetch_add(1, Ordering::SeqCst);
        format!("POS-{seq:06}")
    }

    pub(crate) fn next_receipt_number(&self) -> String {
        let seq = self.receipt_seq.fetch_add(1, Ordering::SeqCst);
        format!("RCP-{seq:06}")
    }

    /// Seed the payment-method definitions with configurable
    /// defaults on first use. These are starting values an admin
    /// edits or deletes through the payment-method endpoints; the
    /// sale flow never assumes any of them exists and validates
    /// every payment against the stored definitions.
    pub(crate) fn seed_payment_methods(&self) {
        if !self.payment_methods.is_empty() {
            return;
        }
        let created = now();
        let defaults: [(&str, &str, &str, bool, bool); 4] = [
            ("cash", "Cash", "cash", true, false),
            ("card", "Card", "card", false, true),
            ("bank_transfer", "Bank Transfer", "transfer", false, true),
            ("e_wallet", "E-Wallet", "wallet", false, true),
        ];
        for (code, label, kind, is_cash, requires_reference) in defaults {
            let method = PosPaymentMethod {
                id: new_id(),
                code: code.to_string(),
                label: label.to_string(),
                kind: kind.to_string(),
                is_cash,
                requires_reference,
                active: true,
                created_at: created,
                updated_at: created,
            };
            self.payment_methods.insert(method.id.clone(), method);
        }
    }
}
