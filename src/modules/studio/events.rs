// Studio - Hook handlers for field, layout, view and record-type requests.

use unified_core::hooks::HookContext;
use unified_core::json;
use serde_json::json;
use std::sync::Arc;

use super::{CreateRecordType, StudioState};

// ---------------------------------------------------------------------------
// Hook query handlers (the hook contract).
// ---------------------------------------------------------------------------

pub(crate) fn on_fields_requested(state: Arc<StudioState>, ctx: &mut HookContext) {
    let record_type = ctx
        .get("record_type")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let company_id = ctx
        .get("company_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if record_type.is_empty() || !state.record_types.contains_key(&record_type) {
        ctx.add_result(json!({
            "record_type": record_type,
            "company_id": company_id,
            "definitions": [],
            "error": "unknown record type",
        }));
        return;
    }
    let definitions = state.effective_definitions(&record_type, &company_id);
    ctx.add_result(json!({
        "record_type": record_type,
        "company_id": company_id,
        "definitions": definitions,
    }));
}

pub(crate) fn on_layout_requested(state: Arc<StudioState>, ctx: &mut HookContext) {
    let record_type = ctx
        .get("record_type")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let company_id = ctx
        .get("company_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let layout = if record_type.is_empty() {
        None
    } else {
        state.default_layout(&record_type, &company_id)
    };
    ctx.add_result(json!({
        "record_type": record_type,
        "company_id": company_id,
        "layout": layout,
    }));
}

pub(crate) fn on_views_requested(state: Arc<StudioState>, ctx: &mut HookContext) {
    let record_type = ctx
        .get("record_type")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let company_id = ctx
        .get("company_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let user_id = ctx
        .get("user_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let views = if record_type.is_empty() || user_id.is_empty() {
        Vec::new()
    } else {
        state.visible_views(&record_type, &company_id, &user_id)
    };
    ctx.add_result(json!({
        "record_type": record_type,
        "company_id": company_id,
        "user_id": user_id,
        "views": views,
    }));
}

pub(crate) fn on_record_type_register(state: Arc<StudioState>, ctx: &mut HookContext) {
    let input: Result<CreateRecordType, _> = serde_json::from_value(ctx.data.clone());
    match input {
        Ok(parsed) => match state.register_record_type(parsed) {
            Ok(registered) => {
                ctx.add_result(json!({
                    "registered": true,
                    "record_type": registered.record_type,
                    "module_id": registered.module_id,
                }));
            }
            Err(error) => {
                ctx.add_result(json!({
                    "registered": false,
                    "error": error.message.clone(),
                }));
            }
        },
        Err(_) => {
            ctx.add_result(json!({
                "registered": false,
                "error": "record_type.register payload must contain record_type, module_id, and label",
            }));
        }
    }
}
