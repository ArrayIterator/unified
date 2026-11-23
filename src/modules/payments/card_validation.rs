//! payments - card validation.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::tenancy::TenantContext;
use actix_web::{web, HttpRequest, HttpResponse};
use serde::{Deserialize};
use serde_json::{json, Value};
use std::sync::{Arc};
use uuid::Uuid;
use super::{events::*, card_settings::*, limits::*, providers::*, types::*, reference_ids::*, credentials::*, records::*, adapter::*, dispatch::*, stripe::*, paypal::*, xendit::*, midtrans::*, hyperpay::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};



// ---------------------------------------------------------------------------
// Batch 18 — card validation wiring.
//
// The gateway never validates a card number itself: it emits
// `cards.validate_requested` and the cards module answers. This
// section is the whole wiring surface — the event contract, the
// reason codes, the capability names, and the fail-closed gate
// (`request_card_validation`) the checkout handlers call before
// any card payment is processed. The cards module itself
// (validation, brand data, custom rules, and its per-tenant
// settings) and the reference-id generator live in their own
// modules; this section only wires them into the payment flow.
// The gateway keeps no `cards.*` / `reference_id.*` settings of
// its own: those namespaces were dropped (see the module header
// and `on_config_validate_cards`) because the cards module's
// per-tenant settings and the store / billing reference settings
// are the single sources of truth.
// ---------------------------------------------------------------------------
/// Emitted before a card payment is processed, asking the cards
/// module to validate the card. The gateway proceeds ONLY on a
/// successful validation answer; see `request_card_validation`.
/// WARNING: the payload carries the card number (`number`,
/// digits only), the expiry (`expiry`, `"MM/YYYY"`), and the CVV
/// (`cvv`, the value) so the validator can run Luhn, brand
/// detection, expiry, and CVV-length checks. All three travel on
/// the in-process hook bus only: the cards module's validator is
/// pure — it never stores, logs, or returns them — and the
/// payload must never be audited, persisted, or forwarded to a
/// webhook. `cards.validate_requested` is therefore deliberately
/// absent from the audit event list and from the webhooks
/// known-events list; only the `cards.validated` outcome event
/// (masked PAN only) is auditable.
pub const CARDS_VALIDATE_REQUESTED: &str = "cards.validate_requested";

/// Emitted by the cards module (not by this gateway) once per
/// answered validation, with the masked PAN only — never the full
/// number. The audit log subscribes to this event, not to the
/// request event above. Payload (cards module): `tenant_id`,
/// `masked_pan`, `brand_id`, `brand_name`, `matched_rule`,
/// `valid`, `reason_codes`, `is_test_number`.
pub const CARDS_VALIDATED: &str = "cards.validated";

/// Emitted by the cards module when an admin creates, updates, or
/// deletes a custom card validation rule. NOTE: the cards module
/// does not emit this event yet (no emit site in `cards.rs` at
/// batch-18 time); the audit snippet lists it as a required
/// addition for the cards module, not as a live event.
pub const CARDS_CUSTOM_RULE_CHANGED: &str = "cards.custom_rule.changed";

/// Emitted by the reference-id generator when it mints a
/// reference order id. The event name is the generator's own
/// (`reference.generated`, see `reference_id::
/// EVENT_REFERENCE_GENERATED`); the payload is `{ "reference",
/// "prefix", "tenant_id" }`.
pub const REFERENCE_ID_GENERATED: &str = "reference.generated";

/// Emitted after the reference-id settings change (see
/// `reference_id::EVENT_REFERENCE_SETTINGS_CHANGED`).
pub const REFERENCE_SETTINGS_CHANGED: &str = "reference.settings_changed";


/// Card validation failed the Luhn checksum.
pub const CARD_REASON_INVALID_LUHN: &str = "invalid_luhn";

/// The card number matches no known brand (and no custom rule).
pub const CARD_REASON_UNKNOWN_BRAND: &str = "unknown_brand";

/// The card number has the wrong length for its brand.
pub const CARD_REASON_INVALID_LENGTH: &str = "invalid_length";

/// The expiry date is past, malformed, or beyond the upper bound
/// in the cards module's per-tenant settings
/// (`max_expiry_years_ahead`).
pub const CARD_REASON_INVALID_EXPIRY: &str = "invalid_expiry";

/// The CVV has the wrong length or format for the brand.
pub const CARD_REASON_INVALID_CVV: &str = "invalid_cvv";

/// The detected brand is disabled by the cards module's
/// per-tenant settings.
pub const CARD_REASON_BRAND_DISABLED: &str = "brand_disabled";

/// The number is a known test number and the tenant is in live
/// mode (the cards module's per-tenant `test_mode` is `live`).
pub const CARD_REASON_TEST_NUMBER_REJECTED: &str = "test_number_rejected";

/// The number is malformed (illegal characters, or missing where
/// the validator requires a number). The gateway pre-checks the
/// shape, so this surfaces mainly when a number-less payload
/// reaches a validator that requires one (method / claim modes
/// before the cards handler learns them).
pub const CARD_REASON_INVALID_FORMAT: &str = "invalid_format";

/// The cards module did not answer (not registered, silent, or an
/// answer that breaks the contract). Fail closed, never proceed.
pub const CARD_REASON_UNAVAILABLE: &str = "cards_unavailable";


/// Every reason code a validation answer may carry. An answer
/// whose reason is outside this list is a contract violation
/// and is treated as `cards_unavailable`. The cards module's
/// `missing_tenant_context` refusal is not a validation reason:
/// it maps to `cards_unavailable` (the gate guarantees a tenant
/// id is always present, so it should never fire).
pub const CARD_VALIDATION_REASONS: &[&str] = &[
    CARD_REASON_INVALID_LUHN,
    CARD_REASON_UNKNOWN_BRAND,
    CARD_REASON_INVALID_LENGTH,
    CARD_REASON_INVALID_EXPIRY,
    CARD_REASON_INVALID_CVV,
    CARD_REASON_BRAND_DISABLED,
    CARD_REASON_TEST_NUMBER_REJECTED,
    CARD_REASON_INVALID_FORMAT,
    CARD_REASON_UNAVAILABLE,
];


/// Capability: read card validation settings and the validation contract.
pub const CARDS_READ: &str = "cards:read";

/// Capability: validate a card (server-side checkout gate and the
/// admin validation flow). Never granted to unprivileged roles:
/// an open validation oracle lets anyone probe card numbers.
pub const CARDS_VALIDATE: &str = "cards:validate";

/// Capability: manage card validation settings and custom rules.
pub const CARDS_MANAGE: &str = "cards:manage";


/// Coarse upper bound for the local expiry-year shape guard in
/// `create_payment_method`: the cards module accepts
/// `max_expiry_years_ahead` 1-50 per tenant, so a card expiring
/// more than 50 years past the current year can never validate.
/// This guard only rejects years no tenant setting could ever
/// accept; the validator stays authoritative for the real
/// per-tenant bound and for past-ness (`invalid_expiry`). There
/// is deliberately no settings-driven bound here: the gateway
/// keeps no `cards.*` settings surface (see the module header),
/// so a gateway-side bound would drift from the cards module's
/// per-tenant value.
pub const CARDS_EXPIRY_YEAR_GUARD_MAX_AHEAD: u32 = 50;


/// Raw card details for one validation, supplied on a checkout
/// request. All fields are optional and defaulted so older
/// clients that send no `card` object keep working: the gate
/// only runs when `number` is non-empty. The struct is
/// deserialize-only and is never serialized anywhere — the number,
/// expiry, and CVV value leave this handler only as the
/// in-memory `cards.validate_requested` bus payload (`number`,
/// `expiry`, `cvv`) to the cards module's pure validator, and
/// all three are dropped with the request. Never store, log, or
/// return these fields.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CardDetailsInput {
    /// Card number; spaces and dashes are tolerated and stripped.
    #[serde(default)]
    pub number: String,
    /// Expiry month, 1-12.
    #[serde(default)]
    pub exp_month: u32,
    /// Expiry year, four digits.
    #[serde(default)]
    pub exp_year: u32,
    /// Card verification value. It is sent to the cards module
    /// only as the `cvv` field of the in-memory
    /// `cards.validate_requested` payload (the validator needs
    /// the value for its length check); it is never stored,
    /// logged, or returned.
    #[serde(default)]
    pub cvv: Option<String>,
}


/// Outcome of a successful card validation: the mode that ran and
/// the validator's brand and masked PAN, when the answer carried
/// them. `Ok(Some(_))` from `request_card_validation` always
/// means the card is valid; failures are errors, never a
/// `valid: false` outcome.
#[derive(Debug, Clone)]
pub struct CardValidationOutcome {
    /// `"full"` (raw card details), `"method"` (stored method),
    /// or `"claim"` (claimed brand / last4 / expiry on
    /// method onboarding).
    pub mode: String,
    /// Brand detected by the validator, when the answer carried it.
    pub brand: Option<String>,
    /// Masked PAN (first 6 + last 4 in full mode, `******` + last
    /// 4 in method mode), when known.
    pub masked_pan: Option<String>,
}


/// Keep only the ASCII digits of a value (strips the spaces and
/// dashes callers use to group a card number).
pub fn digits_only(value: &str) -> String {
    value.chars().filter(|c| c.is_ascii_digit()).collect()
}


/// Mask a card number for logs, errors, and audit records: first 6
/// digits, six asterisks, last 4 digits (`411111******1111`).
/// Shorter inputs mask everything except the last 4 digits; an
/// input shorter than 4 digits masks to `****`. The full number is
/// never returned.
pub fn mask_pan(pan_digits: &str) -> String {
    let digits = digits_only(pan_digits);
    if digits.len() < 4 {
        return "*".repeat(4);
    }
    let last4 = &digits[digits.len() - 4..];
    if digits.len() >= 10 {
        format!("{}******{}", &digits[..6], last4)
    } else {
        format!("{}{}", "*".repeat(digits.len() - 4), last4)
    }
}



// ---------------------------------------------------------------------------
// Card validation: capability check and the checkout gate.
// ---------------------------------------------------------------------------
/// Fail when the caller does not hold `permission` for the cards
/// resource. Follows the entitlements module's `role.check`
/// pattern: ask the hook bus, fail closed on silence (no roles
/// module answering means denied). The role comes from the
/// `x-role` header — the same trusted-header caveat as the rest of
/// the codebase applies until the auth extractor lands. Used by
/// the cards settings endpoint; the checkout gate itself runs
/// server-side on behalf of the paying customer (ownership is
/// enforced by `require_customer` at the call sites) and does not
/// take a role.
pub(crate) fn require_cards_capability(
    ctx: &AppContext,
    req: &HttpRequest,
    permission: &str,
) -> UnifiedResult<()> {
    let denied = || {
        UnifiedError::forbidden(format!(
            "Role lacks required capability: {permission}"
        ))
    };
    let Some(role) = unified_core::caller::caller_from(req)
        .roles
        .into_iter()
        .next()
    else {
        return Err(denied());
    };
    let outcome = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    if outcome
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        Ok(())
    } else {
        Err(denied())
    }
}


/// Fail-closed error for a card payment rejected because the
/// validator could not answer: the cards module is not
/// registered, it stayed silent, or its answer broke the
/// contract. 503, with the generic reason code and the masked PAN
/// only — never the full number, never a per-digit detail.
pub(crate) fn cards_unavailable(masked_pan: Option<&str>) -> UnifiedError {
    let mut error = UnifiedError::new(
        "Card validation is unavailable; the card payment was rejected",
        503,
    )
    .with_metadata(
        "reason",
        Value::String(CARD_REASON_UNAVAILABLE.to_string()),
    );
    if let Some(masked) = masked_pan {
        error = error.with_metadata("masked_pan", Value::String(masked.to_string()));
    }
    error
}


/// Card data the validator can check without a PAN: a brand,
/// last 4 digits, and expiry — as claimed on a method-onboarding
/// request. The stored-method path resolves the same fields from
/// the stored record instead; the two shapes are identical on the
/// wire, only the source (and the mode label) differs.
#[derive(Debug, Clone)]
pub struct CardMethodRef {
    pub brand: String,
    pub last4: String,
    pub exp_month: u32,
    pub exp_year: u32,
}


/// Ask the cards module to validate the card for one checkout and
/// return the outcome, or reject the payment.
///
/// The gate runs only when the gateway actually holds card data:
///
///   - raw `card` details in the request → mode `"full"`: Luhn,
///     brand detection, per-brand length, expiry, CVV length,
///     brand enabled, test-number policy;
///   - a stored `payment_method_id` (and no raw details) → mode
///     `"method"`: brand known / enabled and expiry, from the
///     stored brand, last4, and expiry (no PAN exists to run Luhn
///     over);
///   - a claimed brand / last4 / expiry on a method-onboarding
///     request (and no raw details) → mode `"claim"`: the same
///     brand and expiry checks as `"method"`, so a disabled brand
///     or an out-of-bound expiry is rejected at onboarding instead
///     of at the first charge;
///   - an opaque token alone → no gate: the token came from a
///     PCI-compliant tokenization step upstream and the gateway
///     cannot validate what it cannot see. `Ok(None)` means
///     "nothing to validate", not "validation passed".
///
/// KNOWN GAP (batch-18 integrator item): the cards module's hook
/// handler currently requires `number` and answers
/// `invalid_format` when it is missing, so `"method"` and
/// `"claim"` emits fail closed with `invalid_format` until the
/// handler learns number-less validation (brand / last4 / expiry
/// only). The gate keeps emitting them anyway — silently
/// skipping stored-method validation would be a fail-open — so
/// stored-method card payments stay rejected until that handler
/// extension lands. See the batch report for the exact
/// extension.
///
/// Fail closed, in order: no resolved tenant context (401/403
/// from the tenant extractor — card payments are tenant-scoped
/// and never fall back to the control plane); the cards module
/// is not registered on `cards.validate_requested` (503
/// `cards_unavailable`); the module stays silent or answers
/// without a boolean `valid` (503 `cards_unavailable`); the
/// answer is invalid (422 with the generic reason code from the
/// allowlist, the masked PAN only, and the brand when the answer
/// carried one); the answer carries a reason outside the
/// allowlist (503 `cards_unavailable` — a contract violation,
/// never a guessed reason). The full PAN and the CVV value are
/// never stored, logged, or returned: they travel only on the
/// in-process bus to the cards module's pure validator
/// (`number`, `expiry`, `cvv`), which never stores or logs them,
/// and the request event is never audited or forwarded.
#[allow(clippy::too_many_arguments)]
pub(crate) fn request_card_validation(
    state: &PaymentsState,
    req: &HttpRequest,
    card: Option<&CardDetailsInput>,
    method_id: Option<Uuid>,
    claimed: Option<CardMethodRef>,
    connection: &PaymentConnection,
    customer_id: &str,
    order_id: Option<&str>,
    intent_id: Option<Uuid>,
) -> UnifiedResult<Option<CardValidationOutcome>> {
    let tenant = TenantContext::from_http_request(req)?;
    let full = card
        .map(|details| !details.number.trim().is_empty())
        .unwrap_or(false);
    let method = if full {
        None
    } else {
        method_id.and_then(|id| {
            methods_store()
                .get(&id)
                .map(|entry| entry.value().clone())
        })
    };
    if !full && method.is_none() && claimed.is_none() {
        return Ok(None);
    }
    let mode = if full {
        "full"
    } else if method.is_some() {
        "method"
    } else {
        "claim"
    };
    let (pan_digits, masked_pan, brand, exp_month, exp_year, cvv_present, cvv_length, cvv_digits_only, claimed_last4, cvv_value) =
        if full {
            let details = card.expect("full mode implies card details are present");
            if details.number.trim().len() > 64 {
                return Err(UnifiedError::bad_request(
                    "Card number is too long",
                ));
            }
            let digits = digits_only(&details.number);
            if digits.is_empty()
                || !details
                    .number
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == ' ' || c == '-')
            {
                return Err(UnifiedError::bad_request(
                    "Card number must contain only digits, spaces, or dashes",
                ));
            }
            if !(1..=12).contains(&details.exp_month) {
                return Err(UnifiedError::bad_request("exp_month must be 1-12"));
            }
            if !(1000..=9999).contains(&details.exp_year) {
                return Err(UnifiedError::bad_request(
                    "exp_year must be a four-digit year",
                ));
            }
            let cvv = details.cvv.clone().unwrap_or_default();
            if cvv.len() > 8 {
                return Err(UnifiedError::bad_request("CVV is too long"));
            }
            let masked = mask_pan(&digits);
            (
                Some(digits),
                Some(masked),
                None,
                details.exp_month,
                details.exp_year,
                !cvv.is_empty(),
                cvv.len(),
                cvv.chars().all(|c| c.is_ascii_digit()),
                None,
                if cvv.is_empty() { None } else { Some(cvv) },
            )
        } else {
            // Method mode resolves the stored record (ownership is
            // re-checked here even though `resolve_token` already
            // checked it at the call site); claim mode uses the
            // caller-supplied fields, which the call site already
            // ownership-checked with `require_customer`.
            let reference = match method {
                Some(method) => {
                    if method.customer_id != customer_id {
                        return Err(UnifiedError::forbidden(
                            "Payment method belongs to a different customer",
                        ));
                    }
                    CardMethodRef {
                        brand: method.brand,
                        last4: method.last4,
                        exp_month: method.exp_month,
                        exp_year: method.exp_year,
                    }
                }
                None => claimed
                    .expect("method or claim mode implies a method or claim was supplied"),
            };
            (
                None,
                Some(format!("******{}", reference.last4)),
                Some(reference.brand.clone()),
                reference.exp_month,
                reference.exp_year,
                false,
                0,
                false,
                Some(reference.last4.clone()),
                None,
            )
        };
    if !state.bus().has(CARDS_VALIDATE_REQUESTED) {
        return Err(cards_unavailable(masked_pan.as_deref()));
    }
    // The validator's wire shape: `number` (digits), `expiry`
    // (`"MM/YYYY"`, zero-padded month), `cvv` (the value, or null
    // when none was supplied). No settings ride along: the cards
    // module validates against its own per-tenant settings
    // (`PUT /api/v1/cards/settings`) and ignores any gateway-side
    // values, so the gateway sends none — the old
    // `test_mode` / `gpn_enabled` / `enabled_brands` /
    // `expiry_upper_bound_years` payload fields were dropped with
    // the gateway's `cards.*` settings namespace.
    let expiry = format!("{exp_month:02}/{exp_year}");
    let context = state.bus().emit(
        CARDS_VALIDATE_REQUESTED,
        json!({
            "mode": mode,
            "number": pan_digits,
            "expiry": expiry,
            "cvv": cvv_value,
            "masked_pan": masked_pan,
            "brand": brand,
            "cvv_present": cvv_present,
            "cvv_length": cvv_length,
            "cvv_digits_only": cvv_digits_only,
            "claimed_brand": brand,
            "claimed_last4": claimed_last4,
            "tenant_id": tenant.tenant_id.to_string(),
            "schema_name": tenant.schema_name,
            "customer_id": customer_id,
            "connection_id": connection.id.to_string(),
            "provider": connection.kind.as_str(),
            "order_id": order_id,
            "intent_id": intent_id.map(|id| id.to_string()),
        }),
    );
    // The cards module answers in two channels: `result` set on
    // the payload and the same object contributed via `add_result`.
    // Either may carry the boolean `valid`; the payload's `result`
    // wins when both are present. Silence on both channels is a
    // fail-closed `cards_unavailable`.
    let answered = context
        .results
        .iter()
        .find(|result| result.get("valid").and_then(Value::as_bool).is_some());
    let answer = context.get("result").or(answered);
    let valid = answer
        .and_then(|result| result.get("valid"))
        .and_then(Value::as_bool)
        .or_else(|| context.get("valid").and_then(Value::as_bool));
    let Some(valid) = valid else {
        return Err(cards_unavailable(masked_pan.as_deref()));
    };
    if !valid {
        // The validator reports generic codes in `reason_codes`
        // (first code wins — the validator emits exactly one per
        // failure). Its `missing_tenant_context` refusal is not a
        // validation reason: it maps to `cards_unavailable` (the
        // gate always supplies a tenant id, so it should never
        // fire). Any code outside the allowlist is a contract
        // violation and also maps to `cards_unavailable` — never
        // a guessed reason.
        let reason = answer
            .and_then(|result| result.get("reason_codes"))
            .and_then(Value::as_array)
            .and_then(|codes| codes.first())
            .and_then(Value::as_str)
            .unwrap_or(CARD_REASON_UNAVAILABLE);
        let reason = if reason == "missing_tenant_context" {
            CARD_REASON_UNAVAILABLE
        } else {
            reason
        };
        if !CARD_VALIDATION_REASONS.contains(&reason) {
            return Err(cards_unavailable(masked_pan.as_deref()));
        }
        let mut error = UnifiedError::unprocessable(format!("Card validation failed: {reason}"))
            .with_metadata("reason", Value::String(reason.to_string()))
            .with_metadata("mode", Value::String(mode.to_string()));
        if let Some(masked) = &masked_pan {
            error = error.with_metadata("masked_pan", Value::String(masked.clone()));
        }
        if let Some(answer_brand) = answer
            .and_then(|result| result.get("brand_name"))
            .or_else(|| answer.and_then(|result| result.get("brand_id")))
            .and_then(Value::as_str)
        {
            error = error.with_metadata("brand", Value::String(answer_brand.to_string()));
        }
        return Err(error);
    }
    Ok(Some(CardValidationOutcome {
        mode: mode.to_string(),
        brand: answer
            .and_then(|result| result.get("brand_name"))
            .or_else(|| answer.and_then(|result| result.get("brand_id")))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or(brand),
        masked_pan: answer
            .and_then(|result| result.get("masked_pan"))
            .and_then(Value::as_str)
            .filter(|masked| !masked.is_empty())
            .map(str::to_string)
            .or(masked_pan),
    }))
}


/// Cards settings endpoint: the capability names and the
/// validation contract for the card validation gate, plus
/// pointers to the canonical settings surfaces. Admin-only
/// (`cards:read`). The gateway keeps no `cards.*` /
/// `reference_id.*` settings of its own (see
/// `on_config_validate_cards`): card validation settings are
/// owned per tenant by the cards module
/// (`PUT /api/v1/cards/settings`), and reference-id settings are
/// owned by the store and billing modules
/// (`PUT /api/v1/store/reference-settings`,
/// `PUT /api/v1/billing/reference-settings`).
pub(crate) async fn get_cards_settings(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        require_cards_capability(&ctx, &req, CARDS_READ)?;
        Ok(json!({
            "capabilities": {
                "read": CARDS_READ,
                "validate": CARDS_VALIDATE,
                "manage": CARDS_MANAGE,
            },
            "card_settings": "PUT /api/v1/cards/settings",
            "reference_settings": [
                "PUT /api/v1/store/reference-settings",
                "PUT /api/v1/billing/reference-settings",
            ],
            "validation_event": CARDS_VALIDATE_REQUESTED,
            "validation_reasons": CARD_VALIDATION_REASONS,
        }))
    })();
    json::respond(result, debug)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_only_strips_spaces_dashes_and_letters() {
        assert_eq!(digits_only("4111 1111-1111").as_str(), "411111111111");
        assert_eq!(digits_only("abc").as_str(), "");
        assert_eq!(digits_only("12 34").as_str(), "1234");
    }

    #[test]
    fn mask_pan_shows_first_six_and_last_four() {
        assert_eq!(mask_pan("4111111111111111").as_str(), "411111******1111");
        assert_eq!(mask_pan("4111-1111-1111-1111").as_str(), "411111******1111");
    }

    #[test]
    fn mask_pan_handles_short_inputs() {
        assert_eq!(mask_pan("123456789").as_str(), "*****6789");
        assert_eq!(mask_pan("123").as_str(), "****");
        assert_eq!(mask_pan("").as_str(), "****");
    }
}
