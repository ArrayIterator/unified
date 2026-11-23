//! Hook handlers for user deletion and the AI grounding
//! hooks.

use super::handlers_ai::grounding_facts;
use super::state::SpreadsheetsState;
use super::types::{DELETED_USER_ID, EVENT_USER_DELETED};
use chrono::Utc;
use serde_json::json;
use unified_core::hooks::HookContext;

/// The AI module's runner emits this event for the summary
/// capability; this module answers with the workbook's stored
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
/// same shape the sign module reads). The user's workbooks
/// keep their history but lose the identity (`owner_id` and
/// `created_by` become `deleted-user`); share rows for that
/// user are anonymized to `Deleted user` with a per-share
/// anonymized email, so the sharing record stays auditable
/// without the identity. Workbooks are never deleted: a
/// spreadsheet must stay on record.
pub(crate) fn on_user_deleted(state: SpreadsheetsState, ctx: &mut HookContext) {
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
        ctx.add_result(
            json!({ "spreadsheets": "skipped", "reason": "user.deleted carries no user_id" }),
        );
        return;
    };
    let now = Utc::now();
    let mut anonymized_workbooks = 0i64;
    for mut entry in state.workbooks.iter_mut() {
        if entry.value().owner_id == user_id {
            entry.value_mut().owner_id = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_workbooks += 1;
        }
        if entry.value().created_by == user_id {
            entry.value_mut().created_by = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
        }
    }
    let mut anonymized_shares = 0i64;
    for mut entry in state.shares.iter_mut() {
        if entry.value().shared_with == user_id {
            let share_id = entry.value().id;
            entry.value_mut().shared_with = DELETED_USER_ID.to_string();
            entry.value_mut().shared_with_email = Some(format!("deleted-{share_id}@invalid"));
            entry.value_mut().updated_at = now;
            anonymized_shares += 1;
        }
        if entry.value().created_by == user_id {
            entry.value_mut().created_by = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
        }
    }
    let mut anonymized_versions = 0i64;
    for mut entry in state.versions.iter_mut() {
        if entry.value().created_by == user_id {
            entry.value_mut().created_by = DELETED_USER_ID.to_string();
            anonymized_versions += 1;
        }
    }
    ctx.add_result(json!({
        "spreadsheets": "user_deleted",
        "user_id": user_id,
        "anonymized_workbooks": anonymized_workbooks,
        "anonymized_shares": anonymized_shares,
        "anonymized_versions": anonymized_versions,
    }));
}

/// Answer an AI summary request with the workbook's stored
/// facts. The payload names the workbook under `workbook_id`
/// (falling back to `id`, the shape other modules use).
pub(crate) fn on_ai_summarize_requested(state: SpreadsheetsState, ctx: &mut HookContext) {
    let workbook_id = ctx
        .get("workbook_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let Some(workbook_id) = workbook_id else {
        ctx.add_result(
            json!({ "spreadsheets": "skipped", "reason": "ai.summarize.requested carries no workbook_id" }),
        );
        return;
    };
    let tenant_id = ctx
        .get("tenant_id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    match grounding_facts(&state, tenant_id, &workbook_id) {
        Ok(facts) => ctx.add_result(json!({ "spreadsheets": "summarize", "facts": facts })),
        Err(_) => {
            ctx.add_result(json!({ "spreadsheets": "skipped", "reason": "workbook not found" }))
        }
    }
}

/// Answer an AI readiness request: a workbook is ready to
/// summarize when it has at least one sheet with at least one
/// non-empty cell.
pub(crate) fn on_ai_readiness_requested(state: SpreadsheetsState, ctx: &mut HookContext) {
    let workbook_id = ctx
        .get("workbook_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let Some(workbook_id) = workbook_id else {
        ctx.add_result(
            json!({ "spreadsheets": "skipped", "reason": "ai.readiness.requested carries no workbook_id" }),
        );
        return;
    };
    let tenant_id = ctx
        .get("tenant_id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let ready = match super::helpers::load_workbook(&state, tenant_id, &workbook_id) {
        Ok(workbook) => state
            .sheets_of(workbook.id)
            .iter()
            .any(|sheet| !state.cells_of(sheet.id).is_empty()),
        Err(_) => false,
    };
    ctx.add_result(json!({
        "spreadsheets": "readiness",
        "workbook_id": workbook_id,
        "ready": ready,
        "checklist": ["workbook exists", "at least one sheet has cells"],
    }));
}
