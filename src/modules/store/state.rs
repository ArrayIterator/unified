// Store module - in-memory state.
//
// Split from `src/modules/store.rs`; the module root re-exports the public API of this module.

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use serde_json::Value;
use std::sync::{Arc, RwLock};
use unified_core::db::{
    DbPool,
    models::{
        StoreCatalog as DbStoreCatalog, StoreDownload as DbStoreDownload,
        StoreEntitlement as DbStoreEntitlement, StoreIssuedLicense as DbStoreIssuedLicense,
        StoreOrder as DbStoreOrder, StoreOrderItem as DbStoreOrderItem,
        StorePricingTier as DbStorePricingTier, StoreProduct as DbStoreProduct,
    },
    money::pg_numeric_to_minor,
};
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::reference_id::ReferenceIdSettings;
use uuid::Uuid;

use super::digital::{seed_digital_types, seed_watermark_formats};
use super::reference::reference_settings;
use super::types::{
    Cart, CatalogEntry, Coupon, DigitalDefaults, DigitalDownload, DigitalPayoutRequest,
    DigitalProduct, DigitalProductFile, DigitalProductPreview, DigitalProductType, DigitalRoyalty,
    DownloadEntitlement, DownloadRecord, IssuedLicense, Order, OrderItem, PricingTier, Product,
    Subscription, WatermarkFormat,
};

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

    /// Hydrate the in-memory store maps from the database (C11,
    /// task 271): products, pricing tiers, orders (with their
    /// items), download entitlements, download records, the
    /// update-API catalog, and issued licenses are loaded from
    /// the `store_*` tables and replace the map contents, so a
    /// restart restores the catalogue and the order history
    /// instead of losing them. Rows that cannot be converted —
    /// an amount that does not fit in minor units, an order
    /// item / entitlement / download whose required id is NULL
    /// in the database — are skipped, never fatal: hydration
    /// must not fail startup on one bad row. Fields the
    /// database does not carry take their runtime defaults:
    /// product / order-item dunning and proration policy,
    /// `module_id`, order `discount_evaluation` /
    /// `payment_reference`, and item `course_id` / `exam_id`
    /// (the `store_order_items` table carries neither course
    /// nor exam id yet). Coupons are NOT hydrated: the runtime
    /// coupon splits one `discount_value` into percent-basis
    /// -points vs fixed minor units and no writer records which
    /// encoding a stored value uses, so the mapping is
    /// ambiguous until the coupon write path lands. Carts,
    /// subscriptions, and the digital-product maps have no
    /// tables yet and stay in-memory-only. Call once from
    /// `StoreModule::init` after the pool is attached (see
    /// `docs/integration/INTEGRATION_NOTES_C11.md` for the
    /// exact call). Returns the number of rows loaded.
    pub(crate) fn hydrate_from_db(&self, pool: &DbPool) -> UnifiedResult<usize> {
        use unified_core::db::schema::{
            store_catalog::dsl as cat_dsl, store_downloads::dsl as dl_dsl,
            store_entitlements::dsl as ent_dsl, store_issued_licenses::dsl as il_dsl,
            store_order_items::dsl as oi_dsl, store_orders::dsl as ord_dsl,
            store_pricing_tiers::dsl as tier_dsl, store_products::dsl as prod_dsl,
        };
        let mut conn = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot get database connection: {error}"))
        })?;
        let db_products = prod_dsl::store_products
            .select(DbStoreProduct::as_select())
            .load::<DbStoreProduct>(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load store products: {error}"))
            })?;
        let db_tiers = tier_dsl::store_pricing_tiers
            .select(DbStorePricingTier::as_select())
            .load::<DbStorePricingTier>(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load store pricing tiers: {error}"))
            })?;
        let db_orders = ord_dsl::store_orders
            .select(DbStoreOrder::as_select())
            .load::<DbStoreOrder>(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load store orders: {error}"))
            })?;
        let db_order_items = oi_dsl::store_order_items
            .select(DbStoreOrderItem::as_select())
            .load::<DbStoreOrderItem>(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load store order items: {error}"))
            })?;
        let db_entitlements = ent_dsl::store_entitlements
            .select(DbStoreEntitlement::as_select())
            .load::<DbStoreEntitlement>(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load store entitlements: {error}"))
            })?;
        let db_downloads = dl_dsl::store_downloads
            .select(DbStoreDownload::as_select())
            .load::<DbStoreDownload>(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load store downloads: {error}"))
            })?;
        let db_catalog = cat_dsl::store_catalog
            .select(DbStoreCatalog::as_select())
            .load::<DbStoreCatalog>(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load store catalog: {error}"))
            })?;
        let db_issued = il_dsl::store_issued_licenses
            .select(DbStoreIssuedLicense::as_select())
            .load::<DbStoreIssuedLicense>(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load issued licenses: {error}"))
            })?;

        let mut loaded = 0;
        let mut currency_by_product: std::collections::HashMap<Uuid, String> =
            std::collections::HashMap::new();
        self.products.clear();
        for row in &db_products {
            let Some(price) = pg_numeric_to_minor(&row.price, &row.currency) else {
                continue;
            };
            // A renewal price that does not convert degrades to
            // `None`: the runtime falls back to the base price.
            let renewal_price = row
                .renewal_price
                .as_ref()
                .and_then(|value| pg_numeric_to_minor(value, &row.currency));
            currency_by_product.insert(row.id, row.currency.clone());
            self.products.insert(
                row.id.to_string(),
                Product {
                    id: row.id.to_string(),
                    name: row.name.clone(),
                    description: row.description.clone(),
                    price,
                    currency: row.currency.clone(),
                    kind: row.kind.clone(),
                    license_product: row.license_product,
                    file_id: row.file_id.map(|id| id.to_string()),
                    slug: row.slug.clone(),
                    software_id: row.software_id.map(|id| id.to_string()),
                    software_slug: row.software_slug.clone(),
                    status: row.status.clone(),
                    company_id: row.company_id.map(|id| id.to_string()),
                    pricing_model: row.pricing_model.clone(),
                    billing_period: row.billing_period.clone(),
                    renewal_price,
                    trial_days: row.trial_days.max(0) as u32,
                    proration_policy: super::types::default_proration_policy(),
                    grace_period_days: 0,
                    dunning_max_retries: super::types::default_dunning_max_retries(),
                    dunning_retry_interval_days: super::types::default_dunning_retry_interval_days(
                    ),
                    module_id: None,
                    updates_years: row
                        .updates_years
                        .filter(|years| *years >= 0)
                        .map(|years| years as u32),
                    created_at: row.created_at,
                    updated_at: row.updated_at,
                },
            );
            loaded += 1;
        }
        self.tiers.clear();
        for row in &db_tiers {
            // Tiers carry no currency column: the parent
            // product's currency prices them.
            let Some(currency) = currency_by_product.get(&row.product_id) else {
                continue;
            };
            let Some(price) = pg_numeric_to_minor(&row.price, currency) else {
                continue;
            };
            let renewal_price = row
                .renewal_price
                .as_ref()
                .and_then(|value| pg_numeric_to_minor(value, currency));
            let features = row
                .features
                .as_array()
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            self.tiers.insert(
                row.id.to_string(),
                PricingTier {
                    id: row.id.to_string(),
                    product_id: row.product_id.to_string(),
                    slug: row.slug.clone(),
                    label: row.label.clone(),
                    price,
                    renewal_price,
                    billing_period: row.billing_period.clone(),
                    trial_days: row.trial_days.max(0) as u32,
                    seats: row.seats,
                    download_limit: row.download_limit.max(0) as u32,
                    features,
                    status: row.status.clone(),
                    created_at: row.created_at,
                    updated_at: row.updated_at,
                },
            );
            loaded += 1;
        }
        let mut currency_by_order: std::collections::HashMap<Uuid, String> =
            std::collections::HashMap::new();
        let mut items_by_order: std::collections::HashMap<String, Vec<OrderItem>> =
            std::collections::HashMap::new();
        for row in &db_orders {
            currency_by_order.insert(row.id, row.currency.clone());
        }
        for item in &db_order_items {
            let Some(currency) = currency_by_order.get(&item.order_id) else {
                continue;
            };
            let Some(product_id) = item.product_id.map(|id| id.to_string()) else {
                continue;
            };
            let Some(unit_price) = pg_numeric_to_minor(&item.unit_price, currency) else {
                continue;
            };
            let Some(line_total) = pg_numeric_to_minor(&item.line_total, currency) else {
                continue;
            };
            items_by_order
                .entry(item.order_id.to_string())
                .or_default()
                .push(OrderItem {
                    product_id,
                    name: item.name.clone(),
                    unit_price,
                    quantity: item.quantity.max(0) as u32,
                    line_total,
                    license_product: item.license_product,
                    file_id: item.file_id.map(|id| id.to_string()),
                    tier_id: item.tier_id.map(|id| id.to_string()),
                    tier_name: item.tier_name.clone(),
                    software_id: item.software_id.map(|id| id.to_string()),
                    software_slug: item.software_slug.clone(),
                    company_id: item.company_id.map(|id| id.to_string()),
                    pricing_model: item.pricing_model.clone(),
                    billing_period: item.billing_period.clone(),
                    seats: item.seats,
                    expires_at: item.expires_at,
                    download_limit: item.download_limit.max(0) as u32,
                    course_id: None,
                    exam_id: None,
                    trial_days: 0,
                    proration_policy: super::types::default_proration_policy(),
                    grace_period_days: 0,
                    dunning_max_retries: super::types::default_dunning_max_retries(),
                    dunning_retry_interval_days: super::types::default_dunning_retry_interval_days(
                    ),
                    module_id: None,
                });
        }
        self.orders.clear();
        for row in &db_orders {
            let currency = row.currency.clone();
            let Some(subtotal) = pg_numeric_to_minor(&row.subtotal, &currency) else {
                continue;
            };
            let Some(discount_total) = pg_numeric_to_minor(&row.discount_total, &currency) else {
                continue;
            };
            let Some(total) = pg_numeric_to_minor(&row.total, &currency) else {
                continue;
            };
            let mut order = Order {
                id: row.id.to_string(),
                reference: row.reference.clone(),
                user_id: row.user_id.clone(),
                items: Vec::new(),
                subtotal,
                discount_total,
                total,
                currency,
                coupon_code: row.coupon_code.clone(),
                discount_evaluation: None,
                company_id: row.company_id.map(|id| id.to_string()),
                trial_until: row.trial_until,
                status: row.status.clone(),
                provider: row.provider.clone(),
                payment_reference: None,
                created_at: row.created_at,
                updated_at: row.updated_at,
            };
            if let Some(items) = items_by_order.remove(&order.id) {
                order.items = items;
            }
            self.orders.insert(order.id.clone(), order);
            loaded += 1;
        }
        self.entitlements.clear();
        for row in &db_entitlements {
            let Some(order_id) = row.order_id.map(|id| id.to_string()) else {
                continue;
            };
            let Some(product_id) = row.product_id.map(|id| id.to_string()) else {
                continue;
            };
            self.entitlements.insert(
                row.id.to_string(),
                DownloadEntitlement {
                    id: row.id.to_string(),
                    license_id: row.license_id.map(|id| id.to_string()),
                    license_key: row.license_key.clone(),
                    order_id,
                    user_id: row.user_id.clone(),
                    product_id,
                    tier_id: row.tier_id.map(|id| id.to_string()),
                    software_id: row.software_id.map(|id| id.to_string()),
                    software_slug: row.software_slug.clone(),
                    release_id: row.release_id.map(|id| id.to_string()),
                    download_limit: row.download_limit.max(0) as u32,
                    downloads_used: row.downloads_used.max(0) as u32,
                    status: row.status.clone(),
                    expires_at: row.expires_at,
                    created_at: row.created_at,
                    updated_at: row.updated_at,
                },
            );
            loaded += 1;
        }
        self.downloads.clear();
        for row in &db_downloads {
            let Some(license_id) = row.license_id.map(|id| id.to_string()) else {
                continue;
            };
            let Some(release_id) = row.release_id.map(|id| id.to_string()) else {
                continue;
            };
            self.downloads.insert(
                row.id.to_string(),
                DownloadRecord {
                    id: row.id.to_string(),
                    license_id,
                    release_id,
                    software_slug: row.software_slug.clone(),
                    order_id: row.order_id.map(|id| id.to_string()),
                    downloaded_at: row.downloaded_at,
                },
            );
            loaded += 1;
        }
        self.catalog.clear();
        for row in &db_catalog {
            self.catalog.insert(
                row.slug.clone(),
                CatalogEntry {
                    slug: row.slug.clone(),
                    name: row.name.clone(),
                    kind: row.kind.clone(),
                    product_id: row.product_id.map(|id| id.to_string()),
                    product_name: row.product_name.clone(),
                    latest_version: row.latest_version.clone(),
                    release_id: row.release_id.map(|id| id.to_string()),
                    file_id: row.file_id.map(|id| id.to_string()),
                    changelog: row.changelog.clone(),
                    requires_wp: row.requires_wp.clone(),
                    tested_wp: row.tested_wp.clone(),
                    requires_php: row.requires_php.clone(),
                    license_required: row.license_required,
                    created_at: row.created_at,
                    updated_at: row.updated_at,
                },
            );
            loaded += 1;
        }
        self.issued_licenses.clear();
        for row in &db_issued {
            self.issued_licenses.insert(
                row.license_id.to_string(),
                IssuedLicense {
                    license_id: row.license_id.to_string(),
                    license_key: row.license_key.clone(),
                    product: row.product.clone(),
                    order_id: row.order_id.map(|id| id.to_string()),
                    user_id: row.user_id.clone(),
                    company_id: row.company_id.map(|id| id.to_string()),
                    status: row.status.clone(),
                    created_at: row.created_at,
                },
            );
            loaded += 1;
        }
        Ok(loaded)
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
