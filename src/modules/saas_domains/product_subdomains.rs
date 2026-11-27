//! SaaS Domains - product-subdomain helpers and handlers.
//!
//! Part of the `saas_domains` module split; see `src/modules/saas_domains.rs`.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::error::UnifiedResult;
use unified_core::json;
use unified_core::pagination::PaginationParams;
use crate::modules::company::company_exists;
use crate::modules::licensing::check_module_entitlement;
use actix_web::HttpRequest;
use actix_web::HttpResponse;
use actix_web::web;
use chrono::Utc;
use diesel::prelude::*;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

use super::caller::authorize_company;
use super::caller::authorize_list;
use super::caller::caller_from;
use super::caller::check_capability;
use super::caller::domain_claimed;
use super::caller::require_permission;
use super::caller::require_control_plane;
use super::events::CAP_MANAGE;
use super::events::CAP_READ;
use super::events::CAP_SELF_ASSIGN;
use super::events::EVENT_DOMAIN_VERIFICATION_STATUS_REQUESTED;
use super::events::EVENT_PACKAGE_ENTITLEMENT_CHECK;
use super::events::EVENT_PRODUCT_ENTITLEMENT_CHECK;
use super::events::EVENT_PRODUCT_SUBDOMAIN_ACTIVATED;
use super::events::EVENT_PRODUCT_SUBDOMAIN_CLAIMED;
use super::events::EVENT_PRODUCT_SUBDOMAIN_RELEASE_STARTED;
use super::events::EVENT_PRODUCT_SUBDOMAIN_RELEASED;
use super::events::EVENT_PRODUCT_SUBDOMAIN_SUSPENDED;
use super::events::EVENT_PRODUCT_SUBDOMAIN_VERIFIED;
use super::requests::CheckProductSubdomainRequest;
use super::requests::ClaimProductSubdomainRequest;
use super::store::aliases_store;
use super::store::allocations_store;
use super::store::paginate;
use super::store::product_subdomains_store;
use super::store::reservations_store;
use super::store::settings;
use super::types::ProductSubdomain;
use super::validation::validate_label;

// Product-subdomain helpers.

/// Validate a product or package identifier: 1-128 characters,
/// lowercase letters, digits, hyphens, and underscores, starting
/// with a letter or digit. Product ids are opaque identifiers from
/// the products module or sellable module ids (`academic`, `exams`);
/// this module never invents them.
pub(crate) fn validate_product_id(raw: &str) -> UnifiedResult<String> {
    let value = raw.trim().to_lowercase();
    if value.is_empty()
        || value.len() > 128
        || !value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        || !value
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    {
        return Err(UnifiedError::bad_request(
            "product_id must be 1-128 characters: lowercase letters, digits, hyphens, underscores",
        ));
    }
    Ok(value)
}

/// True when a host is unavailable for a new product subdomain:
/// a live product-subdomain record holds it (active, pending
/// verification, or suspended), a released record is still inside
/// its grace period, or the cross-module conflict check
/// (`domain_claimed`) finds it anywhere else. `except` excludes the
/// record being updated. A released record past its grace period
/// never blocks: its label is free for reclaim.
fn product_host_blocked(ctx: &AppContext, host: &str, except: Option<Uuid>) -> bool {
    let now = Utc::now();
    if product_subdomains_store().iter().any(|entry| {
        let row = entry.value();
        if Some(row.id) == except {
            return false;
        }
        if row.host != host && row.custom_domain.as_deref() != Some(host) {
            return false;
        }
        match row.status.as_str() {
            "active" | "pending_verification" | "suspended" => true,
            "released" => row.grace_until.map_or(true, |until| until > now),
            _ => false,
        }
    }) {
        return true;
    }
    domain_claimed(ctx, host, None, None, None)
}

/// Store-only version of `product_host_blocked` for hook handlers,
/// which have no `AppContext` and therefore cannot run the
/// cross-module (`domain_claimed`) check. The HTTP claim path always
/// runs the full check; hook claims additionally re-check on the
/// next HTTP read of the record.
pub(crate) fn product_host_blocked_stores(host: &str, except: Option<Uuid>) -> bool {
    let now = Utc::now();
    if product_subdomains_store().iter().any(|entry| {
        let row = entry.value();
        if Some(row.id) == except {
            return false;
        }
        if row.host != host && row.custom_domain.as_deref() != Some(host) {
            return false;
        }
        match row.status.as_str() {
            "active" | "pending_verification" | "suspended" => true,
            "released" => row.grace_until.map_or(true, |until| until > now),
            _ => false,
        }
    }) {
        return true;
    }
    if reservations_store().iter().any(|entry| {
        let row = entry.value();
        row.domain == host && (row.status == "reserved" || row.status == "active")
    }) {
        return true;
    }
    if aliases_store().iter().any(|entry| {
        let row = entry.value();
        row.domain == host && row.status != "released"
    }) {
        return true;
    }
    allocations_store().iter().any(|entry| {
        let row = entry.value();
        row.hostname == host && row.status != "released" && row.status != "rejected"
    })
}

/// Count live product subdomains for a tenant, optionally scoped to
/// one product (and one package when given). Live means
/// `pending_verification`, `active`, or `suspended`; released
/// records never count toward a quota.
pub(crate) fn count_live_product_subdomains(
    company_id: Uuid,
    product_id: Option<&str>,
    package_id: Option<&str>,
) -> usize {
    product_subdomains_store()
        .iter()
        .filter(|entry| {
            let row = entry.value();
            row.company_id == company_id
                && (row.status == "pending_verification"
                    || row.status == "active"
                    || row.status == "suspended")
                && product_id.map_or(true, |wanted| row.product_id == wanted)
                && package_id.map_or(true, |wanted| row.package_id.as_deref() == Some(wanted))
        })
        .count()
}

/// Verify the tenant is entitled to the product (or package) before
/// a claim is accepted. Every source is asked and ANY positive
/// answer entitles; when nothing answers positively the claim is
/// rejected with 403 `entitlement.required` (metadata `code`).
/// Sources, in order:
///   1. `product.entitlement.check` (products module, backed by the
///      store's entitlement data).
///   2. `saas_packages.entitlement.check_requested` (packages
///      module, fail-closed `{"allowed": false}` shape), asked once
///      for the product id and once for the package id when given.
///   3. The licensing module's direct check, only when the product
///      id names a registered sellable module (for example
///      `academic` or `exams`); non-sellable and unknown ids never
///      pass through this source, so a core module id cannot be
///      used to claim without a purchase.
/// Fail-closed on purpose: when no entitlement source is
/// registered at all, every claim is refused rather than granted
/// on a guess. See the integration notes for which source is live
/// today.
fn check_product_entitlement(
    ctx: &AppContext,
    company_id: Uuid,
    product_id: &str,
    package_id: Option<&str>,
    user_id: &str,
) -> UnifiedResult<()> {
    let product_answer = ctx.hooks.emit(
        EVENT_PRODUCT_ENTITLEMENT_CHECK,
        json!({ "product_id": product_id, "user_id": user_id }),
    );
    if product_answer.results.iter().any(|result| {
        result
            .get("entitled")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }) {
        return Ok(());
    }
    for module_id in [Some(product_id), package_id] {
        let Some(module_id) = module_id else {
            continue;
        };
        let package_answer = ctx.hooks.emit(
            EVENT_PACKAGE_ENTITLEMENT_CHECK,
            json!({
                "company_id": company_id.to_string(),
                "module_id": module_id,
                "allowed": false,
            }),
        );
        if package_answer.results.iter().any(|result| {
            result
                .get("allowed")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        }) {
            return Ok(());
        }
    }
    let sellable = ctx
        .modules
        .get(product_id)
        .map(|module| module.available_as_product())
        .unwrap_or(false);
    if sellable && check_module_entitlement(ctx, &company_id.to_string(), product_id).is_ok() {
        return Ok(());
    }
    Err(UnifiedError::forbidden(
        "Tenant is not entitled to this product or package; purchase it before claiming a subdomain",
    )
    .with_metadata("code", Value::String("entitlement.required".to_string())))
}

/// Emit one product-subdomain lifecycle event. Every mutation
/// carries the actor and the tenant/product scope so the audit
/// module can record it once the event names are added to its
/// audited list (see the integration notes).
fn emit_product_subdomain_event(
    ctx: &AppContext,
    event: &str,
    row: &ProductSubdomain,
    actor: &str,
) {
    ctx.hooks.emit(
        event,
        json!({
            "product_subdomain_id": row.id,
            "company_id": row.company_id,
            "product_id": row.product_id,
            "package_id": row.package_id,
            "label": row.label,
            "host": row.host,
            "status": row.status,
            "actor": actor,
        }),
    );
}

/// Slugify a product name into a DNS-label candidate base:
/// lowercase, runs of other characters become one hyphen, leading
/// and trailing hyphens stripped. An empty result means the name
/// cannot seed a suggestion.
pub(crate) fn slugify_product_name(name: &str) -> String {
    let mut base = String::new();
    let mut last_was_hyphen = true;
    for c in name.trim().to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            base.push(c);
            last_was_hyphen = false;
        } else if !last_was_hyphen {
            base.push('-');
            last_was_hyphen = true;
        }
    }
    while base.ends_with('-') {
        base.pop();
    }
    base
}

/// Finalize every released product subdomain whose grace period
/// has ended. The record stays `released` (the audit trail is
/// kept); its label becomes free because the availability check
/// treats past-grace releases as non-blocking. Emits one
/// `saas.product_subdomain.released` event per finalized record.
fn finalize_expired_graces(ctx: &AppContext) -> Vec<Uuid> {
    let now = Utc::now();
    let mut finalized: Vec<Uuid> = Vec::new();
    for entry in product_subdomains_store().iter() {
        let row = entry.value();
        if row.status == "released" && row.grace_until.map_or(false, |until| until <= now) {
            finalized.push(row.id);
        }
    }
    for id in &finalized {
        if let Some(row) = product_subdomains_store().get(id) {
            emit_product_subdomain_event(
                ctx,
                EVENT_PRODUCT_SUBDOMAIN_RELEASED,
                &row,
                "system:grace-expiry",
            );
        }
    }
    finalized
}

/// Hook-side version of `finalize_expired_graces`: same state
/// outcome (past-grace labels are free by computation), but hook
/// handlers have no hook bus, so no `.released` event can be
/// emitted here. The HTTP expire endpoint is the emitting path.
pub(crate) fn finalize_expired_graces_hook() -> Vec<Uuid> {
    let now = Utc::now();
    product_subdomains_store()
        .iter()
        .filter_map(|entry| {
            let row = entry.value();
            if row.status == "released" && row.grace_until.map_or(false, |until| until <= now) {
                Some(row.id)
            } else {
                None
            }
        })
        .collect()
}

// Product-subdomain handlers.

pub(crate) async fn check_product_subdomain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CheckProductSubdomainRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller_from(&req)?;
        // Any caller holding a domains capability may check
        // availability; the answer never names the owner of a taken
        // label, so the check itself leaks nothing.
        if !check_capability(&ctx, &caller.role, CAP_MANAGE)
            && !check_capability(&ctx, &caller.role, CAP_READ)
            && !check_capability(&ctx, &caller.role, CAP_SELF_ASSIGN)
        {
            return Err(UnifiedError::forbidden(format!(
                "Role \"{}\" does not hold permission \"{}\"",
                caller.role, CAP_READ
            )));
        }
        if let Some(product_id) = &body.product_id {
            validate_product_id(product_id)?;
        }
        if let Some(company_id) = body.company_id {
            if !check_capability(&ctx, &caller.role, CAP_MANAGE)
                && caller.company_id != Some(company_id)
            {
                return Err(UnifiedError::forbidden(
                    "Cannot check availability for another company's product subdomain",
                ));
            }
        }
        let cfg = settings();
        let label = body.label.trim().to_lowercase();
        if cfg.wildcard_base.trim().is_empty() {
            return Err(UnifiedError::unprocessable(
                "Tenant subdomains are not configured: set saas_domains.wildcard_base first",
            ));
        }
        let host = format!("{label}.{}", cfg.wildcard_base.trim().to_lowercase());
        let (status, reason): (&str, &str) = if validate_label(&label, &cfg).is_err() {
            (
                "invalid",
                "Label does not match the configured label pattern",
            )
        } else if cfg
            .reserved_labels
            .iter()
            .any(|blocked| blocked.trim().to_lowercase() == label)
        {
            ("reserved", "Label is reserved")
        } else if product_host_blocked(&ctx, &host, None) {
            ("taken", "Label is already claimed")
        } else {
            ("available", "Label is available")
        };
        Ok(json!({
            "label": label,
            "host": host,
            "available": status == "available",
            "status": status,
            "reason": reason,
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn claim_product_subdomain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<ClaimProductSubdomainRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProductSubdomain> = (|| {
        let caller = caller_from(&req)?;
        let cfg = settings();
        if cfg.wildcard_base.trim().is_empty() {
            return Err(UnifiedError::unprocessable(
                "Tenant subdomains are not configured: set saas_domains.wildcard_base first",
            ));
        }
        authorize_company(&ctx, &caller, body.company_id)?;
        if !company_exists(&ctx, body.company_id) {
            return Err(UnifiedError::not_found("Company not found"));
        }
        let product_id = validate_product_id(&body.product_id)?;
        let package_id = match &body.package_id {
            Some(raw) if !raw.trim().is_empty() => Some(validate_product_id(raw)?),
            _ => None,
        };
        // Entitlement first: no purchase, no subdomain.
        check_product_entitlement(
            &ctx,
            body.company_id,
            &product_id,
            package_id.as_deref(),
            &caller.actor,
        )?;
        let label = validate_label(&body.label, &cfg)?;
        let host = format!("{label}.{}", cfg.wildcard_base.trim().to_lowercase());
        if product_host_blocked(&ctx, &host, None) {
            return Err(UnifiedError::conflict(format!(
                "Subdomain \"{host}\" is already claimed"
            )));
        }
        if count_live_product_subdomains(body.company_id, None, None)
            >= usize::try_from(cfg.max_product_subdomains_per_tenant).unwrap_or(usize::MAX)
        {
            return Err(UnifiedError::conflict(format!(
                "Tenant already holds the maximum of {} product subdomains",
                cfg.max_product_subdomains_per_tenant
            )));
        }
        if count_live_product_subdomains(body.company_id, Some(&product_id), package_id.as_deref())
            >= usize::try_from(cfg.max_product_subdomains_per_product).unwrap_or(usize::MAX)
        {
            return Err(UnifiedError::conflict(
                "Tenant already holds a product subdomain for this product",
            ));
        }
        if let Some(site_id) = body.site_id {
            if !crate::modules::company::site_exists(&ctx, site_id) {
                return Err(UnifiedError::not_found("Company site not found"));
            }
        }
        let now = Utc::now();
        let record = ProductSubdomain {
            id: Uuid::now_v7(),
            company_id: body.company_id,
            product_id: product_id.clone(),
            package_id: package_id.clone(),
            label: label.clone(),
            host: host.clone(),
            custom_domain: None,
            site_id: body.site_id,
            status: "pending_verification".to_string(),
            claimed_by: caller.actor.clone(),
            claimed_at: now,
            verified_at: None,
            released_at: None,
            grace_until: None,
            created_at: now,
            updated_at: now,
        };
        product_subdomains_store().insert(record.id, record.clone());
        emit_product_subdomain_event(
            &ctx,
            EVENT_PRODUCT_SUBDOMAIN_CLAIMED,
            &record,
            &caller.actor,
        );
        Ok(record)
    })();
    match result {
        Ok(row) => json::success_response(row, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_product_subdomains(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller_from(&req)?;
        let scope = authorize_list(&ctx, &caller)?;
        let mut items: Vec<ProductSubdomain> = product_subdomains_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        if let Some(company_id) = scope {
            items.retain(|row| row.company_id == company_id);
        }
        if let Some(company_id) = params.filters.get("company_id") {
            if let Ok(wanted) = company_id.parse::<Uuid>() {
                if scope.is_some_and(|scoped| scoped != wanted) {
                    return Err(UnifiedError::forbidden(
                        "Cannot list another company's product subdomains",
                    ));
                }
                items.retain(|row| row.company_id == wanted);
            }
        }
        if let Some(product_id) = params.filters.get("product_id") {
            items.retain(|row| row.product_id == *product_id);
        }
        if let Some(package_id) = params.filters.get("package_id") {
            items.retain(|row| row.package_id.as_deref() == Some(package_id.as_str()));
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|row| row.status == *status);
        }
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| {
                row.host.to_lowercase().contains(&needle)
                    || row.label.to_lowercase().contains(&needle)
                    || row.product_id.to_lowercase().contains(&needle)
            });
        }
        let sort = params.allowed_sort(
            &["label", "host", "product_id", "status", "created_at"],
            "created_at",
        );
        match sort.as_str() {
            "label" => items.sort_by(|a, b| a.label.cmp(&b.label)),
            "host" => items.sort_by(|a, b| a.host.cmp(&b.host)),
            "product_id" => items.sort_by(|a, b| a.product_id.cmp(&b.product_id)),
            "status" => items.sort_by(|a, b| a.status.cmp(&b.status)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_product_subdomain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProductSubdomain> = (|| {
        let caller = caller_from(&req)?;
        let row = product_subdomains_store()
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Product subdomain not found"))?;
        authorize_company(&ctx, &caller, row.company_id)?;
        Ok(row)
    })();
    json::respond(result, debug)
}

pub(crate) async fn verify_product_subdomain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProductSubdomain> = (|| {
        let caller = caller_from(&req)?;
        let id = id.into_inner();
        let mut row = product_subdomains_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Product subdomain not found"))?;
        authorize_company(&ctx, &caller, row.company_id)?;
        if row.status != "pending_verification" {
            return Err(UnifiedError::conflict(format!(
                "Only a pending product subdomain can be verified (current status: {})",
                row.status
            )));
        }
        // Idempotent: already verified through the hook path.
        if row.verified_at.is_some() {
            return Ok(row);
        }
        // Verification is owned by the domain-verification module:
        // this endpoint only records its answer, it never verifies
        // by itself. Ask for the status of the subdomain host (and
        // of the attached custom domain, when one is set).
        let mut verified = false;
        for domain in [
            row.host.clone(),
            row.custom_domain.clone().unwrap_or_default(),
        ] {
            if domain.is_empty() {
                continue;
            }
            let answer = ctx.hooks.emit(
                EVENT_DOMAIN_VERIFICATION_STATUS_REQUESTED,
                json!({ "domain": domain }),
            );
            if answer.results.iter().any(|result| {
                result.get("status").and_then(Value::as_str) == Some("verified")
                    || result
                        .get("verified")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
            }) {
                verified = true;
                break;
            }
        }
        if !verified {
            return Err(UnifiedError::unprocessable(
                "Domain verification has not completed for this subdomain; complete verification first",
            ));
        }
        row.verified_at = Some(Utc::now());
        row.updated_at = Utc::now();
        product_subdomains_store().insert(row.id, row.clone());
        emit_product_subdomain_event(&ctx, EVENT_PRODUCT_SUBDOMAIN_VERIFIED, &row, &caller.actor);
        Ok(row)
    })();
    json::respond(result, debug)
}

/// Shared transition for activate / suspend / release. Activation
/// requires a completed verification (`verified_at` set) from ANY
/// source status: a record that went claim -> suspend -> activate
/// without ever being verified must not become active with
/// `verified_at == None`. Release starts the grace period instead
/// of freeing the label immediately.
fn transition_product_subdomain(
    ctx: &AppContext,
    req: &HttpRequest,
    id: Uuid,
    to: &str,
    event: &str,
) -> UnifiedResult<ProductSubdomain> {
    let caller = caller_from(req)?;
    let mut row = product_subdomains_store()
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Product subdomain not found"))?;
    let cfg = settings();
    if to == "active" && cfg.require_product_subdomain_approval {
        require_permission(ctx, &caller.role, CAP_MANAGE)?;
    } else {
        authorize_company(ctx, &caller, row.company_id)?;
    }
    let allowed_from: &[&str] = match to {
        "active" => &["pending_verification", "suspended"],
        "suspended" => &["active", "pending_verification"],
        "released" => &["active", "suspended", "pending_verification"],
        _ => &[],
    };
    if !allowed_from.contains(&row.status.as_str()) {
        return Err(UnifiedError::conflict(format!(
            "Product subdomain in status '{}' cannot move to '{to}' (allowed from: {})",
            row.status,
            allowed_from.join(", ")
        )));
    }
    if to == "active" && row.verified_at.is_none() {
        return Err(UnifiedError::conflict(
            "Product subdomain cannot be activated before its domain is verified",
        ));
    }
    let now = Utc::now();
    row.status = to.to_string();
    if to == "released" {
        row.released_at = Some(now);
        row.grace_until =
            Some(now + chrono::Duration::days(i64::from(cfg.default_product_subdomain_grace_days)));
    }
    row.updated_at = now;
    product_subdomains_store().insert(row.id, row.clone());
    emit_product_subdomain_event(ctx, event, &row, &caller.actor);
    Ok(row)
}

pub(crate) async fn activate_product_subdomain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = transition_product_subdomain(
        &ctx,
        &req,
        id.into_inner(),
        "active",
        EVENT_PRODUCT_SUBDOMAIN_ACTIVATED,
    );
    json::respond(result, debug)
}

pub(crate) async fn suspend_product_subdomain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = transition_product_subdomain(
        &ctx,
        &req,
        id.into_inner(),
        "suspended",
        EVENT_PRODUCT_SUBDOMAIN_SUSPENDED,
    );
    json::respond(result, debug)
}

pub(crate) async fn release_product_subdomain(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = transition_product_subdomain(
        &ctx,
        &req,
        id.into_inner(),
        "released",
        EVENT_PRODUCT_SUBDOMAIN_RELEASE_STARTED,
    );
    json::respond(result, debug)
}

pub(crate) async fn expire_product_subdomains(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_control_plane(&req)?;
        let caller = caller_from(&req)?;
        require_permission(&ctx, &caller.role, CAP_MANAGE)?;
        let finalized = finalize_expired_graces(&ctx);
        Ok(json!({ "finalized": finalized.len(), "ids": finalized }))
    })();
    json::respond(result, debug)
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
    use unified_core::hooks::HookBus;
    use unified_core::hooks::HookContext;
    use unified_core::modules::ModuleRegistry;
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;
    use chrono::DateTime;

    /// App context whose `role.check` hook allows exactly the
    /// given permissions (all other permissions are denied).
    fn test_ctx(allowed: &[&str]) -> web::Data<Arc<AppContext>> {
        let hooks = Arc::new(HookBus::new());
        let allowed: Vec<String> = allowed
            .iter()
            .map(|permission| permission.to_string())
            .collect();
        hooks.register(
            "role.check",
            "test",
            Arc::new(move |ctx: &mut HookContext| {
                let permission = ctx
                    .get("permission")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                ctx.set(
                    "allowed",
                    serde_json::json!(allowed.iter().any(|held| held == permission)),
                );
            }),
        );
        let ctx = AppContext::new(
            AppConfig::default(),
            hooks,
            Arc::new(ModuleRegistry::new()),
        );
        web::Data::new(Arc::new(ctx))
    }

    fn test_request() -> HttpRequest {
        verified_request("tester", "admin")
    }

    fn seed_product_subdomain(
        status: &str,
        verified_at: Option<DateTime<Utc>>,
    ) -> ProductSubdomain {
        let now = Utc::now();
        let label = format!("test-{}", Uuid::now_v7().as_simple());
        let row = ProductSubdomain {
            id: Uuid::now_v7(),
            company_id: Uuid::now_v7(),
            product_id: "product-a".to_string(),
            package_id: None,
            label: label.clone(),
            host: format!("{label}.tenants.example.com"),
            custom_domain: None,
            site_id: None,
            status: status.to_string(),
            claimed_by: "test".to_string(),
            claimed_at: now,
            verified_at,
            released_at: None,
            grace_until: None,
            created_at: now,
            updated_at: now,
        };
        product_subdomains_store().insert(row.id, row.clone());
        row
    }

    #[tokio::test]
    async fn activate_suspended_unverified_product_subdomain_is_conflict() {
        // S3: claim -> suspend -> activate must not yield an active
        // record with verified_at == None.
        let row = seed_product_subdomain("suspended", None);
        let resp = activate_product_subdomain(
            test_ctx(&[CAP_MANAGE]),
            test_request(),
            web::Path::from(row.id),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert_eq!(
            product_subdomains_store()
                .get(&row.id)
                .unwrap()
                .value()
                .status,
            "suspended"
        );
        product_subdomains_store().remove(&row.id);
    }

    #[tokio::test]
    async fn activate_suspended_verified_product_subdomain_succeeds() {
        let row = seed_product_subdomain("suspended", Some(Utc::now()));
        let resp = activate_product_subdomain(
            test_ctx(&[CAP_MANAGE]),
            test_request(),
            web::Path::from(row.id),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            product_subdomains_store()
                .get(&row.id)
                .unwrap()
                .value()
                .status,
            "active"
        );
        product_subdomains_store().remove(&row.id);
    }

    #[tokio::test]
    async fn activate_pending_unverified_product_subdomain_is_conflict() {
        let row = seed_product_subdomain("pending_verification", None);
        let resp = activate_product_subdomain(
            test_ctx(&[CAP_MANAGE]),
            test_request(),
            web::Path::from(row.id),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        product_subdomains_store().remove(&row.id);
    }

    #[tokio::test]
    async fn activate_pending_verified_product_subdomain_succeeds() {
        let row = seed_product_subdomain("pending_verification", Some(Utc::now()));
        let resp = activate_product_subdomain(
            test_ctx(&[CAP_MANAGE]),
            test_request(),
            web::Path::from(row.id),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            product_subdomains_store()
                .get(&row.id)
                .unwrap()
                .value()
                .status,
            "active"
        );
        product_subdomains_store().remove(&row.id);
    }
}
