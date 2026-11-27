//! Hook handlers for scheduled uptime and certificate checks.

use unified_core::hooks::{HookContext};
use unified_core::json;
use serde_json::{json, Value};
use uuid::Uuid;
use super::store::{hook_ctx, runs_store, uptime_monitors_store};
use super::tools_monitor::check_cert_watches_sync;

// Hook handlers.

/// Answer `network_tools.uptime.check_requested` (payload
/// `{"monitor_id", "actor"}`). Hook handlers are synchronous, so this
/// handler only validates the request and records it; the async probe
/// runs through `POST /uptime-monitors/{id}/check`, which the
/// scheduler arm calls. The result carries that pointer, so a caller
/// that only emits the event never mistakes the acknowledgement for a
/// completed probe.
pub(crate) fn on_uptime_check_requested(ctx: &mut HookContext) {
    let monitor_id = ctx
        .data
        .get("monitor_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let actor = ctx
        .data
        .get("actor")
        .and_then(|v| v.as_str())
        .unwrap_or("system")
        .to_string();
    let parsed = Uuid::parse_str(&monitor_id);
    match parsed {
        Ok(id) if uptime_monitors_store().contains_key(&id) => {
            ctx.add_result(json!({
                "monitor_id": id,
                "actor": actor,
                "accepted": true,
                "probe_via": format!("POST /api/v1/network-tools/uptime-monitors/{id}/check"),
                "note": "Hook handlers are synchronous; the async probe runs through the trigger endpoint.",
            }));
        }
        Ok(id) => {
            ctx.add_result(json!({
                "monitor_id": id,
                "accepted": false,
                "error": "Uptime monitor not found",
            }));
        }
        Err(_) => {
            ctx.add_result(json!({
                "accepted": false,
                "error": "monitor_id is missing or is not a UUID",
            }));
        }
    }
}

/// Answer `network_tools.cert.check_requested` by running the
/// synchronous threshold scan over the last known expiry data (no
/// network probe inside a sync handler). Use
/// `POST /api/v1/network-tools/cert-watches/check` for a live probe.
pub(crate) fn on_cert_check_requested(ctx: &mut HookContext) {
    let Some(app_ctx) = hook_ctx() else {
        ctx.add_result(json!({
            "checked": 0,
            "error": "network_tools module is not initialised",
        }));
        return;
    };
    ctx.add_result(check_cert_watches_sync(&app_ctx));
}

/// Answer the sharing `sharing.allow_check` hook for run results:
/// allow only when the scope kind is `network_tools_run`, the run
/// exists, and the requesting actor owns the run. No handler answering
/// means denied (fail closed); this handler never allows
/// unconditionally and never allows a run the actor does not own.
pub(crate) fn on_sharing_allow_check(ctx: &mut HookContext) {
    let kind = ctx.data.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    if kind != "network_tools_run" {
        return;
    }
    let id_text = ctx.data.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let actor = ctx.data.get("actor").and_then(|v| v.as_str()).unwrap_or("");
    let allowed = Uuid::parse_str(id_text)
        .ok()
        .and_then(|id| runs_store().get(&id).map(|entry| entry.value().clone()))
        .map(|run| run.user_id == actor)
        .unwrap_or(false);
    if allowed {
        if let Value::Object(map) = &mut ctx.data {
            map.insert("allowed".to_string(), json!(true));
        }
    }
}
