//! Presets + Setup Wizard module. - Preset handlers.
//!
//! Split from `src/modules/presets.rs`; behaviour unchanged.

use super::apply::*;
use super::events::*;
use super::helpers::*;
use super::identity::*;
use super::lookups::*;
use super::records::*;
use super::requests::*;
use super::seeds::*;
use super::stores::*;
use super::wizard_handlers::*;
use super::wizard_steps::*;

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::json::{error_response, success_response};
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{Map, Value};
use std::sync::Arc;
use uuid::Uuid;

pub(super) async fn list_presets(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    if let Err(error) = require_caller_permission(&ctx, &req, "presets:view") {
        return error_response(&error, debug);
    }
    let pagination = query.normalized();
    let built_in_filter = pagination
        .filters
        .get("built_in")
        .and_then(|value| value.parse::<bool>().ok());
    let archived_filter = pagination
        .filters
        .get("archived")
        .and_then(|value| value.parse::<bool>().ok());
    let business_type_filter = pagination.filters.get("business_type").cloned();
    let mut items: Vec<Preset> = state
        .presets
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|preset| {
            // Archived presets are hidden by default; pass
            // `?archived=true` to list them.
            if archived_filter.is_none() && preset.archived {
                return false;
            }
            if let Some(archived) = archived_filter {
                if preset.archived != archived {
                    return false;
                }
            }
            if let Some(built_in) = built_in_filter {
                if preset.built_in != built_in {
                    return false;
                }
            }
            if let Some(business_type) = &business_type_filter {
                if &preset.business_type != business_type {
                    return false;
                }
            }
            let haystack = format!(
                "{} {} {} {}",
                preset.slug, preset.name, preset.description, preset.business_type
            );
            matches_search(&haystack, &query)
        })
        .collect();
    items.sort_by(|a, b| a.slug.cmp(&b.slug));
    success_response(paginate(items, &query).to_json(), 200)
}

pub(super) async fn create_preset(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    body: web::Json<CreatePresetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    let caller = match require_caller_permission(&ctx, &req, "presets:manage") {
        Ok(caller) => caller,
        Err(error) => return error_response(&error, debug),
    };
    let request = body.into_inner();
    let name = request.name.trim().to_string();
    if name.is_empty() || name.len() > 200 {
        return error_response(
            &UnifiedError::bad_request("Preset name is required (at most 200 characters)"),
            debug,
        );
    }
    let slug = match request.slug {
        Some(slug) => slug.trim().to_lowercase(),
        None => slugify(&name),
    };
    if !valid_slug(&slug) {
        return error_response(
            &UnifiedError::bad_request(
                "Invalid preset slug (lowercase letters, digits, and single hyphens only)",
            ),
            debug,
        );
    }
    if slug_in_use(&state, &slug, None) {
        return error_response(
            &UnifiedError::conflict(format!("A preset with slug \"{slug}\" already exists")),
            debug,
        );
    }
    let business_type = request.business_type.trim().to_string();
    if !valid_business_type(&business_type) {
        return error_response(
            &UnifiedError::bad_request(
                "Invalid business type (lowercase letters, digits, underscores, and hyphens only)",
            ),
            debug,
        );
    }
    let modules = match validate_preset_modules(&ctx, &request.modules) {
        Ok(modules) => modules,
        Err(error) => return error_response(&error, debug),
    };
    let settings = match request.settings {
        Some(Value::Object(_)) | None => request.settings.unwrap_or(Value::Object(Map::new())),
        Some(_) => {
            return error_response(
                &UnifiedError::bad_request("Preset settings must be a JSON object"),
                debug,
            );
        }
    };
    let steps = match request.steps {
        Some(steps) => {
            if let Err(error) = validate_steps(&steps) {
                return error_response(&error, debug);
            }
            steps
        }
        None => DEFAULT_WIZARD_STEPS
            .iter()
            .map(|step| (*step).to_string())
            .collect(),
    };
    let description = request
        .description
        .unwrap_or_default()
        .trim()
        .to_string();
    if description.len() > 2000 {
        return error_response(
            &UnifiedError::bad_request("Preset description is too long (at most 2000 characters)"),
            debug,
        );
    }
    let actor_id = caller.actor.clone();
    let now = now();
    let preset = Preset {
        id: Uuid::now_v7(),
        slug: slug.clone(),
        name: name.clone(),
        description,
        business_type: business_type.clone(),
        built_in: false,
        archived: false,
        modules,
        settings,
        steps,
        created_by: if actor_id.is_empty() {
            None
        } else {
            Some(actor_id.clone())
        },
        created_at: now,
        updated_at: now,
    };
    state.presets.insert(preset.id, preset.clone());
    emit_audit(
        &ctx,
        PRESET_CREATED,
        &actor_id,
        "preset.created",
        "preset",
        &preset.id.to_string(),
        None,
        serde_json::json!({ "slug": slug, "business_type": business_type }),
    );
    success_response(preset, 201)
}

pub(super) async fn get_preset(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    id: web::Path<String>,
    _query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    if let Err(error) = require_caller_permission(&ctx, &req, "presets:view") {
        return error_response(&error, debug);
    }
    let key = id.into_inner();
    match find_preset(&state, &key) {
        Some(preset) => success_response(preset, 200),
        None => error_response(
            &UnifiedError::not_found(format!("Preset \"{key}\" not found")),
            debug,
        ),
    }
}

pub(super) async fn update_preset(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    id: web::Path<String>,
    body: web::Json<UpdatePresetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    let caller = match require_caller_permission(&ctx, &req, "presets:manage") {
        Ok(caller) => caller,
        Err(error) => return error_response(&error, debug),
    };
    let request = body.into_inner();
    let key = id.into_inner();
    let Some(mut preset) = find_preset(&state, &key) else {
        return error_response(
            &UnifiedError::not_found(format!("Preset \"{key}\" not found")),
            debug,
        );
    };
    // Built-in presets are edited by cloning them, never in place,
    // so the seeded definitions stay intact.
    if preset.built_in {
        return error_response(
            &UnifiedError::conflict(format!(
                "Built-in preset \"{}\" cannot be edited directly; clone it and edit the copy",
                preset.slug
            ))
            .with_metadata("hint", serde_json::json!("POST /api/v1/presets/{id}/clone")),
            debug,
        );
    }
    if let Some(name) = request.name {
        let name = name.trim().to_string();
        if name.is_empty() || name.len() > 200 {
            return error_response(
                &UnifiedError::bad_request("Preset name is required (at most 200 characters)"),
                debug,
            );
        }
        preset.name = name;
    }
    if let Some(slug) = request.slug {
        let slug = slug.trim().to_lowercase();
        if !valid_slug(&slug) {
            return error_response(
                &UnifiedError::bad_request(
                    "Invalid preset slug (lowercase letters, digits, and single hyphens only)",
                ),
                debug,
            );
        }
        if slug_in_use(&state, &slug, Some(preset.id)) {
            return error_response(
                &UnifiedError::conflict(format!("A preset with slug \"{slug}\" already exists")),
                debug,
            );
        }
        preset.slug = slug;
    }
    if let Some(description) = request.description {
        let description = description.trim().to_string();
        if description.len() > 2000 {
            return error_response(
                &UnifiedError::bad_request(
                    "Preset description is too long (at most 2000 characters)",
                ),
                debug,
            );
        }
        preset.description = description;
    }
    if let Some(business_type) = request.business_type {
        let business_type = business_type.trim().to_string();
        if !valid_business_type(&business_type) {
            return error_response(
                &UnifiedError::bad_request(
                    "Invalid business type (lowercase letters, digits, underscores, and hyphens only)",
                ),
                debug,
            );
        }
        preset.business_type = business_type;
    }
    if let Some(modules) = request.modules {
        match validate_preset_modules(&ctx, &modules) {
            Ok(validated) => preset.modules = validated,
            Err(error) => return error_response(&error, debug),
        }
    }
    if let Some(settings) = request.settings {
        if !settings.is_object() {
            return error_response(
                &UnifiedError::bad_request("Preset settings must be a JSON object"),
                debug,
            );
        }
        preset.settings = settings;
    }
    if let Some(steps) = request.steps {
        if let Err(error) = validate_steps(&steps) {
            return error_response(&error, debug);
        }
        preset.steps = steps;
    }
    if let Some(archived) = request.archived {
        preset.archived = archived;
    }
    preset.updated_at = now();
    let actor_id = caller.actor.clone();
    state.presets.insert(preset.id, preset.clone());
    emit_audit(
        &ctx,
        PRESET_UPDATED,
        &actor_id,
        "preset.updated",
        "preset",
        &preset.id.to_string(),
        None,
        serde_json::json!({ "slug": preset.slug }),
    );
    success_response(preset, 200)
}

pub(super) async fn clone_preset(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    id: web::Path<String>,
    body: web::Json<ClonePresetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    let caller = match require_caller_permission(&ctx, &req, "presets:manage") {
        Ok(caller) => caller,
        Err(error) => return error_response(&error, debug),
    };
    let request = body.into_inner();
    let key = id.into_inner();
    let Some(source) = find_preset(&state, &key) else {
        return error_response(
            &UnifiedError::not_found(format!("Preset \"{key}\" not found")),
            debug,
        );
    };
    let mut slug = match request.slug {
        Some(slug) => slug.trim().to_lowercase(),
        None => format!("{}-copy", source.slug),
    };
    // Keep the copy's slug unique without failing the clone: append
    // a counter until the slug is free.
    let mut counter = 2;
    while slug_in_use(&state, &slug, None) {
        slug = format!("{}-copy-{counter}", source.slug);
        counter += 1;
        if counter > 100 {
            return error_response(
                &UnifiedError::conflict("Could not find a free slug for the cloned preset"),
                debug,
            );
        }
    }
    if !valid_slug(&slug) {
        return error_response(
            &UnifiedError::bad_request(
                "Invalid preset slug (lowercase letters, digits, and single hyphens only)",
            ),
            debug,
        );
    }
    let name = match request.name {
        Some(name) => name.trim().to_string(),
        None => format!("{} (copy)", source.name),
    };
    if name.is_empty() || name.len() > 200 {
        return error_response(
            &UnifiedError::bad_request("Preset name is required (at most 200 characters)"),
            debug,
        );
    }
    let actor_id = caller.actor.clone();
    let now = now();
    let clone = Preset {
        id: Uuid::now_v7(),
        slug: slug.clone(),
        name,
        description: source.description.clone(),
        business_type: source.business_type.clone(),
        built_in: false,
        archived: false,
        modules: source.modules.clone(),
        settings: source.settings.clone(),
        steps: source.steps.clone(),
        created_by: if actor_id.is_empty() {
            None
        } else {
            Some(actor_id.clone())
        },
        created_at: now,
        updated_at: now,
    };
    state.presets.insert(clone.id, clone.clone());
    emit_audit(
        &ctx,
        PRESET_CLONED,
        &actor_id,
        "preset.cloned",
        "preset",
        &clone.id.to_string(),
        None,
        serde_json::json!({ "slug": slug, "source_preset_id": source.id }),
    );
    success_response(clone, 201)
}

pub(super) async fn archive_preset(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    id: web::Path<String>,
    _body: web::Json<ActorInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    let caller = match require_caller_permission(&ctx, &req, "presets:manage") {
        Ok(caller) => caller,
        Err(error) => return error_response(&error, debug),
    };
    let key = id.into_inner();
    let Some(mut preset) = find_preset(&state, &key) else {
        return error_response(
            &UnifiedError::not_found(format!("Preset \"{key}\" not found")),
            debug,
        );
    };
    if preset.archived {
        return error_response(
            &UnifiedError::conflict(format!("Preset \"{}\" is already archived", preset.slug)),
            debug,
        );
    }
    preset.archived = true;
    preset.updated_at = now();
    let actor_id = caller.actor.clone();
    state.presets.insert(preset.id, preset.clone());
    emit_audit(
        &ctx,
        PRESET_ARCHIVED,
        &actor_id,
        "preset.archived",
        "preset",
        &preset.id.to_string(),
        None,
        serde_json::json!({ "slug": preset.slug }),
    );
    success_response(preset, 200)
}

pub(super) async fn apply_preset_endpoint(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    id: web::Path<String>,
    body: web::Json<ApplyPresetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    let caller = match require_caller_permission(&ctx, &req, "presets:apply") {
        Ok(caller) => caller,
        Err(error) => return error_response(&error, debug),
    };
    let request = body.into_inner();
    // The application always carries a company: a body company
    // other than the caller's scope is refused, and an unresolved
    // caller may not apply a preset for any company.
    let company_id = match require_company_match_uuid(&_tenant, &req, &Some(request.company_id)) {
        Ok(Some(id)) => id,
        Ok(None) => {
            return error_response(
                &UnifiedError::forbidden(
                    "company_id does not match the caller's company scope",
                ),
                debug,
            )
        }
        Err(error) => return error_response(&error, debug),
    };
    let key = id.into_inner();
    let Some(preset) = find_preset(&state, &key) else {
        return error_response(
            &UnifiedError::not_found(format!("Preset \"{key}\" not found")),
            debug,
        );
    };
    if preset.archived {
        return error_response(
            &UnifiedError::conflict(format!(
                "Preset \"{}\" is archived and cannot be applied",
                preset.slug
            )),
            debug,
        );
    }
    let disabled = match request.disabled_optional_modules {
        Some(disabled) => match validate_disabled_optional(&preset, &disabled) {
            Ok(cleaned) => cleaned,
            Err(error) => return error_response(&error, debug),
        },
        None => Vec::new(),
    };
    let settings_override = match &request.settings {
        Some(settings) if !settings.is_object() => {
            return error_response(
                &UnifiedError::bad_request("Settings overrides must be a JSON object"),
                debug,
            );
        }
        settings => settings.as_ref(),
    };
    let actor_id = caller.actor.clone();
    match apply_preset(
        &ctx,
        &state,
        &preset,
        company_id,
        &actor_id,
        &disabled,
        settings_override,
    ) {
        Ok(application) => success_response(application, 201),
        Err(error) => error_response(&error, debug),
    }
}

pub(super) async fn preview_preset(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    id: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    if let Err(error) = require_caller_permission(&ctx, &req, "presets:view") {
        return error_response(&error, debug);
    }
    let scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return error_response(&error, debug),
    };
    // Tenant filter from the query: a data filter, not identity -
    // but a query company other than the caller's scope is refused,
    // and a scoped caller with no query previews their own company.
    let company_id = match resolve_preview_company(&scope, query.filters.get("company_id")) {
        Ok(company_id) => company_id,
        Err(error) => return error_response(&error, debug),
    };
    let key = id.into_inner();
    let Some(preset) = find_preset(&state, &key) else {
        return error_response(
            &UnifiedError::not_found(format!("Preset \"{key}\" not found")),
            debug,
        );
    };
    let mut modules: Vec<PresetPreviewModule> = Vec::new();
    for entry in &preset.modules {
        let module = ctx.modules.get(&entry.module_id);
        let (status, reason, missing_required) = match &module {
            None => (
                "missing".to_string(),
                Some(format!(
                    "Module \"{}\" is not registered",
                    entry.module_id
                )),
                vec![entry.module_id.clone()],
            ),
            Some(module) => {
                let missing: Vec<String> = ctx
                    .modules
                    .requires_of(&entry.module_id)
                    .iter()
                    .filter(|required| ctx.modules.get(required).is_none())
                    .map(|required| (*required).to_string())
                    .collect();
                if !missing.is_empty() {
                    (
                        "missing".to_string(),
                        Some(format!(
                            "Missing dependency module(s): {}",
                            missing.join(", ")
                        )),
                        missing,
                    )
                } else if state.tenant_has_module(
                    &company_id.unwrap_or(Uuid::nil()),
                    &entry.module_id,
                ) && company_id.is_some()
                {
                    ("already_enabled".to_string(), None, Vec::new())
                } else if !entitlement_available(&ctx, company_id, &entry.module_id) {
                    (
                        "not_entitled".to_string(),
                        Some(
                            "No active entitlement for this module for this tenant".to_string(),
                        ),
                        Vec::new(),
                    )
                } else {
                    ("ok".to_string(), None, Vec::new())
                }
            }
        };
        let will_auto_activate: Vec<String> = ctx
            .modules
            .transitive_requires_ordered(&entry.module_id)
            .unwrap_or_default()
            .into_iter()
            .filter(|required| {
                !state.tenant_has_module(&company_id.unwrap_or(Uuid::nil()), required)
                    || company_id.is_none()
            })
            .collect();
        modules.push(PresetPreviewModule {
            module_id: entry.module_id.clone(),
            name: module.map(|module| module.name().to_string()),
            required: entry.required,
            status,
            reason,
            will_auto_activate,
            missing_required,
        });
    }
    let apply_order = resolve_apply_order(
        &ctx,
        &preset
            .modules
            .iter()
            .map(|entry| entry.module_id.clone())
            .collect::<Vec<String>>(),
    )
    .unwrap_or_default();
    success_response(
        PresetPreview {
            preset,
            company_id,
            modules,
            apply_order,
        },
        200,
    )
}

pub(super) async fn list_applications(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    if let Err(error) = require_caller_permission(&ctx, &req, "presets:view") {
        return error_response(&error, debug);
    }
    let scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return error_response(&error, debug),
    };
    let pagination = query.normalized();
    let preset_filter = pagination.filters.get("preset_id").cloned();
    let company_filter = pagination.filters.get("company_id").cloned();
    let status_filter = pagination.filters.get("status").cloned();
    let mut items: Vec<PresetApplication> = state
        .applications
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|application| {
            // Applications always carry a company: a scoped caller
            // sees only their own company's applications, an
            // unresolved caller sees none.
            if !in_company_scope_uuid(&application.company_id, &scope) {
                return false;
            }
            if let Some(preset_id) = &preset_filter {
                if application.preset_id.to_string() != *preset_id
                    && application.preset_slug != *preset_id
                {
                    return false;
                }
            }
            if let Some(company_id) = &company_filter {
                if application.company_id.to_string() != *company_id {
                    return false;
                }
            }
            if let Some(status) = &status_filter {
                if &application.status != status {
                    return false;
                }
            }
            let haystack = format!("{} {}", application.preset_slug, application.actor_id);
            matches_search(&haystack, &query)
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    success_response(paginate(items, &query).to_json(), 200)
}

pub(super) async fn get_application(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    id: web::Path<String>,
    _query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    if let Err(error) = require_caller_permission(&ctx, &req, "presets:view") {
        return error_response(&error, debug);
    }
    let scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return error_response(&error, debug),
    };
    let key = id.into_inner();
    let Ok(application_id) = Uuid::parse_str(&key) else {
        return error_response(
            &UnifiedError::bad_request(format!("Invalid application id \"{key}\"")),
            debug,
        );
    };
    match state.applications.get(&application_id) {
        Some(application) => {
            let application = application.value().clone();
            if let Err(error) =
                require_record_in_company_scope_uuid(&application.company_id, &scope)
            {
                return error_response(&error, debug);
            }
            success_response(application, 200)
        }
        None => error_response(
            &UnifiedError::not_found(format!("Preset application \"{key}\" not found")),
            debug,
        ),
    }
}

#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    use super::*;
    use unified_core::config::AppConfig;
    use unified_core::hooks::{HookBus, HookContext};
    use unified_core::modules::{Module, ModuleRegistry};
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;

    /// A minimal registry module for handler tests.
    struct FakeModule {
        id: &'static str,
    }

    impl Module for FakeModule {
        fn id(&self) -> &'static str {
            self.id
        }

        fn name(&self) -> &'static str {
            self.id
        }

        fn description(&self) -> &'static str {
            "test-only module"
        }
    }

    /// Context whose `role.check` answers from a fixed allow-list
    /// of permissions, over a registry of fake modules.
    fn test_ctx(
        allowed: &[&str],
        modules: &[&'static str],
    ) -> web::Data<Arc<AppContext>> {
        let registry = Arc::new(ModuleRegistry::new());
        for module in modules {
            registry.register(Arc::new(FakeModule { id: module }));
        }
        let hooks = Arc::new(HookBus::new());
        let allowed: Vec<String> = allowed
            .iter()
            .map(|permission| permission.to_string())
            .collect();
        hooks.register(
            "role.check",
            "test",
            Arc::new(move |context: &mut HookContext| {
                let permission = context
                    .get("permission")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                context.set(
                    "allowed",
                    serde_json::json!(allowed.iter().any(|held| held == permission)),
                );
            }),
        );
        web::Data::new(Arc::new(AppContext::new(
            AppConfig::default(),
            hooks,
            registry,
        )))
    }

    fn state_data() -> (web::Data<PresetsState>, PresetsState) {
        let state = PresetsState::new();
        (web::Data::new(state.clone()), state)
    }

    fn create_body() -> web::Json<CreatePresetRequest> {
        // The body claims the admin role and a body actor id: both
        // must be ignored for authorization and audit identity.
        web::Json(CreatePresetRequest {
            actor: ActorInput {
                viewer_id: Some("body-actor".to_string()),
                viewer_role: Some("admin".to_string()),
                company_id: None,
            },
            name: "Test Preset".to_string(),
            slug: None,
            description: None,
            business_type: "test".to_string(),
            modules: vec![PresetModuleEntryInput {
                module_id: "alpha".to_string(),
                required: true,
                sort_order: None,
            }],
            settings: None,
            steps: None,
        })
    }

    #[test]
    fn body_claiming_admin_without_a_verified_caller_is_unauthenticated() {
        let ctx = test_ctx(&["presets:manage"], &["alpha"]);
        let (state, _) = state_data();
        let response = create_preset(
            TestRequest::default().to_http_request(),
            ctx,
            state,
            create_body(),
        );
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn body_claiming_admin_is_denied_when_role_check_denies() {
        // The verified caller is a viewer; no `role.check` listener answers,
        // so the check fails closed even though the body claims
        // admin. This is the exact request the old body-role check
        // used to let through.
        let ctx = test_ctx(&[], &["alpha"]);
        let (state, _) = state_data();
        let request = verified_request("user-1", "viewer");
        let response = create_preset(request, ctx, state, create_body());
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn allowed_create_uses_the_verified_caller_not_the_body_actor() {
        let ctx = test_ctx(&["presets:manage"], &["alpha"]);
        let (state, inner) = state_data();
        let request = verified_request("header-actor", "admin");
        let response = create_preset(request, ctx, state, create_body());
        assert_eq!(response.status(), StatusCode::CREATED);
        let stored = inner
            .presets
            .iter()
            .next()
            .expect("preset stored")
            .value()
            .clone();
        assert_eq!(
            stored.created_by.as_deref(),
            Some(test_user_uuid("header-actor").to_string().as_str())
        );
    }

    #[test]
    fn list_requires_a_verified_caller_and_role_check() {
        let (state, _) = state_data();
        let denied = list_presets(
            TestRequest::default().to_http_request(),
            test_ctx(&["presets:view"], &[]),
            state.clone(),
            web::Query(PaginationParams::default()),
        );
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        let allowed = list_presets(
            verified_request("user-1", "viewer"),
            test_ctx(&["presets:view"], &[]),
            state,
            web::Query(PaginationParams::default()),
        );
        assert_eq!(allowed.status(), StatusCode::OK);
    }
}
