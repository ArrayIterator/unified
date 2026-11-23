// Properties — SVG / print handlers (authenticated).

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// SVG / print handlers (authenticated)
// ---------------------------------------------------------------------------

pub(crate) async fn get_plan_svg(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err(error, debug),
    };
    let caller_result = caller(&req);
    let caller = match caller_result {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW) {
        return err(error, debug);
    }
    let (property_id, plan_id) = path.into_inner();
    let loaded = (|| {
        let property = load_property(&state, &property_id)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        require_record_in_company_scope(&property.company_id, &_scope)?;
        Ok::<(Property, PropertyPlan), UnifiedError>((property, plan))
    })();
    let (property, plan) = match loaded {
        Ok(pair) => pair,
        Err(error) => return err(error, debug),
    };
    let blocks = state.blocks_of_plan(plan.id);
    match render_plan_svg(&plan, &blocks, &format!("{} — {}", property.title, plan.name)) {
        Ok(svg) => HttpResponse::Ok()
            .content_type("image/svg+xml; charset=utf-8")
            .body(svg),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_plan_print(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PropertiesState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err(error, debug),
    };
    let caller_result = caller(&req);
    let caller = match caller_result {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_PROPERTIES_VIEW) {
        return err(error, debug);
    }
    let (property_id, plan_id) = path.into_inner();
    let loaded = (|| {
        let property = load_property(&state, &property_id)?;
        let plan = load_plan(&state, property.id, &plan_id)?;
        require_record_in_company_scope(&property.company_id, &_scope)?;
        Ok::<(Property, PropertyPlan), UnifiedError>((property, plan))
    })();
    let (property, plan) = match loaded {
        Ok(pair) => pair,
        Err(error) => return err(error, debug),
    };
    let blocks = state.blocks_of_plan(plan.id);
    match render_plan_svg(&plan, &blocks, &format!("{} — {}", property.title, plan.name)) {
        Ok(svg) => {
            let html = render_print_html(&property, &plan, &blocks, &svg, true);
            HttpResponse::Ok()
                .content_type("text/html; charset=utf-8")
                .body(html)
        }
        Err(error) => err(error, debug),
    }
}
