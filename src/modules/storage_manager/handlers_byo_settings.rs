// BYO settings handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::json;
use std::sync::Arc;

use super::*;

// ---------------------------------------------------------------------------
// BYO settings (admin-managed provider allowlist).
// ---------------------------------------------------------------------------

pub(crate) async fn get_byo_settings(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return err_response(error, debug);
    }
    let settings = state
        .byo_settings
        .read()
        .expect("BYO settings lock poisoned")
        .clone();
    ok(settings)
}

pub(crate) async fn update_byo_settings(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<UpdateByoSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_MANAGE_BYO) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<ByoSettings> = (|| {
        let input = body.into_inner();
        let mut settings = state
            .byo_settings
            .read()
            .expect("BYO settings lock poisoned")
            .clone();
        if let Some(kinds) = &input.allowed_provider_kinds {
            settings.allowed_provider_kinds = validate_provider_kind_list(kinds)?;
        }
        if let Some(terms_required) = input.terms_required {
            settings.terms_required = terms_required;
        }
        if let Some(max) = input.max_connections_per_user {
            if max < 0 {
                return Err(UnifiedError::bad_request(
                    "max_connections_per_user must not be negative (use 0 for unlimited)",
                ));
            }
            settings.max_connections_per_user = max;
        }
        *state
            .byo_settings
            .write()
            .expect("BYO settings lock poisoned") = settings.clone();
        state.emit(
            events::BYO_TERMS_CHANGED,
            json!({
                "scope": "byo_settings",
                "allowed_provider_kinds": settings.allowed_provider_kinds,
                "terms_required": settings.terms_required,
                "changed_by": found.actor,
            }),
        );
        Ok(settings)
    })();
    json::respond(result, debug)
}
