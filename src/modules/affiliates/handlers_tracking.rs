// affiliates module — Click, conversion, commission, and payout handlers.
//
// Split from the former single-file module `src/modules/affiliates.rs`.
// The module root (`affiliates.rs`) re-exports the public items, so the
// public API is unchanged.

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
use sha2::{Digest, Sha256};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::{MAX_METHOD_LEN, MAX_NAME_LEN, MAX_PRODUCT_ID_LEN, MAX_REASON_LEN, MAX_REFERRER_LEN, MAX_USER_AGENT_LEN, MAX_USER_ID_LEN, PERM_AFFILIATES_MANAGE, PERM_AFFILIATES_VIEW};
use super::types::{Affiliate, AffiliateClick, AffiliateCommission, AffiliateConversion, AffiliatePayout, AffiliateStatus, AffiliatesState, CommissionStatus, CreateConversionRequest, MarkPaidRequest, PayoutStatus, RequestPayoutRequest, ReviewRequest, TrackClickRequest};
use super::helpers::{
    Caller, caller, caller_company, require_company_for_write, created, dashboard_for, err, fraud_flags_for_click, hash_ip,
    load_affiliate_by_user, load_commission, load_link, load_link_by_code, load_payout,
    minor_to_major, ok, paginate, parse_uuid, record_conversion, require_affiliate_access,
    require_affiliate_commission_in_scope, require_affiliate_in_scope,
    require_affiliate_payout_in_scope, require_permission, require_record_in_company_scope,
    require_tenant, require_text, valid_code, valid_currency, valid_price, visitor_ip,
};

/// Clicks only count for approved affiliates. A missing affiliate
/// record (orphaned link) counts as not approved, so the click is
/// skipped the same way an inactive link is.
pub(crate) fn affiliate_may_receive_clicks(affiliate: Option<&Affiliate>) -> bool {
    affiliate.is_some_and(|affiliate| affiliate.status == AffiliateStatus::Approved)
}

/// Public click tracking. No caller identity and no capability
/// check here by design: clicks come from anonymous visitors. The
/// route is gated by the API Manager `ApiAccessRule` for
/// `POST /api/v1/affiliates/track` (see the header doc). The raw
/// visitor IP is hashed immediately and never stored.
pub(crate) async fn track_click(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    body: web::Json<TrackClickRequest>,
) -> HttpResponse {
    let result: UnifiedResult<AffiliateClick> = (|| {
        let input = body.into_inner();
        let code = input.code.trim().to_lowercase();
        if !valid_code(&code) {
            return Err(UnifiedError::bad_request("code is not a valid referral code"));
        }
        let link = load_link_by_code(&state, &code)?;
        if !link.active {
            return Err(UnifiedError::not_found(format!(
                "Referral code not found: {code}"
            )));
        }
        // Clicks only count for approved affiliates: a link whose
        // affiliate is pending, suspended, or rejected behaves
        // exactly like an inactive link, so no clicks accumulate
        // before approval or after suspension / rejection.
        let affiliate = state
            .affiliates
            .get(&link.affiliate_id)
            .map(|entry| entry.value().clone());
        if !affiliate_may_receive_clicks(affiliate.as_ref()) {
            return Err(UnifiedError::not_found(format!(
                "Referral code not found: {code}"
            )));
        }
        let settings = ctx
            .config
            .read()
            .expect("config lock poisoned")
            .security
            .clone();
        let ip = visitor_ip(&req, input.ip.as_deref(), &settings);
        let ip_hash = hash_ip(&ip);
        let now = Utc::now();
        // Fraud flags are advisory only: the click is recorded
        // either way and flagging never blocks anything.
        let flag_reasons = fraud_flags_for_click(&state, link.id, &ip_hash, now);
        let flagged = !flag_reasons.is_empty();
        let user_agent = match &input.user_agent {
            Some(raw) if !raw.trim().is_empty() => {
                Some(raw.trim().chars().take(MAX_USER_AGENT_LEN).collect())
            }
            _ => None,
        };
        let referrer = match &input.referrer {
            Some(raw) if !raw.trim().is_empty() => {
                Some(raw.trim().chars().take(MAX_REFERRER_LEN).collect())
            }
            _ => None,
        };
        let click = AffiliateClick {
            id: Uuid::new_v4(),
            link_id: link.id,
            affiliate_id: link.affiliate_id,
            ip_hash,
            user_agent,
            referrer,
            flagged,
            flag_reasons: flag_reasons.clone(),
            converted: false,
            clicked_at: now,
        };
        state.clicks.insert(click.id, click.clone());
        if flagged {
            state.emit(
                "affiliates.click.flagged",
                json!({
                    "click_id": click.id,
                    "link_id": link.id,
                    "affiliate_id": link.affiliate_id,
                    "flag_reasons": flag_reasons,
                }),
            );
        }
        Ok(click)
    })();
    match result {
        Ok(click) => created(click),
        Err(error) => err(error, false),
    }
}

pub(crate) async fn list_clicks(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AffiliateClick>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_VIEW)?;
        let affiliate = require_affiliate_in_scope(&state, &path, &_scope)?;
        require_affiliate_access(&ctx, &caller, &affiliate)?;
        let params = query.into_inner();
        let flagged_filter = params.filters.get("flagged").cloned();
        let link_filter = params.filters.get("link_id").cloned();
        let mut items: Vec<AffiliateClick> = state
            .clicks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|click| {
                click.affiliate_id == affiliate.id
                    && flagged_filter
                        .as_deref()
                        .is_none_or(|raw| click.flagged.to_string() == raw)
                    && link_filter
                        .as_deref()
                        .is_none_or(|raw| click.link_id.to_string() == raw)
            })
            .collect();
        items.sort_by(|a, b| b.clicked_at.cmp(&a.clicked_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_conversions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AffiliateConversion>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_VIEW)?;
        let affiliate = require_affiliate_in_scope(&state, &path, &_scope)?;
        require_affiliate_access(&ctx, &caller, &affiliate)?;
        let params = query.into_inner();
        let kind_filter = params.filters.get("kind").cloned();
        let flagged_filter = params.filters.get("flagged").cloned();
        let mut items: Vec<AffiliateConversion> = state
            .conversions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|conversion| {
                conversion.affiliate_id == affiliate.id
                    && kind_filter
                        .as_deref()
                        .is_none_or(|raw| conversion.kind == raw)
                    && flagged_filter
                        .as_deref()
                        .is_none_or(|raw| conversion.flagged.to_string() == raw)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_conversion(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    body: web::Json<CreateConversionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AffiliateConversion> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_MANAGE)?;
        let input = body.into_inner();
        let affiliate = require_affiliate_in_scope(&state, &input.affiliate_id, &_scope)?;
        if affiliate.status != AffiliateStatus::Approved {
            return Err(UnifiedError::conflict(format!(
                "Affiliate is {} and cannot earn conversions",
                affiliate.status.as_str()
            )));
        }
        let kind = input.kind.trim().to_lowercase();
        if kind != "signup" && kind != "purchase" {
            return Err(UnifiedError::bad_request(
                "kind must be \"signup\" or \"purchase\"",
            ));
        }
        let customer_user_id =
            require_text(&input.customer_user_id, "customer_user_id", MAX_USER_ID_LEN)?;
        let link_id = match &input.link_id {
            Some(raw) => {
                let link = load_link(&state, raw)?;
                if link.affiliate_id != affiliate.id {
                    return Err(UnifiedError::bad_request(
                        "link_id does not belong to this affiliate",
                    ));
                }
                Some(link.id)
            }
            None => None,
        };
        let click_id = match &input.click_id {
            Some(raw) => {
                let id = parse_uuid(raw, "click")?;
                let click = state
                    .clicks
                    .get(&id)
                    .map(|entry| entry.value().clone())
                    .ok_or_else(|| UnifiedError::not_found(format!("Click not found: {id}")))?;
                if click.affiliate_id != affiliate.id {
                    return Err(UnifiedError::bad_request(
                        "click_id does not belong to this affiliate",
                    ));
                }
                Some(click.id)
            }
            None => None,
        };
        let order_id = match &input.order_id {
            Some(raw) => Some(require_text(raw, "order_id", MAX_NAME_LEN)?),
            None => None,
        };
        if kind == "purchase" {
            if let Some(order_id) = &order_id {
                if state.conversions.iter().any(|entry| {
                    entry.value().order_id.as_deref() == Some(order_id.as_str())
                        && entry.value().kind == "purchase"
                }) {
                    return Err(UnifiedError::conflict(format!(
                        "Order \"{order_id}\" already has a purchase conversion"
                    )));
                }
            }
        }
        let order_total = match input.order_total {
            Some(raw) => valid_price(raw)?,
            None => 0,
        };
        let currency = match &input.currency {
            Some(raw) => valid_currency(raw)?,
            None => affiliate.currency.clone(),
        };
        let product_id = match &input.product_id {
            Some(raw) => Some(require_text(raw, "product_id", MAX_PRODUCT_ID_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let (conversion, _) = record_conversion(
            &state,
            &affiliate,
            link_id,
            click_id,
            &customer_user_id,
            &kind,
            order_id,
            order_total,
            &currency,
            product_id,
            now,
        )?;
        Ok(conversion)
    })();
    match result {
        Ok(conversion) => created(conversion),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_commissions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AffiliateCommission>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_VIEW)?;
        let affiliate = require_affiliate_in_scope(&state, &path, &_scope)?;
        require_affiliate_access(&ctx, &caller, &affiliate)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let flagged_filter = params.filters.get("flagged").cloned();
        let mut items: Vec<AffiliateCommission> = state
            .commissions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|commission| {
                commission.affiliate_id == affiliate.id
                    && status_filter
                        .as_deref()
                        .is_none_or(|raw| commission.status.as_str() == raw)
                    && flagged_filter
                        .as_deref()
                        .is_none_or(|raw| commission.flagged.to_string() == raw)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn approve_commission(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AffiliateCommission> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_MANAGE)?;
        let mut commission = require_affiliate_commission_in_scope(&state, &path, &_scope)?;
        if commission.status != CommissionStatus::Pending {
            return Err(UnifiedError::conflict(format!(
                "Commission is {} and cannot be approved",
                commission.status.as_str()
            )));
        }
        let reason = body
            .reason
            .as_deref()
            .map(|raw| require_text(raw, "reason", MAX_REASON_LEN))
            .transpose()?;
        commission.status = CommissionStatus::Approved;
        commission.reviewed_by = Some(caller.actor.clone());
        commission.review_reason = reason;
        commission.updated_at = Utc::now();
        state.commissions.insert(commission.id, commission.clone());
        state.emit(
            "affiliates.commission.approved",
            json!({
                "commission_id": commission.id,
                "affiliate_id": commission.affiliate_id,
                "amount": minor_to_major(commission.amount, &commission.currency),
                "currency": commission.currency,
                "reviewed_by": caller.actor,
            }),
        );
        Ok(commission)
    })();
    match result {
        Ok(commission) => ok(commission),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn void_commission(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AffiliateCommission> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_MANAGE)?;
        let mut commission = require_affiliate_commission_in_scope(&state, &path, &_scope)?;
        if commission.status != CommissionStatus::Pending
            && commission.status != CommissionStatus::Approved
        {
            return Err(UnifiedError::conflict(format!(
                "Commission is {} and cannot be voided",
                commission.status.as_str()
            )));
        }
        if commission.payout_id.is_some() {
            return Err(UnifiedError::conflict(
                "Commission is attached to a payout request; reject the payout first to release it",
            ));
        }
        let reason = body
            .reason
            .as_deref()
            .map(|raw| require_text(raw, "reason", MAX_REASON_LEN))
            .transpose()?;
        commission.status = CommissionStatus::Void;
        commission.reviewed_by = Some(caller.actor.clone());
        commission.review_reason = reason;
        commission.updated_at = Utc::now();
        state.commissions.insert(commission.id, commission.clone());
        state.emit(
            "affiliates.commission.voided",
            json!({
                "commission_id": commission.id,
                "affiliate_id": commission.affiliate_id,
                "amount": minor_to_major(commission.amount, &commission.currency),
                "currency": commission.currency,
                "reviewed_by": caller.actor,
            }),
        );
        Ok(commission)
    })();
    match result {
        Ok(commission) => ok(commission),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_payouts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AffiliatePayout>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_VIEW)?;
        let affiliate = require_affiliate_in_scope(&state, &path, &_scope)?;
        require_affiliate_access(&ctx, &caller, &affiliate)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<AffiliatePayout> = state
            .payouts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|payout| {
                payout.affiliate_id == affiliate.id
                    && status_filter
                        .as_deref()
                        .is_none_or(|raw| payout.status.as_str() == raw)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

/// A payout request whose commission list repeats an id is
/// rejected (400): a duplicate would double-count the commission
/// in the payout total and attach it twice.
pub(crate) fn reject_duplicate_commission_ids(ids: &[Uuid]) -> UnifiedResult<()> {
    let mut seen = std::collections::HashSet::new();
    for id in ids {
        if !seen.insert(*id) {
            return Err(UnifiedError::bad_request(format!(
                "Duplicate commission id in payout request: {id}"
            )));
        }
    }
    Ok(())
}

/// Payouts are only for approved affiliates: a pending,
/// suspended, or rejected affiliate must not be able to drain
/// commissions through a payout request.
pub(crate) fn require_approved_for_payout(affiliate: &Affiliate) -> UnifiedResult<()> {
    if affiliate.status != AffiliateStatus::Approved {
        return Err(UnifiedError::conflict(format!(
            "Affiliate is {} and cannot request payouts",
            affiliate.status.as_str()
        )));
    }
    Ok(())
}

pub(crate) async fn request_payout(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RequestPayoutRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AffiliatePayout> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_VIEW)?;
        let affiliate = require_affiliate_in_scope(&state, &path, &_scope)?;
        require_affiliate_access(&ctx, &caller, &affiliate)?;
        require_approved_for_payout(&affiliate)?;
        let input = body.into_inner();
        let mut commissions: Vec<AffiliateCommission> = match &input.commission_ids {
            Some(raw_ids) => {
                let mut found = Vec::new();
                for raw in raw_ids {
                    let commission = load_commission(&state, raw)?;
                    if commission.affiliate_id != affiliate.id {
                        return Err(UnifiedError::bad_request(
                            "One of the commission ids does not belong to this affiliate",
                        ));
                    }
                    found.push(commission);
                }
                found
            }
            None => state
                .commissions
                .iter()
                .map(|entry| entry.value().clone())
                .filter(|commission| {
                    commission.affiliate_id == affiliate.id
                        && commission.status == CommissionStatus::Approved
                        && commission.payout_id.is_none()
                })
                .collect(),
        };
        reject_duplicate_commission_ids(
            &commissions
                .iter()
                .map(|commission| commission.id)
                .collect::<Vec<_>>(),
        )?;
        commissions.retain(|commission| {
            commission.status == CommissionStatus::Approved && commission.payout_id.is_none()
        });
        if commissions.is_empty() {
            return Err(UnifiedError::bad_request(
                "No approved, unattached commissions are available for payout",
            ));
        }
        // All commissions in one payout must share a currency.
        let currency = commissions[0].currency.clone();
        if commissions.iter().any(|commission| commission.currency != currency) {
            return Err(UnifiedError::bad_request(
                "All commissions in one payout must share the same currency",
            ));
        }
        let total: i64 = commissions.iter().map(|commission| commission.amount).sum();
        let method = match &input.method {
            Some(raw) => Some(require_text(raw, "method", MAX_METHOD_LEN)?),
            None => affiliate.payout_method.clone(),
        };
        let now = Utc::now();
        let payout = AffiliatePayout {
            id: Uuid::new_v4(),
            affiliate_id: affiliate.id,
            commission_ids: commissions.iter().map(|commission| commission.id).collect(),
            total_amount: total,
            currency: currency.clone(),
            status: PayoutStatus::Requested,
            method,
            reference: None,
            requested_by: caller.actor.clone(),
            reviewed_by: None,
            review_reason: None,
            created_at: now,
            updated_at: now,
        };
        state.payouts.insert(payout.id, payout.clone());
        for commission in &commissions {
            if let Some(mut entry) = state.commissions.get_mut(&commission.id) {
                entry.payout_id = Some(payout.id);
                entry.updated_at = now;
            }
        }
        // Request only: the billing module owns the actual payment.
        state.emit(
            "affiliates.payout.requested",
            json!({
                "payout_id": payout.id,
                "affiliate_id": affiliate.id,
                "amount": minor_to_major(payout.total_amount, &payout.currency),
                "currency": currency,
                "method": payout.method,
                "commission_ids": payout.commission_ids,
                "requested_by": caller.actor,
            }),
        );
        Ok(payout)
    })();
    match result {
        Ok(payout) => created(payout),
        Err(error) => err(error, debug),
    }
}

/// Shared payout transition helper (approve / mark-paid / reject).
pub(crate) fn transition_payout(
    state: &AffiliatesState,
    caller: &Caller,
    raw_id: &str,
    from: &[PayoutStatus],
    target: PayoutStatus,
    event: &str,
    reason: Option<String>,
    reference: Option<String>,
) -> UnifiedResult<AffiliatePayout> {
    let mut payout = load_payout(state, raw_id)?;
    if !from.contains(&payout.status) {
        return Err(UnifiedError::conflict(format!(
            "Payout is {} and cannot move to {}",
            payout.status.as_str(),
            target.as_str()
        )));
    }
    let now = Utc::now();
    payout.status = target;
    payout.reviewed_by = Some(caller.actor.clone());
    payout.review_reason = reason;
    if let Some(reference) = reference {
        payout.reference = Some(reference);
    }
    payout.updated_at = now;
    match target {
        PayoutStatus::Paid => {
            for commission_id in &payout.commission_ids {
                if let Some(mut entry) = state.commissions.get_mut(commission_id) {
                    entry.status = CommissionStatus::Paid;
                    entry.updated_at = now;
                }
            }
        }
        PayoutStatus::Rejected => {
            // Release the commissions back to approved, unattached.
            for commission_id in &payout.commission_ids {
                if let Some(mut entry) = state.commissions.get_mut(commission_id) {
                    entry.payout_id = None;
                    entry.updated_at = now;
                }
            }
        }
        _ => {}
    }
    state.payouts.insert(payout.id, payout.clone());
    state.emit(
        event,
        json!({
            "payout_id": payout.id,
            "affiliate_id": payout.affiliate_id,
            "amount": minor_to_major(payout.total_amount, &payout.currency),
            "currency": payout.currency,
            "status": payout.status.as_str(),
            "reviewed_by": caller.actor,
        }),
    );
    Ok(payout)
}

pub(crate) async fn approve_payout(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AffiliatePayout> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_MANAGE)?;
        let reason = body
            .reason
            .as_deref()
            .map(|raw| require_text(raw, "reason", MAX_REASON_LEN))
            .transpose()?;
        require_affiliate_payout_in_scope(&state, &path, &_scope)?;
        transition_payout(
            &state,
            &caller,
            &path,
            &[PayoutStatus::Requested],
            PayoutStatus::Approved,
            "affiliates.payout.approved",
            reason,
            None,
        )
    })();
    match result {
        Ok(payout) => ok(payout),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn mark_payout_paid(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<MarkPaidRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AffiliatePayout> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_MANAGE)?;
        let reference = body
            .reference
            .as_deref()
            .map(|raw| require_text(raw, "reference", MAX_NAME_LEN))
            .transpose()?;
        require_affiliate_payout_in_scope(&state, &path, &_scope)?;
        transition_payout(
            &state,
            &caller,
            &path,
            &[PayoutStatus::Approved],
            PayoutStatus::Paid,
            "affiliates.payout.paid",
            None,
            reference,
        )
    })();
    match result {
        Ok(payout) => ok(payout),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn reject_payout(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AffiliatePayout> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_MANAGE)?;
        let reason = body
            .reason
            .as_deref()
            .map(|raw| require_text(raw, "reason", MAX_REASON_LEN))
            .transpose()?;
        require_affiliate_payout_in_scope(&state, &path, &_scope)?;
        transition_payout(
            &state,
            &caller,
            &path,
            &[PayoutStatus::Requested, PayoutStatus::Approved],
            PayoutStatus::Rejected,
            "affiliates.payout.rejected",
            reason,
            None,
        )
    })();
    match result {
        Ok(payout) => ok(payout),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_dashboard(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_VIEW)?;
        let affiliate = require_affiliate_in_scope(&state, &path, &_scope)?;
        require_affiliate_access(&ctx, &caller, &affiliate)?;
        Ok(dashboard_for(&state, affiliate.id))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn my_dashboard(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_VIEW)?;
        let affiliate = load_affiliate_by_user(&state, &caller.actor)?;
        require_record_in_company_scope(&affiliate.company_id, &_scope)?;
        Ok(dashboard_for(&state, affiliate.id))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_affiliate(status: AffiliateStatus) -> Affiliate {
        Affiliate {
            id: Uuid::new_v4(),
            user_id: "user-1".to_string(),
            display_name: "Acme".to_string(),
            email: "acme@example.com".to_string(),
            status,
            commission_type: CommissionType::Percent,
            commission_value: 10.0,
            currency: "USD".to_string(),
            attribution_window_days: 30,
            company_id: None,
            payout_method: None,
            payout_reference: None,
            reviewed_by: None,
            review_reason: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn duplicate_commission_ids_are_rejected() {
        let id = Uuid::new_v4();
        assert!(reject_duplicate_commission_ids(&[]).is_ok());
        assert!(reject_duplicate_commission_ids(&[id, Uuid::new_v4()]).is_ok());
        assert!(reject_duplicate_commission_ids(&[id, id]).is_err());
    }

    #[test]
    fn payout_requires_an_approved_affiliate() {
        assert!(require_approved_for_payout(&test_affiliate(AffiliateStatus::Approved)).is_ok());
        for status in [
            AffiliateStatus::Pending,
            AffiliateStatus::Suspended,
            AffiliateStatus::Rejected,
        ] {
            assert!(require_approved_for_payout(&test_affiliate(status)).is_err());
        }
    }

    #[test]
    fn clicks_only_count_for_approved_affiliates() {
        assert!(affiliate_may_receive_clicks(Some(
            &test_affiliate(AffiliateStatus::Approved)
        )));
        for status in [
            AffiliateStatus::Pending,
            AffiliateStatus::Suspended,
            AffiliateStatus::Rejected,
        ] {
            assert!(!affiliate_may_receive_clicks(Some(&test_affiliate(status))));
        }
        assert!(!affiliate_may_receive_clicks(None));
    }
}
