// Properties - Block status machine + audit history.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use uuid::Uuid;

use super::*;

// Block status machine + audit history

/// Write one audited history row for a block status change.
pub(crate) fn record_history(
    state: &PropertiesState,
    block: &PropertyBlock,
    from: BlockStatus,
    to: BlockStatus,
    actor_id: &str,
    reason: Option<String>,
    order_id: Option<String>,
) {
    let entry = PropertyBlockHistory {
        id: Uuid::now_v7(),
        block_id: block.id,
        property_id: block.property_id,
        plan_id: block.plan_id,
        from_status: from.as_str().to_string(),
        to_status: to.as_str().to_string(),
        actor_id: actor_id.to_string(),
        reason,
        order_id,
        created_at: Utc::now(),
    };
    state.history.insert(entry.id, entry);
}

/// Move a block to a new status, writing the audit row and
/// emitting the matching event. The caller owns the transition
/// rules; this helper only records and announces the move.
pub(crate) fn move_block_status(
    state: &PropertiesState,
    block: &mut PropertyBlock,
    to: BlockStatus,
    actor_id: &str,
    reason: Option<String>,
    order_id: Option<String>,
    event: &str,
) {
    let from = block.status;
    block.status = to;
    block.updated_at = Utc::now();
    record_history(state, block, from, to, actor_id, reason.clone(), order_id.clone());
    state.emit(
        event,
        json!({
            "property_id": block.property_id.to_string(),
            "plan_id": block.plan_id.to_string(),
            "block_id": block.id.to_string(),
            "from_status": from.as_str(),
            "to_status": to.as_str(),
            "actor_id": actor_id,
            "reason": reason,
            "order_id": order_id,
        }),
    );
}

/// Public view of a property: plans with their blocks, but never
/// the buyer id or order id of a block.
pub(crate) fn public_property_view(state: &PropertiesState, property: &Property) -> Value {
    let plans: Vec<Value> = state
        .plans_of_property(property.id)
        .into_iter()
        .map(|plan| {
            let blocks: Vec<Value> = state
                .blocks_of_plan(plan.id)
                .into_iter()
                .map(|block| public_block_view(&block))
                .collect();
            let mut value = serde_json::to_value(&plan).unwrap_or(Value::Null);
            if let Value::Object(map) = &mut value {
                map.insert("blocks".to_string(), Value::Array(blocks));
            }
            value
        })
        .collect();
    let mut value = serde_json::to_value(property).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert("plans".to_string(), Value::Array(plans));
    }
    value
}

/// Public view of a block: everything a visitor needs to see the
/// plan, without the buyer id or the order id.
pub(crate) fn public_block_view(block: &PropertyBlock) -> Value {
    let mut value = serde_json::to_value(block).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.remove("assigned_customer_id");
        map.remove("order_id");
    }
    value
}

/// Admin view of a plan: the plan plus its blocks (full detail).
pub(crate) fn plan_view(state: &PropertiesState, plan: &PropertyPlan) -> Value {
    let blocks: Vec<PropertyBlock> = state.blocks_of_plan(plan.id);
    let mut value = serde_json::to_value(plan).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert(
            "blocks".to_string(),
            serde_json::to_value(blocks).unwrap_or(Value::Null),
        );
    }
    value
}
