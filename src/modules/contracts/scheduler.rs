// Contracts - Scheduler listener.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use rand::{distr::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Digest;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// Scheduler listener

/// Handle `contracts.expiry_alert_requested`: send the due alert
/// through `notification.requested`, and mark the contract expired
/// (emitting `contracts.expired`) when its end date passed.
pub(crate) fn on_expiry_alert_requested(state: ContractsState, ctx: &mut HookContext) {
    let contract_id = ctx
        .data
        .get("contract_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let Some(contract_id) = contract_id else {
        ctx.add_result(json!({ "sent": false, "error": "contracts.expiry_alert_requested requires 'contract_id'" }));
        return;
    };
    let days_before = ctx
        .data
        .get("days_before")
        .and_then(|value| value.as_i64())
        .unwrap_or(0);
    let Some(mut contract) = state
        .contracts
        .get(&contract_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({ "sent": false, "contract_id": contract_id.to_string(), "error": "Contract not found" }));
        return;
    };
    if materialise_expiry(&state, &mut contract) {
        state.contracts.insert(contract.id, contract.clone());
        ctx.add_result(json!({ "sent": false, "contract_id": contract_id.to_string(), "expired": true }));
        return;
    }
    if contract.status != ContractStatus::Active {
        ctx.add_result(json!({
            "sent": false,
            "contract_id": contract_id.to_string(),
            "reason": format!("Contract is {}", contract.status.as_str()),
        }));
        return;
    }
    let ends_at = contract.ends_at.map(|value| value.to_rfc3339());
    state.notify(
        "contract.expiring",
        &contract.created_by,
        &format!("Contract expires in {days_before} day(s): {}", contract.title),
        &format!(
            "Contract \"{}\" with {} ends on {}.",
            contract.title,
            contract.counterparty_name,
            ends_at.clone().unwrap_or_default()
        ),
        Some(format!("/api/v1/contracts/{}", contract.id)),
    );
    record_audit(
        &state,
        contract.id,
        None,
        "scheduler",
        "expiry_alert_sent",
        None,
        Some(format!("{days_before} day(s) before end date")),
    );
    ctx.add_result(json!({
        "sent": true,
        "contract_id": contract_id.to_string(),
        "days_before": days_before,
        "ends_at": ends_at,
    }));
}
