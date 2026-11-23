// Joinable tables and same-query groups for the unified-db-core bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); every `joinable!` and
// `allow_tables_to_appear_in_same_query!` declaration from the original
// file lives here, whole, so no join group spans files.

use super::*;
// NOTE: `software`, `tasks`, and `invoices` share their names with their
// schema file-modules (schema/software.rs etc.), so the bare names resolve
// to the file-module, not the table (same collision as the models derives:
// `crate::schema::<file>` is the module, the table lives at
// `crate::schema::<file>::<table>`). Every use below therefore spells out
// the full `crate::schema::<file>::<table>` path. A bare name here fails
// with E0425 ("cannot find type `table` in module `<file>`") and the broken
// allow-group paths also poisoned the coherence check for the other tables
// in the group (594x E0119 `AppearsInFromClause` conflicts).
// ---------------------------------------------------------------------------
// Joinable tables in this bounded context.
// ---------------------------------------------------------------------------

diesel::joinable!(users -> companies (company_id));
diesel::joinable!(user_roles -> users (user_id));
diesel::joinable!(user_roles -> roles (role_id));
diesel::joinable!(company_sites -> companies (company_id));
diesel::joinable!(domains -> companies (company_id));
diesel::joinable!(domains -> company_sites (site_id));
diesel::joinable!(licenses -> users (user_id));
diesel::joinable!(licenses -> companies (company_id));
diesel::joinable!(license_activations -> licenses (license_id));
diesel::joinable!(crate::schema::software::software -> companies (vendor_company_id));
diesel::joinable!(crate::schema::software::software -> software_taxonomy (group_id));
diesel::joinable!(software_versions -> crate::schema::software::software (software_id));
diesel::joinable!(software_releases -> software_versions (version_id));
diesel::joinable!(release_files -> software_releases (release_id));
diesel::joinable!(release_files -> software_versions (version_id));
diesel::joinable!(software_tag_assignments -> crate::schema::software::software (software_id));
diesel::joinable!(software_tag_assignments -> software_tags (tag_id));
diesel::joinable!(store_products -> crate::schema::software::software (software_id));
diesel::joinable!(store_products -> companies (company_id));
diesel::joinable!(store_pricing_tiers -> store_products (product_id));
diesel::joinable!(store_orders -> companies (company_id));
diesel::joinable!(store_order_items -> store_orders (order_id));
diesel::joinable!(store_order_items -> store_products (product_id));
diesel::joinable!(store_order_items -> store_pricing_tiers (tier_id));
diesel::joinable!(store_order_items -> crate::schema::software::software (software_id));
diesel::joinable!(store_order_items -> companies (company_id));
diesel::joinable!(store_entitlements -> licenses (license_id));
diesel::joinable!(store_entitlements -> store_orders (order_id));
diesel::joinable!(store_entitlements -> store_products (product_id));
diesel::joinable!(store_entitlements -> store_pricing_tiers (tier_id));
diesel::joinable!(store_entitlements -> crate::schema::software::software (software_id));
diesel::joinable!(store_entitlements -> software_releases (release_id));
diesel::joinable!(store_downloads -> licenses (license_id));
diesel::joinable!(store_downloads -> store_orders (order_id));
diesel::joinable!(store_downloads -> software_releases (release_id));
diesel::joinable!(store_catalog -> store_products (product_id));
diesel::joinable!(store_catalog -> software_releases (release_id));
diesel::joinable!(store_issued_licenses -> licenses (license_id));
diesel::joinable!(store_issued_licenses -> store_orders (order_id));
diesel::joinable!(store_issued_licenses -> companies (company_id));
diesel::joinable!(vcs_connections -> crate::schema::software::software (software_id));
diesel::joinable!(vcs_version_drafts -> vcs_connections (connection_id));
diesel::joinable!(vcs_version_drafts -> crate::schema::software::software (software_id));
diesel::joinable!(vcs_sync_history -> vcs_connections (connection_id));
diesel::joinable!(api_keys -> users (owner_id));
diesel::joinable!(reseller_links -> users (reseller_id));
diesel::joinable!(view_as_sessions -> users (admin_id));
diesel::joinable!(hosting_accounts -> hosting_plans (plan_id));
diesel::joinable!(hosting_accounts -> companies (company_id));
diesel::joinable!(hosting_order_claims -> hosting_plans (plan_id));
diesel::joinable!(crate::schema::tasks::tasks -> users (assignee_id));
diesel::joinable!(crate::schema::tasks::tasks -> companies (company_id));
diesel::joinable!(task_comments -> crate::schema::tasks::tasks (task_id));
diesel::joinable!(task_comments -> users (author_id));
diesel::joinable!(task_checklist_items -> crate::schema::tasks::tasks (task_id));
diesel::joinable!(crate::schema::invoices::invoices -> companies (company_id));
diesel::joinable!(invoice_items -> crate::schema::invoices::invoices (invoice_id));
diesel::joinable!(invoice_payments -> crate::schema::invoices::invoices (invoice_id));
diesel::joinable!(credit_notes -> crate::schema::invoices::invoices (invoice_id));
diesel::joinable!(mcp_servers -> users (owner_id));
diesel::joinable!(mcp_servers -> companies (company_id));
diesel::joinable!(mcp_servers -> company_sites (site_id));
diesel::joinable!(ai_drafts -> ai_provider_connections (provider_id));
diesel::joinable!(ai_drafts -> ai_usage_records (usage_id));

diesel::joinable!(timetracker_timers -> timetracker_entries (entry_id));
diesel::joinable!(timetracker_teams -> companies (company_id));
diesel::joinable!(timetracker_staff_settings -> companies (company_id));
diesel::joinable!(timetracker_staff_invites -> companies (company_id));
diesel::joinable!(timetracker_buyer_settings -> companies (company_id));
diesel::joinable!(timetracker_team_members -> timetracker_teams (team_id));
diesel::joinable!(timetracker_staff_invites -> timetracker_teams (team_id));

diesel::joinable!(ai_company_policies -> companies (company_id));
diesel::joinable!(ai_user_policies -> users (user_id));
diesel::joinable!(ai_user_policies -> companies (company_id));
diesel::joinable!(ai_personal_keys -> users (user_id));
diesel::joinable!(ai_user_mcp_servers -> users (user_id));
diesel::joinable!(ai_usage_records -> users (user_id));
diesel::joinable!(ai_usage_records -> companies (company_id));
diesel::joinable!(license_products -> users (owner_id));
diesel::joinable!(license_releases -> license_products (product_id));
diesel::joinable!(license_events -> licenses (license_id));
diesel::joinable!(license_offline_tokens -> licenses (license_id));
diesel::joinable!(license_activation_history -> licenses (license_id));

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

// ---------------------------------------------------------------------------
// Tables allowed to appear in the same query, grouped by join graph.
//
// One `allow_tables_to_appear_in_same_query!` invocation generates an
// `AllowTablesToAppearInSameQuery` impl for EVERY ordered pair of its
// tables, so a single list of all 673 tables costs ~275k impls and makes
// rustc run out of memory (observed: SIGKILL at ~5 GiB RSS on a 7.7 GiB
// machine). The groups below are the connected components of the
// `joinable!` declarations in this file: any two tables that can be joined
// share a group, and no pair outside a group is joinable, so splitting
// loses no query. Tables with no `joinable!` edge need no entry at all.
// If a future query needs two tables from different groups, merge the
// groups (the compiler reports the missing pair as E0277).
// ---------------------------------------------------------------------------

// Join group (restored, 57 tables, e.g. ai_company_policies (+56 more)).
diesel::allow_tables_to_appear_in_same_query!(
    ai_company_policies,
    ai_drafts,
    ai_personal_keys,
    ai_provider_connections,
    ai_usage_records,
    ai_user_mcp_servers,
    ai_user_policies,
    api_keys,
    companies,
    company_sites,
    credit_notes,
    domains,
    hosting_accounts,
    hosting_order_claims,
    hosting_plans,
    invoice_items,
    invoice_payments,
    crate::schema::invoices::invoices,
    license_activation_history,
    license_activations,
    license_events,
    license_offline_tokens,
    license_products,
    license_releases,
    licenses,
    mcp_servers,
    release_files,
    reseller_links,
    roles,
    crate::schema::software::software,
    software_releases,
    software_tag_assignments,
    software_tags,
    software_taxonomy,
    software_versions,
    store_catalog,
    store_downloads,
    store_entitlements,
    store_issued_licenses,
    store_order_items,
    store_orders,
    store_pricing_tiers,
    store_products,
    task_checklist_items,
    task_comments,
    crate::schema::tasks::tasks,
    timetracker_buyer_settings,
    timetracker_staff_invites,
    timetracker_staff_settings,
    timetracker_team_members,
    timetracker_teams,
    user_roles,
    users,
    vcs_connections,
    vcs_sync_history,
    vcs_version_drafts,
    view_as_sessions,
);

// Join group 76 (2 tables, e.g. timetracker_entries).
diesel::allow_tables_to_appear_in_same_query!(timetracker_entries, timetracker_timers,);

// ---------------------------------------------------------------------------
// Tables added by the integration round (migrations 000032-000107,
// plus `reference_id_settings` and `tenant_destroy_requests`,
// which have no migration yet).
// ---------------------------------------------------------------------------
