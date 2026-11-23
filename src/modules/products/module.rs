//! Module registration: routes, hooks and metadata.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry};
use crate::modules::ai;
use actix_web::{web};
use std::sync::{Arc};
use super::state::ProductsState;
use super::handlers_products::{create_product, create_support_ticket, delete_product, delist_product, entitlement_check, get_license_template, get_product, get_subscription_plan, list_product, list_products, price_quote, transition_product, update_product};
use super::handlers_catalog::{add_bundle_item, create_option, create_regional_price, create_tax_rule, create_tier, create_usage_price, create_variant, delete_bundle_item, delete_option, delete_regional_price, delete_tax_rule, delete_tier, delete_usage_price, delete_variant, get_tier, get_variant, list_bundle_items, list_module_offerings, list_options, list_regional_prices, list_tax_rules, list_tiers, list_usage_prices, list_variants, update_bundle_rule, update_regional_price, update_tax_rule, update_tier, update_usage_price, update_variant};
use super::handlers_inventory::{adjust_inventory, attach_product_tag, create_product_file, create_product_version, create_tag, create_taxonomy_node, delete_product_file, delete_product_version, delete_tag, delete_taxonomy_node, detach_product_tag, get_tag, get_taxonomy_node, list_inventory, list_low_stock, list_product_files, list_product_tags, list_product_versions, list_tags, list_taxonomy, list_taxonomy_children, move_taxonomy_node, tag_cloud, taxonomy_tree, update_tag, update_taxonomy_node};
use super::hooks::on_product_entitlement_check;
use super::ai_capabilities::{AI_CAPABILITIES};

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct ProductsModule {
    pub(crate) state: Arc<ProductsState>,
}

impl ProductsModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(ProductsState::new()),
        }
    }
}

impl Default for ProductsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ProductsModule {
    fn id(&self) -> &'static str {
        "products"
    }

    fn name(&self) -> &'static str {
        "Product Management"
    }

    fn description(&self) -> &'static str {
        "Product management, the single source of truth for what is sold: lifecycle, kinds, variants and options, bundles, pricing tiers with per-seat, usage-based and regional pricing, taxes, inventory with low-stock alerts, product files and versions referencing the Software Manager, taxonomy and tags, vendor ownership, store listing, license templates, billing price quotes, support routing, and entitlement checks."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["store", "licensing", "billing", "software"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("products", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Only `product.entitlement.check` is answered here. The
        // `product.price.quote`, `product.license.template`,
        // `product.support.route`, and `product.exists` listeners were
        // removed: no emitter for any of them exists anywhere in the
        // tree, so they were dead contracts. The same data is served
        // by the HTTP handlers (`price-quote`, `license-template`,
        // `get_product`, `support-ticket`).
        let state = self.state.clone();
        bus.register(
            "product.entitlement.check",
            "products",
            Arc::new(move |ctx: &mut HookContext| {
                on_product_entitlement_check(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        // Specific single-segment paths are registered before the
        // generic `/{id}` routes so they are matched first.
        cfg.service(
            web::scope("/api/v1/products")
                .route("/taxonomy", web::get().to(list_taxonomy))
                .route("/taxonomy", web::post().to(create_taxonomy_node))
                .route("/taxonomy/tree", web::get().to(taxonomy_tree))
                .route("/taxonomy/{id}", web::get().to(get_taxonomy_node))
                .route("/taxonomy/{id}", web::put().to(update_taxonomy_node))
                .route("/taxonomy/{id}", web::delete().to(delete_taxonomy_node))
                .route("/taxonomy/{id}/move", web::post().to(move_taxonomy_node))
                .route("/taxonomy/{id}/children", web::get().to(list_taxonomy_children))
                .route("/tags", web::get().to(list_tags))
                .route("/tags", web::post().to(create_tag))
                .route("/tags/cloud", web::get().to(tag_cloud))
                .route("/tags/{id}", web::get().to(get_tag))
                .route("/tags/{id}", web::put().to(update_tag))
                .route("/tags/{id}", web::delete().to(delete_tag))
                .route("/tax-rules", web::get().to(list_tax_rules))
                .route("/tax-rules", web::post().to(create_tax_rule))
                .route("/tax-rules/{id}", web::put().to(update_tax_rule))
                .route("/tax-rules/{id}", web::delete().to(delete_tax_rule))
                .route("/entitlement-check", web::post().to(entitlement_check))
                .route("/module-offerings", web::get().to(list_module_offerings))
                .route("/inventory/low-stock", web::get().to(list_low_stock))
                .route("", web::get().to(list_products))
                .route("", web::post().to(create_product))
                .route("/{id}", web::get().to(get_product))
                .route("/{id}", web::put().to(update_product))
                .route("/{id}", web::delete().to(delete_product))
                .route("/{id}/transition", web::post().to(transition_product))
                .route("/{id}/list", web::post().to(list_product))
                .route("/{id}/delist", web::post().to(delist_product))
                .route("/{id}/price-quote", web::post().to(price_quote))
                .route("/{id}/license-template", web::get().to(get_license_template))
                .route("/{id}/subscription-plan", web::get().to(get_subscription_plan))
                .route("/{id}/support-ticket", web::post().to(create_support_ticket))
                .route("/{id}/variants", web::get().to(list_variants))
                .route("/{id}/variants", web::post().to(create_variant))
                .route("/variants/{id}", web::get().to(get_variant))
                .route("/variants/{id}", web::put().to(update_variant))
                .route("/variants/{id}", web::delete().to(delete_variant))
                .route("/{id}/options", web::get().to(list_options))
                .route("/{id}/options", web::post().to(create_option))
                .route("/options/{id}", web::delete().to(delete_option))
                .route("/{id}/bundle", web::get().to(list_bundle_items))
                .route("/{id}/bundle", web::put().to(update_bundle_rule))
                .route("/{id}/bundle/items", web::post().to(add_bundle_item))
                .route("/bundle-items/{id}", web::delete().to(delete_bundle_item))
                .route("/{id}/tiers", web::get().to(list_tiers))
                .route("/{id}/tiers", web::post().to(create_tier))
                .route("/tiers/{id}", web::get().to(get_tier))
                .route("/tiers/{id}", web::put().to(update_tier))
                .route("/tiers/{id}", web::delete().to(delete_tier))
                .route("/{id}/regional-prices", web::get().to(list_regional_prices))
                .route("/{id}/regional-prices", web::post().to(create_regional_price))
                .route("/regional-prices/{id}", web::put().to(update_regional_price))
                .route("/regional-prices/{id}", web::delete().to(delete_regional_price))
                .route("/{id}/usage-prices", web::get().to(list_usage_prices))
                .route("/{id}/usage-prices", web::post().to(create_usage_price))
                .route("/usage-prices/{id}", web::put().to(update_usage_price))
                .route("/usage-prices/{id}", web::delete().to(delete_usage_price))
                .route("/{id}/inventory", web::get().to(list_inventory))
                .route("/{id}/inventory/adjust", web::post().to(adjust_inventory))
                .route("/{id}/files", web::get().to(list_product_files))
                .route("/{id}/files", web::post().to(create_product_file))
                .route("/files/{id}", web::delete().to(delete_product_file))
                .route("/{id}/versions", web::get().to(list_product_versions))
                .route("/{id}/versions", web::post().to(create_product_version))
                .route("/versions/{id}", web::delete().to(delete_product_version))
                .route("/{id}/tags", web::get().to(list_product_tags))
                .route("/{id}/tags", web::post().to(attach_product_tag))
                .route("/{id}/tags/{tag_id}", web::delete().to(detach_product_tag)),
        );
    }

    fn route_info(&self) -> Vec<unified_core::modules::RouteInfo> {
        vec![
            unified_core::modules::RouteInfo { method: "GET", path: "/api/v1/products", owner: "products", description: "List products with pagination and filters.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "POST", path: "/api/v1/products", owner: "products", description: "Create a product (starts as draft).", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "GET", path: "/api/v1/products/{id}", owner: "products", description: "Get one product with tags, counts, and inventory summary.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "PUT", path: "/api/v1/products/{id}", owner: "products", description: "Update a product, including validated lifecycle transitions.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "DELETE", path: "/api/v1/products/{id}", owner: "products", description: "Delete a product with no sub-resources.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "POST", path: "/api/v1/products/{id}/transition", owner: "products", description: "Move a product to another lifecycle status.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "POST", path: "/api/v1/products/{id}/list", owner: "products", description: "List an active product in the store (emits product.listed).", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "POST", path: "/api/v1/products/{id}/delist", owner: "products", description: "Remove a product from the store (emits product.delisted).", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "POST", path: "/api/v1/products/{id}/price-quote", owner: "products", description: "Exact price quote with tax, in integer minor units.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "GET", path: "/api/v1/products/{id}/license-template", owner: "products", description: "License terms declared by a product.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "GET", path: "/api/v1/products/{id}/subscription-plan", owner: "products", description: "Subscription plan of a product: billing period, trial, renewal price, proration policy, grace period, and dunning settings.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "POST", path: "/api/v1/products/{id}/support-ticket", owner: "products", description: "Open a support ticket routed to the product's department.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "POST", path: "/api/v1/products/entitlement-check", owner: "products", description: "Check whether a user holds an active entitlement for a product.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "GET", path: "/api/v1/products/module-offerings", owner: "products", description: "List sellable modules as product offerings (paginated, filter by category).", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "GET", path: "/api/v1/products/tax-rules", owner: "products", description: "List tax rules.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "POST", path: "/api/v1/products/tax-rules", owner: "products", description: "Create a tax rule (rate in basis points).", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "GET", path: "/api/v1/products/inventory/low-stock", owner: "products", description: "List inventory records at or below their low-stock threshold.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "GET", path: "/api/v1/products/taxonomy", owner: "products", description: "List product groups and categories.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "GET", path: "/api/v1/products/taxonomy/tree", owner: "products", description: "Product taxonomy as a tree.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "GET", path: "/api/v1/products/tags", owner: "products", description: "List product tags.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "GET", path: "/api/v1/products/tags/cloud", owner: "products", description: "Tag cloud with product counts.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "POST", path: "/api/v1/products/taxonomy", owner: "products", description: "Create a taxonomy node (group or category).", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "GET", path: "/api/v1/products/taxonomy/{id}", owner: "products", description: "Get one taxonomy node.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "PUT", path: "/api/v1/products/taxonomy/{id}", owner: "products", description: "Update a taxonomy node.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "DELETE", path: "/api/v1/products/taxonomy/{id}", owner: "products", description: "Delete a taxonomy node with no children or products.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "POST", path: "/api/v1/products/taxonomy/{id}/move", owner: "products", description: "Move a taxonomy node under another parent.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "GET", path: "/api/v1/products/taxonomy/{id}/children", owner: "products", description: "List the child nodes of one taxonomy node (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "POST", path: "/api/v1/products/tags", owner: "products", description: "Create a product tag.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "GET", path: "/api/v1/products/tags/{id}", owner: "products", description: "Get one product tag.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "PUT", path: "/api/v1/products/tags/{id}", owner: "products", description: "Update a product tag.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "DELETE", path: "/api/v1/products/tags/{id}", owner: "products", description: "Delete a product tag.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "PUT", path: "/api/v1/products/tax-rules/{id}", owner: "products", description: "Update a tax rule.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "DELETE", path: "/api/v1/products/tax-rules/{id}", owner: "products", description: "Delete a tax rule.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "GET", path: "/api/v1/products/{id}/variants", owner: "products", description: "List the variants of one product (paginated, filter by status).", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "POST", path: "/api/v1/products/{id}/variants", owner: "products", description: "Create a variant for one product.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "GET", path: "/api/v1/products/variants/{id}", owner: "products", description: "Get one product variant.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "PUT", path: "/api/v1/products/variants/{id}", owner: "products", description: "Update a product variant.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "DELETE", path: "/api/v1/products/variants/{id}", owner: "products", description: "Delete a product variant.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "GET", path: "/api/v1/products/{id}/options", owner: "products", description: "List the options of one product (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "POST", path: "/api/v1/products/{id}/options", owner: "products", description: "Create an option for one product.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "DELETE", path: "/api/v1/products/options/{id}", owner: "products", description: "Delete a product option.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "GET", path: "/api/v1/products/{id}/bundle", owner: "products", description: "List the bundle items of one product (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "PUT", path: "/api/v1/products/{id}/bundle", owner: "products", description: "Update the bundle rule of one product.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "POST", path: "/api/v1/products/{id}/bundle/items", owner: "products", description: "Add an item to a product bundle.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "DELETE", path: "/api/v1/products/bundle-items/{id}", owner: "products", description: "Delete a bundle item.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "GET", path: "/api/v1/products/{id}/tiers", owner: "products", description: "List the pricing tiers of one product (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "POST", path: "/api/v1/products/{id}/tiers", owner: "products", description: "Create a pricing tier for one product.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "GET", path: "/api/v1/products/tiers/{id}", owner: "products", description: "Get one pricing tier.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "PUT", path: "/api/v1/products/tiers/{id}", owner: "products", description: "Update a pricing tier.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "DELETE", path: "/api/v1/products/tiers/{id}", owner: "products", description: "Delete a pricing tier.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "GET", path: "/api/v1/products/{id}/regional-prices", owner: "products", description: "List the regional prices of one product (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "POST", path: "/api/v1/products/{id}/regional-prices", owner: "products", description: "Create a regional price for one product.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "PUT", path: "/api/v1/products/regional-prices/{id}", owner: "products", description: "Update a regional price.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "DELETE", path: "/api/v1/products/regional-prices/{id}", owner: "products", description: "Delete a regional price.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "GET", path: "/api/v1/products/{id}/usage-prices", owner: "products", description: "List the usage prices of one product (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "POST", path: "/api/v1/products/{id}/usage-prices", owner: "products", description: "Create a usage price for one product.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "PUT", path: "/api/v1/products/usage-prices/{id}", owner: "products", description: "Update a usage price.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "DELETE", path: "/api/v1/products/usage-prices/{id}", owner: "products", description: "Delete a usage price.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "GET", path: "/api/v1/products/{id}/inventory", owner: "products", description: "List the inventory records of one product (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "POST", path: "/api/v1/products/{id}/inventory/adjust", owner: "products", description: "Adjust the inventory of one product.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "GET", path: "/api/v1/products/{id}/files", owner: "products", description: "List the files of one product (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "POST", path: "/api/v1/products/{id}/files", owner: "products", description: "Attach a file to one product.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "DELETE", path: "/api/v1/products/files/{id}", owner: "products", description: "Delete a product file.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "GET", path: "/api/v1/products/{id}/versions", owner: "products", description: "List the versions of one product (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "POST", path: "/api/v1/products/{id}/versions", owner: "products", description: "Create a version of one product.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "DELETE", path: "/api/v1/products/versions/{id}", owner: "products", description: "Delete a product version.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "GET", path: "/api/v1/products/{id}/tags", owner: "products", description: "List the tags attached to one product (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "POST", path: "/api/v1/products/{id}/tags", owner: "products", description: "Attach a tag to one product.", auth: unified_core::auth::RouteAuth::Authenticated},
            unified_core::modules::RouteInfo { method: "DELETE", path: "/api/v1/products/{id}/tags/{tag_id}", owner: "products", description: "Detach a tag from one product.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ProductsModule::new()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::hooks::HookBus;

    #[test]
    fn register_hooks_answers_only_entitlement_check() {
        // The product.price.quote, product.license.template,
        // product.support.route, and product.exists listeners were
        // removed: no emitter for any of them exists in the tree.
        let bus = HookBus::new();
        let module = ProductsModule::new();
        module.register_hooks(&bus);
        assert!(bus.has("product.entitlement.check"));
        assert!(!bus.has("product.price.quote"));
        assert!(!bus.has("product.license.template"));
        assert!(!bus.has("product.support.route"));
        assert!(!bus.has("product.exists"));
    }
}
