//! Hook handlers answering cross-module product questions.

use unified_core::hooks::{HookContext};
use unified_core::json;
use serde_json::{json};
use std::sync::{Arc};
use super::state::ProductsState;

// ---------------------------------------------------------------------------
// Hook handlers: this module answers other modules' questions.
// ---------------------------------------------------------------------------

/// Answer `product.entitlement.check` with payload
/// `{"product_id", "user_id"}` and result `{"entitled": bool, ...}`.
/// The check itself runs through the store's entitlement data via
/// `store.entitlement.check` (see `ProductsState::check_entitlement`).
pub(crate) fn on_product_entitlement_check(state: Arc<ProductsState>, ctx: &mut HookContext) {
    let product_id = ctx
        .get("product_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let user_id = ctx
        .get("user_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if product_id.trim().is_empty() || user_id.trim().is_empty() {
        ctx.add_result(json!({
            "entitled": false,
            "reason": "product_id and user_id are required",
        }));
        return;
    }
    ctx.add_result(state.check_entitlement(&product_id, &user_id));
}
