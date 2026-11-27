//! quality module - Hook handlers answering other modules.

use unified_core::hooks::HookContext;
use serde_json::{Value, json};
use uuid::Uuid;

use super::constants::ALERT_RESOLVED;
use super::handlers_inspections::request_inspection;
use super::helpers::opt_string;
use super::helpers::tenant_matches;
use super::state::QualityState;
use super::types::QualityPoint;

// ERP event listeners (events only - no direct calls into other
// modules, and no other module is called from here)

/// Points that match an incoming ERP event: active points whose
/// scope is the event's scope and whose `target_ref` is either
/// unset (the point applies to every target in the scope) or
/// equal to the event's target. When the event carries a tenant,
/// only that tenant's points match.
pub(crate) fn matching_points(
    state: &QualityState,
    scope: &str,
    target_ref: &str,
    tenant_id: Option<&str>,
) -> Vec<QualityPoint> {
    let mut rows: Vec<QualityPoint> = state
        .points
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|point| {
            point.active
                && point.scope == scope
                && match tenant_id {
                    Some(tenant) => tenant_matches(&point.tenant_id, tenant),
                    None => true,
                }
                && match &point.target_ref {
                    Some(target) => target == target_ref,
                    None => true,
                }
        })
        .collect();
    rows.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    rows
}

/// Answer one ERP inspection request. When the payload names a
/// point (`point_id`), that point is used; otherwise every active
/// point matching the scope and target gets a requested inspection.
/// A missing field, an unknown point, or no matching point is
/// skipped silently - an event must never fail the emitter.
pub(crate) fn answer_inspection_request(state: QualityState, ctx: &mut HookContext, scope: &str) {
    let target_ref = ctx
        .get("target_ref")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| opt_string(&ctx.data, "work_order_ref"))
        .or_else(|| opt_string(&ctx.data, "product_ref"))
        .or_else(|| opt_string(&ctx.data, "supplier_id"))
        .or_else(|| opt_string(&ctx.data, "shipment_ref"))
        .or_else(|| opt_string(&ctx.data, "receipt_ref"));
    let Some(target_ref) = target_ref else {
        return;
    };
    if target_ref.trim().is_empty() {
        return;
    }
    let target_kind = ctx
        .get("target_kind")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| scope.to_string());
    let inspector_id = ctx
        .get("inspector_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let company_id = ctx
        .get("company_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    // The tenant carried by the event, when present: created
    // inspections inherit it, and only that tenant's points match.
    let tenant_id = ctx
        .get("tenant_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let points = match ctx.get("point_id").and_then(|value| value.as_str()) {
        Some(raw) => match Uuid::parse_str(raw) {
            Ok(id) => state
                .points
                .get(&id)
                .map(|entry| entry.value().clone())
                .filter(|point| {
                    point.active
                        && point.scope == scope
                        && match &tenant_id {
                            Some(tenant) => tenant_matches(&point.tenant_id, tenant),
                            None => true,
                        }
                })
                .into_iter()
                .collect::<Vec<QualityPoint>>(),
            Err(_) => return,
        },
        None => matching_points(&state, scope, &target_ref, tenant_id.as_deref()),
    };
    for point in points {
        request_inspection(
            &state,
            &point,
            target_ref.clone(),
            target_kind.clone(),
            inspector_id.clone(),
            company_id.clone(),
            tenant_id.clone(),
        );
    }
}

/// Answer `quality.blocks.query` - the hook form of `GET /blocks`.
/// Given `{"target_ref", "target_kind?"}` it answers with the
/// blocking alerts covering that target, using the same rule as
/// `list_blocks`: an alert blocks while `blocked` is true and its
/// status is not resolved. The answer is set on the `blocks` key
/// and added as a result row (`{"target_ref", "target_kind",
/// "blocks", "blocked"}`), so owning modules can refuse to proceed
/// with a blocked record. A query without a `target_ref` gets no
/// answer - there is nothing to match against.
pub(crate) fn on_blocks_query(state: QualityState, ctx: &mut HookContext) {
    let Some(target_ref) = ctx
        .get("target_ref")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
    else {
        return;
    };
    let target_kind = ctx
        .get("target_kind")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    // A blocks query carrying a tenant only sees that tenant's
    // blocks; without one, the answer is unscoped (legacy callers).
    let tenant_id = ctx
        .get("tenant_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let mut blocks: Vec<Value> = state
        .alerts
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|row| {
            if !row.blocked || row.status == ALERT_RESOLVED {
                return false;
            }
            if let Some(tenant) = &tenant_id {
                if !tenant_matches(&row.tenant_id, tenant) {
                    return false;
                }
            }
            if row.target_ref != target_ref {
                return false;
            }
            if let Some(kind) = &target_kind {
                if &row.target_kind != kind {
                    return false;
                }
            }
            true
        })
        .map(|row| json!(row))
        .collect();
    blocks.sort_by(|a, b| {
        let at = a.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
        let bt = b.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
        bt.cmp(at)
    });
    let blocked = !blocks.is_empty();
    let answer = json!({
        "target_ref": target_ref,
        "target_kind": target_kind,
        "blocks": blocks,
        "blocked": blocked,
    });
    ctx.set("blocks", answer["blocks"].clone());
    ctx.set("blocked", json!(blocked));
    ctx.add_result(answer);
}

pub(crate) fn on_manufacturing_inspection_requested(state: QualityState, ctx: &mut HookContext) {
    answer_inspection_request(state, ctx, "work_order");
}

pub(crate) fn on_inventory_goods_received(state: QualityState, ctx: &mut HookContext) {
    answer_inspection_request(state, ctx, "incoming_goods");
}

pub(crate) fn on_procurement_quality_check_requested(state: QualityState, ctx: &mut HookContext) {
    answer_inspection_request(state, ctx, "supplier");
}

pub(crate) fn on_logistics_shipment_inspection_requested(
    state: QualityState,
    ctx: &mut HookContext,
) {
    answer_inspection_request(state, ctx, "shipment");
}
