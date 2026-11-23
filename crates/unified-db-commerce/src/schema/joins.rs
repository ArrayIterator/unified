// Joinable tables and same-query groups for the unified-db-commerce bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); every `joinable!` and
// `allow_tables_to_appear_in_same_query!` declaration from the original
// file lives here, whole, so no join group spans files.

use super::*;
// ---------------------------------------------------------------------------
// Joinable tables in this bounded context.
// ---------------------------------------------------------------------------

diesel::joinable!(payment_intents -> payment_connections (connection_id));
diesel::joinable!(payment_transactions -> payment_intents (intent_id));
diesel::joinable!(payment_transactions -> payment_connections (connection_id));
diesel::joinable!(payment_methods -> payment_connections (connection_id));
diesel::joinable!(gateway_subscriptions -> payment_connections (connection_id));
diesel::joinable!(gateway_subscriptions -> payment_methods (payment_method_id));
diesel::joinable!(split_parts -> split_bills (split_id));
diesel::joinable!(split_parts -> payment_connections (connection_id));
diesel::joinable!(split_parts -> payment_transactions (paid_transaction_id));
diesel::joinable!(bank_transfer_verifications -> split_bills (split_id));
diesel::joinable!(bank_transfer_verifications -> split_parts (part_id));
diesel::joinable!(bank_transfer_verifications -> bank_accounts (bank_account_id));
diesel::joinable!(bank_transfer_verification_audit -> bank_transfer_verifications (verification_id));
diesel::joinable!(products -> product_taxonomy (group_id));
diesel::joinable!(product_options -> products (product_id));
diesel::joinable!(product_variants -> products (product_id));
diesel::joinable!(product_bundle_items -> products (bundle_product_id));
diesel::joinable!(product_price_tiers -> products (product_id));
diesel::joinable!(product_regional_prices -> products (product_id));
diesel::joinable!(product_usage_prices -> products (product_id));
diesel::joinable!(product_inventory -> products (product_id));
diesel::joinable!(product_inventory -> product_variants (variant_id));
diesel::joinable!(product_files -> products (product_id));
diesel::joinable!(product_versions -> products (product_id));
diesel::joinable!(product_tag_assignments -> products (product_id));
diesel::joinable!(product_tag_assignments -> product_tags (tag_id));

diesel::joinable!(dunning_attempts -> subscriptions (subscription_id));

diesel::joinable!(pricing_tiers -> pricing_tables (table_id));
diesel::joinable!(pricing_tier_features -> pricing_tiers (tier_id));
diesel::joinable!(marketplace_product_approvals -> marketplace_vendors (vendor_id));
diesel::joinable!(marketplace_commissions -> marketplace_vendors (vendor_id));
diesel::joinable!(marketplace_payouts -> marketplace_vendors (vendor_id));

diesel::joinable!(pricing_coupon_redemptions -> pricing_coupons (coupon_id));
diesel::joinable!(pricing_cost_lines -> pricing_recommendations (recommendation_id));
diesel::joinable!(pricing_bundle_components -> pricing_bundles (bundle_id));
diesel::joinable!(pricing_price_approvals -> pricing_price_versions (price_version_id));

diesel::joinable!(affiliate_commission_rules -> affiliates (affiliate_id));
diesel::joinable!(affiliate_links -> affiliates (affiliate_id));
diesel::joinable!(affiliate_clicks -> affiliate_links (link_id));
diesel::joinable!(affiliate_clicks -> affiliates (affiliate_id));
diesel::joinable!(affiliate_conversions -> affiliates (affiliate_id));
diesel::joinable!(affiliate_conversions -> affiliate_links (link_id));
diesel::joinable!(affiliate_conversions -> affiliate_clicks (click_id));
diesel::joinable!(affiliate_commissions -> affiliates (affiliate_id));
diesel::joinable!(affiliate_commissions -> affiliate_conversions (conversion_id));
diesel::joinable!(affiliate_payouts -> affiliates (affiliate_id));

diesel::joinable!(loyalty_earn_rules -> loyalty_programs (program_id));
diesel::joinable!(loyalty_tiers -> loyalty_programs (program_id));
diesel::joinable!(loyalty_ledger -> loyalty_programs (program_id));
diesel::joinable!(loyalty_redemptions -> loyalty_programs (program_id));

diesel::joinable!(purchase_orders -> vendors (vendor_id));
diesel::joinable!(purchase_order_lines -> purchase_orders (purchase_order_id));
diesel::joinable!(purchase_order_approvals -> purchase_orders (purchase_order_id));
diesel::joinable!(goods_receipts -> purchase_orders (purchase_order_id));
diesel::joinable!(goods_receipts -> purchase_order_lines (line_id));
diesel::joinable!(procurement_budgets -> vendors (vendor_id));

diesel::joinable!(pos_shifts -> pos_registers (register_id));
diesel::joinable!(pos_sales -> pos_registers (register_id));
diesel::joinable!(pos_sales -> pos_shifts (shift_id));
diesel::joinable!(pos_sale_items -> pos_sales (sale_id));
diesel::joinable!(pos_payments -> pos_sales (sale_id));
diesel::joinable!(pos_refunds -> pos_sales (sale_id));
diesel::joinable!(pos_refunds -> pos_shifts (shift_id));
diesel::joinable!(pos_receipts -> pos_sales (sale_id));
diesel::joinable!(pos_receipts -> pos_registers (register_id));

diesel::joinable!(digital_products -> digital_product_types (type_id));
diesel::joinable!(digital_product_files -> digital_products (digital_product_id));
diesel::joinable!(digital_product_previews -> digital_products (digital_product_id));
diesel::joinable!(digital_royalties -> digital_products (digital_product_id));
diesel::joinable!(digital_royalties -> digital_payout_requests (payout_id));
diesel::joinable!(digital_downloads -> digital_products (digital_product_id));
diesel::joinable!(digital_downloads -> digital_product_files (file_id));
diesel::joinable!(digital_downloads -> digital_product_previews (preview_id));

diesel::joinable!(expense_reports -> expense_policies (policy_id));
diesel::joinable!(expenses -> expense_reports (report_id));
diesel::joinable!(expenses -> expense_categories (category_id));
diesel::joinable!(expense_receipts -> expenses (expense_id));
diesel::joinable!(expense_reimbursements -> expense_reports (report_id));
diesel::joinable!(expense_adjustments -> expense_reports (report_id));

diesel::joinable!(invoice_templates -> paper_sizes (paper_size_code));
diesel::joinable!(invoice_template_versions -> invoice_templates (template_id));
diesel::joinable!(printer_profiles -> paper_sizes (paper_size_code));
diesel::joinable!(print_jobs -> invoice_templates (template_id));
diesel::joinable!(print_jobs -> paper_sizes (paper_size_code));
diesel::joinable!(print_jobs -> printer_profiles (printer_profile_id));

// ---------------------------------------------------------------------------
// Tables allowed to appear in the same query, grouped by join graph.
//
// One `allow_tables_to_appear_in_same_query!` invocation generates an
// `AllowTablesToAppearInSameQuery` impl for EVERY ordered pair of its
// tables, so a single list of all tables costs ~275k impls and makes
// rustc run out of memory. The groups below are the connected
// components of the `joinable!` declarations: any two tables that can
// be joined share a group, and no pair outside a group is joinable,
// so splitting loses no query. Tables with no `joinable!` edge need
// no entry at all. If a future query needs two tables from different
// groups, merge the groups (the compiler reports the missing pair
// as E0277).
// ---------------------------------------------------------------------------

// Join group 4 (13 tables, e.g. product_bundle_items (+12 more)).
diesel::allow_tables_to_appear_in_same_query!(
    product_bundle_items,
    product_files,
    product_inventory,
    product_options,
    product_price_tiers,
    product_regional_prices,
    product_tag_assignments,
    product_tags,
    product_taxonomy,
    product_usage_prices,
    product_variants,
    product_versions,
    products,
);

// Join group 8 (10 tables, e.g. bank_accounts (+9 more)).
diesel::allow_tables_to_appear_in_same_query!(
    bank_accounts,
    bank_transfer_verification_audit,
    bank_transfer_verifications,
    gateway_subscriptions,
    payment_connections,
    payment_intents,
    payment_methods,
    payment_transactions,
    split_bills,
    split_parts,
);

// Join group 14 (7 tables, e.g. affiliate_clicks (+6 more)).
diesel::allow_tables_to_appear_in_same_query!(
    affiliate_clicks,
    affiliate_commission_rules,
    affiliate_commissions,
    affiliate_conversions,
    affiliate_links,
    affiliate_payouts,
    affiliates,
);

// Join group 16 (7 tables, e.g. digital_downloads (+6 more)).
diesel::allow_tables_to_appear_in_same_query!(
    digital_downloads,
    digital_payout_requests,
    digital_product_files,
    digital_product_previews,
    digital_product_types,
    digital_products,
    digital_royalties,
);

// Join group 18 (7 tables, e.g. expense_adjustments (+6 more)).
diesel::allow_tables_to_appear_in_same_query!(
    expense_adjustments,
    expense_categories,
    expense_policies,
    expense_receipts,
    expense_reimbursements,
    expense_reports,
    expenses,
);

// Join group 19 (7 tables, e.g. pos_payments (+6 more)).
diesel::allow_tables_to_appear_in_same_query!(
    pos_payments,
    pos_receipts,
    pos_refunds,
    pos_registers,
    pos_sale_items,
    pos_sales,
    pos_shifts,
);

// Join group 24 (6 tables, e.g. goods_receipts (+5 more)).
diesel::allow_tables_to_appear_in_same_query!(
    goods_receipts,
    procurement_budgets,
    purchase_order_approvals,
    purchase_order_lines,
    purchase_orders,
    vendors,
);

// Join group 30 (5 tables, e.g. invoice_template_versions (+4 more)).
diesel::allow_tables_to_appear_in_same_query!(
    invoice_template_versions,
    invoice_templates,
    paper_sizes,
    print_jobs,
    printer_profiles,
);

// Join group 31 (5 tables, e.g. loyalty_earn_rules (+4 more)).
diesel::allow_tables_to_appear_in_same_query!(
    loyalty_earn_rules,
    loyalty_ledger,
    loyalty_programs,
    loyalty_redemptions,
    loyalty_tiers,
);

// Join group 39 (4 tables, e.g. marketplace_commissions (+3 more)).
diesel::allow_tables_to_appear_in_same_query!(
    marketplace_commissions,
    marketplace_payouts,
    marketplace_product_approvals,
    marketplace_vendors,
);

// Join group 48 (3 tables, e.g. pricing_tables).
diesel::allow_tables_to_appear_in_same_query!(pricing_tables, pricing_tier_features, pricing_tiers,);

// Join group 58 (2 tables, e.g. dunning_attempts).
diesel::allow_tables_to_appear_in_same_query!(dunning_attempts, subscriptions,);

// Join group 67 (2 tables, e.g. pricing_bundle_components).
diesel::allow_tables_to_appear_in_same_query!(pricing_bundle_components, pricing_bundles,);

// Join group 68 (2 tables, e.g. pricing_cost_lines).
diesel::allow_tables_to_appear_in_same_query!(pricing_cost_lines, pricing_recommendations,);

// Join group 69 (2 tables, e.g. pricing_coupon_redemptions).
diesel::allow_tables_to_appear_in_same_query!(pricing_coupon_redemptions, pricing_coupons,);

// Join group 70 (2 tables, e.g. pricing_price_approvals).
diesel::allow_tables_to_appear_in_same_query!(pricing_price_approvals, pricing_price_versions,);
