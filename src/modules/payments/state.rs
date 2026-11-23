//! payments - state.

use unified_core::hooks::{HookBus};
use serde_json::{Value};
use std::sync::{Arc, RwLock};
use super::{events::*, card_validation::*, card_settings::*, limits::*, providers::*, types::*, reference_ids::*, credentials::*, records::*, adapter::*, dispatch::*, stripe::*, paypal::*, xendit::*, midtrans::*, hyperswitch::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};



// ---------------------------------------------------------------------------
// Module state.
// ---------------------------------------------------------------------------
pub struct PaymentsState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
}


impl PaymentsState {
    pub(crate) fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
        }
    }

    pub(crate) fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }
}
