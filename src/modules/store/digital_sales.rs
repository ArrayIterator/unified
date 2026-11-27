// Store module - digital download, royalty, and payout handlers.
//
// Split from `src/modules/store.rs`; the module root re-exports the public API of this module.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

use super::digital::{check_sample_rate_limit, digital_can_manage, ensure_digital_entitlement, require_digital_manage, require_digital_public, require_digital_read, validate_royalty, watermark_descriptor};
use super::digital_products::{digital_product_for_read, load_digital_product};
use super::helpers::{
    caller_company, in_company_scope, matches_search, minor_to_major, owner_of, paginate,
    require_caller_company, require_record_in_company_scope,
    require_record_writable_in_company_scope, require_tenant, respond_created,
};
use super::signing::{build_signed_url};
use super::state::{StoreState, new_id, now};
use super::types::{CreateDigitalPayout, DEFAULT_ROYALTY_KIND, DEFAULT_ROYALTY_PERCENT_BP, DigitalDefaults, DigitalDownload, DigitalDownloadLinkRequest, DigitalPayoutRequest, DigitalProductFile, DigitalProductPreview, DigitalRoyalty, UpdateDigitalSettings, UpdateWatermarkFormat, WatermarkFormat};

#[derive(Debug, Clone, Deserialize)]
pub struct SampleQuery {
    pub preview_id: Option<String>,
}

/// Download the free sample of a digital product. Public by design
/// when the seller allows samples, and rate-limited per identity and
/// preview plus logged in the download history, so a free excerpt
/// cannot be harvested without a trace. Samples are excerpts, not
/// the purchased file, so they carry no buyer watermark.
pub(crate) async fn download_sample(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<SampleQuery>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        require_digital_public(&state, &req)?;
        let digital = digital_product_for_read(&state, &req, path.as_str())?;
        if !digital.sample_allowed {
            return Err(UnifiedError::forbidden(
                "This digital product offers no free sample",
            ));
        }
        let preview = match &query.into_inner().preview_id {
            Some(preview_id) => {
                let preview = state
                    .digital_previews
                    .get(preview_id)
                    .map(|entry| entry.value().clone())
                    .ok_or_else(|| UnifiedError::not_found("Digital preview not found"))?;
                if preview.digital_product_id != digital.id {
                    return Err(UnifiedError::not_found("Digital preview not found"));
                }
                if !preview.allowed {
                    return Err(UnifiedError::forbidden(
                        "This preview is not available for public download",
                    ));
                }
                preview
            }
            None => {
                let mut allowed: Vec<DigitalProductPreview> = state
                    .digital_previews
                    .iter()
                    .filter(|entry| {
                        entry.value().digital_product_id == digital.id
                            && entry.value().allowed
                    })
                    .map(|entry| entry.value().clone())
                    .collect();
                allowed.sort_by(|a, b| a.created_at.cmp(&b.created_at));
                match allowed.len() {
                    0 => {
                        return Err(UnifiedError::not_found(
                            "This digital product has no public sample",
                        ));
                    }
                    1 => allowed.into_iter().next().expect("one allowed preview"),
                    _ => {
                        return Err(UnifiedError::bad_request(
                            "This product has several samples; pass preview_id to choose one",
                        ));
                    }
                }
            }
        };
        let identity = owner_of(&req);
        check_sample_rate_limit(&state, &identity, &preview.id)?;
        let record = DigitalDownload {
            id: new_id(),
            digital_product_id: digital.id.clone(),
            file_id: None,
            preview_id: Some(preview.id.clone()),
            buyer_user_id: identity.clone(),
            order_id: None,
            grant_id: None,
            kind: "sample".to_string(),
            watermark_subject: None,
            watermark_applied: false,
            watermark_formats: Vec::new(),
            downloaded_at: now(),
        };
        state
            .digital_downloads
            .insert(record.id.clone(), record.clone());
        state.emit(
            "store.digital.sample.downloaded",
            serde_json::to_value(&record).unwrap_or(Value::Null),
        );
        Ok(json!({
            "sample": {
                "digital_product_id": digital.id,
                "preview_id": preview.id,
                "storage_file_id": preview.storage_file_id,
                "format": preview.format,
                "status": "ready",
                "download_id": record.id,
            }
        }))
    })();
    json::respond(result, false)
}

// Digital products: downloads, royalties, payouts, settings

/// Create a signed expiring download link for one file of a digital
/// product the caller bought. This reuses the existing signed-link
/// flow end to end: the caller must hold a paid or completed order
/// containing the product, the entitlement (minted at payment, or
/// lazily here when the order was paid before the digital profile
/// existed) enforces the per-file download limit, and the actual
/// download is counted by `signed_download`, exactly like a software
/// release. The response also carries the watermark descriptor for
/// the file, so the buyer sees what will be embedded.
pub(crate) async fn create_digital_download_link(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: Option<web::Json<DigitalDownloadLinkRequest>>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let _company = require_caller_company(&tenant, &req)?;
        let caller = require_digital_read(&state, &req)?;
        let digital = load_digital_product(&state, path.as_str())?;
        let order = state
            .orders
            .iter()
            .filter(|entry| {
                let order = entry.value();
                order.user_id == caller.actor
                    && matches!(order.status.as_str(), "paid" | "completed")
                    && order
                        .items
                        .iter()
                        .any(|item| item.product_id == digital.product_id)
            })
            .map(|entry| entry.value().clone())
            .max_by(|a, b| a.created_at.cmp(&b.created_at))
            .ok_or_else(|| {
                UnifiedError::forbidden(
                    "No paid order for this digital product belongs to the caller",
                )
            })?;
        let requested_file_id = body.and_then(|body| body.into_inner().file_id);
        let files: Vec<DigitalProductFile> = state
            .digital_files
            .iter()
            .filter(|entry| entry.value().digital_product_id == digital.id)
            .map(|entry| entry.value().clone())
            .collect();
        let file = match &requested_file_id {
            Some(file_id) => files
                .iter()
                .find(|file| file.id == *file_id)
                .ok_or_else(|| UnifiedError::not_found("Digital file not found"))?,
            None => files
                .iter()
                .find(|file| file.is_primary)
                .or_else(|| {
                    files.iter().min_by(|a, b| {
                        a.sort_order
                            .cmp(&b.sort_order)
                            .then(a.created_at.cmp(&b.created_at))
                    })
                })
                .ok_or_else(|| {
                    UnifiedError::unprocessable(
                        "This digital product has no deliverable file",
                    )
                })?,
        };
        let (entitlement, _) = ensure_digital_entitlement(&state, &order, &digital, file);
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
                "Download limit reached for this purchase",
            ));
        }
        let license_id = entitlement.license_id.clone().ok_or_else(|| {
            UnifiedError::unprocessable("Entitlement has no purchase grant attached")
        })?;
        let (download_url, expires_at) = build_signed_url(&license_id, &file.id);
        let watermark =
            watermark_descriptor(&state, &file.format, &caller.actor, &order.id);
        Ok(json!({
            "download_url": download_url,
            "digital_product_id": digital.id,
            "file_id": file.id,
            "storage_file_id": file.storage_file_id,
            "format": file.format,
            "grant_id": license_id,
            "expires_at": expires_at.to_rfc3339(),
            "downloads_used": entitlement.downloads_used,
            "download_limit": entitlement.download_limit,
            "watermark": watermark,
        }))
    })();
    json::respond(result, false)
}

pub(crate) async fn list_digital_downloads(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<DigitalDownload>> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = caller_company(&tenant, &req)?;
        let caller = require_digital_read(&state, &req)?;
        let params = query.into_inner();
        let user_id = params
            .filters
            .get("user_id")
            .cloned()
            .unwrap_or_else(|| caller.actor.clone());
        if user_id != caller.actor && !digital_can_manage(&state, &req) {
            return Err(UnifiedError::forbidden(
                "Download history of another user requires the manage permission",
            ));
        }
        let mut items: Vec<DigitalDownload> = state
            .digital_downloads
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|download| {
                download.buyer_user_id == user_id
                    && (!digital_can_manage(&state, &req)
                        || user_id == caller.actor
                        || state
                            .digital_products
                            .get(&download.digital_product_id)
                            .is_some_and(|entry| {
                                in_company_scope(&entry.value().royalty_payee_company_id, &scope)
                            }))
                    && params.filters.get("product_id").is_none_or(|product_id| {
                        download.digital_product_id == *product_id
                    })
                    && params
                        .filters
                        .get("kind")
                        .is_none_or(|kind| download.kind == *kind)
                    && params.filters.get("file_id").is_none_or(|file_id| {
                        download.file_id.as_deref() == Some(file_id.as_str())
                    })
            })
            .collect();
        items.sort_by(|a, b| a.downloaded_at.cmp(&b.downloaded_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

pub(crate) async fn list_digital_royalties(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<DigitalRoyalty>> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = caller_company(&tenant, &req)?;
        let caller = require_digital_read(&state, &req)?;
        let params = query.into_inner();
        let payee = params
            .filters
            .get("payee")
            .cloned()
            .unwrap_or_else(|| caller.actor.clone());
        if payee != caller.actor && !digital_can_manage(&state, &req) {
            return Err(UnifiedError::forbidden(
                "Royalty history of another payee requires the manage permission",
            ));
        }
        let mut items: Vec<DigitalRoyalty> = state
            .digital_royalties
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|royalty| {
                royalty.payee.as_deref() == Some(payee.as_str())
                    && (!digital_can_manage(&state, &req)
                        || royalty.payee.as_deref() == Some(caller.actor.as_str())
                        || in_company_scope(&royalty.payee_company_id, &scope))
                    && params.filters.get("status").is_none_or(|status| {
                        royalty.status == *status
                    })
                    && params.filters.get("product_id").is_none_or(|product_id| {
                        royalty.product_id == *product_id
                    })
                    && params.filters.get("order_id").is_none_or(|order_id| {
                        royalty.order_id == *order_id
                    })
                    && params.filters.get("currency").is_none_or(|currency| {
                        royalty.currency.eq_ignore_ascii_case(currency)
                    })
            })
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

pub(crate) async fn get_digital_royalty(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<DigitalRoyalty> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = caller_company(&tenant, &req)?;
        let caller = require_digital_read(&state, &req)?;
        let royalty = state
            .digital_royalties
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Royalty not found"))?;
        if royalty.payee.as_deref() != Some(caller.actor.as_str())
            && !digital_can_manage(&state, &req)
        {
            return Err(UnifiedError::forbidden(
                "Royalty belongs to a different payee",
            ));
        }
        if royalty.payee.as_deref() != Some(caller.actor.as_str()) {
            require_record_in_company_scope(&royalty.payee_company_id, &scope)?;
        }
        Ok(royalty)
    })();
    json::respond(result, false)
}

pub(crate) async fn create_digital_payout(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    body: web::Json<CreateDigitalPayout>,
) -> HttpResponse {
    let result: UnifiedResult<DigitalPayoutRequest> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = Some(require_caller_company(&tenant, &req)?);
        let caller = require_digital_read(&state, &req)?;
        let input = body.into_inner();
        if input.royalty_ids.is_empty() {
            return Err(UnifiedError::bad_request(
                "At least one royalty id is required",
            ));
        }
        if input.royalty_ids.len() > 100 {
            return Err(UnifiedError::bad_request(
                "At most 100 royalties can be grouped into one payout request",
            ));
        }
        let mut royalties: Vec<DigitalRoyalty> = Vec::new();
        for royalty_id in &input.royalty_ids {
            let royalty = state
                .digital_royalties
                .get(royalty_id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| {
                    UnifiedError::not_found(format!("Royalty {royalty_id} not found"))
                })?;
            if royalty.status != "accrued" {
                return Err(UnifiedError::unprocessable(format!(
                    "Royalty {} is {} and cannot be paid out",
                    royalty.id, royalty.status
                )));
            }
            royalties.push(royalty);
        }
        let first = royalties.first().expect("royalties is not empty");
        let payee = first.payee.clone();
        let payee_company_id = first.payee_company_id.clone();
        let currency = first.currency.clone();
        if payee.is_none() && payee_company_id.is_none() {
            return Err(UnifiedError::unprocessable(
                "Royalty has no payee, so no payout can be requested for it",
            ));
        }
        for royalty in &royalties {
            if royalty.payee != payee
                || royalty.payee_company_id != payee_company_id
                || royalty.currency != currency
            {
                return Err(UnifiedError::unprocessable(
                    "All royalties in one payout request must share the same payee and currency",
                ));
            }
        }
        // A payee requests their own payout; anyone else needs the
        // manage permission (an administrator requesting on a
        // payee's behalf).
        if payee.as_deref() != Some(caller.actor.as_str())
            && !digital_can_manage(&state, &req)
        {
            return Err(UnifiedError::forbidden(
                "Payout can only be requested by its payee",
            ));
        }
        if payee.as_deref() != Some(caller.actor.as_str()) {
            require_record_in_company_scope(&payee_company_id, &scope)?;
        }
        let amount: i64 = royalties.iter().map(|royalty| royalty.amount).sum();
        let payout = DigitalPayoutRequest {
            id: new_id(),
            payee: payee.clone(),
            payee_company_id: payee_company_id.clone(),
            currency,
            amount,
            royalty_ids: input.royalty_ids.clone(),
            status: "requested".to_string(),
            requested_by: caller.actor.clone(),
            created_at: now(),
            updated_at: now(),
        };
        state
            .digital_payouts
            .insert(payout.id.clone(), payout.clone());
        for royalty_id in &input.royalty_ids {
            if let Some(mut royalty) = state.digital_royalties.get_mut(royalty_id) {
                royalty.status = "payout_requested".to_string();
                royalty.payout_id = Some(payout.id.clone());
                royalty.updated_at = now();
            }
        }
        // Events only: billing owns the payout itself. The payout
        // request carries everything billing needs to pay it.
        state.emit(
            "store.digital.payout.requested",
            serde_json::to_value(&payout).unwrap_or(Value::Null),
        );
        Ok(payout)
    })();
    respond_created(result)
}

pub(crate) async fn list_digital_payouts(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<DigitalPayoutRequest>> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = caller_company(&tenant, &req)?;
        let caller = require_digital_read(&state, &req)?;
        let params = query.into_inner();
        let payee = params
            .filters
            .get("payee")
            .cloned()
            .unwrap_or_else(|| caller.actor.clone());
        if payee != caller.actor && !digital_can_manage(&state, &req) {
            return Err(UnifiedError::forbidden(
                "Payout history of another payee requires the manage permission",
            ));
        }
        let mut items: Vec<DigitalPayoutRequest> = state
            .digital_payouts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|payout| {
                payout.payee.as_deref() == Some(payee.as_str())
                    && (!digital_can_manage(&state, &req)
                        || payout.payee.as_deref() == Some(caller.actor.as_str())
                        || in_company_scope(&payout.payee_company_id, &scope))
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| payout.status == *status)
                    && params.filters.get("currency").is_none_or(|currency| {
                        payout.currency.eq_ignore_ascii_case(currency)
                    })
            })
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

pub(crate) async fn get_digital_payout(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<DigitalPayoutRequest> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = caller_company(&tenant, &req)?;
        let caller = require_digital_read(&state, &req)?;
        let payout = state
            .digital_payouts
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Payout request not found"))?;
        if payout.payee.as_deref() != Some(caller.actor.as_str())
            && !digital_can_manage(&state, &req)
        {
            return Err(UnifiedError::forbidden(
                "Payout request belongs to a different payee",
            ));
        }
        if payout.payee.as_deref() != Some(caller.actor.as_str()) {
            require_record_in_company_scope(&payout.payee_company_id, &scope)?;
        }
        Ok(payout)
    })();
    json::respond(result, false)
}

/// Shared payout transition (approve / mark-paid / reject). The
/// payout moves only from the listed states; the grouped royalties
/// move with it, and a rejection releases them back to `accrued`
/// with the payout link cleared so they can be requested again.
pub(crate) fn move_digital_payout(
    state: &StoreState,
    scope: &Option<String>,
    payout_id: &str,
    allowed_from: &[&str],
    to_status: &str,
    royalty_status: &str,
    event: &str,
) -> UnifiedResult<DigitalPayoutRequest> {
    let mut payout = state
        .digital_payouts
        .get_mut(payout_id)
        .ok_or_else(|| UnifiedError::not_found("Payout request not found"))?;
    require_record_writable_in_company_scope(&payout.payee_company_id, scope)?;
    if !allowed_from.contains(&payout.status.as_str()) {
        return Err(UnifiedError::unprocessable(format!(
            "A {} payout request cannot move to {}",
            payout.status, to_status
        )));
    }
    payout.status = to_status.to_string();
    payout.updated_at = now();
    let updated = payout.clone();
    drop(payout);
    for royalty_id in &updated.royalty_ids {
        if let Some(mut royalty) = state.digital_royalties.get_mut(royalty_id) {
            royalty.status = royalty_status.to_string();
            if royalty_status == "accrued" {
                royalty.payout_id = None;
            }
            royalty.updated_at = now();
        }
    }
    state.emit(event, serde_json::to_value(&updated).unwrap_or(Value::Null));
    Ok(updated)
}

pub(crate) async fn approve_digital_payout(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<DigitalPayoutRequest> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = Some(require_caller_company(&tenant, &req)?);
        require_digital_manage(&state, &req)?;
        move_digital_payout(
            &state,
            &scope,
            path.as_str(),
            &["requested"],
            "approved",
            "approved",
            "store.digital.payout.approved",
        )
    })();
    json::respond(result, false)
}

pub(crate) async fn mark_digital_payout_paid(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<DigitalPayoutRequest> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = Some(require_caller_company(&tenant, &req)?);
        require_digital_manage(&state, &req)?;
        // Records that billing moved the money; it never moves money
        // itself. Only an approved payout can be marked paid.
        move_digital_payout(
            &state,
            &scope,
            path.as_str(),
            &["approved"],
            "paid",
            "paid",
            "store.digital.payout.paid",
        )
    })();
    json::respond(result, false)
}

pub(crate) async fn reject_digital_payout(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<DigitalPayoutRequest> = (|| {
        let tenant = require_tenant(&req)?;
        let scope = Some(require_caller_company(&tenant, &req)?);
        require_digital_manage(&state, &req)?;
        move_digital_payout(
            &state,
            &scope,
            path.as_str(),
            &["requested", "approved"],
            "rejected",
            "accrued",
            "store.digital.payout.rejected",
        )
    })();
    json::respond(result, false)
}

pub(crate) async fn get_digital_settings(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let _company = caller_company(&tenant, &req)?;
        require_digital_read(&state, &req)?;
        let defaults = state
            .digital_defaults
            .read()
            .map(|defaults| defaults.clone())
            .unwrap_or_default();
        let formats: Vec<WatermarkFormat> = state
            .watermark_formats
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        Ok(json!({
            "defaults": defaults,
            "type_count": state.digital_types.len(),
            "watermark_formats": formats,
        }))
    })();
    json::respond(result, false)
}

pub(crate) async fn update_digital_settings(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    body: web::Json<UpdateDigitalSettings>,
) -> HttpResponse {
    let result: UnifiedResult<DigitalDefaults> = (|| {
        let tenant = require_tenant(&req)?;
        let _company = require_caller_company(&tenant, &req)?;
        require_digital_manage(&state, &req)?;
        let input = body.into_inner();
        // Resolve the effective royalty rule (incoming overrides,
        // else the stored defaults) and validate it as a whole, so
        // a kind change can never strand the stored value in the
        // wrong meaning.
        let (effective_kind, effective_percent_bp, effective_fixed_minor) = {
            let stored = state.digital_defaults.read().ok();
            let kind = input.default_royalty_kind.clone().unwrap_or_else(|| {
                stored
                    .as_ref()
                    .map(|defaults| defaults.default_royalty_kind.clone())
                    .unwrap_or_else(|| DEFAULT_ROYALTY_KIND.to_string())
            });
            let percent_bp = input.default_royalty_percent_bp.unwrap_or_else(|| {
                stored
                    .as_ref()
                    .map(|defaults| defaults.default_royalty_percent_bp)
                    .unwrap_or(DEFAULT_ROYALTY_PERCENT_BP)
            });
            let fixed_minor = input.default_royalty_fixed_minor.unwrap_or_else(|| {
                stored
                    .as_ref()
                    .map(|defaults| defaults.default_royalty_fixed_minor)
                    .unwrap_or(0)
            });
            (kind, percent_bp, fixed_minor)
        };
        if input.default_royalty_kind.is_some()
            || input.default_royalty_percent_bp.is_some()
            || input.default_royalty_fixed_minor.is_some()
        {
            validate_royalty(&effective_kind, effective_percent_bp, effective_fixed_minor)?;
        }
        let mut defaults = state
            .digital_defaults
            .write()
            .expect("digital defaults lock poisoned");
        if let Some(limit) = input.default_download_limit {
            if limit == 0 || limit > 10_000 {
                return Err(UnifiedError::bad_request(
                    "Default download limit must be between 1 and 10000",
                ));
            }
            defaults.default_download_limit = limit;
        }
        if let Some(limit) = input.sample_rate_limit_per_hour {
            if limit == 0 || limit > 1_000 {
                return Err(UnifiedError::bad_request(
                    "Sample rate limit must be between 1 and 1000 per hour",
                ));
            }
            defaults.sample_rate_limit_per_hour = limit;
        }
        if let Some(window) = input.sample_rate_limit_window_minutes {
            if window == 0 || window > 1_440 {
                return Err(UnifiedError::bad_request(
                    "Sample rate limit window must be between 1 and 1440 minutes",
                ));
            }
            defaults.sample_rate_limit_window_minutes = window;
        }
        if let Some(kind) = input.default_royalty_kind {
            defaults.default_royalty_kind = kind;
        }
        if let Some(percent_bp) = input.default_royalty_percent_bp {
            defaults.default_royalty_percent_bp = percent_bp;
        }
        if let Some(fixed_minor) = input.default_royalty_fixed_minor {
            defaults.default_royalty_fixed_minor = fixed_minor;
        }
        if let Some(enabled) = input.watermark_enabled {
            defaults.watermark_enabled = enabled;
        }
        Ok(defaults.clone())
    })();
    json::respond(result, false)
}

pub(crate) async fn list_watermark_formats(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<WatermarkFormat>> = (|| {
        let tenant = require_tenant(&req)?;
        let _company = caller_company(&tenant, &req)?;
        require_digital_read(&state, &req)?;
        let params = query.into_inner();
        let mut items: Vec<WatermarkFormat> = state
            .watermark_formats
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|format| {
                matches_search(&format!("{} {}", format.format, format.notes), &params)
                    && params.filters.get("metadata_supported").is_none_or(|flag| {
                        format.metadata_supported == (*flag == "true" || *flag == "1")
                    })
            })
            .collect();
        items.sort_by(|a, b| a.format.cmp(&b.format));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

pub(crate) async fn update_watermark_format(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateWatermarkFormat>,
) -> HttpResponse {
    let result: UnifiedResult<WatermarkFormat> = (|| {
        let tenant = require_tenant(&req)?;
        let _company = require_caller_company(&tenant, &req)?;
        require_digital_manage(&state, &req)?;
        let input = body.into_inner();
        if let Some(fields) = &input.metadata_fields {
            if fields.len() > 16 {
                return Err(UnifiedError::bad_request(
                    "At most 16 metadata fields can be listed for a format",
                ));
            }
            for field in fields {
                if field.trim().is_empty() || field.len() > 64 {
                    return Err(UnifiedError::bad_request(
                        "Metadata field names must not be empty and must be at most 64 characters",
                    ));
                }
            }
        }
        if let Some(notes) = &input.notes {
            if notes.len() > 500 {
                return Err(UnifiedError::bad_request(
                    "Format notes must be at most 500 characters",
                ));
            }
        }
        let mut format = state
            .watermark_formats
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Watermark format not found"))?;
        if let Some(metadata_supported) = input.metadata_supported {
            format.metadata_supported = metadata_supported;
        }
        if let Some(metadata_fields) = input.metadata_fields {
            format.metadata_fields = metadata_fields;
        }
        if let Some(notes) = input.notes {
            format.notes = notes;
        }
        format.updated_at = now();
        Ok(format.clone())
    })();
    json::respond(result, false)
}

// Vendor / company payout linkage

pub(crate) async fn vendor_payout_summary(
    state: web::Data<Arc<StoreState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    let company = match require_caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, false),
    };
    let company_id = path.into_inner();
    if company_id != company {
        return json::error_response(
            &UnifiedError::forbidden("Payout summary belongs to a different company"),
            false,
        );
    }
    let mut by_currency: HashMap<String, Value> = HashMap::new();
    let mut order_count: u64 = 0;
    for entry in state.orders.iter() {
        let order = entry.value();
        if order.company_id.as_deref() != Some(company_id.as_str()) {
            continue;
        }
        order_count += 1;
        let totals = by_currency
            .entry(order.currency.clone())
            .or_insert_with(|| {
                json!({
                    "currency": order.currency,
                    "orders": 0,
                    "gross": 0,
                    "paid": 0,
                    "refunded": 0,
                })
            });
        let totals = totals.as_object_mut().expect("totals object");
        totals["orders"] = json!(totals["orders"].as_u64().unwrap_or(0) + 1);
        totals["gross"] = json!(totals["gross"].as_i64().unwrap_or(0) + order.total);
        match order.status.as_str() {
            "paid" | "completed" => {
                totals["paid"] = json!(totals["paid"].as_i64().unwrap_or(0) + order.total);
            }
            "refunded" => {
                totals["refunded"] = json!(totals["refunded"].as_i64().unwrap_or(0) + order.total);
            }
            _ => {}
        }
    }
    let mut totals: Vec<Value> = by_currency.into_values().collect();
    // The totals were accumulated in integer minor units; convert
    // them back to major units for the response, as before.
    for totals in &mut totals {
        let Some(map) = totals.as_object_mut() else {
            continue;
        };
        let currency = map
            .get("currency")
            .and_then(|value| value.as_str())
            .unwrap_or("USD")
            .to_string();
        for key in ["gross", "paid", "refunded"] {
            let minor = map.get(key).and_then(|value| value.as_i64()).unwrap_or(0);
            map.insert(key.to_string(), json!(minor_to_major(minor, &currency)));
        }
    }
    totals.sort_by(|a, b| {
        a.get("currency")
            .and_then(|value| value.as_str())
            .cmp(&b.get("currency").and_then(|value| value.as_str()))
    });
    json::success_response(
        json!({
            "company_id": company_id,
            "orders": order_count,
            "totals": totals,
        }),
        200,
    )
}
