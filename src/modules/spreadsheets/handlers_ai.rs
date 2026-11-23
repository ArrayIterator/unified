//! HTTP handler for the spreadsheets AI surface: one
//! registered capability at a time through the AI module's
//! gated runner (`run_ai_capability`, capability allowlist
//! gate first), exactly like the delivery notes module's
//! `ai_capability` handler. The AI never creates, edits, or
//! shares a workbook on its own: both capabilities return
//! text inline only.

use actix_web::{web, HttpRequest, HttpResponse};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;

use super::helpers::{caller, err, ok, require_caller_company, require_tenant};
use super::state::SpreadsheetsState;
use super::types::PERM_SPREADSHEETS_VIEW;
use crate::modules::ai;

/// Run one registered spreadsheets AI capability through the
/// AI module's gated runner.
pub(crate) async fn ai_capability(
    state: web::Data<SpreadsheetsState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ai::CapabilityCallInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let capability = path.into_inner();
    let input = body.into_inner().input;
    let _ = state;
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_caller_company(&tenant, &req) {
        return err(error, debug);
    }
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) =
        super::helpers::require_permission(&ctx, &caller.role, PERM_SPREADSHEETS_VIEW)
    {
        return err(error, debug);
    }
    let ai_caller = match ai::caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = ai::require_permission(&ctx, &ai_caller.role, ai::PERM_AI_USE) {
        return err(error, debug);
    }
    let company_id = ai::resolve_capability_company(&ctx, &req, &ai_caller);
    match ai::run_ai_capability(
        &ctx,
        &ai_caller,
        company_id,
        "spreadsheets",
        &capability,
        input,
    )
    .await
    {
        Ok(output) => ok(output),
        Err(error) => err(error, debug),
    }
}

/// The grounding facts for one workbook, used by the AI
/// grounding hooks in `hooks.rs`: the stored workbook plus
/// its sheets and cell counts, so the AI summarizes stored
/// facts instead of inventing a spreadsheet.
pub(crate) fn grounding_facts(
    state: &SpreadsheetsState,
    tenant_id: &str,
    workbook_id: &str,
) -> UnifiedResult<serde_json::Value> {
    let workbook = super::helpers::load_workbook(state, tenant_id, workbook_id)?;
    let sheets: Vec<serde_json::Value> = state
        .sheets_of(workbook.id)
        .iter()
        .map(|sheet| {
            serde_json::json!({
                "sheet_id": sheet.id.to_string(),
                "title": sheet.title,
                "position": sheet.position,
                "cell_count": state.cells_of(sheet.id).len(),
            })
        })
        .collect();
    Ok(serde_json::json!({
        "workbook_id": workbook.id.to_string(),
        "title": workbook.title,
        "description": workbook.description,
        "owner_id": workbook.owner_id,
        "public_link_enabled": workbook.public_link_enabled,
        "sheets": sheets,
    }))
}
