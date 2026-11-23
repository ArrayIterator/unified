// Store module - order, entitlement, and download handlers.
//
// Split from `src/modules/store.rs`; the module root re-exports the public API of this module.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookContext, events};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::DateTime;
use serde_json::{json, Value};
use std::sync::Arc;

use super::digital::{digital_can_manage, grant_digital_purchases_for_paid_order, require_digital_permission, watermark_descriptor};
use super::events::{order_paid_value, transition_order};
use super::helpers::{
    caller_company, caller_owns, entitlement_company, in_company_scope, license_company,
    matches_search, minor_to_major, paginate, record_visible_to_caller, require_caller_company,
    require_entitlement_owner, require_order_owner, require_record_in_company_scope,
    require_tenant,
};
use super::signing::{build_signed_url, catalog_release_file, record_download, require_paid_order_for_entitlement, sign_download, signature_matches};
use super::state::{StoreState, new_id, now};
use super::subscriptions::{activate_entitlements_for_paid_order, activate_subscription_for_paid_order, cancel_subscriptions_for_order};
use super::types::{DigitalDownload, DownloadEntitlement, DownloadLinkRequest, DownloadRecord, Order, PERM_STORE_MANAGE, PayRequest, RefundRequest, SignedDownloadQuery, UpdateOrder};

/// The admin-recorded receipt reference a manual payment must
/// carry. A missing or blank reference is rejected: a manual
/// payment with no proof of payment never completes an order.
pub(crate) fn manual_payment_reference(input: &PayRequest) -> UnifiedResult<String> {
    input
        .payment_reference
        .as_deref()
        .map(str::trim)
        .filter(|reference| !reference.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            UnifiedError::bad_request(
                "A manual payment requires payment_reference: the admin-recorded receipt reference proving the money arrived",
            )
        })
}

/// Resolve the owner of a license id for the download-history
/// ownership check: the entitlement holder, the issued-license
/// holder, or the buyer of an order a download record points at.
/// `None` when nothing in the store references the license.
pub(crate) fn license_owner(state: &StoreState, license_id: &str) -> Option<String> {
    if let Some(owner) = state.entitlements.iter().find_map(|entry| {
        let entitlement = entry.value();
        (entitlement.license_id.as_deref() == Some(license_id)).then(|| entitlement.user_id.clone())
    }) {
        return Some(owner);
    }
    if let Some(owner) = state
        .issued_licenses
        .get(license_id)
        .and_then(|entry| entry.user_id.clone())
    {
        return Some(owner);
    }
    state.downloads.iter().find_map(|entry| {
        let record = entry.value();
        if record.license_id != license_id {
            return None;
        }
        record.order_id.as_ref().and_then(|order_id| {
            state
                .orders
                .get(order_id)
                .map(|order| order.user_id.clone())
        })
    })
}

/// Answer `store.order.licenses` with payload `{"order_id"}`:
/// the license ids the store issued for that order — the same list
/// the refunded event carries. Licensing asks this when an upgrade
/// event names an order but no license ids, so a billing plan
/// change can still reach the licenses it upgrades.
pub(crate) fn on_store_order_licenses(state: Arc<StoreState>, ctx: &mut HookContext) {
    let order_id = ctx
        .get("order_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("order")
                .and_then(|value| value.get("id"))
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let Some(order_id) = order_id else {
        ctx.add_result(json!({ "license_ids": [] }));
        return;
    };
    let license_ids: Vec<String> = state
        .issued_licenses
        .iter()
        .filter(|entry| entry.value().order_id.as_deref() == Some(order_id.as_str()))
        .map(|entry| entry.value().license_id.clone())
        .collect();
    ctx.add_result(json!({ "order_id": order_id, "license_ids": license_ids }));
}

pub(crate) async fn list_orders(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let scope = match caller_company(&tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let params = query.into_inner();
    let manage = digital_can_manage(&state, &req);
    let sort = params.allowed_sort(&["created_at", "total"], "created_at");
    let mut items: Vec<Order> = state
        .orders
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|order| {
            record_visible_to_caller(&req, &order.user_id, manage)
                && (!manage || in_company_scope(&order.company_id, &scope))
                && matches_search(&format!("{} {}", order.id, order.user_id), &params)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| order.status == *status)
                && params
                    .filters
                    .get("user_id")
                    .is_none_or(|user_id| order.user_id == *user_id)
                && params.filters.get("company_id").is_none_or(|company_id| {
                    order.company_id.as_deref() == Some(company_id.as_str())
                })
                && params.filters.get("coupon_code").is_none_or(|code| {
                    order.coupon_code.as_deref() == Some(code.as_str())
                })
        })
        .collect();
    match sort.as_str() {
        "total" => items.sort_by(|a, b| a.total.cmp(&b.total)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn get_order(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Order> = (|| {
        let tenant = require_tenant(&req)?;
        let _company = caller_company(&tenant, &req)?;
        let order = state
            .orders
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Order not found"))?;
        require_order_owner(&req, &order)?;
        Ok(order)
    })();
    json::respond(result, false)
}

pub(crate) async fn update_order(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateOrder>,
) -> HttpResponse {
    let result: UnifiedResult<Order> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = Some(require_caller_company(&tenant, &req)?);
        let input = body.into_inner();
        let Some(status) = input.status else {
            let order = state
                .orders
                .get(path.as_str())
                .map(|entry| entry.value().clone())
                .ok_or_else(|| UnifiedError::not_found("Order not found"))?;
            require_order_owner(&req, &order)?;
            return Ok(order);
        };
        let current = state
            .orders
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Order not found"))?;
        if status == "paid" {
            // Moving an order to `paid` directly is a store-manager
            // action: a buyer must never mark their own order paid,
            // and a manager records it on the buyer's order. The
            // manager check replaces the owner check on this path;
            // every other status still requires the owner.
            // Gateway and bank-transfer payments move orders to
            // paid through `payment.captured` instead and never
            // pass through here.
            require_digital_permission(&state, &req, PERM_STORE_MANAGE)?;
            require_record_in_company_scope(&current.company_id, &scope)?;
        } else {
            require_order_owner(&req, &current)?;
        }
        let updated = transition_order(&state, path.as_str(), &status)?;
        let order_value = serde_json::to_value(&updated).unwrap_or(Value::Null);
        match status.as_str() {
            "paid" => {
                state.emit("payment.completed", order_value.clone());
                state.emit("order.paid", order_paid_value(&updated));
                activate_subscription_for_paid_order(&state, &updated);
                grant_digital_purchases_for_paid_order(&state, &updated);
            }
            "completed" => state.emit("store.order.completed", order_value),
            "refunded" => state.emit(
                "store.order.refunded",
                json!({
                    "order": order_value,
                    "license_ids": state
                        .issued_licenses
                        .iter()
                        .filter(|entry| entry.value().order_id.as_deref() == Some(path.as_str()))
                        .map(|entry| entry.value().license_id.clone())
                        .collect::<Vec<String>>(),
                }),
            ),
            "cancelled" => state.emit("store.order.cancelled", order_value),
            _ => {}
        }
        Ok(updated)
    })();
    json::respond(result, false)
}

pub(crate) async fn delete_order(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let _company = require_caller_company(&tenant, &req)?;
        let order = state
            .orders
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Order not found"))?;
        require_order_owner(&req, &order)?;
        if order.status != "pending" && order.status != "cancelled" {
            return Err(UnifiedError::unprocessable(
                "Only pending or cancelled orders can be deleted",
            ));
        }
        state.orders.remove(path.as_str());
        Ok(json!({ "deleted": true, "id": path.as_str() }))
    })();
    json::respond(result, false)
}

pub(crate) async fn pay_order(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PayRequest>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = Some(require_caller_company(&tenant, &req)?);
        let input = body.into_inner();
        let provider = input.provider.trim().to_string();
        if provider.is_empty() {
            return Err(UnifiedError::bad_request("Payment provider must not be empty"));
        }
        // Validate the manual-payment gate before touching the
        // order: only a caller holding `store.manage` may record
        // an offline payment, and only with the admin-recorded
        // receipt reference proving the money arrived. A buyer
        // calling this without that permission or reference gets
        // a 403/400, never a paid order.
        let manual_reference = if provider == "manual" {
            require_digital_permission(&state, &req, PERM_STORE_MANAGE)?;
            Some(manual_payment_reference(&input)?)
        } else {
            None
        };
        let mut order = state
            .orders
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Order not found"))?;
        if provider == "manual" {
            require_record_in_company_scope(&order.company_id, &scope)?;
        }
        if provider != "manual" {
            // A manager recording a manual payment acts on the
            // buyer's order by design (the `store.manage` check
            // above already ran); every other provider pays the
            // caller's own order.
            require_order_owner(&req, &order)?;
        }
        if order.status == "paid" || order.status == "completed" {
            return Err(UnifiedError::conflict("Order is already paid"));
        }
        if order.status == "cancelled" || order.status == "refunded" {
            return Err(UnifiedError::unprocessable(
                "Cancelled or refunded orders cannot be paid",
            ));
        }
        if order.status != "pending" && order.status != "payment_pending" {
            return Err(UnifiedError::unprocessable(format!(
                "Order in status {} cannot be paid",
                order.status
            )));
        }
        order.provider = Some(provider.clone());
        if provider == "manual" {
            order.status = "paid".to_string();
            order.payment_reference = manual_reference;
            order.updated_at = now();
            let order_value = serde_json::to_value(&*order).unwrap_or(Value::Null);
            drop(order);
            activate_entitlements_for_paid_order(&state, path.as_str());
            // The order was just held mutably above; a concurrent
            // delete can still remove it before this re-read, so a
            // missing order is an error, not a panic.
            let Some(paid) = state
                .orders
                .get(path.as_str())
                .map(|entry| entry.value().clone())
            else {
                return Err(UnifiedError::internal(
                    "Order was deleted while recording its payment",
                ));
            };
            state.emit("payment.completed", order_value.clone());
            state.emit("order.paid", order_paid_value(&paid));
            let subscription = activate_subscription_for_paid_order(&state, &paid);
            grant_digital_purchases_for_paid_order(&state, &paid);
            return Ok(json!({ "order": paid, "payment_status": "completed", "provider": provider, "subscription_id": subscription.map(|subscription| subscription.id) }));
        }
        // Any other provider is handed off through hooks: a real gateway
        // module listens to `payment.initiated` and completes the payment
        // asynchronously. Only "manual" completes synchronously today.
        order.status = "payment_pending".to_string();
        order.updated_at = now();
        let order_value = serde_json::to_value(&*order).unwrap_or(Value::Null);
        drop(order);
        state.emit(
            "payment.initiated",
            json!({
                "order": order_value,
                "provider": provider,
                "amount": state
                    .orders
                    .get(path.as_str())
                    .map(|o| minor_to_major(o.total, &o.currency))
                    .unwrap_or(0.0),
            }),
        );
        // The order was just held mutably above; a concurrent
        // delete can still remove it before this re-read, so a
        // missing order is an error, not a panic.
        let Some(pending) = state
            .orders
            .get(path.as_str())
            .map(|entry| entry.value().clone())
        else {
            return Err(UnifiedError::internal(
                "Order was deleted while initiating its payment",
            ));
        };
        Ok(json!({ "order": pending, "payment_status": "pending", "provider": provider }))
    })();
    json::respond(result, false)
}

pub(crate) async fn complete_order(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Order> = (|| {
        let tenant = require_tenant(&req)?;
        let _company = require_caller_company(&tenant, &req)?;
        let current = state
            .orders
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Order not found"))?;
        require_order_owner(&req, &current)?;
        let updated = transition_order(&state, path.as_str(), "completed")?;
        state.emit("store.order.completed", order_paid_value(&updated));
        Ok(updated)
    })();
    json::respond(result, false)
}

pub(crate) async fn cancel_order(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Order> = (|| {
        let tenant = require_tenant(&req)?;
        let _company = require_caller_company(&tenant, &req)?;
        let current = state
            .orders
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Order not found"))?;
        require_order_owner(&req, &current)?;
        let updated = transition_order(&state, path.as_str(), "cancelled")?;
        state.emit(
            "store.order.cancelled",
            serde_json::to_value(&updated).unwrap_or(Value::Null),
        );
        Ok(updated)
    })();
    json::respond(result, false)
}

pub(crate) async fn refund_order(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: Option<web::Json<RefundRequest>>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let _company = require_caller_company(&tenant, &req)?;
        let reason = body.and_then(|b| b.into_inner().reason);
        let current = state
            .orders
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Order not found"))?;
        require_order_owner(&req, &current)?;
        let updated = transition_order(&state, path.as_str(), "refunded")?;
        let license_ids: Vec<String> = state
            .issued_licenses
            .iter()
            .filter(|entry| entry.value().order_id.as_deref() == Some(path.as_str()))
            .map(|entry| entry.value().license_id.clone())
            .collect();
        let cancelled_subscriptions =
            cancel_subscriptions_for_order(&state, path.as_str(), "order_refunded");
        state.emit(
            "store.order.refunded",
            json!({
                "order": serde_json::to_value(&updated).unwrap_or(Value::Null),
                "order_id": updated.id,
                "license_ids": license_ids,
                "cancelled_subscriptions": cancelled_subscriptions,
                "reason": reason,
            }),
        );
        Ok(json!({
            "order": updated,
            "license_ids": license_ids,
            "cancelled_subscriptions": cancelled_subscriptions,
            "reason": reason,
        }))
    })();
    json::respond(result, false)
}

pub(crate) async fn download_order(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let _company = caller_company(&tenant, &req)?;
        let order = state
            .orders
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Order not found"))?;
        require_order_owner(&req, &order)?;
        if order.status != "paid" && order.status != "completed" {
            return Err(UnifiedError::forbidden(
                "Digital delivery is only available for paid orders",
            ));
        }
        let file_id = order
            .items
            .iter()
            .find_map(|item| item.file_id.clone())
            .ok_or_else(|| {
                UnifiedError::unprocessable("This order has no downloadable file attached")
            })?;
        let job_id = new_id();
        state.emit(
            events::DOWNLOAD_STARTED,
            json!({
                "order_id": order.id,
                "user_id": order.user_id,
                "file_id": file_id,
                "job_id": job_id,
            }),
        );
        Ok(json!({
            "download": {
                "job_id": job_id,
                "order_id": order.id,
                "file_id": file_id,
                "status": "ready",
                "expires_at": (now() + chrono::Duration::hours(24)).to_rfc3339(),
            }
        }))
    })();
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Entitlements and signed downloads
// ---------------------------------------------------------------------------

pub(crate) async fn list_entitlements(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let scope = match caller_company(&tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let params = query.into_inner();
    let manage = digital_can_manage(&state, &req);
    let mut items: Vec<DownloadEntitlement> = state
        .entitlements
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|entitlement| {
            record_visible_to_caller(&req, &entitlement.user_id, manage)
                && (!manage || in_company_scope(&entitlement_company(&state, entitlement), &scope))
                && matches_search(
                &format!(
                    "{} {}",
                    entitlement.license_key.clone().unwrap_or_default(),
                    entitlement.software_slug.clone().unwrap_or_default()
                ),
                &params,
            ) && params.filters.get("license_id").is_none_or(|id| {
                entitlement.license_id.as_deref() == Some(id.as_str())
            }) && params.filters.get("software_id").is_none_or(|id| {
                entitlement.software_id.as_deref() == Some(id.as_str())
            }) && params.filters.get("software_slug").is_none_or(|slug| {
                entitlement.software_slug.as_deref() == Some(slug.as_str())
            }) && params.filters.get("order_id").is_none_or(|id| {
                entitlement.order_id == *id
            }) && params.filters.get("user_id").is_none_or(|id| {
                entitlement.user_id == *id
            }) && params.filters.get("status").is_none_or(|status| {
                entitlement.status == *status
            })
        })
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn get_entitlement(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<DownloadEntitlement> = (|| {
        let tenant = require_tenant(&req)?;
        let _company = caller_company(&tenant, &req)?;
        let entitlement = state
            .entitlements
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Download entitlement not found"))?;
        require_entitlement_owner(&req, &entitlement)?;
        Ok(entitlement)
    })();
    json::respond(result, false)
}

pub(crate) async fn create_download_link(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: Option<web::Json<DownloadLinkRequest>>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let _company = require_caller_company(&tenant, &req)?;
        let entitlement = state
            .entitlements
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Download entitlement not found"))?;
        require_entitlement_owner(&req, &entitlement)?;
        // An unpaid order downloads nothing, even with a valid
        // entitlement id: the order must be paid or completed.
        require_paid_order_for_entitlement(&state, &entitlement)?;
        if entitlement.status == "revoked" {
            return Err(UnifiedError::forbidden(
                "Download entitlement has been revoked",
            ));
        }
        if let Some(expires_at) = entitlement.expires_at {
            if expires_at <= now() {
                return Err(UnifiedError::forbidden(
                    "Download entitlement has expired",
                ));
            }
        }
        if entitlement.downloads_used >= entitlement.download_limit {
            return Err(UnifiedError::forbidden(
                "Download limit reached for this license",
            ));
        }
        let license_id = entitlement
            .license_id
            .clone()
            .ok_or_else(|| {
                UnifiedError::unprocessable(
                    "Entitlement has no license attached; the license was not issued",
                )
            })?;
        let release_id = body
            .and_then(|b| b.into_inner().release_id)
            .or_else(|| entitlement.release_id.clone())
            .ok_or_else(|| {
                UnifiedError::unprocessable(
                    "No release is linked to this entitlement; pass release_id",
                )
            })?;
        let (download_url, expires_at) = build_signed_url(&license_id, &release_id);
        Ok(json!({
            "download_url": download_url,
            "license_id": license_id,
            "release_id": release_id,
            "expires_at": expires_at.to_rfc3339(),
            "downloads_used": entitlement.downloads_used,
            "download_limit": entitlement.download_limit,
        }))
    })();
    json::respond(result, false)
}

pub(crate) async fn signed_download(
    state: web::Data<Arc<StoreState>>,
    query: web::Query<SignedDownloadQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let input = query.into_inner();
        let expires_at = DateTime::from_timestamp(input.expires, 0)
            .ok_or_else(|| UnifiedError::bad_request("Invalid expiry timestamp"))?;
        if expires_at <= now() {
            return Err(UnifiedError::forbidden("Download link has expired"));
        }
        let expected = sign_download(&input.license_id, &input.release_id, input.expires);
        if !signature_matches(&expected, &input.sig) {
            return Err(UnifiedError::forbidden("Download link signature is invalid"));
        }
        let entitlement_id = state
            .entitlements
            .iter()
            .find(|entry| {
                entry.value().license_id.as_deref() == Some(input.license_id.as_str())
                    && entry
                        .value()
                        .release_id
                        .as_deref()
                        .is_none_or(|release_id| release_id == input.release_id)
            })
            .map(|entry| entry.key().clone())
            .ok_or_else(|| {
                UnifiedError::forbidden(
                    "No download entitlement exists for this license and release",
                )
            })?;
        let entitlement = record_download(&state, &entitlement_id, &input.release_id)?;
        // A digital file resolves through the digital file map (its
        // file id is the `release_id` of the entitlement); software
        // releases resolve through the catalog as before.
        let digital_file = state
            .digital_files
            .get(&input.release_id)
            .map(|entry| entry.value().clone());
        let file_id = catalog_release_file(&state, &input.release_id)
            .or_else(|| digital_file.as_ref().map(|file| file.storage_file_id.clone()));
        let mut body = json!({
            "download": {
                "license_id": input.license_id,
                "release_id": input.release_id,
                "software_slug": entitlement.software_slug,
                "file_id": file_id,
                "status": "ready",
                "downloads_used": entitlement.downloads_used,
                "download_limit": entitlement.download_limit,
                "expires_at": expires_at.to_rfc3339(),
            }
        });
        // A digital download is watermarked and recorded in the
        // per-buyer digital download history. The descriptor tells
        // the delivery layer which metadata fields to embed; the
        // byte-level embedding itself happens there, from this
        // descriptor (see `watermark_descriptor`).
        if let Some(file) = digital_file {
            let watermark = watermark_descriptor(
                &state,
                &file.format,
                &entitlement.user_id,
                &entitlement.order_id,
            );
            let watermark_applied = watermark
                .get("metadata_supported")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            let record = DigitalDownload {
                id: new_id(),
                digital_product_id: file.digital_product_id.clone(),
                file_id: Some(file.id.clone()),
                preview_id: None,
                buyer_user_id: entitlement.user_id.clone(),
                order_id: Some(entitlement.order_id.clone()),
                grant_id: entitlement.license_id.clone(),
                kind: "purchase".to_string(),
                watermark_subject: Some(entitlement.user_id.clone()),
                watermark_applied,
                watermark_formats: if watermark_applied {
                    vec![file.format.clone()]
                } else {
                    Vec::new()
                },
                downloaded_at: now(),
            };
            state
                .digital_downloads
                .insert(record.id.clone(), record.clone());
            state.emit(
                "store.digital.download",
                json!({
                    "download": serde_json::to_value(&record).unwrap_or(Value::Null),
                    "watermark": watermark.clone(),
                    "file_id": file.storage_file_id,
                }),
            );
            if let Some(download) = body
                .get_mut("download")
                .and_then(|value| value.as_object_mut())
            {
                download.insert(
                    "digital_product_id".to_string(),
                    json!(file.digital_product_id),
                );
                download.insert("watermark".to_string(), watermark);
            }
        }
        Ok(body)
    })();
    json::respond(result, false)
}

pub(crate) async fn list_license_downloads(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let scope = match caller_company(&tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, false),
    };
    let params = query.into_inner();
    let license_id = path.into_inner();
    // Download history belongs to the license holder: a manager
    // sees every license, anyone else only a license they own.
    // A license nothing in the store references cannot be proven
    // owned, so it is refused rather than listed.
    if !digital_can_manage(&state, &req) {
        let allowed = match license_owner(&state, &license_id) {
            Some(owner) => caller_owns(&req, &owner),
            None => false,
        };
        if !allowed {
            return json::error_response(
                &UnifiedError::forbidden("Download history belongs to a different user"),
                false,
            );
        }
    }
    if digital_can_manage(&state, &req) {
        let company = license_company(&state, &license_id);
        if !in_company_scope(&company, &scope) {
            return json::error_response(
                &UnifiedError::forbidden("Download history belongs to a different company"),
                false,
            );
        }
    }
    let mut items: Vec<DownloadRecord> = state
        .downloads
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|record| {
            record.license_id == license_id
                && params
                    .filters
                    .get("release_id")
                    .is_none_or(|release_id| record.release_id == *release_id)
        })
        .collect();
    items.sort_by(|a, b| a.downloaded_at.cmp(&b.downloaded_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pay_request(provider: &str, payment_reference: Option<&str>) -> PayRequest {
        PayRequest {
            provider: provider.to_string(),
            payment_reference: payment_reference.map(str::to_string),
        }
    }

    #[test]
    fn manual_payment_reference_requires_a_receipt() {
        assert!(manual_payment_reference(&pay_request("manual", None)).is_err());
        assert!(manual_payment_reference(&pay_request("manual", Some(""))).is_err());
        assert!(manual_payment_reference(&pay_request("manual", Some("   "))).is_err());
        assert_eq!(
            manual_payment_reference(&pay_request("manual", Some("  rcpt-123  "))).unwrap(),
            "rcpt-123"
        );
    }

    #[test]
    fn pay_request_deserializes_without_a_reference() {
        let parsed: PayRequest =
            serde_json::from_value(json!({ "provider": "stripe" })).unwrap();
        assert_eq!(parsed.provider, "stripe");
        assert_eq!(parsed.payment_reference, None);
        let parsed: PayRequest = serde_json::from_value(
            json!({ "provider": "manual", "payment_reference": "rcpt-9" }),
        )
        .unwrap();
        assert_eq!(parsed.payment_reference.as_deref(), Some("rcpt-9"));
    }
}
