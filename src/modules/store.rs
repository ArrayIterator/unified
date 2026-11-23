// Cart / Store / Marketplace module.
//
// Products linked to software releases, pricing tiers and editions,
// coupons, a per-session cart with server-side totals, checkout that
// creates an order and emits `order.created` (licensing and billing listen
// to it) as well as `store.order.created`, payment initiation, refunds,
// cancellations, signed expiring download links with per-license limits,
// download entitlements, a WordPress-style update API, and per-company
// payout attribution.
//
// Subscription checkout: a paid order holding exactly one
// subscription product creates a `Subscription` (`trialing` when the
// product has trial days, else `active`), snapshotting the product's
// plan fields (proration policy, grace period, dunning settings).
// Plan changes mid-cycle apply the proration policy, cancellation
// is immediate or at period end, and the billing module drives the
// lifecycle (renewals, dunning, grace, reminders) through
// `subscription.lifecycle.apply` events, which this module applies
// to the authoritative record. A subscription to a product bound to
// a sellable module counts as a module entitlement through
// `subscription.module_entitlement.check` (see the integration
// notes for the licensing convergence point).
//
// Digital products (ebooks and other downloadable digital goods):
// a store product gains a digital profile (an admin-extensible
// type from the digital type registry, file metadata such as
// author, ISBN, pages, language, publisher and edition, a cover
// image reference, and its deliverable files as storage
// references). Digital products sell through the normal checkout
// like any other product. When an order containing one becomes
// paid, this module mints one download entitlement per file and
// accrues the per-sale royalty for the author or vendor. Delivery
// reuses the existing signed expiring download-link flow: the
// purchase grant id plays the `license_id` role in the signed URL
// and the file id plays the `release_id` role, so no second
// download system exists. Free samples are served from preview
// files the seller allows, rate-limited and logged. Watermarking
// is metadata-level only: the buyer identity is embedded into the
// delivered file's metadata where the format supports it (see the
// watermark format registry); it is NOT unbreakable DRM and must
// never be claimed as such. Royalties accrue here; payout requests
// only emit `store.digital.payout.requested` for billing, which
// owns moving money. Digital-products selling is a sellable store
// capability, category `commerce` (see
// `DIGITAL_PRODUCTS_AVAILABLE_AS_PRODUCT`); the store module
// itself is the marketplace, not a sellable module.
//
// Interconnection happens only through the hook bus: this module never
// calls another module directly. License issuance is done by the
// licensing module through the `order.created` hook; this module reads
// the hook results back and records the download entitlement for each
// license that was issued.
//
// All data lives in memory in this module; persistence of
// products, tiers, coupons, orders, entitlements and download
// records is a wiring step for the database layer. The digital
// products tables (`digital_products`, `digital_product_files`,
// `digital_product_previews`, `digital_royalties`,
// `digital_downloads`, plus the `digital_product_types` registry
// and `digital_payout_requests`) exist as persistence targets in
// migration 2026-11-22-000075 and are not yet this module's read
// path: the in-memory maps stay authoritative until the database
// layer is wired.

pub mod ai_capabilities;
pub mod types;
pub mod state;
pub mod reference;
mod pricing;
mod helpers;
mod digital;
mod signing;
mod events;
mod products;
mod cart;
mod subscriptions;
mod orders;
mod catalog;
mod digital_products;
pub mod digital_sales;
pub mod routes;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::Product;
pub use self::types::CreateProduct;
pub use self::types::UpdateProduct;
pub use self::types::PricingTier;
pub use self::types::CreateTier;
pub use self::types::UpdateTier;
pub use self::types::Coupon;
pub use self::types::CreateCoupon;
pub use self::types::UpdateCoupon;
pub use self::types::PriceCalculateRequest;
pub use self::types::CartItem;
pub use self::types::Cart;
pub use self::types::AddCartItem;
pub use self::types::RemoveCartItem;
pub use self::types::OrderItem;
pub use self::types::Order;
pub use self::types::Subscription;
pub use self::types::ChangePlanRequest;
pub use self::types::CancelSubscriptionRequest;
pub use self::types::CheckoutItem;
pub use self::types::CheckoutRequest;
pub use self::types::PayRequest;
pub use self::types::UpdateOrder;
pub use self::types::RefundRequest;
pub use self::types::DownloadEntitlement;
pub use self::types::DownloadRecord;
pub use self::types::DownloadLinkRequest;
pub use self::types::IssuedLicense;
pub use self::types::CatalogEntry;
pub use self::types::CreateCatalogEntry;
pub use self::types::UpdateCatalogEntry;
pub use self::types::UpdateCheckRequest;
pub use self::types::SignedDownloadQuery;
pub use self::types::PackageQuery;
pub use self::types::DIGITAL_PRODUCTS_AVAILABLE_AS_PRODUCT;
pub use self::types::DIGITAL_PRODUCTS_PRODUCT_CATEGORY;
pub use self::types::DigitalProductType;
pub use self::types::CreateDigitalProductType;
pub use self::types::UpdateDigitalProductType;
pub use self::types::DigitalProduct;
pub use self::types::CreateDigitalProduct;
pub use self::types::UpdateDigitalProduct;
pub use self::types::DigitalProductFile;
pub use self::types::CreateDigitalFile;
pub use self::types::DigitalProductPreview;
pub use self::types::CreateDigitalPreview;
pub use self::types::DigitalRoyalty;
pub use self::types::DigitalPayoutRequest;
pub use self::types::CreateDigitalPayout;
pub use self::types::DigitalDownload;
pub use self::types::WatermarkFormat;
pub use self::types::DigitalDefaults;
pub use self::types::UpdateDigitalSettings;
pub use self::types::UpdateWatermarkFormat;
pub use self::types::DigitalDownloadLinkRequest;
pub use self::state::StoreState;
pub use self::reference::UpdateReferenceSettings;
pub use self::digital_sales::SampleQuery;
pub use self::routes::StoreModule;
pub use self::routes::register;
