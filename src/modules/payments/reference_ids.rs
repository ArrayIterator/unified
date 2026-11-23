//! payments - reference ids.

use unified_core::reference_id;
use super::{events::*, card_validation::*, card_settings::*, limits::*, providers::*, types::*, credentials::*, records::*, adapter::*, dispatch::*, stripe::*, paypal::*, xendit::*, midtrans::*, hyperswitch::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};



// ---------------------------------------------------------------------------
// Batch 18: payment reference ids (BEGIN — append-only section)
// ---------------------------------------------------------------------------
//
// The provider-facing reference for every charge, payout, refund,
// and subscription name in this module is a reference in the single
// shared format (`PREFIX-12hex-12hex`, 30 characters; see
// `unified_core::reference_id`). The store order id (a UUID) is NEVER
// sent to a provider as the order reference: the provider sees only
// the 30-character reference, while the store link stays in
// `ChargeInput::order_id` and on the recorded transaction, so
// reconciliation never loses the order. The reference for a charge
// resolves in this order: the reference threaded on the input, the
// order id when it already is a valid reference, otherwise a freshly
// generated reference. Nothing here invents a second format: the
// `pay-{uuid}`, `ord-{uuid}`, `txn-{uuid}`, `payout-{uuid}`,
// `subscription-{uuid}`, `bt-{uuid}`, and `bt-refund-{uuid}`
// fallbacks are all replaced by this one helper.
/// Resolve the provider-facing reference for a charge: the threaded
/// `reference_id` when it is a valid reference, otherwise the order
/// id when it already is one, otherwise a freshly generated
/// reference.
pub(crate) fn charge_reference(input: &ChargeInput) -> String {
    if let Some(reference) = input
        .reference_id
        .as_deref()
        .filter(|reference| reference_id::is_valid(reference))
    {
        return reference.to_string();
    }
    provider_reference(input.order_id.as_deref())
}


/// Resolve a provider-facing reference from an order id alone: the
/// id itself when it already is a valid reference, otherwise a
/// freshly generated reference with the default prefix.
pub(crate) fn provider_reference(order_id: Option<&str>) -> String {
    match order_id.filter(|id| reference_id::is_valid(id)) {
        Some(id) => id.to_string(),
        None => reference_id::generate(None),
    }
}


/// The store order's reference for a pending checkout, when the
/// store recorded one through `payment.initiated`. The pending
/// checkout payload carries the order JSON, which includes the
/// `reference` issued at checkout. Returns `None` when there is no
/// pending checkout for the order or its reference is not valid.
pub(crate) fn pending_checkout_reference(order_id: Option<&str>) -> Option<String> {
    let order_id = order_id?;
    let record = pending_checkouts_store().get(order_id)?;
    let reference = record
        .get("order")?
        .get("reference")?
        .as_str()?;
    if reference_id::is_valid(reference) {
        Some(reference.to_string())
    } else {
        None
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn charge_input(reference_id: Option<&str>, order_id: Option<&str>) -> ChargeInput {
        ChargeInput {
            amount_minor: 1000,
            currency: "USD".to_string(),
            customer_id: "cust-1".to_string(),
            order_id: order_id.map(str::to_string),
            reference_id: reference_id.map(str::to_string),
            invoice_id: None,
            description: None,
            payment_method_token: None,
            idempotency_key: "key-1".to_string(),
            three_ds: false,
            billing: None,
        }
    }

    #[test]
    fn provider_reference_keeps_valid_ids_and_generates_others() {
        let valid = reference_id::EXAMPLE_REFERENCE_ID;
        assert_eq!(provider_reference(Some(valid)).as_str(), valid);
        let generated = provider_reference(None);
        assert!(reference_id::is_valid(&generated));
        assert_eq!(generated.len(), 30);
        let from_invalid = provider_reference(Some("not-a-reference"));
        assert!(reference_id::is_valid(&from_invalid));
        assert_ne!(from_invalid.as_str(), "not-a-reference");
    }

    #[test]
    fn charge_reference_prefers_threaded_then_order_then_generated() {
        let valid = reference_id::EXAMPLE_REFERENCE_ID;
        let input = charge_input(Some(valid), None);
        assert_eq!(charge_reference(&input).as_str(), valid);
        let input = charge_input(None, Some(valid));
        assert_eq!(charge_reference(&input).as_str(), valid);
        let generated = charge_reference(&charge_input(None, None));
        assert!(reference_id::is_valid(&generated));
    }

    #[test]
    fn pending_checkout_reference_is_none_without_a_checkout() {
        assert_eq!(pending_checkout_reference(None), None);
        assert_eq!(pending_checkout_reference(Some("no-such-order")), None);
    }
}
