// Handlers: anonymization and visibility.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::json;
use std::sync::Arc;

use super::*;

// ---------------------------------------------------------------------------
// Handlers: anonymization and visibility
// ---------------------------------------------------------------------------

pub(crate) async fn anonymize_user(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    body: web::Json<AnonymizeInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AnonymizationRecord> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_ADMIN)?;
        let input = body.into_inner();
        let user_id = require_text(&input.user_id, "user_id", 256)?;
        let scope_modules = validate_module_list(&input.scope_modules)?;
        let mut fields: Vec<String> = Vec::new();
        for field in &input.fields {
            if !valid_field_name(field) {
                return Err(UnifiedError::bad_request(format!(
                    "Field \"{field}\" must be lowercase letters, digits, or '_'"
                )));
            }
            if !fields.contains(field) {
                fields.push(field.clone());
            }
        }
        let reason = if input.reason.trim().is_empty() {
            "admin anonymization".to_string()
        } else {
            require_text(&input.reason, "reason", 500)?
        };
        state.anonymize_internal(&user_id, &scope_modules, &fields, &reason, &caller.actor)
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_anonymizations(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AnonymizationRecord>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        let params = query.into_inner();
        let mut items: Vec<AnonymizationRecord> = state
            .anonymizations
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                matches_search(&format!("{} {}", record.user_id, record.token), &params)
                    && params
                        .filters
                        .get("user_id")
                        .is_none_or(|user_id| record.user_id == *user_id)
            })
            .collect();
        sort_desc_by_created(&mut items, |record| record.created_at);
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_anonymization(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AnonymizationRecord> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        state
            .anonymizations
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Anonymization record not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_visibility_defaults(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<VisibilityDefault>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        let params = query.into_inner();
        let mut items: Vec<VisibilityDefault> = state
            .visibility_defaults
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                matches_search(&record.module_id, &params)
                    && params
                        .filters
                        .get("module_id")
                        .is_none_or(|module_id| record.module_id == *module_id)
                    && params
                        .filters
                        .get("visibility")
                        .is_none_or(|visibility| record.visibility.as_str() == visibility)
            })
            .collect();
        items.sort_by(|a, b| a.module_id.cmp(&b.module_id));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn set_visibility_default(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    body: web::Json<SetVisibilityInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VisibilityDefault> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_ADMIN)?;
        let input = body.into_inner();
        if !valid_slug(&input.module_id) {
            return Err(UnifiedError::bad_request(
                "module_id must be a lowercase slug",
            ));
        }
        let visibility = Visibility::parse(&input.visibility).ok_or_else(|| {
            UnifiedError::bad_request(
                "visibility must be one of: private, company, public",
            )
        })?;
        // Widening beyond private is only allowed when the admin
        // explicitly confirms it; a missing or false `explicit`
        // flag keeps the default at private.
        if visibility != Visibility::Private && !input.explicit {
            return Err(UnifiedError::unprocessable(
                "Widening a visibility default beyond private requires explicit=true",
            ));
        }
        let record = VisibilityDefault {
            module_id: input.module_id.clone(),
            visibility,
            explicit: input.explicit && visibility != Visibility::Private,
            configured_by: caller.actor.clone(),
            updated_at: now(),
        };
        state
            .visibility_defaults
            .insert(input.module_id.clone(), record.clone());
        state.audit(
            EVENT_PRIVACY_VISIBILITY_CHANGED,
            &caller.actor,
            "privacy.visibility.changed",
            &format!("module:{}", record.module_id),
            json!({
                "module_id": record.module_id,
                "visibility": record.visibility.as_str(),
                "explicit": record.explicit,
            }),
        );
        Ok(record)
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_effective_visibility(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<EffectiveVisibility> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        let module_id = path.into_inner();
        if !valid_slug(&module_id) {
            return Err(UnifiedError::bad_request(
                "module_id must be a lowercase slug",
            ));
        }
        Ok(state.effective_visibility(&module_id))
    })();
    json::respond(result, debug)
}
