// bi module -- Hook event handlers.

use unified_core::hooks::HookContext;
use unified_core::json;
use serde_json::{json, Value};

use super::*;

pub(crate) fn on_invoice_payment(state: BiState, ctx: &mut HookContext) {
    mirror_event(&state, "billing", "payment", &ctx.data.clone());
}

pub(crate) fn on_payment_captured(state: BiState, ctx: &mut HookContext) {
    mirror_event(&state, "billing", "payment", &ctx.data.clone());
}

pub(crate) fn on_store_order_completed(state: BiState, ctx: &mut HookContext) {
    mirror_event(&state, "store", "order_completed", &ctx.data.clone());
}

pub(crate) fn on_store_order_refunded(state: BiState, ctx: &mut HookContext) {
    let mut data = ctx.data.clone();
    if let Some(amount) = payload_number(&data, &["amount", "total_amount", "total"]) {
        if let Value::Object(map) = &mut data {
            map.insert("amount".to_string(), json!(-amount));
        }
    }
    mirror_event(&state, "store", "order_refunded", &data);
}

pub(crate) fn on_ticket(state: BiState, kind: &str, ctx: &mut HookContext) {
    mirror_event(&state, "tickets", kind, &ctx.data.clone());
}

pub(crate) fn on_license(state: BiState, kind: &str, ctx: &mut HookContext) {
    mirror_event(&state, "licensing", kind, &ctx.data.clone());
}

pub(crate) fn on_project(state: BiState, kind: &str, ctx: &mut HookContext) {
    mirror_event(&state, "projects", kind, &ctx.data.clone());
}

pub(crate) fn on_task(state: BiState, kind: &str, ctx: &mut HookContext) {
    mirror_event(&state, "projects", kind, &ctx.data.clone());
}
