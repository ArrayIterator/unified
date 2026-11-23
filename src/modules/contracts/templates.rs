// Contracts — Template handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use rand::{distr::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Digest;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Template handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_templates(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_VIEW)?;
        let params = query.into_inner();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Value> = state
            .templates
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|template| {
                if search.is_empty() {
                    return true;
                }
                let haystack = format!(
                    "{} {}",
                    template.name,
                    template.description.clone().unwrap_or_default()
                )
                .to_lowercase();
                haystack.contains(&search)
            })
            .map(|template| json!(template))
            .collect();
        let sort = params.allowed_sort(&["name", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "name" => a
                    .get("name")
                    .and_then(Value::as_str)
                    .cmp(&b.get("name").and_then(Value::as_str)),
                _ => a
                    .get("created_at")
                    .and_then(Value::as_str)
                    .cmp(&b.get("created_at").and_then(Value::as_str)),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    body: web::Json<CreateTemplateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ContractTemplate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_TERMS_LEN)?),
            None => None,
        };
        let body_text = input.body.trim().to_string();
        if body_text.is_empty() {
            return Err(UnifiedError::bad_request("body must not be empty"));
        }
        validate_template_body(&body_text, &input.placeholders)?;
        let now = Utc::now();
        let template = ContractTemplate {
            id: Uuid::now_v7(),
            name,
            description,
            body: body_text,
            placeholders: input.placeholders,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.templates.insert(template.id, template.clone());
        state.emit(
            "contracts.template.created",
            json!({
                "template_id": template.id.to_string(),
                "name": template.name,
                "placeholders": template.placeholders,
                "created_by": caller.actor,
            }),
        );
        Ok(template)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ContractTemplate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_VIEW)?;
        load_template(&state, &path.into_inner())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateTemplateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ContractTemplate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let mut template = load_template(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(name) = &input.name {
            template.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = &input.description {
            template.description = match description {
                Some(raw) => Some(require_text(raw, "description", MAX_TERMS_LEN)?),
                None => None,
            };
        }
        // Body and placeholder list are validated together so a
        // body can never reference an undeclared placeholder.
        let new_body = match &input.body {
            Some(raw) => {
                let trimmed = raw.trim().to_string();
                if trimmed.is_empty() {
                    return Err(UnifiedError::bad_request("body must not be empty"));
                }
                trimmed
            }
            None => template.body.clone(),
        };
        let new_placeholders = match &input.placeholders {
            Some(list) => list.clone(),
            None => template.placeholders.clone(),
        };
        validate_template_body(&new_body, &new_placeholders)?;
        template.body = new_body;
        template.placeholders = new_placeholders;
        template.updated_at = Utc::now();
        state.templates.insert(template.id, template.clone());
        state.emit(
            "contracts.template.updated",
            json!({
                "template_id": template.id.to_string(),
                "name": template.name,
                "updated_by": caller.actor,
            }),
        );
        Ok(template)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_MANAGE)?;
        let template = load_template(&state, &path.into_inner())?;
        if state
            .contracts
            .iter()
            .any(|entry| entry.value().template_id == Some(template.id))
        {
            return Err(UnifiedError::conflict(
                "Template is used by at least one contract and cannot be deleted",
            ));
        }
        state.templates.remove(&template.id);
        state.emit(
            "contracts.template.deleted",
            json!({
                "template_id": template.id.to_string(),
                "name": template.name,
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "template_id": template.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn render_template_handler(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ContractsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RenderTemplateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CONTRACTS_VIEW)?;
        let template = load_template(&state, &path.into_inner())?;
        let input = body.into_inner();
        for (key, value) in &input.values {
            if value.len() > MAX_TERMS_LEN {
                return Err(UnifiedError::bad_request(format!(
                    "Value for placeholder \"{key}\" is too long"
                )));
            }
        }
        let rendered = render_template(&template.body, &template.placeholders, &input.values)?;
        Ok(json!({
            "template_id": template.id.to_string(),
            "rendered": rendered,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
