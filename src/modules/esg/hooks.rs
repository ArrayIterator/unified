//! Hook handlers for user deletion and the AI grounding
//! hooks.

use super::handlers_ai::grounding_facts;
use super::state::EsgState;
use super::types::DELETED_USER_ID;
use chrono::Utc;
use serde_json::json;
use unified_core::hooks::HookContext;

/// The AI module's runner emits this event for the summary
/// capability; this module answers with the period's stored
/// facts.
pub(crate) fn ai_summarize_event() -> &'static str {
    "ai.summarize.requested"
}

/// The AI module's runner emits this event for the readiness
/// capability; this module answers with a readiness checklist.
pub(crate) fn ai_readiness_event() -> &'static str {
    "ai.readiness.requested"
}

/// `user.deleted` carries `user_id` (falling back to `id`, the
/// same shape the sign and spreadsheets modules read). The
/// tenant's ESG rows keep their history but lose the identity
/// (`created_by` and `entered_by` become `deleted-user`, and
/// audit actors are anonymized the same way). Rows are never
/// deleted: a sustainability report must stay on record.
/// Scopes are company / site reference ids, not user ids, so
/// they are untouched.
pub(crate) fn on_user_deleted(state: EsgState, ctx: &mut HookContext) {
    let user_id = ctx
        .get("user_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let Some(user_id) = user_id else {
        ctx.add_result(json!({ "esg": "skipped", "reason": "user.deleted carries no user_id" }));
        return;
    };
    let now = Utc::now();
    let mut anonymized_frameworks = 0i64;
    for mut entry in state.frameworks.iter_mut() {
        if entry.value().created_by == user_id {
            entry.value_mut().created_by = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_frameworks += 1;
        }
    }
    let mut anonymized_metrics = 0i64;
    for mut entry in state.metrics.iter_mut() {
        if entry.value().created_by == user_id {
            entry.value_mut().created_by = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_metrics += 1;
        }
    }
    let mut anonymized_periods = 0i64;
    for mut entry in state.periods.iter_mut() {
        if entry.value().created_by == user_id {
            entry.value_mut().created_by = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_periods += 1;
        }
    }
    let mut anonymized_entries = 0i64;
    for mut entry in state.entries.iter_mut() {
        if entry.value().entered_by == user_id {
            entry.value_mut().entered_by = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_entries += 1;
        }
    }
    let mut anonymized_history = 0i64;
    for mut entry in state.entry_history.iter_mut() {
        if entry.value().entered_by == user_id {
            entry.value_mut().entered_by = DELETED_USER_ID.to_string();
            anonymized_history += 1;
        }
    }
    let mut anonymized_targets = 0i64;
    for mut entry in state.targets.iter_mut() {
        if entry.value().created_by == user_id {
            entry.value_mut().created_by = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_targets += 1;
        }
    }
    let mut anonymized_audit = 0i64;
    for mut entry in state.audit.iter_mut() {
        if entry.value().actor == user_id {
            entry.value_mut().actor = DELETED_USER_ID.to_string();
            anonymized_audit += 1;
        }
    }
    ctx.add_result(json!({
        "esg": "user_deleted",
        "user_id": user_id,
        "anonymized_frameworks": anonymized_frameworks,
        "anonymized_metrics": anonymized_metrics,
        "anonymized_periods": anonymized_periods,
        "anonymized_entries": anonymized_entries,
        "anonymized_history": anonymized_history,
        "anonymized_targets": anonymized_targets,
        "anonymized_audit": anonymized_audit,
    }));
}

/// Answer an AI summary request with the period's stored
/// facts. The payload names the period under `period_id`
/// (falling back to `id`, the shape other modules use).
pub(crate) fn on_ai_summarize_requested(state: EsgState, ctx: &mut HookContext) {
    let period_id = ctx
        .get("period_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let Some(period_id) = period_id else {
        ctx.add_result(
            json!({ "esg": "skipped", "reason": "ai.summarize.requested carries no period_id" }),
        );
        return;
    };
    let tenant_id = ctx
        .get("tenant_id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    match grounding_facts(&state, tenant_id, &period_id) {
        Ok(facts) => ctx.add_result(json!({ "esg": "summarize", "facts": facts })),
        Err(_) => ctx.add_result(json!({ "esg": "skipped", "reason": "period not found" })),
    }
}

/// Answer an AI readiness request: a period is ready to
/// summarize when it has at least one reported metric.
pub(crate) fn on_ai_readiness_requested(state: EsgState, ctx: &mut HookContext) {
    let period_id = ctx
        .get("period_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let Some(period_id) = period_id else {
        ctx.add_result(
            json!({ "esg": "skipped", "reason": "ai.readiness.requested carries no period_id" }),
        );
        return;
    };
    let tenant_id = ctx
        .get("tenant_id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let ready = match super::helpers::load_period(&state, tenant_id, &period_id) {
        Ok(period) => !state.entries_of_period(tenant_id, period.id).is_empty(),
        Err(_) => false,
    };
    ctx.add_result(json!({
        "esg": "readiness",
        "period_id": period_id,
        "ready": ready,
        "checklist": ["period exists", "at least one metric has a recorded value"],
    }));
}
