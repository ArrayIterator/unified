// Store module - in-memory state.
//
// Split from `src/modules/store.rs`; the module root re-exports the public API of this module.

use unified_core::hooks::{HookBus, HookContext};
use unified_core::reference_id::ReferenceIdSettings;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde_json::Value;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::digital::{seed_digital_types, seed_watermark_formats};
use super::reference::{reference_settings};
use super::types::{Cart, CatalogEntry, Coupon, DigitalDefaults, DigitalDownload, DigitalPayoutRequest, DigitalProduct, DigitalProductFile, DigitalProductPreview, DigitalProductType, DigitalRoyalty, DownloadEntitlement, DownloadRecord, IssuedLicense, Order, PricingTier, Product, Subscription, WatermarkFormat};

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct StoreState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    pub(crate) products: DashMap<String, Product>,
    pub(crate) tiers: DashMap<String, PricingTier>,
    pub(crate) coupons: DashMap<String, Coupon>,
    pub(crate) carts: DashMap<String, Cart>,
    pub(crate) orders: DashMap<String, Order>,
    /// Paid subscriptions, keyed by subscription id. Authoritative
    /// for the subscription lifecycle; billing keeps a mirror and
    /// drives decisions through `subscription.lifecycle.apply`.
    pub(crate) subscriptions: DashMap<String, Subscription>,
    pub(crate) entitlements: DashMap<String, DownloadEntitlement>,
    pub(crate) downloads: DashMap<String, DownloadRecord>,
    pub(crate) catalog: DashMap<String, CatalogEntry>,
    pub(crate) issued_licenses: DashMap<String, IssuedLicense>,
    pub(crate) digital_products: DashMap<String, DigitalProduct>,
    /// Admin-extensible digital type registry, seeded with the
    /// configurable default types (see `seed_digital_types`).
    pub(crate) digital_types: DashMap<String, DigitalProductType>,
    pub(crate) digital_files: DashMap<String, DigitalProductFile>,
    pub(crate) digital_previews: DashMap<String, DigitalProductPreview>,
    pub(crate) digital_royalties: DashMap<String, DigitalRoyalty>,
    pub(crate) digital_payouts: DashMap<String, DigitalPayoutRequest>,
    pub(crate) digital_downloads: DashMap<String, DigitalDownload>,
    /// Admin-editable watermark format registry, seeded with the
    /// configurable default formats (see `seed_watermark_formats`).
    pub(crate) watermark_formats: DashMap<String, WatermarkFormat>,
    pub(crate) digital_defaults: RwLock<DigitalDefaults>,
    /// Sample-download timestamps per identity and preview, for the
    /// sample rate limit. Entries are pruned on each check.
    pub(crate) sample_downloads: DashMap<String, Vec<DateTime<Utc>>>,
    /// Admin-editable payment reference settings (default prefix and
    /// per-product prefix map; see `unified_core::reference_id`).
    /// Read at checkout; changed through
    /// `GET/PUT /api/v1/store/reference-settings`.
    pub(crate) reference_settings: RwLock<ReferenceIdSettings>,
}

impl StoreState {
    pub(crate) fn new() -> Self {
        let state = Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            products: DashMap::new(),
            tiers: DashMap::new(),
            coupons: DashMap::new(),
            carts: DashMap::new(),
            orders: DashMap::new(),
            subscriptions: DashMap::new(),
            entitlements: DashMap::new(),
            downloads: DashMap::new(),
            catalog: DashMap::new(),
            issued_licenses: DashMap::new(),
            digital_products: DashMap::new(),
            digital_types: DashMap::new(),
            digital_files: DashMap::new(),
            digital_previews: DashMap::new(),
            digital_royalties: DashMap::new(),
            digital_payouts: DashMap::new(),
            digital_downloads: DashMap::new(),
            watermark_formats: DashMap::new(),
            digital_defaults: RwLock::new(DigitalDefaults::default()),
            sample_downloads: DashMap::new(),
            reference_settings: RwLock::new(ReferenceIdSettings::default()),
        };
        for digital_type in seed_digital_types() {
            state
                .digital_types
                .insert(digital_type.id.clone(), digital_type);
        }
        for format in seed_watermark_formats() {
            state
                .watermark_formats
                .insert(format.format.clone(), format);
        }
        state
    }

    pub(crate) fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    pub(crate) fn emit_with_results(&self, event: &str, data: Value) -> HookContext {
        self.bus().emit(event, data)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
}
