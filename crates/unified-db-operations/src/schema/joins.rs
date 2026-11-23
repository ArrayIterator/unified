// Joinable tables and same-query groups for the unified-db-operations bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); every `joinable!` and
// `allow_tables_to_appear_in_same_query!` declaration from the original
// file lives here, whole, so no join group spans files.

use super::*;
// ---------------------------------------------------------------------------
// Joinable tables in this bounded context.
// ---------------------------------------------------------------------------

diesel::joinable!(cluster_nodes -> clusters (cluster_id));
diesel::joinable!(cluster_sync_plans -> clusters (cluster_id));
diesel::joinable!(cluster_sync_plans -> cluster_nodes (target_node_id));
diesel::joinable!(cluster_sync_logs -> cluster_sync_plans (plan_id));
diesel::joinable!(cluster_sync_logs -> clusters (cluster_id));
diesel::joinable!(cluster_sync_logs -> cluster_nodes (node_id));
diesel::joinable!(cluster_health_checks -> cluster_nodes (node_id));
diesel::joinable!(cluster_health_checks -> clusters (cluster_id));
diesel::joinable!(backup_runs -> backup_jobs (job_id));
diesel::joinable!(pool_usage_samples -> capacity_pools (pool_id));
diesel::joinable!(allocation_plans -> capacity_pools (pool_id));
diesel::joinable!(lb_backends -> load_balancers (balancer_id));
diesel::joinable!(lb_backend_stats -> lb_backends (backend_id));

diesel::joinable!(risk_mitigation_plans -> risks (risk_id));
diesel::joinable!(risk_mitigation_tasks -> risk_mitigation_plans (plan_id));
diesel::joinable!(risk_mitigation_tasks -> risks (risk_id));
diesel::joinable!(risk_checklist_template_items -> risk_checklist_templates (template_id));
diesel::joinable!(risk_checklist_instances -> risk_checklist_templates (template_id));
diesel::joinable!(risk_checklist_instances -> risks (risk_id));
diesel::joinable!(risk_checklist_instance_items -> risk_checklist_instances (instance_id));
diesel::joinable!(risk_alerts -> risks (risk_id));

diesel::joinable!(property_plans -> properties (property_id));
diesel::joinable!(property_blocks -> property_plans (plan_id));
diesel::joinable!(property_blocks -> properties (property_id));
diesel::joinable!(property_block_history -> property_blocks (block_id));
diesel::joinable!(property_block_history -> properties (property_id));
diesel::joinable!(property_block_history -> property_plans (plan_id));

diesel::joinable!(logistics_shipments -> logistics_drivers (driver_id));
diesel::joinable!(logistics_shipments -> logistics_vehicles (vehicle_id));
diesel::joinable!(logistics_shipments -> logistics_warehouses (warehouse_id));
diesel::joinable!(logistics_shipment_events -> logistics_shipments (shipment_id));
diesel::joinable!(logistics_stock_movements -> logistics_warehouses (warehouse_id));
diesel::joinable!(logistics_stock_movements -> logistics_shipments (shipment_id));
diesel::joinable!(logistics_routes -> logistics_drivers (driver_id));
diesel::joinable!(logistics_routes -> logistics_vehicles (vehicle_id));
diesel::joinable!(logistics_route_stops -> logistics_routes (route_id));
diesel::joinable!(logistics_route_stops -> logistics_shipments (shipment_id));
diesel::joinable!(logistics_proofs -> logistics_shipments (shipment_id));
diesel::joinable!(logistics_exceptions -> logistics_shipments (shipment_id));

diesel::joinable!(asset_assignments -> assets (asset_id));
diesel::joinable!(asset_stock -> assets (asset_id));
diesel::joinable!(asset_stock_adjustments -> asset_stock (stock_id));
diesel::joinable!(asset_stock_adjustments -> assets (asset_id));
diesel::joinable!(asset_stock_take_lines -> asset_stock_takes (stock_take_id));
diesel::joinable!(asset_stock_take_lines -> assets (asset_id));
diesel::joinable!(asset_stock_take_lines -> asset_stock (stock_id));

diesel::joinable!(incident_timeline -> incidents (incident_id));
diesel::joinable!(on_call_schedules -> on_call_rotations (rotation_id));
diesel::joinable!(incident_escalations -> incidents (incident_id));
diesel::joinable!(incident_escalations -> escalation_policies (policy_id));
diesel::joinable!(page_attempts -> incidents (incident_id));
diesel::joinable!(incident_postmortems -> incidents (incident_id));
diesel::joinable!(postmortem_action_items -> incident_postmortems (postmortem_id));
diesel::joinable!(incidents -> escalation_policies (escalation_policy_id));
diesel::joinable!(incidents -> on_call_rotations (rotation_id));

diesel::joinable!(cluster_sync_conflicts -> cluster_sync_logs (log_id));
diesel::joinable!(cluster_sync_conflicts -> cluster_sync_plans (plan_id));
diesel::joinable!(cluster_sync_conflicts -> clusters (cluster_id));
diesel::joinable!(cluster_sync_conflicts -> cluster_nodes (node_id));
diesel::joinable!(cluster_sync_retries -> cluster_sync_plans (plan_id));
diesel::joinable!(cluster_sync_retries -> clusters (cluster_id));
diesel::joinable!(cluster_sync_retries -> cluster_nodes (node_id));

diesel::joinable!(mfg_bom_versions -> mfg_boms (bom_id));
diesel::joinable!(mfg_bom_version_lines -> mfg_bom_versions (bom_version_id));
diesel::joinable!(mfg_bom_routing_steps -> mfg_bom_versions (bom_version_id));
diesel::joinable!(mfg_bom_routing_steps -> mfg_work_centers (work_center_id));
diesel::joinable!(mfg_work_orders -> mfg_boms (bom_id));
diesel::joinable!(mfg_work_orders -> mfg_bom_versions (bom_version_id));
diesel::joinable!(mfg_work_order_requirements -> mfg_work_orders (work_order_id));
diesel::joinable!(mfg_production_runs -> mfg_work_orders (work_order_id));
diesel::joinable!(mfg_production_runs -> mfg_scrap_reasons (scrap_reason_id));
diesel::joinable!(mfg_production_runs -> mfg_work_centers (work_center_id));
diesel::joinable!(mfg_reservations -> mfg_work_orders (work_order_id));
diesel::joinable!(mfg_stock_notices -> mfg_work_orders (work_order_id));
diesel::joinable!(mfg_stock_notices -> mfg_reservations (reservation_id));

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

// Join group 7 (11 tables, e.g. mfg_bom_routing_steps (+10 more)).
diesel::allow_tables_to_appear_in_same_query!(
    mfg_bom_routing_steps,
    mfg_bom_version_lines,
    mfg_bom_versions,
    mfg_boms,
    mfg_production_runs,
    mfg_reservations,
    mfg_scrap_reasons,
    mfg_stock_notices,
    mfg_work_centers,
    mfg_work_order_requirements,
    mfg_work_orders,
);

// Join group 9 (10 tables, e.g. logistics_drivers (+9 more)).
diesel::allow_tables_to_appear_in_same_query!(
    logistics_drivers,
    logistics_exceptions,
    logistics_proofs,
    logistics_route_stops,
    logistics_routes,
    logistics_shipment_events,
    logistics_shipments,
    logistics_stock_movements,
    logistics_vehicles,
    logistics_warehouses,
);

// Join group 12 (9 tables, e.g. escalation_policies (+8 more)).
diesel::allow_tables_to_appear_in_same_query!(
    escalation_policies,
    incident_escalations,
    incident_postmortems,
    incident_timeline,
    incidents,
    on_call_rotations,
    on_call_schedules,
    page_attempts,
    postmortem_action_items,
);

// Join group 13 (8 tables, e.g. risk_alerts (+7 more)).
diesel::allow_tables_to_appear_in_same_query!(
    risk_alerts,
    risk_checklist_instance_items,
    risk_checklist_instances,
    risk_checklist_template_items,
    risk_checklist_templates,
    risk_mitigation_plans,
    risk_mitigation_tasks,
    risks,
);

// Join group 15 (7 tables, e.g. cluster_health_checks (+6 more)).
diesel::allow_tables_to_appear_in_same_query!(
    cluster_health_checks,
    cluster_nodes,
    cluster_sync_conflicts,
    cluster_sync_logs,
    cluster_sync_plans,
    cluster_sync_retries,
    clusters,
);

// Join group 22 (6 tables, e.g. asset_assignments (+5 more)).
diesel::allow_tables_to_appear_in_same_query!(
    asset_assignments,
    asset_stock,
    asset_stock_adjustments,
    asset_stock_take_lines,
    asset_stock_takes,
    assets,
);

// Join group 41 (4 tables, e.g. properties (+3 more)).
diesel::allow_tables_to_appear_in_same_query!(
    properties,
    property_block_history,
    property_blocks,
    property_plans,
);

// Join group 43 (3 tables, e.g. allocation_plans).
diesel::allow_tables_to_appear_in_same_query!(allocation_plans, capacity_pools, pool_usage_samples,);

// Join group 46 (3 tables, e.g. lb_backend_stats).
diesel::allow_tables_to_appear_in_same_query!(lb_backend_stats, lb_backends, load_balancers,);

// Join group 51 (2 tables, e.g. backup_jobs).
diesel::allow_tables_to_appear_in_same_query!(backup_jobs, backup_runs,);
