//! manufacturing - validation.

use unified_core::error::{UnifiedError, UnifiedResult};
use dashmap::DashMap;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicU64},
    Arc, RwLock,
};
use super::{constants::*, types::*, inputs::*, helpers::*, store::*, stock::*, module::*, hooks::*, handlers_boms::*, handlers_work_centers::*, handlers_work_orders::*, handlers_runs::*, handlers_scrap::*, handlers_units::*, handlers_settings::*, reports::*};



// ---------------------------------------------------------------------------
// BOM validation and explosion
// ---------------------------------------------------------------------------
/// Validate BOM lines against the stored unit definitions and
/// the caller's own limits. Returns validated lines with the
/// caller's scrap allowance filled from the settings default.
pub(crate) fn validate_lines(
    state: &ManufacturingState,
    inputs: &[BomLineInput],
    settings: &ManufacturingSettings,
) -> UnifiedResult<Vec<BomLine>> {
    if inputs.is_empty() {
        return Err(UnifiedError::bad_request(
            "A BOM must have at least one component line",
        ));
    }
    if inputs.len() > MAX_BOM_LINES {
        return Err(UnifiedError::bad_request(format!(
            "A BOM may have at most {MAX_BOM_LINES} lines"
        )));
    }
    let mut lines = Vec::with_capacity(inputs.len());
    let mut seen: HashSet<String> = HashSet::new();
    for input in inputs {
        let component = require_text(&input.component_product_id, "component_product_id", MAX_CODE_LEN)?;
        if !seen.insert(component.clone()) {
            return Err(UnifiedError::bad_request(format!(
                "Duplicate component in BOM lines: {component}"
            )));
        }
        let quantity = valid_quantity(input.quantity, "quantity")?;
        let unit = require_text(&input.unit, "unit", MAX_UNIT_LEN)?;
        if !settings.units.iter().any(|u| u == &unit) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown unit \"{unit}\"; units are admin-defined (see GET /api/v1/manufacturing/units)"
            )));
        }
        let allowance = match input.scrap_allowance_percent {
            Some(value) => valid_percent(value, "scrap_allowance_percent")?,
            None => settings.default_scrap_allowance_percent,
        };
        let notes = optional_text(&input.notes, "notes", MAX_NOTES_LEN)?;
        lines.push(BomLine {
            component_product_id: component,
            quantity,
            unit,
            scrap_allowance_percent: allowance,
            notes,
        });
    }
    Ok(lines)
}


/// Validate routing steps: work centers must exist, sequences
/// must be unique, durations positive.
pub(crate) fn validate_routing(
    state: &ManufacturingState,
    inputs: &[RoutingStepInput],
) -> UnifiedResult<Vec<RoutingStep>> {
    if inputs.len() > MAX_ROUTING_STEPS {
        return Err(UnifiedError::bad_request(format!(
            "A BOM may have at most {MAX_ROUTING_STEPS} routing steps"
        )));
    }
    let mut steps = Vec::with_capacity(inputs.len());
    let mut seen_seq: HashSet<u32> = HashSet::new();
    for input in inputs {
        if !seen_seq.insert(input.sequence) {
            return Err(UnifiedError::bad_request(format!(
                "Duplicate routing sequence: {}",
                input.sequence
            )));
        }
        let name = require_text(&input.name, "routing step name", MAX_NAME_LEN)?;
        let center_id = require_text(&input.work_center_id, "work_center_id", MAX_CODE_LEN)?;
        load_work_center(state, &center_id)?;
        let duration = valid_quantity(input.duration_minutes, "duration_minutes")?;
        let description = optional_text(&input.description, "description", MAX_NOTES_LEN)?;
        steps.push(RoutingStep {
            sequence: input.sequence,
            name,
            work_center_id: center_id,
            duration_minutes: duration,
            description,
        });
    }
    steps.sort_by_key(|step| step.sequence);
    Ok(steps)
}


pub(crate) fn create_version(
    state: &ManufacturingState,
    bom_id: &str,
    version_number: u32,
    lines: Vec<BomLine>,
    routing: Vec<RoutingStep>,
    actor: &str,
) -> BomVersion {
    let version = BomVersion {
        id: new_id(),
        bom_id: bom_id.to_string(),
        version: version_number,
        lines,
        routing,
        created_by: actor.to_string(),
        created_at: now(),
    };
    state
        .bom_versions
        .insert(version.id.clone(), version.clone());
    state.emit(
        EVENT_BOM_VERSION_CREATED,
        json!({
            "bom_id": bom_id,
            "version_id": version.id,
            "version": version_number,
            "created_by": actor,
        }),
    );
    version
}


/// Explode a BOM version into a multi-level tree for `quantity`
/// units of the finished product. Components that have an active
/// BOM of their own are expanded recursively; a component already
/// on the current path is a cycle and is reported as an error
/// naming the product, and the configurable depth cap stops
/// runaway trees.
pub(crate) fn explode_version(
    state: &ManufacturingState,
    version: &BomVersion,
    quantity: f64,
    settings: &ManufacturingSettings,
) -> UnifiedResult<Vec<ExplosionNode>> {
    let max_depth = settings.max_bom_depth.min(MAX_BOM_DEPTH_HARD_CAP).max(1);
    let mut path: Vec<String> = Vec::new();
    let mut nodes = Vec::new();
    for line in &version.lines {
        nodes.push(explode_line(state, line, quantity, 0, max_depth, &mut path)?);
    }
    Ok(nodes)
}


pub(crate) fn explode_line(
    state: &ManufacturingState,
    line: &BomLine,
    parent_quantity: f64,
    depth: u32,
    max_depth: u32,
    path: &mut Vec<String>,
) -> UnifiedResult<ExplosionNode> {
    if path.contains(&line.component_product_id) {
        let mut cycle = path.clone();
        cycle.push(line.component_product_id.clone());
        return Err(UnifiedError::conflict(format!(
            "BOM cycle detected: {}",
            cycle.join(" -> ")
        )));
    }
    // Scrap allowance: the exploded quantity covers the expected
    // loss on this line, matching the work-order requirements
    // built from the same BOM (build_requirements applies the
    // same factor). The scrap-adjusted quantity is the parent
    // quantity for child lines, so scrap compounds down levels.
    let quantity = parent_quantity * line.quantity * (1.0 + line.scrap_allowance_percent / 100.0);
    let mut node = ExplosionNode {
        product_id: line.component_product_id.clone(),
        quantity,
        unit: line.unit.clone(),
        depth,
        bom_id: None,
        bom_version: None,
        children: Vec::new(),
    };
    if depth < max_depth {
        if let Some(child_bom) = active_bom_for_product(state, &line.component_product_id) {
            if let Some(active_id) = &child_bom.active_version_id {
                if let Ok(child_version) = load_bom_version(state, active_id) {
                    node.bom_id = Some(child_bom.id.clone());
                    node.bom_version = Some(child_version.version);
                    path.push(line.component_product_id.clone());
                    for child_line in &child_version.lines {
                        node.children.push(explode_line(
                            state,
                            child_line,
                            quantity,
                            depth + 1,
                            max_depth,
                            path,
                        )?);
                    }
                    path.pop();
                }
            }
        }
    }
    Ok(node)
}


/// Flatten an explosion tree into total quantities per product
/// (leaf totals, keyed by product id with its unit).
///
/// Only leaf nodes are accumulated: an intermediate node is a
/// sub-assembly produced from its children, not something to
/// purchase, so counting it would double-count its demand once
/// as the sub-assembly and again as the raw components it is
/// made from. This matters when a product is used both as a
/// sub-assembly (with its own BOM) and as a direct component:
/// only its direct leaf occurrences land in the totals, while
/// its sub-assembly occurrences contribute their leaf inputs.
/// The full tree (including intermediate quantities) is still
/// returned by the explode endpoint; these totals are the
/// purchase / raw-material requirements.
pub(crate) fn flatten_explosion(nodes: &[ExplosionNode], totals: &mut HashMap<String, (f64, String)>) {
    for node in nodes {
        if node.children.is_empty() {
            let entry = totals
                .entry(node.product_id.clone())
                .or_insert((0.0, node.unit.clone()));
            entry.0 += node.quantity;
        }
        flatten_explosion(&node.children, totals);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_state() -> ManufacturingState {
        ManufacturingState {
            boms: Arc::new(DashMap::new()),
            bom_versions: Arc::new(DashMap::new()),
            work_centers: Arc::new(DashMap::new()),
            work_orders: Arc::new(DashMap::new()),
            production_runs: Arc::new(DashMap::new()),
            reservations: Arc::new(DashMap::new()),
            scrap_reasons: Arc::new(DashMap::new()),
            units: Arc::new(DashMap::new()),
            stock_notices: Arc::new(DashMap::new()),
            settings: Arc::new(RwLock::new(ManufacturingSettings::default())),
            order_seq: Arc::new(AtomicU64::new(0)),
            hooks: Arc::new(RwLock::new(None)),
        }
    }

    fn line(component: &str, quantity: f64, unit: &str) -> BomLineInput {
        BomLineInput {
            component_product_id: component.to_string(),
            quantity,
            unit: unit.to_string(),
            scrap_allowance_percent: None,
            notes: None,
        }
    }

    #[test]
    fn validate_lines_accepts_valid_lines_and_fills_default_scrap() {
        let state = test_state();
        let settings = ManufacturingSettings::default();
        let lines = validate_lines(&state, &[line("prod-1", 2.0, "piece")], &settings)
            .expect("valid line");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].component_product_id, "prod-1");
        assert_eq!(
            lines[0].scrap_allowance_percent,
            settings.default_scrap_allowance_percent
        );
    }

    #[test]
    fn validate_lines_rejects_empty_duplicate_and_unknown_unit() {
        let state = test_state();
        let settings = ManufacturingSettings::default();
        assert!(validate_lines(&state, &[], &settings).is_err());
        assert!(validate_lines(
            &state,
            &[line("prod-1", 1.0, "piece"), line("prod-1", 1.0, "piece")],
            &settings
        )
        .is_err());
        assert!(
            validate_lines(&state, &[line("prod-1", 1.0, "parsec")], &settings).is_err()
        );
    }

    #[test]
    fn validate_lines_rejects_non_positive_quantity() {
        let state = test_state();
        let settings = ManufacturingSettings::default();
        assert!(validate_lines(&state, &[line("prod-1", 0.0, "kg")], &settings).is_err());
        assert!(validate_lines(&state, &[line("prod-1", -2.0, "kg")], &settings).is_err());
    }

    fn work_center(id: &str) -> WorkCenter {
        WorkCenter {
            id: id.to_string(),
            name: "Cutting".to_string(),
            code: id.to_string(),
            capacity_hours_per_day: 8.0,
            cost_per_hour: None,
            status: "active".to_string(),
            description: None,
            created_at: now(),
            updated_at: now(),
        }
    }

    fn node(product_id: &str, quantity: f64, children: Vec<ExplosionNode>) -> ExplosionNode {
        ExplosionNode {
            product_id: product_id.to_string(),
            quantity,
            unit: "piece".to_string(),
            depth: 0,
            bom_id: None,
            bom_version: None,
            children,
        }
    }

    #[test]
    fn validate_routing_accepts_empty_and_rejects_duplicate_sequence() {
        let state = test_state();
        // The duplicate-sequence check must be exercised, not the
        // missing-work-center check: seed the work center first so
        // both steps load it and only the duplicate sequence fails.
        state
            .work_centers
            .insert("wc-1".to_string(), work_center("wc-1"));
        assert!(validate_routing(&state, &[]).expect("empty routing").is_empty());
        let step = |sequence: u32| RoutingStepInput {
            sequence,
            name: "Cut".to_string(),
            work_center_id: "wc-1".to_string(),
            duration_minutes: 30.0,
            description: None,
        };
        assert!(validate_routing(&state, &[step(1), step(1)]).is_err());
    }

    #[test]
    fn validate_routing_accepts_distinct_sequences_and_rejects_unknown_center() {
        let state = test_state();
        state
            .work_centers
            .insert("wc-1".to_string(), work_center("wc-1"));
        let step = |sequence: u32| RoutingStepInput {
            sequence,
            name: "Cut".to_string(),
            work_center_id: "wc-1".to_string(),
            duration_minutes: 30.0,
            description: None,
        };
        let steps = validate_routing(&state, &[step(2), step(1)])
            .expect("distinct sequences with an existing work center");
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].sequence, 1);
        assert_eq!(steps[1].sequence, 2);
        let unknown = RoutingStepInput {
            sequence: 1,
            name: "Cut".to_string(),
            work_center_id: "wc-missing".to_string(),
            duration_minutes: 30.0,
            description: None,
        };
        assert!(validate_routing(&state, &[unknown]).is_err());
    }

    #[test]
    fn flatten_explosion_counts_only_leaves() {
        // Sub-assembly "sub" (qty 2) is made from "raw" (qty 6);
        // "part" (qty 3) is a direct leaf. The totals must hold
        // only the purchasable leaves, not the sub-assembly.
        let tree = vec![
            node("sub", 2.0, vec![node("raw", 6.0, vec![])]),
            node("part", 3.0, vec![]),
        ];
        let mut totals: HashMap<String, (f64, String)> = HashMap::new();
        flatten_explosion(&tree, &mut totals);
        assert!(!totals.contains_key("sub"));
        assert_eq!(totals.get("raw").map(|(qty, _)| *qty), Some(6.0));
        assert_eq!(totals.get("part").map(|(qty, _)| *qty), Some(3.0));
    }

    #[test]
    fn flatten_explosion_counts_product_used_as_both_subassembly_and_leaf() {
        // "sub" appears as a sub-assembly (qty 2, made from "raw"
        // qty 4) and as a direct leaf component (qty 5). Only the
        // direct leaf occurrence is a purchase requirement; the
        // sub-assembly occurrence contributes its leaf input.
        let tree = vec![
            node("sub", 2.0, vec![node("raw", 4.0, vec![])]),
            node("sub", 5.0, vec![]),
        ];
        let mut totals: HashMap<String, (f64, String)> = HashMap::new();
        flatten_explosion(&tree, &mut totals);
        assert_eq!(totals.get("sub").map(|(qty, _)| *qty), Some(5.0));
        assert_eq!(totals.get("raw").map(|(qty, _)| *qty), Some(4.0));
    }

    #[test]
    fn explode_line_applies_scrap_allowance() {
        let state = test_state();
        let line = BomLine {
            component_product_id: "comp-1".to_string(),
            quantity: 2.0,
            unit: "piece".to_string(),
            scrap_allowance_percent: 10.0,
            notes: None,
        };
        let mut path: Vec<String> = Vec::new();
        let node = explode_line(&state, &line, 10.0, 0, 10, &mut path)
            .expect("explosion without a child BOM");
        // 10 parent units * 2 per unit * 1.10 scrap allowance.
        assert_eq!(node.quantity, 22.0);
        assert!(node.children.is_empty());
    }

    #[test]
    fn explode_line_without_scrap_is_unchanged() {
        let state = test_state();
        let line = BomLine {
            component_product_id: "comp-1".to_string(),
            quantity: 2.0,
            unit: "piece".to_string(),
            scrap_allowance_percent: 0.0,
            notes: None,
        };
        let mut path: Vec<String> = Vec::new();
        let node = explode_line(&state, &line, 10.0, 0, 10, &mut path)
            .expect("explosion without a child BOM");
        assert_eq!(node.quantity, 20.0);
    }

    #[test]
    fn explode_line_compounds_scrap_into_child_lines() {
        // "sub" has its own BOM: 3x "raw" per sub-assembly.
        // Parent demand 10, sub line qty 2 with 10% scrap, raw
        // line qty 3 with no scrap: sub needs 22 units, so raw
        // needs 22 * 3 = 66.
        let state = test_state();
        state.boms.insert(
            "bom-sub".to_string(),
            Bom {
                id: "bom-sub".to_string(),
                product_id: "sub".to_string(),
                name: "Sub".to_string(),
                code: "SUB".to_string(),
                description: None,
                status: "active".to_string(),
                active_version_id: Some("ver-sub".to_string()),
                latest_version: 1,
                created_by: "test".to_string(),
                created_at: now(),
                updated_at: now(),
            },
        );
        state.bom_versions.insert(
            "ver-sub".to_string(),
            BomVersion {
                id: "ver-sub".to_string(),
                bom_id: "bom-sub".to_string(),
                version: 1,
                lines: vec![BomLine {
                    component_product_id: "raw".to_string(),
                    quantity: 3.0,
                    unit: "piece".to_string(),
                    scrap_allowance_percent: 0.0,
                    notes: None,
                }],
                routing: vec![],
                created_by: "test".to_string(),
                created_at: now(),
            },
        );
        let line = BomLine {
            component_product_id: "sub".to_string(),
            quantity: 2.0,
            unit: "piece".to_string(),
            scrap_allowance_percent: 10.0,
            notes: None,
        };
        let mut path: Vec<String> = Vec::new();
        let node = explode_line(&state, &line, 10.0, 0, 10, &mut path)
            .expect("explosion with a child BOM");
        assert_eq!(node.quantity, 22.0);
        assert_eq!(node.children.len(), 1);
        assert_eq!(node.children[0].product_id, "raw");
        assert_eq!(node.children[0].quantity, 66.0);
    }
}
