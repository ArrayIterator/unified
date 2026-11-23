// Store module - module registration and routes.
//
// Split from `src/modules/store.rs`; the module root re-exports the public API of this module.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;

use super::cart::{add_to_cart, checkout, get_cart, remove_from_cart};
use super::catalog::{check_update, create_catalog_entry, delete_catalog_entry, download_package, get_catalog_entry, list_catalog, update_catalog_entry};
use super::digital_products::{create_digital_file, create_digital_preview, create_digital_product, create_digital_type, delete_digital_file, delete_digital_preview, delete_digital_product, delete_digital_type, get_digital_product, list_digital_files, list_digital_previews, list_digital_products, list_digital_types, publish_digital_product, unpublish_digital_product, update_digital_product, update_digital_type};
use super::digital_sales::{approve_digital_payout, create_digital_download_link, create_digital_payout, download_sample, get_digital_payout, get_digital_royalty, get_digital_settings, list_digital_downloads, list_digital_payouts, list_digital_royalties, list_watermark_formats, mark_digital_payout_paid, reject_digital_payout, update_digital_settings, update_watermark_format, vendor_payout_summary};
use super::events::{BANK_TRANSFER_REJECTED, BANK_TRANSFER_VERIFICATION_CREATED, PAYMENT_CAPTURED, PAYMENT_REFUNDED, PRODUCT_DELISTED, PRODUCT_LISTED, STORE_ENTITLEMENT_CHECK, on_bank_transfer_rejected, on_bank_transfer_verification_created, on_payment_captured, on_payment_refunded, on_product_delisted, on_product_listed, on_properties_purchase_requested, on_software_release_published, on_store_entitlement_check};
use super::orders::{
    cancel_order, complete_order, create_download_link, delete_order, download_order,
    get_entitlement, get_order, list_entitlements, list_license_downloads, list_orders,
    on_store_order_licenses, pay_order, refund_order, signed_download, update_order,
};
use super::products::{calculate_price, create_coupon, create_product, create_tier, delete_coupon, delete_product, delete_tier, get_coupon, get_product, get_tier, list_coupons, list_products, list_tiers, publish_product, unpublish_product, update_coupon, update_product, update_tier};
use super::reference::{get_reference_settings, update_reference_settings};
use super::signing::{STORE_CONFIG};
use super::state::{StoreState};
use super::subscriptions::{cancel_subscription, change_subscription_plan, expire_due_subscriptions, get_subscription, list_subscriptions, on_subscription_lifecycle_apply, on_subscription_module_entitlement_check};
use super::ai_capabilities::AI_CAPABILITIES;

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct StoreModule {
    pub(crate) state: Arc<StoreState>,
}

impl StoreModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(StoreState::new()),
        }
    }
}

impl Default for StoreModule {
    fn default() -> Self {
        Self::new()
    }
}

/// Digital-products selling is declared sellable for the product
/// registry through `DIGITAL_PRODUCTS_AVAILABLE_AS_PRODUCT` (category
/// `commerce`, see the constants near the digital types): the
/// integrator flags that capability, not this module. This module
/// deliberately does NOT override `available_as_product()` /
/// `product_category()`: the store is the marketplace itself, not a
/// sellable module, so listing "Store" as a buyable product
/// offering would be wrong.
impl Module for StoreModule {
    fn id(&self) -> &'static str {
        "store"
    }

    fn name(&self) -> &'static str {
        "Store"
    }

    fn description(&self) -> &'static str {
        "Store for selling software: products with pricing tiers, coupons, cart, checkout, orders, refunds, signed downloads, entitlements, update checks, and subscription checkout with plan changes and cancellation. Digital products (ebooks, audiobooks, templates, course packs, digital assets) sell through the same checkout: paid orders mint per-file download entitlements served by the signed download flow, accrue per-sale royalties for authors and vendors (payout requests are events for billing), offer rate-limited public samples, and watermark delivered files at the metadata level (never claimed as unbreakable DRM)."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["licensing", "billing"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("store", AI_CAPABILITIES);
        let _ = STORE_CONFIG.set(ctx.config.clone());
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            "software.release.published",
            "store",
            Arc::new(move |ctx: &mut HookContext| {
                on_software_release_published(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            PAYMENT_CAPTURED,
            "store",
            Arc::new(move |ctx: &mut HookContext| {
                on_payment_captured(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            PAYMENT_REFUNDED,
            "store",
            Arc::new(move |ctx: &mut HookContext| {
                on_payment_refunded(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            BANK_TRANSFER_VERIFICATION_CREATED,
            "store",
            Arc::new(move |ctx: &mut HookContext| {
                on_bank_transfer_verification_created(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            BANK_TRANSFER_REJECTED,
            "store",
            Arc::new(move |ctx: &mut HookContext| {
                on_bank_transfer_rejected(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            PRODUCT_LISTED,
            "store",
            Arc::new(move |ctx: &mut HookContext| {
                on_product_listed(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            PRODUCT_DELISTED,
            "store",
            Arc::new(move |ctx: &mut HookContext| {
                on_product_delisted(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            STORE_ENTITLEMENT_CHECK,
            "store",
            Arc::new(move |ctx: &mut HookContext| {
                on_store_entitlement_check(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "subscription.lifecycle.apply",
            "store",
            Arc::new(move |ctx: &mut HookContext| {
                on_subscription_lifecycle_apply(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "store.order.licenses",
            "store",
            Arc::new(move |ctx: &mut HookContext| on_store_order_licenses(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            "subscription.module_entitlement.check",
            "store",
            Arc::new(move |ctx: &mut HookContext| {
                on_subscription_module_entitlement_check(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "properties.block.purchase_requested",
            "store",
            Arc::new(move |ctx: &mut HookContext| {
                on_properties_purchase_requested(state.clone(), ctx)
            }),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/store/products", owner: "store", description: "List products", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/store/products", owner: "store", description: "Create product", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/products/{id}", owner: "store", description: "Get product", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/store/products/{id}", owner: "store", description: "Update product", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/store/products/{id}", owner: "store", description: "Delete product", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/store/products/{id}/publish", owner: "store", description: "Publish product", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/store/products/{id}/unpublish", owner: "store", description: "Unpublish product", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/products/{id}/tiers", owner: "store", description: "List tiers", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/store/products/{id}/tiers", owner: "store", description: "Create tier", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/tiers/{id}", owner: "store", description: "Get tier", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/store/tiers/{id}", owner: "store", description: "Update tier", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/store/tiers/{id}", owner: "store", description: "Delete tier", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/coupons", owner: "store", description: "List coupons", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/store/coupons", owner: "store", description: "Create coupon", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/coupons/{id}", owner: "store", description: "Get coupon", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/store/coupons/{id}", owner: "store", description: "Update coupon", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/store/coupons/{id}", owner: "store", description: "Delete coupon", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/store/price-calculate", owner: "store", description: "Calculate the price of a product (optionally a tier and a coupon) without creating an order.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/cart", owner: "store", description: "Get cart", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/store/cart", owner: "store", description: "Add to cart", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/store/cart", owner: "store", description: "Remove from cart", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/store/checkout", owner: "store", description: "Checkout", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/orders", owner: "store", description: "List orders", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/orders/{id}", owner: "store", description: "Get order", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/store/orders/{id}", owner: "store", description: "Update order", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/store/orders/{id}", owner: "store", description: "Delete order", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/store/orders/{id}/pay", owner: "store", description: "Pay order", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/reference-settings", owner: "store", description: "Read payment reference settings (default prefix, per-product prefixes).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/store/reference-settings", owner: "store", description: "Update payment reference settings (capability store.reference:manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/store/orders/{id}/complete", owner: "store", description: "Complete order", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/store/orders/{id}/cancel", owner: "store", description: "Cancel order", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/store/orders/{id}/refund", owner: "store", description: "Refund order", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/orders/{id}/download", owner: "store", description: "Download order", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/entitlements", owner: "store", description: "List entitlements", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/entitlements/{id}", owner: "store", description: "Get entitlement", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/subscriptions", owner: "store", description: "List subscriptions with pagination and filters.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/store/subscriptions/expire-due", owner: "store", description: "Expire subscriptions whose access window has closed (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/subscriptions/{id}", owner: "store", description: "Get one subscription.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/store/subscriptions/{id}/change-plan", owner: "store", description: "Change a subscription plan mid-cycle, applying the product proration policy.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/store/subscriptions/{id}/cancel", owner: "store", description: "Cancel a subscription immediately or at the end of the current period.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/store/entitlements/{id}/download-link", owner: "store", description: "Create download link", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/downloads/signed", owner: "store", description: "Signed download", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/licenses/{license_id}/downloads", owner: "store", description: "List license downloads", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/updates/catalog", owner: "store", description: "List catalog", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/store/updates/catalog", owner: "store", description: "Create catalog entry. Requires store.catalog:manage.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/updates/catalog/{slug}", owner: "store", description: "Get catalog entry", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/store/updates/catalog/{slug}", owner: "store", description: "Update catalog entry. Requires store.catalog:manage.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/store/updates/catalog/{slug}", owner: "store", description: "Delete catalog entry. Requires store.catalog:manage.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/store/updates/check", owner: "store", description: "WordPress-style version check: given a slug and the installed version, return the latest version, whether an update is available, compatibility metadata,...", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/updates/{slug}/package", owner: "store", description: "Download the latest package for a slug.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/digital/types", owner: "store", description: "List digital product types (read; admin-extensible registry).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/store/digital/types", owner: "store", description: "Create digital product type (manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/store/digital/types/{type_id}", owner: "store", description: "Update digital product type (manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/store/digital/types/{type_id}", owner: "store", description: "Delete digital product type (manage; refused while in use).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/digital/downloads", owner: "store", description: "List per-buyer digital download history (read; own history, or any with manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/digital/royalties", owner: "store", description: "List digital royalty ledger (read; own royalties, or all with manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/digital/royalties/{id}", owner: "store", description: "Get one digital royalty (read; own royalty, or any with manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/digital/payouts", owner: "store", description: "List digital payout requests (read; own requests, or all with manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/store/digital/payouts", owner: "store", description: "Request a royalty payout (read; emits store.digital.payout.requested for billing).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/digital/payouts/{id}", owner: "store", description: "Get one digital payout request (read; own request, or any with manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/store/digital/payouts/{id}/approve", owner: "store", description: "Approve a digital payout request (manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/store/digital/payouts/{id}/mark-paid", owner: "store", description: "Mark a digital payout paid after billing moved the money (manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/store/digital/payouts/{id}/reject", owner: "store", description: "Reject a digital payout request and release its royalties (manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/digital/settings", owner: "store", description: "Get digital defaults and watermark formats (read).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/store/digital/settings", owner: "store", description: "Update digital defaults (manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/digital/watermark-formats", owner: "store", description: "List watermark formats (read).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/store/digital/watermark-formats/{format}", owner: "store", description: "Update a watermark format (manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/store/digital/files/{file_id}", owner: "store", description: "Delete a digital file (manage; refused with entitlements or history).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/store/digital/previews/{preview_id}", owner: "store", description: "Delete a digital preview (manage; refused with download history).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/digital", owner: "store", description: "List digital products (public storefront; published only).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "POST", path: "/api/v1/store/digital", owner: "store", description: "Create digital product (manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/digital/{id}", owner: "store", description: "Get digital product (public storefront; published only).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "PUT", path: "/api/v1/store/digital/{id}", owner: "store", description: "Update digital product (manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/store/digital/{id}", owner: "store", description: "Delete digital product (manage; refused with files, previews, or history).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/store/digital/{id}/publish", owner: "store", description: "Publish digital product (manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/store/digital/{id}/unpublish", owner: "store", description: "Unpublish digital product (manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/digital/{id}/files", owner: "store", description: "List files of a digital product (read).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/store/digital/{id}/files", owner: "store", description: "Add a file to a digital product (manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/digital/{id}/previews", owner: "store", description: "List previews of a digital product (public storefront; allowed only).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "POST", path: "/api/v1/store/digital/{id}/previews", owner: "store", description: "Add a preview to a digital product (manage).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/digital/{id}/sample", owner: "store", description: "Download the free sample of a digital product (public when allowed; rate-limited and logged).", auth: unified_core::auth::RouteAuth::Public},
            RouteInfo { method: "POST", path: "/api/v1/store/digital/{id}/download-link", owner: "store", description: "Create a signed expiring download link for a purchased digital file (read; paid order required).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/store/vendors/{company_id}/payout-summary", owner: "store", description: "Vendor payout summary", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/store")
                .route("/products", web::get().to(list_products))
                .route("/products", web::post().to(create_product))
                .route("/products/{id}", web::get().to(get_product))
                .route("/products/{id}", web::put().to(update_product))
                .route("/products/{id}", web::delete().to(delete_product))
                .route("/products/{id}/publish", web::post().to(publish_product))
                .route("/products/{id}/unpublish", web::post().to(unpublish_product))
                .route("/products/{id}/tiers", web::get().to(list_tiers))
                .route("/products/{id}/tiers", web::post().to(create_tier))
                .route("/tiers/{id}", web::get().to(get_tier))
                .route("/tiers/{id}", web::put().to(update_tier))
                .route("/tiers/{id}", web::delete().to(delete_tier))
                .route("/coupons", web::get().to(list_coupons))
                .route("/coupons", web::post().to(create_coupon))
                .route("/coupons/{id}", web::get().to(get_coupon))
                .route("/coupons/{id}", web::put().to(update_coupon))
                .route("/coupons/{id}", web::delete().to(delete_coupon))
                .route("/price-calculate", web::post().to(calculate_price))
                .route("/cart", web::get().to(get_cart))
                .route("/cart", web::post().to(add_to_cart))
                .route("/cart", web::delete().to(remove_from_cart))
                .route("/checkout", web::post().to(checkout))
                .route("/orders", web::get().to(list_orders))
                .route("/orders/{id}", web::get().to(get_order))
                .route("/orders/{id}", web::put().to(update_order))
                .route("/orders/{id}", web::delete().to(delete_order))
                .route("/orders/{id}/pay", web::post().to(pay_order))
                .route("/reference-settings", web::get().to(get_reference_settings))
                .route("/reference-settings", web::put().to(update_reference_settings))
                .route("/orders/{id}/complete", web::post().to(complete_order))
                .route("/orders/{id}/cancel", web::post().to(cancel_order))
                .route("/orders/{id}/refund", web::post().to(refund_order))
                .route("/orders/{id}/download", web::get().to(download_order))
                .route("/entitlements", web::get().to(list_entitlements))
                .route("/entitlements/{id}", web::get().to(get_entitlement))
                .route("/subscriptions", web::get().to(list_subscriptions))
                .route(
                    "/subscriptions/expire-due",
                    web::post().to(expire_due_subscriptions),
                )
                .route("/subscriptions/{id}", web::get().to(get_subscription))
                .route(
                    "/subscriptions/{id}/change-plan",
                    web::post().to(change_subscription_plan),
                )
                .route(
                    "/subscriptions/{id}/cancel",
                    web::post().to(cancel_subscription),
                )
                .route(
                    "/entitlements/{id}/download-link",
                    web::post().to(create_download_link),
                )
                .route("/downloads/signed", web::get().to(signed_download))
                .route(
                    "/licenses/{license_id}/downloads",
                    web::get().to(list_license_downloads),
                )
                .route("/updates/catalog", web::get().to(list_catalog))
                .route("/updates/catalog", web::post().to(create_catalog_entry))
                .route("/updates/catalog/{slug}", web::get().to(get_catalog_entry))
                .route(
                    "/updates/catalog/{slug}",
                    web::put().to(update_catalog_entry),
                )
                .route(
                    "/updates/catalog/{slug}",
                    web::delete().to(delete_catalog_entry),
                )
                .route("/updates/check", web::post().to(check_update))
                .route("/updates/{slug}/package", web::get().to(download_package))
                // Digital products: static paths first so they win
                // over `/digital/{id}` in every router.
                .route("/digital/types", web::get().to(list_digital_types))
                .route("/digital/types", web::post().to(create_digital_type))
                .route("/digital/types/{type_id}", web::put().to(update_digital_type))
                .route(
                    "/digital/types/{type_id}",
                    web::delete().to(delete_digital_type),
                )
                .route("/digital/downloads", web::get().to(list_digital_downloads))
                .route("/digital/royalties", web::get().to(list_digital_royalties))
                .route(
                    "/digital/royalties/{id}",
                    web::get().to(get_digital_royalty),
                )
                .route("/digital/payouts", web::get().to(list_digital_payouts))
                .route("/digital/payouts", web::post().to(create_digital_payout))
                .route("/digital/payouts/{id}", web::get().to(get_digital_payout))
                .route(
                    "/digital/payouts/{id}/approve",
                    web::post().to(approve_digital_payout),
                )
                .route(
                    "/digital/payouts/{id}/mark-paid",
                    web::post().to(mark_digital_payout_paid),
                )
                .route(
                    "/digital/payouts/{id}/reject",
                    web::post().to(reject_digital_payout),
                )
                .route("/digital/settings", web::get().to(get_digital_settings))
                .route(
                    "/digital/settings",
                    web::put().to(update_digital_settings),
                )
                .route(
                    "/digital/watermark-formats",
                    web::get().to(list_watermark_formats),
                )
                .route(
                    "/digital/watermark-formats/{format}",
                    web::put().to(update_watermark_format),
                )
                .route(
                    "/digital/files/{file_id}",
                    web::delete().to(delete_digital_file),
                )
                .route(
                    "/digital/previews/{preview_id}",
                    web::delete().to(delete_digital_preview),
                )
                .route("/digital", web::get().to(list_digital_products))
                .route("/digital", web::post().to(create_digital_product))
                .route("/digital/{id}", web::get().to(get_digital_product))
                .route("/digital/{id}", web::put().to(update_digital_product))
                .route("/digital/{id}", web::delete().to(delete_digital_product))
                .route(
                    "/digital/{id}/publish",
                    web::post().to(publish_digital_product),
                )
                .route(
                    "/digital/{id}/unpublish",
                    web::post().to(unpublish_digital_product),
                )
                .route("/digital/{id}/files", web::get().to(list_digital_files))
                .route("/digital/{id}/files", web::post().to(create_digital_file))
                .route(
                    "/digital/{id}/previews",
                    web::get().to(list_digital_previews),
                )
                .route(
                    "/digital/{id}/previews",
                    web::post().to(create_digital_preview),
                )
                .route("/digital/{id}/sample", web::get().to(download_sample))
                .route(
                    "/digital/{id}/download-link",
                    web::post().to(create_digital_download_link),
                )
                .route(
                    "/vendors/{company_id}/payout-summary",
                    web::get().to(vendor_payout_summary),
                ),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(StoreModule::new()));
}
