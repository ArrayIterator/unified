//! Saas Packages - HTTP handlers for purchases, provisioning, and lifecycle.

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
use super::{constants::*, types::*, state::*, seed::*, auth::*, helpers::*, provisioning::*, handlers_packages::*, handlers_ai::*, events::*, module::*};

// HTTP handlers -- purchases, provisioning, lifecycle.

pub(crate) async fn purchase_package(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PurchaseRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PackagePurchase> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_PURCHASE)?;
        let package = load_package(&state, &path.into_inner())?;
        if package.status != STATUS_PUBLISHED {
            return Err(UnifiedError::conflict(format!(
                "Only a published package can be purchased (current status: {})",
                package.status
            )));
        }
        let input = body.into_inner();
        let buyer_id = match &input.buyer_id {
            Some(raw) => require_text(raw, "buyer_id", MAX_REF_LEN)?,
            None => caller.actor.clone(),
        };
        let company_id = parse_uuid(&input.company_id, "company")?;
        require_tenant_scope(&req, &company_id)?;
        let subdomain = match &input.subdomain {
            Some(raw) => {
                let label = require_text(raw, "subdomain", MAX_SUBDOMAIN_LEN)?.to_lowercase();
                if !valid_subdomain(&label) {
                    return Err(UnifiedError::bad_request(
                        "subdomain must be lowercase letters, digits, and single hyphens, without leading or trailing hyphens",
                    ));
                }
                Some(label)
            }
            None => None,
        };
        let order_id = optional_text(&input.order_id, "order_id", MAX_REF_LEN)?;
        let subscription_id = optional_text(&input.subscription_id, "subscription_id", MAX_REF_LEN)?;
        let now = Utc::now();
        let purchase = PackagePurchase {
            id: Uuid::now_v7(),
            package_id: package.id,
            package_version: package.current_version,
            buyer_id: buyer_id.clone(),
            company_id,
            status: PURCHASE_PENDING_PAYMENT.to_string(),
            subdomain,
            subdomain_pending: false,
            order_id: order_id.clone(),
            subscription_id: subscription_id.clone(),
            product_id: package.product_id.clone(),
            enabled_modules: Vec::new(),
            grace_ends_at: None,
            cancelled_at: None,
            cancel_reason: None,
            provision_attempts: 0,
            created_at: now,
            updated_at: now,
        };
        state.purchases.insert(purchase.id, purchase.clone());
        state.record_history(
            purchase.id,
            "purchase.requested",
            &caller.actor,
            json!({
                "package_id": package.id,
                "package_version": package.current_version,
                "company_id": company_id,
            }),
        );
        // The store owns checkout: this only requests the purchase.
        // Provisioning starts when the store reports the order paid.
        ctx.hooks.emit(
            EVENT_PURCHASE_REQUESTED,
            json!({
                "purchase_id": purchase.id,
                "package_id": package.id,
                "package_version": package.current_version,
                "product_id": package.product_id,
                "buyer_id": buyer_id,
                "company_id": company_id,
                "tenant_schema": tenant_schema(&company_id),
                "module": "saas_packages",
                "item_type": "saas_package",
                "order_id": order_id,
                "subscription_id": subscription_id,
            }),
        );
        Ok(purchase)
    })();
    match result {
        Ok(purchase) => created(purchase),
        Err(error) => err(error, debug),
    }
}

pub(crate) fn purchase_visible(ctx: &AppContext, caller: &Caller, purchase: &PackagePurchase) -> bool {
    if check_capability(ctx, &caller.role, PERM_SAAS_PACKAGES_MANAGE) {
        return true;
    }
    purchase.buyer_id == caller.actor
}

pub(crate) async fn list_purchases(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        let params = query.into_inner();
        let package_filter = params.filters.get("package_id").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        if let Some(company_id) = &company_filter {
            require_tenant_scope(&req, &parse_uuid(company_id, "company")?)?;
        }
        let buyer_filter = params.filters.get("buyer_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let is_manager = check_capability(&ctx, &caller.role, PERM_SAAS_PACKAGES_MANAGE);
        let mut items: Vec<PackagePurchase> = state
            .purchases
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|purchase| {
                if !is_manager && purchase.buyer_id != caller.actor {
                    return false;
                }
                if let Some(package_id) = &package_filter {
                    if purchase.package_id.to_string() != *package_id {
                        return false;
                    }
                }
                if let Some(company_id) = &company_filter {
                    if purchase.company_id.to_string() != *company_id {
                        return false;
                    }
                }
                if let Some(buyer_id) = &buyer_filter {
                    if &purchase.buyer_id != buyer_id {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if &purchase.status != status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_purchase(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        let purchase = load_purchase(&state, &path.into_inner())?;
        if !purchase_visible(&ctx, &caller, &purchase) {
            return Err(UnifiedError::not_found("Purchase not found"));
        }
        require_tenant_scope(&req, &purchase.company_id)?;
        let grace_expired = purchase
            .grace_ends_at
            .map(|ends| Utc::now() > ends)
            .unwrap_or(false);
        Ok(json!({
            "purchase": purchase,
            "grace_expired": grace_expired,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_tenant_purchases(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        let company_id = parse_uuid(&path.into_inner(), "company")?;
        require_tenant_scope(&req, &company_id)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let is_manager = check_capability(&ctx, &caller.role, PERM_SAAS_PACKAGES_MANAGE);
        let mut items: Vec<PackagePurchase> = state
            .purchases
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|purchase| {
                if purchase.company_id != company_id {
                    return false;
                }
                if !is_manager && purchase.buyer_id != caller.actor {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &purchase.status != status {
                        return false;
                    }
                }
                true
            })
            .collect();
        // A non-manager with no purchase of their own for this tenant
        // sees nothing (the filter above already enforces it); an
        // empty list is the answer, never another tenant's rows.
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_purchase_history(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        let purchase = load_purchase(&state, &path.into_inner())?;
        if !purchase_visible(&ctx, &caller, &purchase) {
            return Err(UnifiedError::not_found("Purchase not found"));
        }
        require_tenant_scope(&req, &purchase.company_id)?;
        let params = query.into_inner();
        let action_filter = params.filters.get("action").cloned();
        let mut items: Vec<PurchaseHistoryEntry> = state
            .history
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.purchase_id == purchase.id)
            .filter(|entry| {
                if let Some(action) = &action_filter {
                    entry.action == *action
                } else {
                    true
                }
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Statuses from which the manual `POST /purchases/{id}/provision`
/// endpoint may start a full provisioning run.
///
/// A purchase that has not been paid yet (`pending_payment`) is
/// deliberately excluded: provisioning starts only when the store
/// reports the order paid (`order.paid`), which runs provisioning
/// on the hook path. Letting the manual endpoint provision an
/// unpaid purchase would bypass the order.paid gate and provision
/// a purchase to active without payment. A purchase in
/// `provisioning` or `failed` has already been paid (the hook path
/// moved it there), so starting or re-running provisioning for it
/// through this endpoint is safe.
pub(crate) fn check_start_provisioning_status(status: &str) -> UnifiedResult<()> {
    match status {
        PURCHASE_PROVISIONING | PURCHASE_FAILED => Ok(()),
        PURCHASE_PENDING_PAYMENT => Err(UnifiedError::conflict(
            "The purchase has not been paid yet: provisioning starts when the store reports the order paid",
        )),
        PURCHASE_ACTIVE => Err(UnifiedError::conflict(
            "The purchase is already provisioned and active",
        )),
        other => Err(UnifiedError::conflict(format!(
            "A purchase in status \"{other}\" cannot be provisioned"
        ))),
    }
}

pub(crate) async fn start_provisioning(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProvisionSummary> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_PROVISION)?;
        let purchase = load_purchase(&state, &path.into_inner())?;
        require_tenant_scope(&req, &purchase.company_id)?;
        check_start_provisioning_status(&purchase.status)?;
        run_provisioning(&state, purchase.id, &caller.actor, ProvisionMode::Full, Some(&ctx))
    })();
    match result {
        Ok(summary) => ok(summary),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn retry_provisioning(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProvisionSummary> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_PROVISION)?;
        let purchase = load_purchase(&state, &path.into_inner())?;
        require_tenant_scope(&req, &purchase.company_id)?;
        match purchase.status.as_str() {
            PURCHASE_PROVISIONING | PURCHASE_FAILED => {}
            PURCHASE_ACTIVE => {
                return Err(UnifiedError::conflict(
                    "The purchase is already provisioned and active; nothing to retry",
                ));
            }
            other => {
                return Err(UnifiedError::conflict(format!(
                    "A purchase in status \"{other}\" has no failed provisioning steps to retry"
                )));
            }
        }
        run_provisioning(&state, purchase.id, &caller.actor, ProvisionMode::RetryFailed, Some(&ctx))
    })();
    match result {
        Ok(summary) => ok(summary),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_provisioning(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProvisionSummary> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        let purchase = load_purchase(&state, &path.into_inner())?;
        require_tenant_scope(&req, &purchase.company_id)?;
        if !purchase_visible(&ctx, &caller, &purchase) {
            return Err(UnifiedError::not_found("Purchase not found"));
        }
        Ok(build_summary(&state, &purchase))
    })();
    match result {
        Ok(summary) => ok(summary),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_provision_steps(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        let purchase = load_purchase(&state, &path.into_inner())?;
        require_tenant_scope(&req, &purchase.company_id)?;
        if !purchase_visible(&ctx, &caller, &purchase) {
            return Err(UnifiedError::not_found("Purchase not found"));
        }
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let step_filter = params.filters.get("step").cloned();
        let mut items = steps_for_purchase(&state, purchase.id);
        if let Some(status) = &status_filter {
            items.retain(|step| &step.status == status);
        }
        if let Some(step_name) = &step_filter {
            items.retain(|step| &step.step == step_name);
        }
        items.reverse();
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_provision_step(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProvisionStep> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_VIEW)?;
        let id = parse_uuid(&path.into_inner(), "provision step")?;
        let step = state
            .steps
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Provision step not found"))?;
        let purchase = load_purchase(&state, &step.purchase_id.to_string())?;
        require_tenant_scope(&req, &purchase.company_id)?;
        if !purchase_visible(&ctx, &caller, &purchase) {
            return Err(UnifiedError::not_found("Provision step not found"));
        }
        Ok(step)
    })();
    match result {
        Ok(step) => ok(step),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn cancel_purchase(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CancelPurchaseRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PackagePurchase> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_MANAGE)?;
        let mut purchase = load_purchase(&state, &path.into_inner())?;
        require_tenant_scope(&req, &purchase.company_id)?;
        match purchase.status.as_str() {
            PURCHASE_ACTIVE | PURCHASE_PROVISIONING | PURCHASE_PENDING_PAYMENT | PURCHASE_SUSPENDED => {}
            PURCHASE_GRACE_PERIOD => {
                return Err(UnifiedError::conflict(
                    "The purchase is already in its cancellation grace period",
                ));
            }
            other => {
                return Err(UnifiedError::conflict(format!(
                    "A purchase in status \"{other}\" cannot be cancelled"
                )));
            }
        }
        let input = body.into_inner();
        let reason = optional_text(&input.reason, "reason", MAX_TEXT_LEN)?;
        let settings = state.settings();
        let now = Utc::now();
        purchase.status = if settings.grace_period_days > 0 {
            PURCHASE_GRACE_PERIOD.to_string()
        } else {
            PURCHASE_CANCELLED.to_string()
        };
        purchase.grace_ends_at = if settings.grace_period_days > 0 {
            Some(now + chrono::Duration::days(settings.grace_period_days))
        } else {
            None
        };
        purchase.cancelled_at = Some(now);
        purchase.cancel_reason = reason.clone();
        purchase.updated_at = now;
        state.purchases.insert(purchase.id, purchase.clone());
        state.record_history(
            purchase.id,
            "purchase.cancelled",
            &caller.actor,
            json!({
                "reason": reason.clone(),
                "grace_ends_at": purchase.grace_ends_at,
            }),
        );
        ctx.hooks.emit(
            EVENT_CANCELLED,
            json!({
                "purchase_id": purchase.id,
                "package_id": purchase.package_id,
                "company_id": purchase.company_id,
                "actor_id": caller.actor,
                "reason": reason.clone(),
                "grace_ends_at": purchase.grace_ends_at,
            }),
        );
        // The billing module owns the subscription itself: ask it to
        // cancel the backing subscription (handoff, events only).
        if let Some(subscription_id) = &purchase.subscription_id {
            ctx.hooks.emit(
                "billing.subscription.cancel_requested",
                json!({
                    "subscription_id": subscription_id,
                    "purchase_id": purchase.id,
                    "company_id": purchase.company_id,
                    "actor_id": caller.actor,
                    "reason": reason,
                }),
            );
        }
        Ok(purchase)
    })();
    match result {
        Ok(purchase) => ok(purchase),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn renew_purchase(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PackagePurchase> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SAAS_PACKAGES_PURCHASE)?;
        let purchase = load_purchase(&state, &path.into_inner())?;
        require_tenant_scope(&req, &purchase.company_id)?;
        if !purchase_visible(&ctx, &caller, &purchase)
            && !check_capability(&ctx, &caller.role, PERM_SAAS_PACKAGES_MANAGE)
        {
            return Err(UnifiedError::not_found("Purchase not found"));
        }
        match purchase.status.as_str() {
            PURCHASE_ACTIVE | PURCHASE_GRACE_PERIOD | PURCHASE_EXPIRED => {}
            other => {
                return Err(UnifiedError::conflict(format!(
                    "A purchase in status \"{other}\" cannot be renewed"
                )));
            }
        }
        state.record_history(
            purchase.id,
            "purchase.renewal_requested",
            &caller.actor,
            json!({ "subscription_id": purchase.subscription_id }),
        );
        ctx.hooks.emit(
            EVENT_RENEWAL_REQUESTED,
            json!({
                "purchase_id": purchase.id,
                "package_id": purchase.package_id,
                "company_id": purchase.company_id,
                "actor_id": caller.actor,
                "subscription_id": purchase.subscription_id,
            }),
        );
        if let Some(subscription_id) = &purchase.subscription_id {
            ctx.hooks.emit(
                "billing.subscription.renew_requested",
                json!({
                    "subscription_id": subscription_id,
                    "purchase_id": purchase.id,
                    "company_id": purchase.company_id,
                    "actor_id": caller.actor,
                }),
            );
        }
        Ok(purchase)
    })();
    match result {
        Ok(purchase) => ok(purchase),
        Err(error) => err(error, debug),
    }
}

/// Shared upgrade/downgrade implementation: move a purchase to
/// another published package, enabling new modules and reporting
/// dropped modules as kept unless the caller explicitly confirms
/// disabling them. A module the tenant uses outside this package
/// (shared with another purchase) is NEVER disabled here.
pub(crate) fn change_package(
    ctx: &AppContext,
    state: &SaasPackagesState,
    req: &HttpRequest,
    caller: &Caller,
    purchase_id: &str,
    input: ChangePackageRequest,
    upgrade: bool,
) -> UnifiedResult<Value> {
    require_permission(ctx, &caller.role, PERM_SAAS_PACKAGES_MANAGE)?;
    let mut purchase = load_purchase(state, purchase_id)?;
    require_tenant_scope(req, &purchase.company_id)?;
    if purchase.status != PURCHASE_ACTIVE && purchase.status != PURCHASE_GRACE_PERIOD {
        return Err(UnifiedError::conflict(format!(
            "Only an active purchase can change package (current status: {})",
            purchase.status
        )));
    }
    let target_id = parse_uuid(&input.target_package_id, "target package")?;
    let target = state
        .packages
        .get(&target_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| {
            UnifiedError::not_found(format!(
                "Target package \"{}\" not found",
                input.target_package_id
            ))
        })?;
    if target.status != STATUS_PUBLISHED {
        return Err(UnifiedError::conflict(format!(
            "Only a published package can be moved to (target status: {})",
            target.status
        )));
    }
    if target.id == purchase.package_id {
        return Err(UnifiedError::conflict(
            "The purchase already uses this package",
        ));
    }
    let current_version = purchase_version(state, &purchase)?;
    let mut diff = module_diff(&current_version.modules, &target.modules);
    diff.from_package_id = purchase.package_id;
    diff.to_package_id = target.id;

    // Enable the new modules first (entitlement blocks the move
    // with a 403-style error naming the module -- never silently
    // skipped). Already-enabled modules are reported, not touched.
    let mut enabled: Vec<String> = Vec::new();
    let mut already_enabled: Vec<String> = Vec::new();
    for module_id in &diff.to_enable {
        if state.tenant_has_module(&purchase.company_id, module_id) {
            already_enabled.push(module_id.clone());
            continue;
        }
        if ctx.modules.get(module_id).is_none() {
            return Err(UnifiedError::not_found(format!(
                "Module \"{module_id}\" not found"
            )));
        }
        match entitlement_available_http(ctx, &purchase.company_id, module_id) {
            Ok(true) => {}
            Ok(false) => {
                return Err(UnifiedError::forbidden(format!(
                    "Tenant \"{}\" is not entitled to module \"{module_id}\": package change blocked (the module requires an active license entitlement)",
                    purchase.company_id
                )));
            }
            Err(error) => return Err(error),
        }
        state.mark_tenant_module(&purchase.company_id, module_id, purchase.id);
        ctx.hooks.emit(
            events::MODULE_ENABLED,
            json!({
                "id": module_id,
                "company_id": purchase.company_id,
                "package_id": target.id,
                "purchase_id": purchase.id,
                "actor_id": caller.actor,
            }),
        );
        ctx.hooks.emit(
            EVENT_MODULE_ENABLED,
            json!({
                "package_id": target.id,
                "purchase_id": purchase.id,
                "company_id": purchase.company_id,
                "module_id": module_id,
                "actor_id": caller.actor,
                "auto_activated": [],
            }),
        );
        enabled.push(module_id.clone());
    }

    // Dropped modules: shared ones are always kept; the rest are
    // kept too unless the caller explicitly confirmed disabling
    // them by listing them with `confirm_disable`.
    let mut disabled: Vec<String> = Vec::new();
    let mut kept: Vec<String> = Vec::new();
    let mut kept_shared: Vec<String> = Vec::new();
    for module_id in &diff.to_disable {
        let shared_owners = state.other_module_owners(&purchase.company_id, module_id, purchase.id);
        if !shared_owners.is_empty() {
            kept.push(module_id.clone());
            kept_shared.push(module_id.clone());
            continue;
        }
        if input.confirm_disable && input.modules_to_disable.contains(module_id) {
            state.unmark_tenant_module(&purchase.company_id, module_id, purchase.id);
            ctx.hooks.emit(
                events::MODULE_DISABLED,
                json!({
                    "id": module_id,
                    "company_id": purchase.company_id,
                    "package_id": purchase.package_id,
                    "purchase_id": purchase.id,
                    "actor_id": caller.actor,
                }),
            );
            ctx.hooks.emit(
                EVENT_MODULE_DISABLED,
                json!({
                    "package_id": purchase.package_id,
                    "purchase_id": purchase.id,
                    "company_id": purchase.company_id,
                    "module_id": module_id,
                    "actor_id": caller.actor,
                }),
            );
            disabled.push(module_id.clone());
        } else {
            kept.push(module_id.clone());
        }
    }
    diff.kept_shared = kept_shared;

    let from_package_id = purchase.package_id;
    let from_version = purchase.package_version;
    purchase.package_id = target.id;
    purchase.package_version = target.current_version;
    purchase.product_id = target.product_id.clone();
    purchase.enabled_modules = target
        .modules
        .iter()
        .map(|entry| entry.module_id.clone())
        .filter(|module_id| {
            state.tenant_has_module(&purchase.company_id, module_id)
                || enabled.contains(module_id)
        })
        .collect();
    purchase.updated_at = now();
    state.purchases.insert(purchase.id, purchase.clone());
    let action = if upgrade { "purchase.upgraded" } else { "purchase.downgraded" };
    let event = if upgrade { EVENT_UPGRADED } else { EVENT_DOWNGRADED };
    state.record_history(
        purchase.id,
        action,
        &caller.actor,
        json!({
            "from_package_id": from_package_id,
            "from_package_version": from_version,
            "to_package_id": target.id,
            "to_package_version": target.current_version,
            "enabled": enabled,
            "already_enabled": already_enabled,
            "disabled": disabled,
            "kept": kept,
        }),
    );
    emit_audit(
        ctx,
        event,
        &caller.actor,
        action,
        "saas_package_purchase",
        &purchase.id.to_string(),
        Some(purchase.company_id),
        json!({
            "from_package_id": from_package_id,
            "to_package_id": target.id,
            "enabled": enabled,
            "disabled": disabled,
            "kept": kept,
        }),
    );
    Ok(json!({
        "purchase": purchase,
        "diff": diff,
        "enabled": enabled,
        "already_enabled": already_enabled,
        "disabled": disabled,
        "kept": kept,
    }))
}

pub(crate) async fn upgrade_purchase(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ChangePackageRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| {
        let caller = caller(&req)?;
        change_package(&ctx, &state, &req, &caller, &path.into_inner(), body.into_inner(), true)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn downgrade_purchase(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SaasPackagesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ChangePackageRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| {
        let caller = caller(&req)?;
        change_package(&ctx, &state, &req, &caller, &path.into_inner(), body.into_inner(), false)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_provisioning_gate_rejects_unpaid_purchases() {
        // The order.paid gate: an unpaid purchase must never be
        // provisioned through the manual endpoint.
        let error = check_start_provisioning_status(PURCHASE_PENDING_PAYMENT)
            .expect_err("pending_payment must be rejected");
        assert_eq!(error.code, 409);
        assert!(
            error.message.contains("not been paid"),
            "error must say the purchase has not been paid: {}",
            error.message
        );
    }

    #[test]
    fn start_provisioning_gate_allows_paid_but_unfinished_purchases() {
        // provisioning / failed can only have been reached through
        // the paid hook path, so starting (or re-running) is safe.
        assert!(check_start_provisioning_status(PURCHASE_PROVISIONING).is_ok());
        assert!(check_start_provisioning_status(PURCHASE_FAILED).is_ok());
    }

    #[test]
    fn start_provisioning_gate_rejects_other_statuses() {
        assert!(check_start_provisioning_status(PURCHASE_ACTIVE).is_err());
        assert!(check_start_provisioning_status(PURCHASE_CANCELLED).is_err());
        assert!(check_start_provisioning_status(PURCHASE_SUSPENDED).is_err());
        assert!(check_start_provisioning_status(PURCHASE_GRACE_PERIOD).is_err());
        assert!(check_start_provisioning_status(PURCHASE_EXPIRED).is_err());
    }
}
