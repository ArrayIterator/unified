//! calendar module - Hook handlers for feed pruning and AI grounding.

use unified_core::hooks::HookContext;
use chrono::{NaiveDate, TimeZone, Utc};
use serde_json::{Value, json};

use super::constants::EVENT_ITEMS_REQUESTED;
use super::constants::MAX_AI_ITEMS;
use super::helpers::ok;
use super::state::CalendarState;

// Hook listeners.

/// `calendar.feeds.prune_requested` - expire/revoke-stale-feed
/// pruning: delete expired tokens and revoked tokens older than
/// the retention window. The scheduler kind `calendar.feeds.prune`
/// is a known kind (listed in the scheduler's `KNOWN_KINDS`) and
/// its dispatcher emits this event, so this listener is live -
/// the scheduler runs it, it is not waiting on registration.
pub(crate) fn on_feeds_prune_requested(state: CalendarState, ctx: &mut HookContext) {
    let now = Utc::now();
    let retention = now - chrono::Duration::days(30);
    let mut pruned = 0usize;
    state.feeds.retain(|_, feed| {
        let stale = feed.expires_at <= now || (feed.revoked && feed.created_at <= retention);
        if stale {
            pruned += 1;
            false
        } else {
            true
        }
    });
    ctx.add_result(json!({ "pruned": pruned }));
    state.emit(
        "calendar.feed.pruned",
        json!({ "pruned": pruned, "at": now.to_rfc3339() }),
    );
}

/// Grounding for the AI capability runner: answer
/// `calendar.ai.<capability>_requested` with the caller's items
/// for the requested date. The runner's payload carries the
/// tenant id (`tenant_id`) and the caller (`caller` as a user id,
/// or `user_id`); when either is missing the listener answers
/// with an empty grounding instead of guessing a tenant - fail
/// closed, never aggregate across tenants.
pub(crate) fn on_ai_grounding_requested(
    state: CalendarState,
    ctx: &mut HookContext,
    capability: &str,
) {
    let data = ctx.data.clone();
    let tenant_id = data
        .get("tenant_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let user_id = data
        .get("caller")
        .and_then(Value::as_str)
        .or_else(|| data.get("user_id").and_then(Value::as_str))
        .map(str::to_string);
    let date = data
        .get("input")
        .and_then(|input| input.get("date"))
        .and_then(Value::as_str)
        .and_then(|raw| NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok())
        .unwrap_or_else(|| Utc::now().date_naive());
    let (tenant_id, user_id) = match (tenant_id, user_id) {
        (Some(tenant_id), Some(user_id)) => (tenant_id, user_id),
        _ => {
            ctx.add_result(json!({
                "capability": capability,
                "date": date.to_string(),
                "items": [],
                "context_text": "",
                "grounding_unavailable": "tenant context or caller missing",
            }));
            return;
        }
    };
    let bus = match state.bus() {
        Some(bus) => bus,
        None => {
            ctx.add_result(json!({
                "capability": capability,
                "date": date.to_string(),
                "items": [],
                "context_text": "",
                "grounding_unavailable": "hook bus not attached",
            }));
            return;
        }
    };
    let day_start = Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).expect("midnight is valid"));
    let day_end = day_start + chrono::Duration::days(1);
    let answer = bus.emit(
        EVENT_ITEMS_REQUESTED,
        json!({
            "tenant_id": tenant_id,
            "company_id": null,
            "user_id": user_id,
            "start": day_start.to_rfc3339(),
            "end": day_end.to_rfc3339(),
        }),
    );
    let mut items: Vec<Value> = Vec::new();
    for result in &answer.results {
        match result {
            Value::Array(values) => items.extend(values.clone()),
            Value::Object(map) => match map.get("items") {
                Some(Value::Array(values)) => items.extend(values.clone()),
                _ => items.push(result.clone()),
            },
            _ => {}
        }
    }
    items.retain(|item| {
        // Keep only items the subject may see: private items only
        // for their owner, and only items of this tenant. A
        // tenant-less item passes only when it is explicitly
        // marked system/global - the same rule `aggregate_items`
        // applies, so an unmarked unscoped item never leaks in
        // through grounding either.
        let visibility = item
            .get("visibility")
            .and_then(Value::as_str)
            .unwrap_or("shared");
        if visibility == "private"
            && item.get("owner_user_id").and_then(Value::as_str) != Some(user_id.as_str())
        {
            return false;
        }
        match item.get("tenant_id").and_then(Value::as_str) {
            Some(item_tenant) => item_tenant == tenant_id,
            None => item
                .get("is_system")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }
    });
    items.truncate(MAX_AI_ITEMS);
    let lines: Vec<String> = items
        .iter()
        .filter_map(|item| {
            let title = item.get("title")?.as_str()?;
            let module = item.get("module")?.as_str().unwrap_or("unknown");
            let start = item.get("start")?.as_str().unwrap_or("");
            Some(format!("- [{module}] {title} ({start})"))
        })
        .collect();
    ctx.add_result(json!({
        "capability": capability,
        "date": date.to_string(),
        "items": items,
        "context_text": lines.join("\n"),
    }));
}
