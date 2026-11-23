//! HTTP handlers for reporting frameworks: admin-entered
//! framework definitions (a standard a tenant reports
//! against, with its own version). Frameworks are data, never
//! a hard-coded list.

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    caller, created, err, load_framework, ok, paginate, require_manage, require_view,
    require_tenant, resolve_tenant, validate_framework_input,
};
use super::state::EsgState;
use super::types::{
    CreateFrameworkRequest, EVENT_METRIC_DEFINED, Framework, UpdateFrameworkRequest,
};

/// List frameworks (paginated, search by name/description).
pub(crate) async fn list_frameworks(
    state: web::Data<EsgState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let params = query.into_inner();
        let search = params.search.clone().map(|needle| needle.to_lowercase());
        let mut items = state.frameworks_of(&tenant_id);
        items.retain(|framework| {
            if let Some(needle) = &search {
                if !framework.name.to_lowercase().contains(needle)
                    && !framework
                        .description
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(needle)
                {
                    return false;
                }
            }
            true
        });
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

/// Create a framework definition (manager).
pub(crate) async fn create_framework(
    state: web::Data<EsgState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateFrameworkRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let (name, description, version) =
            validate_framework_input(&input.name, &input.description, &input.version)?;
        let now = Utc::now();
        let framework = Framework {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.clone(),
            name,
            description,
            version,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.frameworks.insert(framework.id, framework.clone());
        state.emit(
            EVENT_METRIC_DEFINED,
            json!({
                "kind": "framework",
                "tenant_id": tenant_id,
                "framework_id": framework.id.to_string(),
                "name": framework.name,
                "created_by": caller.actor,
            }),
        );
        Ok(serde_json::to_value(&framework).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

/// Get one framework.
pub(crate) async fn get_framework(
    state: web::Data<EsgState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let framework = load_framework(&state, &tenant_id, &path.into_inner())?;
        Ok(serde_json::to_value(&framework).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Update a framework definition (manager). Metrics that
/// tagged the framework keep their tag: framework ids are
/// references, and deleting or renaming a framework never
/// deletes the metrics that tagged it.
pub(crate) async fn update_framework(
    state: web::Data<EsgState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<UpdateFrameworkRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut framework = load_framework(&state, &tenant_id, &path.into_inner())?;
        if let Some(name) = &input.name {
            framework.name =
                super::helpers::require_text(name, "name", super::types::MAX_NAME_LEN)?;
        }
        if let Some(description) = &input.description {
            framework.description = match description {
                Some(text) => Some(super::helpers::require_text(
                    text,
                    "description",
                    super::types::MAX_DESCRIPTION_LEN,
                )?),
                None => None,
            };
        }
        if let Some(version) = &input.version {
            framework.version = match version {
                Some(text) => Some(super::helpers::require_text(
                    text,
                    "version",
                    super::types::MAX_CODE_LEN,
                )?),
                None => None,
            };
        }
        framework.updated_at = Utc::now();
        state.frameworks.insert(framework.id, framework.clone());
        Ok(serde_json::to_value(&framework).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Delete a framework definition (manager). Metrics that
/// tagged it keep their tag strings; the tag simply names a
/// framework that no longer exists, and the metric list
/// filters still work.
pub(crate) async fn delete_framework(
    state: web::Data<EsgState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let tenant_id = resolve_tenant(&req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let framework = load_framework(&state, &tenant_id, &path.into_inner())?;
        state.frameworks.remove(&framework.id);
        Ok(json!({ "deleted": true, "framework_id": framework.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
