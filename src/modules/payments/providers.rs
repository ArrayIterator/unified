//! payments - providers.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use unified_core::provider_registry::{
    ProviderDefinition, leak_str, leak_str_slice, provider_registry,
};
use super::{events::*, card_validation::*, card_settings::*, limits::*, types::*, reference_ids::*, credentials::*, records::*, adapter::*, dispatch::*, stripe::*, paypal::*, xendit::*, midtrans::*, hyperpay::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};



#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
// ---------------------------------------------------------------------------
// Provider kinds, capabilities, currencies, settings fields.
// ---------------------------------------------------------------------------
/// Every payment provider this module can talk to.
pub enum PaymentProviderKind {
    Xendit,
    HyperPay,
    TwoCheckout,
    Card,
    Mastercard,
    GooglePay,
    ApplePay,
    PayPal,
    Stripe,
    Midtrans,
    Custom,
    BankTransfer,
}


impl PaymentProviderKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Xendit => "xendit",
            Self::HyperPay => "hyperpay",
            Self::TwoCheckout => "2checkout",
            Self::Card => "card",
            Self::Mastercard => "mastercard",
            Self::GooglePay => "google_pay",
            Self::ApplePay => "apple_pay",
            Self::PayPal => "paypal",
            Self::Stripe => "stripe",
            Self::Midtrans => "midtrans",
            Self::Custom => "custom",
            Self::BankTransfer => "bank_transfer",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "xendit" => Some(Self::Xendit),
            "hyperpay" => Some(Self::HyperPay),
            "2checkout" | "2co" | "twocheckout" => Some(Self::TwoCheckout),
            "card" => Some(Self::Card),
            "mastercard" => Some(Self::Mastercard),
            "google_pay" | "googlepay" => Some(Self::GooglePay),
            "apple_pay" | "applepay" => Some(Self::ApplePay),
            "paypal" => Some(Self::PayPal),
            "stripe" => Some(Self::Stripe),
            "midtrans" => Some(Self::Midtrans),
            "custom" => Some(Self::Custom),
            "bank_transfer" | "banktransfer" => Some(Self::BankTransfer),
            _ => None,
        }
    }

    /// All kinds, in a stable order for the catalog endpoint.
    pub fn all() -> &'static [PaymentProviderKind] {
        &[
            Self::Xendit,
            Self::HyperPay,
            Self::TwoCheckout,
            Self::Card,
            Self::Mastercard,
            Self::GooglePay,
            Self::ApplePay,
            Self::PayPal,
            Self::Stripe,
            Self::Midtrans,
            Self::Custom,
            Self::BankTransfer,
        ]
    }

    /// Human-readable display name, from the provider registry.
    pub fn display_name(&self) -> &'static str {
        attrs(self).display_name
    }

    /// Hosts this provider's API is allowed to talk to, from the
    /// provider registry. A request to any other host is refused
    /// before it is sent. Empty means the host is validated by the
    /// adapter instead (per-region hosts, or a user-supplied
    /// endpoint).
    pub fn allowed_hosts(&self) -> &'static [&'static str] {
        attrs(self).allowed_hosts
    }

    /// Whether the API host is fixed (allow-listed) or user-supplied.
    /// Exactly the providers with a non-empty host list.
    pub fn has_fixed_hosts(&self) -> bool {
        attrs(self).has_fixed_hosts
    }

    /// Whether the provider needs a delegate gateway connection to
    /// perform charges (wallet adapters only).
    pub fn needs_delegate(&self) -> bool {
        attrs(self).needs_delegate
    }

    /// Whether the provider needs a user-supplied endpoint on the
    /// connection.
    pub fn needs_endpoint(&self) -> bool {
        attrs(self).needs_endpoint
    }

    /// The registry definition for this kind, if one is registered.
    /// Every built-in kind has one; `None` only when the registry
    /// was built without seeds (tests).
    pub fn definition(&self) -> Option<Arc<ProviderDefinition>> {
        provider_registry().get("payments", self.as_str())
    }
}

/// The enabled provider catalog, built-ins plus any provider an admin
/// registered, sorted by id. New providers appear here with no code
/// change; the enum-based `all()` lists built-ins only.
pub fn catalog() -> Vec<Arc<ProviderDefinition>> {
    provider_registry().list_family_enabled("payments")
}

// ---------------------------------------------------------------------------
// Registry-backed attributes (C12, task 269).
// ---------------------------------------------------------------------------
/// The `&'static` view of one provider's registry definition. The
/// public methods above predate the registry and return `&'static`
/// data, so the attributes are built once from registry data and the
/// strings are leaked once (see `unified_core::provider_registry::
/// leak_str`). New code reads the owned `ProviderDefinition`
/// directly through `PaymentProviderKind::definition` / `catalog`
/// instead of this cache.
struct ProviderAttrs {
    display_name: &'static str,
    allowed_hosts: &'static [&'static str],
    currencies: &'static [&'static str],
    capabilities: PaymentCapabilities,
    settings_fields: Vec<SettingField>,
    needs_endpoint: bool,
    needs_delegate: bool,
    has_fixed_hosts: bool,
}

fn attrs_from_definition(def: &ProviderDefinition) -> ProviderAttrs {
    let capabilities = PaymentCapabilities {
        supports_authorize: def.capability("authorize"),
        supports_capture: def.capability("capture"),
        supports_charge: def.capability("charge"),
        supports_refund: def.capability("refund"),
        supports_void: def.capability("void"),
        supports_subscriptions: def.capability("subscriptions"),
        supports_payouts: def.capability("payouts"),
        supports_3ds: def.capability("3ds"),
        supports_webhooks: def.capability("webhooks"),
        subscriptions_are_local_schedule: def.capability("subscriptions_are_local_schedule"),
    };
    let settings_fields = def
        .credentials_shape
        .iter()
        .map(|shape| {
            field(
                leak_str(&shape.name),
                leak_str(&shape.label),
                leak_str(&shape.field_type),
                shape.required,
                shape.secret,
                leak_str(&shape.applies_to),
                leak_str(&shape.description),
            )
        })
        .collect();
    ProviderAttrs {
        display_name: leak_str(&def.display_name),
        allowed_hosts: leak_str_slice(&def.allowed_hosts),
        currencies: leak_str_slice(&def.currencies),
        capabilities,
        settings_fields,
        needs_endpoint: def.needs_endpoint,
        needs_delegate: def.needs_delegate,
        has_fixed_hosts: def.has_fixed_hosts(),
    }
}

static ATTRS: OnceLock<HashMap<PaymentProviderKind, ProviderAttrs>> = OnceLock::new();

fn attrs(kind: &PaymentProviderKind) -> &'static ProviderAttrs {
    let map = ATTRS.get_or_init(|| {
        let mut out = HashMap::new();
        for def in provider_registry().list_family("payments") {
            if let Some(parsed) = PaymentProviderKind::from_str(&def.id) {
                out.insert(parsed, attrs_from_definition(&def));
            }
        }
        out
    });
    // Every enum variant has a seed definition; the expect documents
    // that invariant (a registry built without seeds breaks it).
    map.get(kind)
        .expect("every PaymentProviderKind has a registry definition")
}


/// What a provider adapter can do. Callers must check these flags
/// before calling an operation: an unsupported call fails with a 400
/// naming the missing capability.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct PaymentCapabilities {
    pub supports_authorize: bool,
    pub supports_capture: bool,
    pub supports_charge: bool,
    pub supports_refund: bool,
    pub supports_void: bool,
    pub supports_subscriptions: bool,
    pub supports_payouts: bool,
    pub supports_3ds: bool,
    pub supports_webhooks: bool,
    /// True when subscriptions are a local schedule over tokenized
    /// charges rather than a native provider subscription object.
    pub subscriptions_are_local_schedule: bool,
}


/// Capabilities per kind (single source of truth for the catalog, the
/// read views, and the operation guards). From the provider registry
/// (C12, task 269); the `subscriptions_are_local_schedule` flag marks
/// providers where the subscription record lives in this module and
/// each period is a tokenized charge, because the provider exposes no
/// subscription object to this adapter.
pub fn capabilities_for(kind: PaymentProviderKind) -> PaymentCapabilities {
    attrs(&kind).capabilities
}


/// Currencies the adapter can charge in, from the provider
/// registry (C12, task 269). These are the currencies the provider
/// documents for the API surface used here; a charge in any other
/// currency is rejected before any provider call. Wallet kinds use
/// the delegate connection's list at charge time; the catalog shows
/// the common wallet list.
pub fn currencies_for(kind: PaymentProviderKind) -> &'static [&'static str] {
    attrs(&kind).currencies
}


/// One field of a provider's settings page: name, type, whether it is
/// required, whether it is a secret (write-only, masked on read), and
/// whether it applies to sandbox, live, or both modes.
#[derive(Debug, Clone, Serialize)]
pub struct SettingField {
    pub name: &'static str,
    pub label: &'static str,
    #[serde(rename = "type")]
    pub field_type: &'static str,
    pub required: bool,
    pub secret: bool,
    pub applies_to: &'static str,
    pub description: &'static str,
}


pub(crate) fn field(
    name: &'static str,
    label: &'static str,
    field_type: &'static str,
    required: bool,
    secret: bool,
    applies_to: &'static str,
    description: &'static str,
) -> SettingField {
    SettingField {
        name,
        label,
        field_type,
        required,
        secret,
        applies_to,
        description,
    }
}


/// Settings-page field definitions per provider kind, from the
/// provider registry (C12, task 269).
pub fn settings_fields_for(kind: PaymentProviderKind) -> Vec<SettingField> {
    attrs(&kind).settings_fields.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::provider_registry::{FAMILY_PAYMENTS, ProviderDefinition};

    #[test]
    fn builtin_attributes_come_from_registry_data() {
        // Values that used to live in five parallel match tables.
        assert_eq!(PaymentProviderKind::Stripe.display_name(), "Stripe");
        assert_eq!(
            PaymentProviderKind::Midtrans.allowed_hosts(),
            &["api.midtrans.com", "api.sandbox.midtrans.com"]
        );
        assert_eq!(
            currencies_for(PaymentProviderKind::Midtrans),
            &["IDR", "SGD"]
        );
        let stripe = capabilities_for(PaymentProviderKind::Stripe);
        assert!(stripe.supports_payouts);
        assert!(!stripe.subscriptions_are_local_schedule);
        let xendit = capabilities_for(PaymentProviderKind::Xendit);
        assert!(!xendit.supports_void);
        assert!(xendit.subscriptions_are_local_schedule);
        let wallet = capabilities_for(PaymentProviderKind::GooglePay);
        assert!(wallet.supports_charge);
        assert!(!wallet.supports_subscriptions);
        assert!(PaymentProviderKind::GooglePay.needs_delegate());
        assert!(PaymentProviderKind::Card.needs_endpoint());
        assert!(!PaymentProviderKind::Stripe.needs_endpoint());
        assert!(PaymentProviderKind::Stripe.has_fixed_hosts());
        assert!(!PaymentProviderKind::BankTransfer.has_fixed_hosts());
        let fields = settings_fields_for(PaymentProviderKind::Stripe);
        let secret = fields
            .iter()
            .find(|field| field.name == "secret_key")
            .unwrap();
        assert!(secret.required && secret.secret);
        // Base URLs come from the same definitions.
        let stripe_def = PaymentProviderKind::Stripe.definition().unwrap();
        assert_eq!(stripe_def.base_url.as_deref(), Some("https://api.stripe.com"));
        let paypal_def = PaymentProviderKind::PayPal.definition().unwrap();
        assert_eq!(
            paypal_def.effective_base_url(true),
            Some("https://api-m.sandbox.paypal.com")
        );
    }

    #[test]
    fn add_one_payment_provider_without_code_change() {
        // Register a brand-new provider through the registry only, then
        // prove the catalog, the definition lookup, and every attribute
        // resolve from that data. No enum variant exists for it and no
        // match arm was added; it runs through the generic adapter
        // (`adapter_kind` "custom").
        let mut def = ProviderDefinition::new("payments", "acme_pay", "Acme Pay", "custom");
        def.allowed_hosts = vec!["api.acme.test".to_string()];
        def.capabilities = serde_json::json!({"charge": true, "refund": true});
        def.currencies = vec!["USD".to_string(), "IDR".to_string()];
        def.credentials_shape = vec![unified_core::provider_registry::CredentialFieldDef {
            name: "api_key".to_string(),
            label: "API key".to_string(),
            field_type: "password".to_string(),
            required: true,
            secret: true,
            ..Default::default()
        }];
        provider_registry().register(def).unwrap();
        assert!(catalog().iter().any(|entry| entry.id == "acme_pay"));
        let found = provider_registry()
            .get(FAMILY_PAYMENTS, "acme_pay")
            .unwrap();
        assert_eq!(found.display_name, "Acme Pay");
        assert_eq!(found.adapter_kind, "custom");
        assert!(found.capability("charge"));
        assert!(found.capability("refund"));
        assert!(!found.capability("payouts"));
        assert!(found.host_allowed("api.acme.test"));
        assert!(!found.host_allowed("api.evil.test"));
        assert_eq!(found.currencies, vec!["USD", "IDR"]);
        assert!(found.credential_field("api_key").unwrap().required);
    }
}
