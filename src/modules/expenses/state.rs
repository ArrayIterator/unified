// Expenses state.

use unified_core::hooks::HookBus;
use dashmap::DashMap;
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// State

#[derive(Debug, Clone)]
pub struct ExpensesState {
    pub categories: Arc<DashMap<Uuid, ExpenseCategory>>,
    pub policies: Arc<DashMap<Uuid, ExpensePolicy>>,
    pub reports: Arc<DashMap<Uuid, ExpenseReport>>,
    pub expenses: Arc<DashMap<Uuid, Expense>>,
    pub receipts: Arc<DashMap<Uuid, ExpenseReceipt>>,
    pub reimbursements: Arc<DashMap<Uuid, Reimbursement>>,
    pub adjustments: Arc<DashMap<Uuid, ExpenseAdjustment>>,
    pub currency_rates: Arc<DashMap<Uuid, CurrencyRate>>,
    pub settings: Arc<RwLock<ExpensesSettings>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for ExpensesState {
    fn default() -> Self {
        Self {
            categories: Arc::new(DashMap::new()),
            policies: Arc::new(DashMap::new()),
            reports: Arc::new(DashMap::new()),
            expenses: Arc::new(DashMap::new()),
            receipts: Arc::new(DashMap::new()),
            reimbursements: Arc::new(DashMap::new()),
            adjustments: Arc::new(DashMap::new()),
            currency_rates: Arc::new(DashMap::new()),
            settings: Arc::new(RwLock::new(ExpensesSettings::default())),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl ExpensesState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    pub(crate) fn settings(&self) -> ExpensesSettings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
    }

    pub(crate) fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "expenses",
                "category": "expenses",
                "priority": "normal",
                "link": link,
            }),
        );
    }
}
