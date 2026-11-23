//! pricing tables - handlers vendors.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::Value;
use std::sync::Arc;
use super::types::*;
use super::state::*;
use super::validation::*;
use super::auth::*;

pub(crate) async fn list_vendors(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_READ)?;
        let params = query.into_inner();
        let mut items: Vec<Vendor> = state
            .vendors
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|vendor| {
                matches_search(
                    &format!("{} {} {} {}", vendor.company_name, vendor.slug, vendor.email, vendor.user_id),
                    &params,
                ) && in_company_scope(&vendor.company_id, &_scope)
                    && params
                    .filters
                    .get("status")
                    .is_none_or(|status| vendor.status == *status)
                    && params
                        .filters
                        .get("user_id")
                        .is_none_or(|user_id| vendor.user_id == *user_id)
            })
            .collect();
        let sort = params.allowed_sort(&["company_name", "slug", "status", "created_at"], "created_at");
        match sort.as_str() {
            "company_name" => items.sort_by(|a, b| a.company_name.cmp(&b.company_name)),
            "slug" => items.sort_by(|a, b| a.slug.cmp(&b.slug)),
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

/// Vendor application. Any authenticated caller holding
/// `marketplace:vendor` may apply for themselves; an admin holding
/// `marketplace:manage` may file an application for another user.
/// One vendor record per user.
pub(crate) async fn apply_vendor(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    body: web::Json<CreateVendor>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vendor> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        let is_admin = check_capability(&ctx, &caller.role, PERM_MARKETPLACE_MANAGE);
        let is_vendor = check_capability(&ctx, &caller.role, PERM_MARKETPLACE_VENDOR);
        if !is_admin && !is_vendor {
            return Err(UnifiedError::forbidden(
                "Role does not hold permission \"marketplace:vendor\" or \"marketplace:manage\"",
            ));
        }
        let input = body.into_inner();
        // A non-admin applicant applies for themselves only.
        let user_id = match input.user_id {
            Some(user_id) if is_admin => require_text(&user_id, "user_id", MAX_NAME_LEN)?,
            Some(_) => {
                return Err(UnifiedError::forbidden(
                    "Only a marketplace administrator may file an application for another user",
                ));
            }
            None => caller.actor.clone(),
        };
        if state
            .vendors
            .iter()
            .any(|entry| entry.value().user_id == user_id)
        {
            return Err(UnifiedError::conflict(format!(
                "User {user_id} already has a vendor record"
            )));
        }
        let company_name = require_text(&input.company_name, "company_name", MAX_NAME_LEN)?;
        let slug = match input.slug {
            Some(slug) if !slug.trim().is_empty() => slug.trim().to_lowercase(),
            _ => slugify(&company_name),
        };
        if !valid_slug(&slug) {
            return Err(UnifiedError::bad_request(
                "slug must be lowercase letters, digits, and hyphens",
            ));
        }
        if state.vendors.iter().any(|entry| entry.value().slug == slug) {
            return Err(UnifiedError::conflict(format!(
                "A vendor with slug \"{slug}\" already exists"
            )));
        }
        let email = require_text(&input.email, "email", MAX_NAME_LEN)?;
        if !valid_email(&email) {
            return Err(UnifiedError::bad_request("email must be a valid email address"));
        }
        let body_company = input
            .company_id
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let company_id = require_company_match(&_tenant, &req, &body_company)?;
        let vendor = Vendor {
            id: new_id(),
            user_id: user_id.clone(),
            company_id,
            company_name,
            slug,
            email,
            status: "pending".to_string(),
            application_notes: optional_text(input.application_notes, MAX_TEXT_LEN)?,
            review_reason: String::new(),
            reviewed_by: None,
            reviewed_at: None,
            created_at: now(),
            updated_at: now(),
        };
        state.vendors.insert(vendor.id.clone(), vendor.clone());
        let value = serde_json::to_value(&vendor).unwrap_or(Value::Null);
        state.emit("marketplace.vendor_applied", value);
        Ok(vendor)
    })();
    respond_created(result, debug)
}

pub(crate) async fn get_vendor(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vendor> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        let vendor = require_vendor_in_scope(&state, path.as_str(), &_scope)?;
        require_vendor_self_or_admin(&ctx, &caller, &vendor)
            .or_else(|_| {
                // Admins listing vendors use the read permission; a
                // vendor reading another vendor's record is refused.
                require_permission(&ctx, &caller.role, PERM_MARKETPLACE_READ)
            })?;
        Ok(vendor)
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_vendor(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    body: web::Json<UpdateVendor>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vendor> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_MANAGE)?;
        require_vendor_in_scope(&state, path.as_str(), &_scope)?;
        let input = body.into_inner();
        if let Some(slug) = &input.slug {
            if !slug.trim().is_empty() && !valid_slug(&slug.trim().to_lowercase()) {
                return Err(UnifiedError::bad_request(
                    "slug must be lowercase letters, digits, and hyphens",
                ));
            }
            if !slug.trim().is_empty()
                && state.vendors.iter().any(|entry| {
                    entry.key() != path.as_str() && entry.value().slug == slug.trim().to_lowercase()
                })
            {
                return Err(UnifiedError::conflict(format!(
                    "A vendor with slug \"{}\" already exists",
                    slug.trim().to_lowercase()
                )));
            }
        }
        if let Some(email) = &input.email {
            if !email.trim().is_empty() && !valid_email(email.trim()) {
                return Err(UnifiedError::bad_request("email must be a valid email address"));
            }
        }
        let mut vendor = state
            .vendors
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Vendor not found"))?;
        if let Some(company_id) = input.company_id {
            let trimmed = company_id.trim().to_string();
            if trimmed.is_empty() {
                // An explicit clear stays a clear.
                vendor.company_id = None;
            } else {
                require_company_match_for(&_scope, &Some(trimmed.clone()))?;
                vendor.company_id = Some(trimmed);
            }
        }
        if let Some(company_name) = input.company_name {
            vendor.company_name = require_text(&company_name, "company_name", MAX_NAME_LEN)?;
        }
        if let Some(slug) = input.slug {
            if !slug.trim().is_empty() {
                vendor.slug = slug.trim().to_lowercase();
            }
        }
        if let Some(email) = input.email {
            vendor.email = require_text(&email, "email", MAX_NAME_LEN)?;
        }
        if let Some(application_notes) = input.application_notes {
            if application_notes.len() > MAX_TEXT_LEN {
                return Err(UnifiedError::bad_request(
                    "application_notes must be at most 2000 characters",
                ));
            }
            vendor.application_notes = application_notes;
        }
        vendor.updated_at = now();
        Ok(vendor.clone())
    })();
    json::respond(result, debug)
}

/// Shared vendor status transition: pending -> approved/rejected,
/// approved -> suspended, suspended -> approved. Records the
/// reviewer, the reason, and the review time.
pub(crate) fn transition_vendor(
    state: &PricingTablesState,
    vendor_id: &str,
    from: &[&str],
    to: &str,
    event: &str,
    reviewer: &str,
    reason: &str,
) -> UnifiedResult<Vendor> {
    let mut vendor = state
        .vendors
        .get_mut(vendor_id)
        .ok_or_else(|| UnifiedError::not_found("Vendor not found"))?;
    if !from.contains(&vendor.status.as_str()) {
        return Err(UnifiedError::conflict(format!(
            "Vendor status is \"{}\"; expected one of: {}",
            vendor.status,
            from.join(", ")
        )));
    }
    vendor.status = to.to_string();
    vendor.review_reason = reason.trim().to_string();
    vendor.reviewed_by = Some(reviewer.to_string());
    vendor.reviewed_at = Some(now());
    vendor.updated_at = now();
    let updated = vendor.clone();
    drop(vendor);
    let value = serde_json::to_value(&updated).unwrap_or(Value::Null);
    state.emit(event, value);
    Ok(updated)
}

pub(crate) async fn approve_vendor(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    body: web::Json<ReviewInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vendor> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_MANAGE)?;
        require_vendor_in_scope(&state, path.as_str(), &_scope)?;
        let reason = body.into_inner().reason.unwrap_or_default();
        transition_vendor(
            &state,
            path.as_str(),
            &["pending", "suspended"],
            "approved",
            "marketplace.vendor_approved",
            &caller.actor,
            &reason,
        )
    })();
    json::respond(result, debug)
}

pub(crate) async fn reject_vendor(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    body: web::Json<ReviewInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vendor> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_MANAGE)?;
        require_vendor_in_scope(&state, path.as_str(), &_scope)?;
        let reason = body.into_inner().reason.unwrap_or_default();
        if reason.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "A rejection reason is required",
            ));
        }
        transition_vendor(
            &state,
            path.as_str(),
            &["pending"],
            "rejected",
            "marketplace.vendor_rejected",
            &caller.actor,
            &reason,
        )
    })();
    json::respond(result, debug)
}

pub(crate) async fn suspend_vendor(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    body: web::Json<ReviewInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vendor> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_MANAGE)?;
        require_vendor_in_scope(&state, path.as_str(), &_scope)?;
        let reason = body.into_inner().reason.unwrap_or_default();
        transition_vendor(
            &state,
            path.as_str(),
            &["approved"],
            "suspended",
            "marketplace.vendor_suspended",
            &caller.actor,
            &reason,
        )
    })();
    json::respond(result, debug)
}

pub(crate) async fn reactivate_vendor(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    body: web::Json<ReviewInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vendor> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETPLACE_MANAGE)?;
        require_vendor_in_scope(&state, path.as_str(), &_scope)?;
        let reason = body.into_inner().reason.unwrap_or_default();
        transition_vendor(
            &state,
            path.as_str(),
            &["suspended"],
            "approved",
            "marketplace.vendor_reactivated",
            &caller.actor,
            &reason,
        )
    })();
    json::respond(result, debug)
}

pub(crate) async fn vendor_dashboard(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VendorDashboard> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        let vendor = state
            .vendors
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Vendor not found"))?;
        require_record_in_company_scope(&vendor.company_id, &_scope)?;
        require_vendor_self_or_admin(&ctx, &caller, &vendor)?;
        Ok(state.dashboard_for(&vendor))
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_vendor_commissions(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        let vendor = state
            .vendors
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Vendor not found"))?;
        require_record_in_company_scope(&vendor.company_id, &_scope)?;
        require_vendor_self_or_admin(&ctx, &caller, &vendor)?;
        let params = query.into_inner();
        let mut items: Vec<Commission> = state
            .commissions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|commission| {
                commission.vendor_id == vendor.id
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| commission.status == *status)
                    && params
                        .filters
                        .get("order_id")
                        .is_none_or(|order_id| commission.order_id == *order_id)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_vendor_payouts(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        let vendor = state
            .vendors
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Vendor not found"))?;
        require_record_in_company_scope(&vendor.company_id, &_scope)?;
        require_vendor_self_or_admin(&ctx, &caller, &vendor)?;
        let params = query.into_inner();
        let mut items: Vec<Payout> = state
            .payouts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|payout| {
                payout.vendor_id == vendor.id
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| payout.status == *status)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

/// Public storefront data for one approved vendor. Non-approved
/// vendors answer 404 so pending or suspended storefronts are not
/// enumerable.
pub(crate) async fn vendor_storefront(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PricingTablesState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MarketplaceStorefront> = (|| {
        let slug = path.as_str();
        let vendor = state
            .vendors
            .iter()
            .find(|entry| entry.value().slug == slug && entry.value().status == "approved")
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Marketplace storefront not found"))?;
        let mut approved_product_ids: Vec<String> = state
            .product_approvals
            .iter()
            .filter(|entry| entry.value().vendor_id == vendor.id && entry.value().status == "approved")
            .map(|entry| entry.value().product_id.clone())
            .collect();
        approved_product_ids.sort();
        let product_count = approved_product_ids.len() as u64;
        Ok(MarketplaceStorefront {
            vendor,
            approved_product_ids,
            product_count,
        })
    })();
    json::respond(result, debug)
}
