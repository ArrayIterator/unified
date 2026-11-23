//! printing - handlers paper.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde_json::{json, Value};
use std::sync::{Arc};
use super::{events::*, paper::*, thermal::*, profiles::*, jobs::*, state::*, auth::*, hooks::*, handlers_profiles::*, handlers_jobs::*, module::*};



// ---------------------------------------------------------------------------
// Paper-size handlers
// ---------------------------------------------------------------------------
pub(crate) async fn list_paper_sizes(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<PaperSize>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<PaperSize> = state
            .paper_sizes
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|size| {
                tenant_visible(size.tenant_id.as_deref(), &caller.tenant_id)
                    && matches_search(&format!("{} {}", size.code, size.name), &params)
                    && params
                        .filters
                        .get("is_standard")
                        .is_none_or(|flag| size.is_standard.to_string() == *flag)
                    && params
                        .filters
                        .get("is_thermal")
                        .is_none_or(|flag| size.is_thermal.to_string() == *flag)
                    && params.filters.get("company_id").is_none_or(|company_id| {
                        size.company_id.as_deref() == Some(company_id.as_str())
                    })
            })
            .collect();
        items.sort_by(|a, b| a.code.cmp(&b.code));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, ctx.is_debug())
}


pub(crate) async fn create_paper_size(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreatePaperSize>,
) -> HttpResponse {
    let result: UnifiedResult<PaperSize> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_MANAGE)?;
        let input = body.into_inner();
        let code = input.code.trim().to_lowercase();
        if !valid_paper_size_code(&code) {
            return Err(UnifiedError::bad_request(
                "Paper size code must be a lowercase slug (letters, digits, - and _)",
            ));
        }
        if is_standard_paper_size(&code) {
            return Err(UnifiedError::conflict(format!(
                "Paper size \"{code}\" is a standard size and cannot be created as a custom size"
            )));
        }
        if state.paper_sizes.contains_key(&code) {
            return Err(UnifiedError::conflict(format!(
                "Paper size \"{code}\" already exists"
            )));
        }
        if input.name.trim().is_empty() || input.name.len() > 120 {
            return Err(UnifiedError::bad_request(
                "Paper size name must be 1 to 120 characters",
            ));
        }
        let margins = (
            input.margin_top_mm.unwrap_or(10.0),
            input.margin_bottom_mm.unwrap_or(10.0),
            input.margin_left_mm.unwrap_or(10.0),
            input.margin_right_mm.unwrap_or(10.0),
        );
        validate_paper_size_dimensions(
            input.width_mm,
            input.height_mm,
            margins.0,
            margins.1,
            margins.2,
            margins.3,
            input.is_thermal,
            input.thermal_width_mm,
        )?;
        let now = now();
        let size = PaperSize {
            code: code.clone(),
            name: input.name.trim().to_string(),
            width_mm: input.width_mm,
            height_mm: input.height_mm,
            margin_top_mm: margins.0,
            margin_bottom_mm: margins.1,
            margin_left_mm: margins.2,
            margin_right_mm: margins.3,
            is_standard: false,
            is_thermal: input.is_thermal,
            thermal_width_mm: input.thermal_width_mm,
            company_id: input.company_id,
            tenant_id: Some(caller.tenant_id.clone()),
            created_by: Some(caller.actor.clone()),
            created_at: now,
            updated_at: now,
        };
        state.paper_sizes.insert(code, size.clone());
        state.emit(
            EVENT_PAPER_SIZE_CREATED,
            json!({ "code": size.code.clone(), "company_id": size.company_id.clone(), "created_by": caller.actor }),
        );
        Ok(size)
    })();
    match result {
        Ok(size) => json::success_response(size, 201),
        Err(error) => json::error_response(&error, ctx.is_debug()),
    }
}


pub(crate) async fn get_paper_size(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PaperSize> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_VIEW)?;
        let size = state
            .paper_size(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Paper size not found"))?;
        // Standard rows are global; custom rows belong to one tenant.
        require_same_tenant(size.tenant_id.as_deref(), &caller.tenant_id)?;
        Ok(size)
    })();
    json::respond(result, ctx.is_debug())
}


pub(crate) async fn update_paper_size(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePaperSize>,
) -> HttpResponse {
    let result: UnifiedResult<PaperSize> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_MANAGE)?;
        let code = path.into_inner();
        if is_standard_paper_size(&code) {
            return Err(UnifiedError::forbidden(
                "Standard paper sizes cannot be edited; create a custom size instead",
            ));
        }
        let mut size = state
            .paper_sizes
            .get(&code)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Paper size not found"))?;
        require_same_tenant(size.tenant_id.as_deref(), &caller.tenant_id)?;
        let input = body.into_inner();
        if let Some(name) = input.name {
            if name.trim().is_empty() || name.len() > 120 {
                return Err(UnifiedError::bad_request(
                    "Paper size name must be 1 to 120 characters",
                ));
            }
            size.name = name.trim().to_string();
        }
        if let Some(width) = input.width_mm {
            size.width_mm = width;
        }
        if let Some(height) = input.height_mm {
            size.height_mm = height;
        }
        if let Some(margin) = input.margin_top_mm {
            size.margin_top_mm = margin;
        }
        if let Some(margin) = input.margin_bottom_mm {
            size.margin_bottom_mm = margin;
        }
        if let Some(margin) = input.margin_left_mm {
            size.margin_left_mm = margin;
        }
        if let Some(margin) = input.margin_right_mm {
            size.margin_right_mm = margin;
        }
        if let Some(is_thermal) = input.is_thermal {
            size.is_thermal = is_thermal;
        }
        if let Some(thermal_width) = input.thermal_width_mm {
            size.thermal_width_mm = thermal_width;
        }
        if let Some(company_id) = input.company_id {
            size.company_id = company_id;
        }
        validate_paper_size_dimensions(
            size.width_mm,
            size.height_mm,
            size.margin_top_mm,
            size.margin_bottom_mm,
            size.margin_left_mm,
            size.margin_right_mm,
            size.is_thermal,
            size.thermal_width_mm,
        )?;
        size.updated_at = now();
        state.paper_sizes.insert(code.clone(), size.clone());
        state.emit(
            EVENT_PAPER_SIZE_UPDATED,
            json!({ "code": size.code.clone(), "updated_by": caller.actor }),
        );
        Ok(size)
    })();
    json::respond(result, ctx.is_debug())
}


pub(crate) async fn delete_paper_size(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_MANAGE)?;
        let code = path.into_inner();
        if is_standard_paper_size(&code) {
            return Err(UnifiedError::forbidden(
                "Standard paper sizes cannot be deleted",
            ));
        }
        let existing = state
            .paper_sizes
            .get(&code)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Paper size not found"))?;
        require_same_tenant(existing.tenant_id.as_deref(), &caller.tenant_id)?;
        let removed = state.paper_sizes.remove(&code);
        if removed.is_none() {
            return Err(UnifiedError::not_found("Paper size not found"));
        }
        state.emit(
            EVENT_PAPER_SIZE_DELETED,
            json!({ "code": code.clone(), "deleted_by": caller.actor }),
        );
        Ok(json!({ "deleted": true, "code": code }))
    })();
    json::respond(result, ctx.is_debug())
}


pub(crate) async fn get_default_paper_size(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_VIEW)?;
        let company_id = path.into_inner();
        // Defaults are per tenant: the key carries the tenant so
        // one tenant cannot read another's default.
        let key = format!("{}:{company_id}", caller.tenant_id);
        let code = state
            .company_default_paper_size
            .get(&key)
            .map(|entry| entry.value().clone())
            .unwrap_or_else(|| "a4".to_string());
        let size = state
            .paper_size(&code)
            .ok_or_else(|| UnifiedError::not_found("Default paper size not found"))?;
        Ok(json!({ "company_id": company_id, "paper_size_code": code, "paper_size": size }))
    })();
    json::respond(result, ctx.is_debug())
}


pub(crate) async fn set_default_paper_size(
    state: web::Data<Arc<PrintingState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<Value>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRINTING_MANAGE)?;
        let company_id = path.into_inner();
        if company_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("company_id must not be empty"));
        }
        let code = body
            .get("paper_size_code")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .trim()
            .to_lowercase();
        let size = state
            .paper_size(&code)
            .ok_or_else(|| UnifiedError::bad_request("Unknown paper size"))?;
        state
            .company_default_paper_size
            .insert(format!("{}:{company_id}", caller.tenant_id), code.clone());
        Ok(json!({ "company_id": company_id, "paper_size_code": code, "paper_size": size, "set_by": caller.actor }))
    })();
    json::respond(result, ctx.is_debug())
}
