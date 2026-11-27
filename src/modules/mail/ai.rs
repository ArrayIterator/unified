//! Mail - AI handlers and grounding.
//!
//! Part of the `mail` module split; see `src/modules/mail.rs`.
//!
//! Capability declarations live in `ai_capabilities.rs`.

use unified_core::context::AppContext;
use unified_core::json as api_json;
use crate::modules::ai;
use super::{caller_company, require_tenant};
use actix_web::HttpRequest;
use actix_web::HttpResponse;
use actix_web::web;
use std::sync::Arc;

// AI capabilities (AI Capabilities Contract, see `crate::modules::ai`).
//
// Declared here, registered once in `init`, grounded through the
// `mail.ai.draft_requested` hook, and run through the AI module's
// gated runner. Drafts are returned inline and are never sent by
// this module: sending stays on the existing human endpoints.

/// Run one registered mail AI capability:
/// `POST /api/v1/mail/ai/{capability}` with body
/// `{"input": {...}}`. Requires `ai:use`; delegates to the AI
/// module's gated runner.
pub(crate) async fn ai_capability(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ai::CapabilityCallInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let capability = path.into_inner();
    let _tenant = match require_tenant(&req) {
        Ok(value) => value,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(value) => value,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let caller = match ai::caller(&req) {
        Ok(caller) => caller,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = ai::require_permission(&ctx, &caller.role, ai::PERM_AI_USE) {
        return api_json::error_response(&error, debug);
    }
    let company_id = ai::resolve_capability_company(&ctx, &req, &caller);
    match ai::run_ai_capability(
        &ctx,
        &caller,
        company_id,
        "mail",
        &capability,
        body.into_inner().input,
    )
    .await
    {
        Ok(data) => api_json::success_response(data, 200),
        Err(error) => api_json::error_response(&error, debug),
    }
}
