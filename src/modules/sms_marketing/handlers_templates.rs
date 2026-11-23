//! HTTP handlers for plain-text SMS templates: bodies
//! with `{{variable}}` placeholders, character / segment
//! counting, and a render preview that reports missing
//! variables instead of silently blanking them.

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    caller, caller_company, created, err, load_template, ok, optional_text, paginate, record_audit,
    require_caller_company, require_manage, require_tenant, require_text, require_view,
    validate_template_body,
};
use super::state::SmsMarketingState;
use super::types::{
    CreateTemplateRequest, MAX_DESCRIPTION_LEN, MAX_NAME_LEN, PreviewTemplateRequest, SmsTemplate,
    UpdateTemplateRequest,
};

/// List templates (paginated, search by name/body).
pub(crate) async fn list_templates(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let params = query.into_inner();
        let search = params.search.clone().map(|needle| needle.to_lowercase());
        let mut items = state.templates_of(&tenant_id);
        if let Some(needle) = &search {
            items.retain(|template| {
                template.name.to_lowercase().contains(needle)
                    || template.body.to_lowercase().contains(needle)
            });
        }
        let views: Vec<Value> = items
            .iter()
            .map(|item| serde_json::to_value(item).unwrap_or(Value::Null))
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Create a template (manager). The variable list is
/// derived from the body at write time.
pub(crate) async fn create_template(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateTemplateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let body_text = validate_template_body(&input.body)?;
        let variables = super::types::template_variables(&body_text);
        let now = Utc::now();
        let template = SmsTemplate {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.clone(),
            name,
            description,
            body: body_text,
            variables,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.templates.insert(template.id, template.clone());
        record_audit(
            &state,
            &tenant_id,
            "template",
            &template.id.to_string(),
            "create",
            &caller.actor,
            None,
            Some(serde_json::to_value(&template).unwrap_or(Value::Null)),
        );
        Ok(serde_json::to_value(&template).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

/// Get one template.
pub(crate) async fn get_template(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let template = load_template(&state, &tenant_id, &path.into_inner())?;
        Ok(serde_json::to_value(&template).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Update a template (manager). Changing the body
/// re-derives the variable list; messages already sent keep
/// their rendered body.
pub(crate) async fn update_template(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<UpdateTemplateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut template = load_template(&state, &tenant_id, &path.into_inner())?;
        let before = serde_json::to_value(&template).unwrap_or(Value::Null);
        if let Some(name) = &input.name {
            template.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = &input.description {
            template.description = match description {
                Some(text) => Some(require_text(text, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(body_text) = &input.body {
            template.body = validate_template_body(body_text)?;
            template.variables = super::types::template_variables(&template.body);
        }
        template.updated_at = Utc::now();
        state.templates.insert(template.id, template.clone());
        record_audit(
            &state,
            &tenant_id,
            "template",
            &template.id.to_string(),
            "update",
            &caller.actor,
            Some(before),
            Some(serde_json::to_value(&template).unwrap_or(Value::Null)),
        );
        Ok(serde_json::to_value(&template).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Delete a template (manager). Refused while any campaign
/// references it; sent messages keep their rendered body.
pub(crate) async fn delete_template(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let template = load_template(&state, &tenant_id, &path.into_inner())?;
        let referenced = state.campaigns.iter().any(|entry| {
            entry.value().tenant_id == tenant_id && entry.value().template_id == template.id
        });
        if referenced {
            return Err(unified_core::error::UnifiedError::conflict(
                "Template is referenced by a campaign; delete or re-point the campaign first",
            ));
        }
        state.templates.remove(&template.id);
        record_audit(
            &state,
            &tenant_id,
            "template",
            &template.id.to_string(),
            "delete",
            &caller.actor,
            Some(serde_json::to_value(&template).unwrap_or(Value::Null)),
            None,
        );
        Ok(json!({ "deleted": true, "template_id": template.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Preview a template with caller-supplied variables: the
/// rendered body, the missing variable names (placeholders
/// left in place, never blanked), and the GSM-7 / UCS-2
/// segment count of the rendered text.
pub(crate) async fn preview_template(
    state: web::Data<SmsMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<PreviewTemplateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let template = load_template(&state, &tenant_id, &path.into_inner())?;
        let input = body.into_inner();
        let (rendered, missing) = super::types::render_template(&template.body, &input.variables);
        let info = super::types::segment_info(&rendered);
        Ok(json!({
            "template_id": template.id.to_string(),
            "rendered": rendered,
            "missing_variables": missing,
            "variables": template.variables,
            "encoding": info.encoding.as_str(),
            "units": info.units,
            "segments": info.segments,
            "per_segment": info.per_segment,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
