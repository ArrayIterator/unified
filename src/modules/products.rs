// Product Management module - the single source of truth for "what
// we sell".
//
// This module owns the full product lifecycle (draft -> active ->
// deprecated -> retired, with validated transitions), product kinds
// (digital, physical, service, subscription, bundle, meeting,
// hosting), variants and options, bundles of products with bundle
// pricing rules, pricing (tiers, per-seat, usage-based, one-time /
// subscription, regional pricing, taxes), subscription plan fields
// on subscription products (trial, proration policy, grace period,
// dunning settings - read back through
// `GET /api/v1/products/{id}/subscription-plan` and snapshotted
// into subscriptions by the store at checkout), inventory for physical
// goods with low-stock alerts, product files and versions that only
// reference Software Manager records (this module never handles
// files itself), product groups / categories / tags mirroring the
// software taxonomy shapes, and vendor ownership validated through
// the `company.exists` hook.
//
// The store sells what this module defines: a product becomes
// buyable in the store when it is listed here, via the
// `product.listed` / `product.delisted` events. Licensed products
// declare their license template and seat counts (read back through
// `GET /api/v1/products/{id}/license-template`), billing reads price
// and tax data through `POST /api/v1/products/{id}/price-quote`, and
// support routing reads the owning department stored on the product
// (and can open a ticket through `ticket.create_requested`). Entitlement questions are answered by
// the `product.entitlement.check` hook, which asks the store through
// `store.entitlement.check` and says so honestly when no store
// handler answers.
//
// Money is decimal-safe: every amount is stored as integer minor
// units (`i64`, cents for two-decimal currencies, whole units for
// zero-decimal ones) and parsed from decimal strings with exact
// integer arithmetic. No floating-point value is ever used for
// money. Durable storage uses NUMERIC(19,4); see the module report
// for the migration sketch.
//
// Persistence is in-memory (DashMap) in this module; the Diesel
// tables for durable storage are listed in the module report and
// are applied by a later wiring step.

pub mod ai_capabilities;
pub mod money;
pub mod types;
pub mod state;
mod validation;
mod handlers_products;
mod helpers;
mod handlers_catalog;
mod handlers_inventory;
mod hooks;
mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::PRODUCT_KINDS;
pub use self::types::PRODUCT_STATUSES;
pub use self::types::PRICING_MODELS;
pub use self::types::BILLING_PERIODS;
pub use self::types::PRORATION_POLICIES;
pub use self::types::MAX_DUNNING_RETRIES;
pub use self::types::MAX_GRACE_PERIOD_DAYS;
pub use self::types::MAX_DUNNING_RETRY_INTERVAL_DAYS;
pub use self::types::BUNDLE_RULES;
pub use self::types::TAXONOMY_KINDS;
pub use self::types::PERM_PRODUCTS_READ;
pub use self::types::PERM_PRODUCTS_CREATE;
pub use self::types::PERM_PRODUCTS_MANAGE;
pub use self::money::Money;
pub use self::money::MoneyInput;
pub use self::types::LicenseTemplate;
pub use self::types::Product;
pub use self::types::CreateProduct;
pub use self::types::SubscriptionPlanView;
pub use self::types::UpdateProduct;
pub use self::types::TransitionProduct;
pub use self::types::InventorySummary;
pub use self::types::ProductDetail;
pub use self::types::ProductOption;
pub use self::types::CreateProductOption;
pub use self::types::ProductVariant;
pub use self::types::CreateProductVariant;
pub use self::types::UpdateProductVariant;
pub use self::types::BundleItem;
pub use self::types::AddBundleItem;
pub use self::types::UpdateBundleRule;
pub use self::types::PriceTier;
pub use self::types::CreatePriceTier;
pub use self::types::UpdatePriceTier;
pub use self::types::RegionalPrice;
pub use self::types::CreateRegionalPrice;
pub use self::types::UpdateRegionalPrice;
pub use self::types::UsagePrice;
pub use self::types::CreateUsagePrice;
pub use self::types::UpdateUsagePrice;
pub use self::types::TaxRule;
pub use self::types::CreateTaxRule;
pub use self::types::UpdateTaxRule;
pub use self::types::PriceQuote;
pub use self::types::PriceQuoteRequest;
pub use self::types::Inventory;
pub use self::types::AdjustInventory;
pub use self::types::ProductFile;
pub use self::types::CreateProductFile;
pub use self::types::ProductVersion;
pub use self::types::CreateProductVersion;
pub use self::types::TaxonomyNode;
pub use self::types::TaxonomyTreeNode;
pub use self::types::CreateTaxonomyNode;
pub use self::types::UpdateTaxonomyNode;
pub use self::types::MoveTaxonomyNode;
pub use self::types::Tag;
pub use self::types::CreateTag;
pub use self::types::UpdateTag;
pub use self::types::TagCount;
pub use self::types::AttachTag;
pub use self::types::EntitlementCheckRequest;
pub use self::types::CreateSupportTicket;
pub use self::state::ProductsState;
pub use self::module::ProductsModule;
pub use self::module::register;
