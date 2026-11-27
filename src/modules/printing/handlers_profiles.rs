//! printing - handlers profiles.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde_json::{json, Value};
use std::sync::{Arc};
use super::{events::*, paper::*, thermal::*, profiles::*, jobs::*, state::*, auth::*, hooks::*, handlers_paper::*, handlers_jobs::*, module::*};



// Printer-profile handlers
pub(crate) async fn list_printer_profiles(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<PrinterProfile>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<PrinterProfile> = state
            .printer_profiles
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|profile| {
                tenant_visible(profile.tenant_id.as_deref(), &caller.tenant_id)
                    && matches_search(&format!("{} {}", profile.name, profile.connection_type), &params)
                    && params
                        .filters
                        .get("connection_type")
                        .is_none_or(|kind| profile.connection_type == *kind)
                    && params.filters.get("company_id").is_none_or(|company_id| {
                        profile.company_id.as_deref() == Some(company_id.as_str())
                    })
                    && params
                        .filters
                        .get("is_default")
                        .is_none_or(|flag| profile.is_default.to_string() == *flag)
            })
            .collect();
        items.sort_by(|a, b| a.name.cmp(&b.name));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, ctx.is_debug())
}


pub(crate) async fn create_printer_profile(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreatePrinterProfile>,
) -> HttpResponse {
    let result: UnifiedResult<PrinterProfile> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_MANAGE)?;
        let input = body.into_inner();
        if input.name.trim().is_empty() || input.name.len() > 120 {
            return Err(UnifiedError::bad_request(
                "Printer profile name must be 1 to 120 characters",
            ));
        }
        if let Some(code) = &input.paper_size_code {
            if state.paper_size(code).is_none() {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown paper size \"{code}\""
                )));
            }
        }
        validate_printer_profile(
            &input.connection_type,
            input.connection_target.as_deref(),
            input.auth_token_ref.as_deref(),
            input.width_columns,
            &input.character_set,
            &input.code_page,
            input.paper_size_code.as_deref(),
        )?;
        let now = now();
        let profile = PrinterProfile {
            id: new_id(),
            company_id: input.company_id,
            tenant_id: Some(caller.tenant_id.clone()),
            name: input.name.trim().to_string(),
            connection_type: input.connection_type,
            connection_target: input.connection_target,
            auth_token_ref: input.auth_token_ref,
            width_columns: input.width_columns,
            character_set: input.character_set,
            code_page: input.code_page,
            supports_drawer_kick: input.supports_drawer_kick,
            supports_cut: input.supports_cut,
            paper_size_code: input.paper_size_code,
            is_default: false,
            created_by: Some(caller.actor.clone()),
            created_at: now,
            updated_at: now,
        };
        state
            .printer_profiles
            .insert(profile.id.clone(), profile.clone());
        state.emit(
            EVENT_PRINTER_PROFILE_CREATED,
            json!({ "id": profile.id.clone(), "company_id": profile.company_id.clone(), "created_by": caller.actor }),
        );
        Ok(profile)
    })();
    match result {
        Ok(profile) => json::success_response(profile, 201),
        Err(error) => json::error_response(&error, ctx.is_debug()),
    }
}


pub(crate) async fn get_printer_profile(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PrinterProfile> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_VIEW)?;
        let profile = state
            .printer_profiles
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Printer profile not found"))?;
        require_same_tenant(profile.tenant_id.as_deref(), &caller.tenant_id)?;
        Ok(profile)
    })();
    json::respond(result, ctx.is_debug())
}


pub(crate) async fn update_printer_profile(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePrinterProfile>,
) -> HttpResponse {
    let result: UnifiedResult<PrinterProfile> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_MANAGE)?;
        let id = path.into_inner();
        let mut profile = state
            .printer_profiles
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Printer profile not found"))?;
        require_same_tenant(profile.tenant_id.as_deref(), &caller.tenant_id)?;
        let input = body.into_inner();
        if let Some(name) = input.name {
            if name.trim().is_empty() || name.len() > 120 {
                return Err(UnifiedError::bad_request(
                    "Printer profile name must be 1 to 120 characters",
                ));
            }
            profile.name = name.trim().to_string();
        }
        if let Some(connection_type) = input.connection_type {
            profile.connection_type = connection_type;
        }
        if let Some(target) = input.connection_target {
            profile.connection_target = target;
        }
        if let Some(reference) = input.auth_token_ref {
            profile.auth_token_ref = reference;
        }
        if let Some(width) = input.width_columns {
            profile.width_columns = width;
        }
        if let Some(character_set) = input.character_set {
            profile.character_set = character_set;
        }
        if let Some(code_page) = input.code_page {
            profile.code_page = code_page;
        }
        if let Some(supports_drawer_kick) = input.supports_drawer_kick {
            profile.supports_drawer_kick = supports_drawer_kick;
        }
        if let Some(supports_cut) = input.supports_cut {
            profile.supports_cut = supports_cut;
        }
        if let Some(paper_size_code) = input.paper_size_code {
            profile.paper_size_code = paper_size_code;
        }
        if let Some(code) = &profile.paper_size_code {
            if state.paper_size(code).is_none() {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown paper size \"{code}\""
                )));
            }
        }
        validate_printer_profile(
            &profile.connection_type,
            profile.connection_target.as_deref(),
            profile.auth_token_ref.as_deref(),
            profile.width_columns,
            &profile.character_set,
            &profile.code_page,
            profile.paper_size_code.as_deref(),
        )?;
        profile.updated_at = now();
        state
            .printer_profiles
            .insert(id.clone(), profile.clone());
        state.emit(
            EVENT_PRINTER_PROFILE_UPDATED,
            json!({ "id": profile.id.clone(), "updated_by": caller.actor }),
        );
        Ok(profile)
    })();
    json::respond(result, ctx.is_debug())
}


pub(crate) async fn delete_printer_profile(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_MANAGE)?;
        let id = path.into_inner();
        let existing = state
            .printer_profiles
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Printer profile not found"))?;
        require_same_tenant(existing.tenant_id.as_deref(), &caller.tenant_id)?;
        let in_use = state
            .print_jobs
            .iter()
            .any(|entry| entry.value().printer_profile_id.as_deref() == Some(id.as_str()));
        if in_use {
            return Err(UnifiedError::conflict(
                "Printer profile is referenced by print jobs and cannot be deleted",
            ));
        }
        let removed = state.printer_profiles.remove(&id);
        if removed.is_none() {
            return Err(UnifiedError::not_found("Printer profile not found"));
        }
        state.emit(
            EVENT_PRINTER_PROFILE_DELETED,
            json!({ "id": id.clone(), "deleted_by": caller.actor }),
        );
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, ctx.is_debug())
}


pub(crate) async fn set_default_printer_profile(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PrinterProfile> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_MANAGE)?;
        let id = path.into_inner();
        let mut profile = state
            .printer_profiles
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Printer profile not found"))?;
        require_same_tenant(profile.tenant_id.as_deref(), &caller.tenant_id)?;
        // Clear the default flag on every other profile of the same
        // scope (same company and same tenant, or global when the company is NULL),
        // then mark this one default.
        let scope = profile.company_id.clone();
        let tenant_scope = profile.tenant_id.clone();
        for mut entry in state.printer_profiles.iter_mut() {
            if entry.company_id == scope && entry.tenant_id == tenant_scope && entry.id != id {
                entry.is_default = false;
                entry.updated_at = now();
            }
        }
        profile.is_default = true;
        profile.updated_at = now();
        state
            .printer_profiles
            .insert(id.clone(), profile.clone());
        state.emit(
            EVENT_PRINTER_PROFILE_UPDATED,
            json!({ "id": profile.id.clone(), "is_default": true, "updated_by": caller.actor }),
        );
        Ok(profile)
    })();
    json::respond(result, ctx.is_debug())
}
