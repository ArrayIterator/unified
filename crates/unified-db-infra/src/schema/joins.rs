// Joinable tables and same-query groups for the unified-db-infra bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); every `joinable!` and
// `allow_tables_to_appear_in_same_query!` declaration from the original
// file lives here, whole, so no join group spans files.

use super::*;
// ---------------------------------------------------------------------------
// Joinable tables in this bounded context.
// ---------------------------------------------------------------------------

diesel::joinable!(provider_servers -> provider_connections (connection_id));
diesel::joinable!(panel_synced_records -> panel_connections (connection_id));
diesel::joinable!(webhook_deliveries -> webhook_endpoints (endpoint_id));
diesel::joinable!(maintenance_history -> maintenance_windows (window_id));
diesel::joinable!(status_daily_uptime -> status_components (component_id));
diesel::joinable!(status_incident_updates -> status_incidents (incident_id));
diesel::joinable!(google_drive_links -> google_accounts (account_id));
diesel::joinable!(route_redirects -> named_routes (route));
diesel::joinable!(department_routing_rules -> crate::schema::departments::departments (department_id));

diesel::joinable!(calculator_reports -> calculator_saved_calculations (calculation_id));

diesel::joinable!(network_tool_uptime_events -> network_tool_uptime_monitors (monitor_id));

diesel::joinable!(vault_documents -> vault_folders (folder_id));
diesel::joinable!(vault_document_versions -> vault_documents (document_id));
diesel::joinable!(vault_access_log -> vault_documents (document_id));
diesel::joinable!(vault_share_links -> vault_documents (document_id));
diesel::joinable!(vault_access_requests -> vault_documents (document_id));

diesel::joinable!(compliance_deletion_items -> compliance_deletion_requests (deletion_request_id));
diesel::joinable!(compliance_retention_reviews -> compliance_retention_policies (policy_id));
diesel::joinable!(compliance_breach_timeline -> compliance_breach_log (breach_id));

diesel::joinable!(gamification_points_ledger -> gamification_rules (rule_id));
diesel::joinable!(gamification_awards -> gamification_badges (badge_id));
diesel::joinable!(gamification_reward_requests -> gamification_rewards (reward_id));

diesel::joinable!(privacy_consent_history -> privacy_consents (consent_id));
diesel::joinable!(privacy_erasure_requests -> privacy_anonymizations (anonymization_id));

diesel::joinable!(web3_watched_addresses -> web3_networks (network_id));
diesel::joinable!(web3_transactions -> web3_networks (network_id));
diesel::joinable!(web3_transactions -> web3_watched_addresses (watched_address_id));
diesel::joinable!(web3_anchors -> web3_networks (network_id));
diesel::joinable!(web3_wallets -> web3_networks (network_id));
diesel::joinable!(web3_balances -> web3_wallets (wallet_id));
diesel::joinable!(web3_balances -> web3_networks (network_id));

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

// Join group 26 (6 tables, e.g. vault_access_log (+5 more)).
diesel::allow_tables_to_appear_in_same_query!(
    vault_access_log,
    vault_access_requests,
    vault_document_versions,
    vault_documents,
    vault_folders,
    vault_share_links,
);

// Join group 27 (6 tables, e.g. web3_anchors (+5 more)).
diesel::allow_tables_to_appear_in_same_query!(
    web3_anchors,
    web3_balances,
    web3_networks,
    web3_transactions,
    web3_wallets,
    web3_watched_addresses,
);

// Join group 53 (2 tables, e.g. calculator_reports).
diesel::allow_tables_to_appear_in_same_query!(calculator_reports, calculator_saved_calculations,);

// Join group 54 (2 tables, e.g. compliance_breach_log).
diesel::allow_tables_to_appear_in_same_query!(compliance_breach_log, compliance_breach_timeline,);

// Join group 55 (2 tables, e.g. compliance_deletion_items).
diesel::allow_tables_to_appear_in_same_query!(
    compliance_deletion_items,
    compliance_deletion_requests,
);

// Join group 56 (2 tables, e.g. compliance_retention_policies).
diesel::allow_tables_to_appear_in_same_query!(
    compliance_retention_policies,
    compliance_retention_reviews,
);

// Join group 57 (2 tables, e.g. department_routing_rules).
diesel::allow_tables_to_appear_in_same_query!(department_routing_rules, crate::schema::departments::departments,);

// Join group 59 (2 tables, e.g. gamification_awards).
diesel::allow_tables_to_appear_in_same_query!(gamification_awards, gamification_badges,);

// Join group 60 (2 tables, e.g. gamification_points_ledger).
diesel::allow_tables_to_appear_in_same_query!(gamification_points_ledger, gamification_rules,);

// Join group 61 (2 tables, e.g. gamification_reward_requests).
diesel::allow_tables_to_appear_in_same_query!(gamification_reward_requests, gamification_rewards,);

// Join group 62 (2 tables, e.g. google_accounts).
diesel::allow_tables_to_appear_in_same_query!(google_accounts, google_drive_links,);

// Join group 63 (2 tables, e.g. maintenance_history).
diesel::allow_tables_to_appear_in_same_query!(maintenance_history, maintenance_windows,);

// Join group 64 (2 tables, e.g. named_routes).
diesel::allow_tables_to_appear_in_same_query!(named_routes, route_redirects,);

// Join group 65 (2 tables, e.g. network_tool_uptime_events).
diesel::allow_tables_to_appear_in_same_query!(
    network_tool_uptime_events,
    network_tool_uptime_monitors,
);

// Join group 66 (2 tables, e.g. panel_connections).
diesel::allow_tables_to_appear_in_same_query!(panel_connections, panel_synced_records,);

// Join group 71 (2 tables, e.g. privacy_anonymizations).
diesel::allow_tables_to_appear_in_same_query!(privacy_anonymizations, privacy_erasure_requests,);

// Join group 72 (2 tables, e.g. privacy_consent_history).
diesel::allow_tables_to_appear_in_same_query!(privacy_consent_history, privacy_consents,);

// Join group 73 (2 tables, e.g. provider_connections).
diesel::allow_tables_to_appear_in_same_query!(provider_connections, provider_servers,);

// Join group 74 (2 tables, e.g. status_components).
diesel::allow_tables_to_appear_in_same_query!(status_components, status_daily_uptime,);

// Join group 75 (2 tables, e.g. status_incident_updates).
diesel::allow_tables_to_appear_in_same_query!(status_incident_updates, status_incidents,);

// Join group 77 (2 tables, e.g. webhook_deliveries).
diesel::allow_tables_to_appear_in_same_query!(webhook_deliveries, webhook_endpoints,);
