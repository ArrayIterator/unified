//! Saas Packages - HTTP handlers for the AI surface and settings.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json;
use unified_core::middleware::TenantContext;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::tenancy as tenancy;
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai::{register_ai_capabilities, run_ai_capability, AiCapability};
use crate::modules::licensing::check_module_entitlement;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::{constants::*, types::*, state::*, seed::*, auth::*, helpers::*, provisioning::*, handlers_packages::*, handlers_purchases::*, events::*, module::*};

// ---------------------------------------------------------------------------
// HTTP handlers -- AI and settings.
// ---------------------------------------------------------------------------

pub(crate) async fn ai_recommend_package(
    ctx: web::Data<Arc<AppContext>>,
    _state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    body: web::Json<AiRecommendRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW) {
        return err(error, debug);
    }
    let input = body.into_inner();
    let company_id = match &input.company_id {
        Some(raw) => match parse_uuid(raw, "company") {
            Ok(id) => Some(id),
            Err(error) => return err(error, debug),
        },
        None => None,
    };
    if let Some(company_id) = &company_id {
        if let Err(error) = require_tenant_scope(&req, company_id) {
            return err(error, debug);
        }
    }
    // Grounding context for the AI call is contributed by this
    // module's own hook handler; the helper runs the AI gate.
    // Recommendations are suggestions only: nothing is purchased
    // or provisioned from here.
    let ai_input = json!({
        "business_type": input.business_type,
        "needs": input.needs.unwrap_or_default(),
    });
    // The AI module's Caller is the type `run_ai_capability`
    // takes; it carries the same actor and role as this module's
    // local caller.
    let ai_caller = crate::modules::ai::Caller {
        actor: caller.actor.clone(),
        role: caller.role.clone(),
    };
    match run_ai_capability(&ctx, &ai_caller, company_id, "saas_packages", "recommend_package", ai_input).await {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn ai_describe_package(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    body: web::Json<AiDescribeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW) {
        return err(error, debug);
    }
    let input = body.into_inner();
    let company_id = match &input.company_id {
        Some(raw) => match parse_uuid(raw, "company") {
            Ok(id) => Some(id),
            Err(error) => return err(error, debug),
        },
        None => None,
    };
    if let Some(company_id) = &company_id {
        if let Err(error) = require_tenant_scope(&req, company_id) {
            return err(error, debug);
        }
    }
    // The package must exist before the AI is asked to describe it;
    // the description itself comes from the AI helper.
    if let Err(error) = load_package(&state, &input.package_id) {
        return err(error, debug);
    }
    let ai_input = json!({ "package_id": input.package_id });
    // The AI module's Caller is the type `run_ai_capability`
    // takes; it carries the same actor and role as this module's
    // local caller.
    let ai_caller = crate::modules::ai::Caller {
        actor: caller.actor.clone(),
        role: caller.role.clone(),
    };
    match run_ai_capability(&ctx, &ai_caller, company_id, "saas_packages", "describe_package", ai_input).await {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_ai_capabilities(
    ctx: web::Data<Arc<AppContext>>,
    _state: web::Data<SaasPackagesState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        // Built from the capability fields directly, so this
        // endpoint does not depend on the AI struct's Serialize.
        let capabilities: Vec<Value> = AI_CAPABILITIES
            .iter()
            .map(|capability| {
                json!({
                    "id": capability.id,
                    "module_id": capability.module_id,
                    "name": capability.name,
                    "description": capability.description,
                    "input_fields": capability.input_fields,
                    "allows_pii": capability.allows_pii,
                    "use_case": capability.use_case,
                })
            })
            .collect();
        Ok(json!({ "capabilities": capabilities }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SaasPackagesSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        require_control_plane(&req)?;
        Ok(state.settings())
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    body: web::Json<SettingsUpdateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SaasPackagesSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_MANAGE)?;
        require_control_plane(&req)?;
        let input = body.into_inner();
        let mut settings = state.settings();
        if let Some(value) = input.grace_period_days {
            settings.grace_period_days = value;
        }
        if let Some(value) = input.max_modules_per_package {
            settings.max_modules_per_package = value;
        }
        if let Some(value) = input.max_wizard_steps {
            settings.max_wizard_steps = value;
        }
        if let Some(value) = input.provisioning_max_attempts {
            settings.provisioning_max_attempts = value;
        }
        if let Some(value) = input.subdomain_base_domain {
            settings.subdomain_base_domain = value.trim().to_string();
        }
        if let Some(value) = input.vendor_packages_enabled {
            settings.vendor_packages_enabled = value;
        }
        if let Some(value) = input.wizard_step_names {
            settings.wizard_step_names = value;
        }
        settings.validate()?;
        *state
            .settings
            .write()
            .expect("settings lock poisoned") = settings.clone();
        emit_audit(
            &ctx,
            "saas_packages.settings.updated",
            &caller.actor,
            "saas_packages.settings.updated",
            "saas_packages_settings",
            "settings",
            None,
            json!({
                "grace_period_days": settings.grace_period_days,
                "max_modules_per_package": settings.max_modules_per_package,
            }),
        );
        Ok(settings)
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}
