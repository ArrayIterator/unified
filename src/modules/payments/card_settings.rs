//! payments - card settings.

use unified_core::hooks::{HookContext};
use unified_core::json;
use serde_json::{json};
use super::{events::*, card_validation::*, limits::*, providers::*, types::*, reference_ids::*, credentials::*, records::*, adapter::*, dispatch::*, stripe::*, paypal::*, xendit::*, midtrans::*, hyperpay::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};




// ---------------------------------------------------------------------------
// Card settings: the gateway keeps none (single source of truth).
// ---------------------------------------------------------------------------
//
// The gateway's old `cards.*` / `reference_id.*` settings
// namespace (definitions, effective-value store, `config.validate`
// arms, `settings.changed` mirror) was DROPPED, not mirrored —
// see the module header and `on_config_validate_cards`. Card
// validation settings are owned per tenant by the cards module
// (`PUT /api/v1/cards/settings`); reference-id settings are owned
// by the store and billing modules. The only card-settings
// behaviour left in this module is the coarse expiry-year guard
// constant above, used by `create_payment_method`.
// ---------------------------------------------------------------------------
// Card / reference-id settings: the gateway owns none (refuse writes).
// ---------------------------------------------------------------------------
/// `config.validate` handler for the `cards.*` and
/// `reference_id.*` namespaces: every admin write to these
/// namespaces through the settings module is refused with 422.
///
/// Single-source-of-truth decision (batch-18 integration): the
/// cards module owns card validation settings per tenant
/// (`PUT /api/v1/cards/settings`: `test_mode` live/sandbox,
/// `max_expiry_years_ahead` 1-50, `gpn_enabled`,
/// `disabled_standard_brands`), and the store / billing modules
/// own reference-id settings (`PUT /api/v1/store/reference-settings`
/// and `PUT /api/v1/billing/reference-settings`). The gateway
/// keeps no settings surface for either namespace — its old
/// namespace was dropped, not mirrored (see the module header).
/// Accepting a write here would silently do nothing (the
/// validator reads only its own per-tenant settings; the gateway
/// never reads these keys), so the write is refused and the
/// caller is pointed at the canonical endpoint instead of being
/// left believing the value took effect. The handler only
/// validates — it never applies a value — and there is no
/// `settings.changed` listener left in this module.
pub(crate) fn on_config_validate_cards(ctx: &mut HookContext) {
    let namespace = ctx
        .get("namespace")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if namespace != "cards" && namespace != "reference_id" {
        return;
    }
    let key = ctx
        .get("key")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let message = if namespace == "cards" {
        format!(
            "Setting \"cards.{key}\" is owned by the cards module; edit it with PUT /api/v1/cards/settings (test_mode, max_expiry_years_ahead, gpn_enabled, disabled_standard_brands). The gateway keeps no cards.* settings."
        )
    } else {
        format!(
            "Setting \"reference_id.{key}\" is owned by the store and billing modules; edit it with PUT /api/v1/store/reference-settings or PUT /api/v1/billing/reference-settings. The gateway keeps no reference_id.* settings."
        )
    };
    ctx.set("valid", json!(false));
    ctx.set("message", json!(message));
}
