// affiliates module — Affiliate and link handlers.
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

use super::constants::{DEFAULT_ATTRIBUTION_WINDOW_DAYS, MAX_CAMPAIGN_LEN, MAX_COMPANY_ID_LEN, MAX_EMAIL_LEN, MAX_METHOD_LEN, MAX_NAME_LEN, MAX_PRODUCT_ID_LEN, MAX_REASON_LEN, MAX_USER_ID_LEN, PERM_AFFILIATES_MANAGE, PERM_AFFILIATES_VIEW};
use super::types::{Affiliate, AffiliateCommissionRule, AffiliateLink, AffiliateStatus, AffiliatesState, CommissionStatus, CommissionType, CreateAffiliateRequest, CreateCommissionRuleRequest, CreateLinkRequest, PayoutStatus, ReviewRequest, UpdateAffiliateRequest};
use super::helpers::{
    Caller, caller, caller_company, require_company_for_write, code_in_use, created, err, generate_code, in_company_scope,
    is_manager, load_affiliate, load_link, ok, paginate, parse_uuid, require_affiliate_access,
    require_affiliate_in_scope, require_company_match, require_company_match_for,
    require_permission, require_tenant, require_text, valid_code, valid_currency, valid_email,
    validate_commission,
};

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_affiliates(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Affiliate>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let user_filter = params.filters.get("user_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let manager = is_manager(&ctx, &caller.role);
        let mut items: Vec<Affiliate> = state
            .affiliates
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|affiliate| {
                // Non-managers only ever see their own record.
                if !manager && affiliate.user_id != caller.actor {
                    return false;
                }
                if !in_company_scope(&affiliate.company_id, &_scope) {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if affiliate.status.as_str() != status {
                        return false;
                    }
                }
                if let Some(company_id) = &company_filter {
                    if affiliate.company_id.as_deref() != Some(company_id.as_str()) {
                        return false;
                    }
                }
                if let Some(user_id) = &user_filter {
                    if affiliate.user_id != *user_id {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !affiliate.display_name.to_lowercase().contains(&search)
                    && !affiliate.email.to_lowercase().contains(&search)
                    && !affiliate.user_id.to_lowercase().contains(&search)
                {
                    return false;
                }
                true
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

pub(crate) async fn create_affiliate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    body: web::Json<CreateAffiliateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Affiliate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_VIEW)?;
        let input = body.into_inner();
        // Only a manager may apply for someone else; everyone else
        // applies as themselves.
        let user_id = match &input.user_id {
            Some(other) if other.trim() != caller.actor => {
                require_permission(&ctx, &caller.role, PERM_AFFILIATES_MANAGE)?;
                require_text(other, "user_id", MAX_USER_ID_LEN)?
            }
            _ => caller.actor.clone(),
        };
        if let Some(reason) = reapply_blocked(&state, &user_id) {
            return Err(UnifiedError::conflict(reason));
        }
        let display_name = require_text(&input.display_name, "display_name", MAX_NAME_LEN)?;
        let email = require_text(&input.email, "email", MAX_EMAIL_LEN)?;
        if !valid_email(&email) {
            return Err(UnifiedError::bad_request("email must be a valid email address"));
        }
        let commission_type = input.commission_type.unwrap_or(CommissionType::Percent);
        let commission_value = validate_commission(
            commission_type,
            input.commission_value.unwrap_or(10.0),
        )?;
        let currency = match &input.currency {
            Some(raw) => valid_currency(raw)?,
            None => "USD".to_string(),
        };
        let attribution_window_days = input
            .attribution_window_days
            .unwrap_or(DEFAULT_ATTRIBUTION_WINDOW_DAYS);
        if !(1..=365).contains(&attribution_window_days) {
            return Err(UnifiedError::bad_request(
                "attribution_window_days must be between 1 and 365",
            ));
        }
        let body_company = match &input.company_id {
            Some(raw) => Some(require_text(raw, "company_id", MAX_COMPANY_ID_LEN)?),
            None => None,
        };
        let company_id = require_company_match(&_tenant, &req, &body_company)?;
        let payout_method = match &input.payout_method {
            Some(raw) => Some(require_text(raw, "payout_method", MAX_METHOD_LEN)?),
            None => None,
        };
        let payout_reference = match &input.payout_reference {
            Some(raw) => Some(require_text(raw, "payout_reference", MAX_NAME_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let affiliate = Affiliate {
            id: Uuid::new_v4(),
            user_id,
            display_name,
            email,
            status: AffiliateStatus::Pending,
            commission_type,
            commission_value,
            currency,
            attribution_window_days,
            company_id,
            payout_method,
            payout_reference,
            reviewed_by: None,
            review_reason: None,
            created_at: now,
            updated_at: now,
        };
        state.affiliates.insert(affiliate.id, affiliate.clone());
        state.emit(
            "affiliates.created",
            json!({
                "affiliate_id": affiliate.id,
                "user_id": affiliate.user_id,
                "status": affiliate.status.as_str(),
            }),
        );
        Ok(affiliate)
    })();
    match result {
        Ok(affiliate) => created(affiliate),
        Err(error) => err(error, debug),
    }
}

/// Whether a user still has money outstanding on one affiliate
/// record: a payout still requested / approved, or a commission
/// still pending / approved. Paid, rejected, and void records no
/// longer block anything.
pub(crate) fn has_outstanding_items(state: &AffiliatesState, affiliate_id: Uuid) -> bool {
    state.payouts.iter().any(|entry| {
        entry.value().affiliate_id == affiliate_id
            && matches!(
                entry.value().status,
                PayoutStatus::Requested | PayoutStatus::Approved
            )
    }) || state.commissions.iter().any(|entry| {
        entry.value().affiliate_id == affiliate_id
            && matches!(
                entry.value().status,
                CommissionStatus::Pending | CommissionStatus::Approved
            )
    })
}

/// Whether a new affiliate application for `user_id` is blocked,
/// returning the conflict message when it is. The lifecycle
/// (see `types::AffiliateStatus`) lets a rejected affiliate
/// re-apply by creating a new record, so a rejected record only
/// blocks when it still has payouts or commissions outstanding;
/// any live (pending / approved / suspended) record always blocks.
pub(crate) fn reapply_blocked(state: &AffiliatesState, user_id: &str) -> Option<String> {
    let existing: Vec<Affiliate> = state
        .affiliates
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|affiliate| affiliate.user_id == user_id)
        .collect();
    if existing.is_empty() {
        return None;
    }
    if existing
        .iter()
        .any(|affiliate| affiliate.status != AffiliateStatus::Rejected)
    {
        return Some(format!(
            "User \"{user_id}\" already has an affiliate record"
        ));
    }
    if existing
        .iter()
        .any(|affiliate| has_outstanding_items(state, affiliate.id))
    {
        return Some(format!(
            "User \"{user_id}\" has a rejected affiliate record with outstanding payouts or commissions"
        ));
    }
    None
}

pub(crate) async fn get_affiliate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Affiliate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_VIEW)?;
        let affiliate = require_affiliate_in_scope(&state, &path, &_scope)?;
        require_affiliate_access(&ctx, &caller, &affiliate)?;
        Ok(affiliate)
    })();
    match result {
        Ok(affiliate) => ok(affiliate),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_affiliate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateAffiliateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Affiliate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_MANAGE)?;
        let input = body.into_inner();
        let mut affiliate = require_affiliate_in_scope(&state, &path, &_scope)?;
        if let Some(raw) = &input.display_name {
            affiliate.display_name = require_text(raw, "display_name", MAX_NAME_LEN)?;
        }
        if let Some(raw) = &input.email {
            let email = require_text(raw, "email", MAX_EMAIL_LEN)?;
            if !valid_email(&email) {
                return Err(UnifiedError::bad_request("email must be a valid email address"));
            }
            affiliate.email = email;
        }
        if let Some(commission_type) = input.commission_type {
            let value = input
                .commission_value
                .unwrap_or(affiliate.commission_value);
            affiliate.commission_value = validate_commission(commission_type, value)?;
            affiliate.commission_type = commission_type;
        } else if let Some(value) = input.commission_value {
            affiliate.commission_value =
                validate_commission(affiliate.commission_type, value)?;
        }
        if let Some(raw) = &input.currency {
            affiliate.currency = valid_currency(raw)?;
        }
        if let Some(window) = input.attribution_window_days {
            if !(1..=365).contains(&window) {
                return Err(UnifiedError::bad_request(
                    "attribution_window_days must be between 1 and 365",
                ));
            }
            affiliate.attribution_window_days = window;
        }
        if let Some(raw) = &input.company_id {
            let company = require_text(raw, "company_id", MAX_COMPANY_ID_LEN)?;
            require_company_match_for(&_scope, &Some(company.clone()))?;
            affiliate.company_id = Some(company);
        }
        if let Some(raw) = &input.payout_method {
            affiliate.payout_method = Some(require_text(raw, "payout_method", MAX_METHOD_LEN)?);
        }
        if let Some(raw) = &input.payout_reference {
            affiliate.payout_reference =
                Some(require_text(raw, "payout_reference", MAX_NAME_LEN)?);
        }
        affiliate.updated_at = Utc::now();
        state.affiliates.insert(affiliate.id, affiliate.clone());
        Ok(affiliate)
    })();
    match result {
        Ok(affiliate) => ok(affiliate),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_affiliate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_MANAGE)?;
        let affiliate = require_affiliate_in_scope(&state, &path, &_scope)?;
        if state
            .conversions
            .iter()
            .any(|entry| entry.value().affiliate_id == affiliate.id)
            || state
                .commissions
                .iter()
                .any(|entry| entry.value().affiliate_id == affiliate.id)
            || state
                .payouts
                .iter()
                .any(|entry| entry.value().affiliate_id == affiliate.id)
        {
            return Err(UnifiedError::conflict(
                "Affiliate has conversions, commissions, or payouts and cannot be deleted; suspend it instead",
            ));
        }
        state.links.retain(|_, link| link.affiliate_id != affiliate.id);
        state
            .commission_rules
            .retain(|_, rule| rule.affiliate_id != affiliate.id);
        state.clicks.retain(|_, click| click.affiliate_id != affiliate.id);
        state.affiliates.remove(&affiliate.id);
        Ok(json!({ "deleted": true, "id": affiliate.id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Shared approval-transition helper: only the listed source
/// statuses may move to `target`.
pub(crate) fn transition_affiliate(
    state: &AffiliatesState,
    caller: &Caller,
    raw_id: &str,
    from: &[AffiliateStatus],
    target: AffiliateStatus,
    event: &str,
    reason: Option<String>,
) -> UnifiedResult<Affiliate> {
    let mut affiliate = load_affiliate(state, raw_id)?;
    if !from.contains(&affiliate.status) {
        return Err(UnifiedError::conflict(format!(
            "Affiliate is {} and cannot move to {}",
            affiliate.status.as_str(),
            target.as_str()
        )));
    }
    affiliate.status = target;
    affiliate.reviewed_by = Some(caller.actor.clone());
    affiliate.review_reason = reason;
    affiliate.updated_at = Utc::now();
    state.affiliates.insert(affiliate.id, affiliate.clone());
    state.emit(
        event,
        json!({
            "affiliate_id": affiliate.id,
            "user_id": affiliate.user_id,
            "status": affiliate.status.as_str(),
            "reviewed_by": caller.actor,
        }),
    );
    Ok(affiliate)
}

pub(crate) async fn approve_affiliate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Affiliate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_MANAGE)?;
        let reason = body
            .reason
            .as_deref()
            .map(|raw| require_text(raw, "reason", MAX_REASON_LEN))
            .transpose()?;
        require_affiliate_in_scope(&state, &path, &_scope)?;
        transition_affiliate(
            &state,
            &caller,
            &path,
            &[AffiliateStatus::Pending],
            AffiliateStatus::Approved,
            "affiliates.approved",
            reason,
        )
    })();
    match result {
        Ok(affiliate) => ok(affiliate),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn reject_affiliate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Affiliate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_MANAGE)?;
        let reason = body
            .reason
            .as_deref()
            .map(|raw| require_text(raw, "reason", MAX_REASON_LEN))
            .transpose()?;
        require_affiliate_in_scope(&state, &path, &_scope)?;
        transition_affiliate(
            &state,
            &caller,
            &path,
            &[AffiliateStatus::Pending],
            AffiliateStatus::Rejected,
            "affiliates.rejected",
            reason,
        )
    })();
    match result {
        Ok(affiliate) => ok(affiliate),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn suspend_affiliate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Affiliate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_MANAGE)?;
        let reason = body
            .reason
            .as_deref()
            .map(|raw| require_text(raw, "reason", MAX_REASON_LEN))
            .transpose()?;
        require_affiliate_in_scope(&state, &path, &_scope)?;
        transition_affiliate(
            &state,
            &caller,
            &path,
            &[AffiliateStatus::Approved],
            AffiliateStatus::Suspended,
            "affiliates.suspended",
            reason,
        )
    })();
    match result {
        Ok(affiliate) => ok(affiliate),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn reactivate_affiliate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Affiliate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_MANAGE)?;
        let reason = body
            .reason
            .as_deref()
            .map(|raw| require_text(raw, "reason", MAX_REASON_LEN))
            .transpose()?;
        require_affiliate_in_scope(&state, &path, &_scope)?;
        transition_affiliate(
            &state,
            &caller,
            &path,
            &[AffiliateStatus::Suspended],
            AffiliateStatus::Approved,
            "affiliates.reactivated",
            reason,
        )
    })();
    match result {
        Ok(affiliate) => ok(affiliate),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_links(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AffiliateLink>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_VIEW)?;
        let affiliate = require_affiliate_in_scope(&state, &path, &_scope)?;
        require_affiliate_access(&ctx, &caller, &affiliate)?;
        let params = query.into_inner();
        let active_filter = params.filters.get("active").cloned();
        let campaign_filter = params.filters.get("campaign").cloned();
        let mut items: Vec<AffiliateLink> = state
            .links
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|link| {
                link.affiliate_id == affiliate.id
                    && active_filter
                        .as_deref()
                        .is_none_or(|raw| link.active.to_string() == raw)
                    && campaign_filter
                        .as_deref()
                        .is_none_or(|raw| link.campaign.as_deref() == Some(raw))
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

pub(crate) async fn create_link(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateLinkRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AffiliateLink> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_VIEW)?;
        let affiliate = require_affiliate_in_scope(&state, &path, &_scope)?;
        require_affiliate_access(&ctx, &caller, &affiliate)?;
        let input = body.into_inner();
        let code = match &input.code {
            Some(raw) => {
                let code = raw.trim().to_lowercase();
                if !valid_code(&code) {
                    return Err(UnifiedError::bad_request(
                        "code must be lowercase letters, digits, and single hyphens only (no leading/trailing hyphen, max 64 characters)",
                    ));
                }
                if code_in_use(&state, &code, None) {
                    return Err(UnifiedError::conflict(format!(
                        "Referral code is already in use: {code}"
                    )));
                }
                code
            }
            None => {
                let mut code = generate_code(&affiliate.display_name);
                let mut attempts = 0;
                while code_in_use(&state, &code, None) && attempts < 10 {
                    code = generate_code(&affiliate.display_name);
                    attempts += 1;
                }
                if code_in_use(&state, &code, None) {
                    return Err(UnifiedError::conflict(
                        "Could not generate a unique referral code; try again with an explicit code",
                    ));
                }
                code
            }
        };
        let campaign = match &input.campaign {
            Some(raw) => Some(require_text(raw, "campaign", MAX_CAMPAIGN_LEN)?),
            None => None,
        };
        let product_id = match &input.product_id {
            Some(raw) => Some(require_text(raw, "product_id", MAX_PRODUCT_ID_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let link = AffiliateLink {
            id: Uuid::new_v4(),
            affiliate_id: affiliate.id,
            code,
            campaign,
            product_id,
            active: true,
            created_at: now,
            updated_at: now,
        };
        state.links.insert(link.id, link.clone());
        Ok(link)
    })();
    match result {
        Ok(link) => created(link),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_link(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AffiliateLink> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_VIEW)?;
        let link = load_link(&state, &path)?;
        let affiliate =
            require_affiliate_in_scope(&state, &link.affiliate_id.to_string(), &_scope)?;
        require_affiliate_access(&ctx, &caller, &affiliate)?;
        Ok(link)
    })();
    match result {
        Ok(link) => ok(link),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_link(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_VIEW)?;
        let link = load_link(&state, &path)?;
        let affiliate =
            require_affiliate_in_scope(&state, &link.affiliate_id.to_string(), &_scope)?;
        require_affiliate_access(&ctx, &caller, &affiliate)?;
        if state
            .clicks
            .iter()
            .any(|entry| entry.value().link_id == link.id)
        {
            return Err(UnifiedError::conflict(
                "Referral link has recorded clicks and cannot be deleted; keep the link for audit instead",
            ));
        }
        state.links.remove(&link.id);
        Ok(json!({ "deleted": true, "id": link.id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_commission_rules(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AffiliateCommissionRule>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_VIEW)?;
        let affiliate = require_affiliate_in_scope(&state, &path, &_scope)?;
        require_affiliate_access(&ctx, &caller, &affiliate)?;
        let params = query.into_inner();
        let mut items: Vec<AffiliateCommissionRule> = state
            .commission_rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| rule.affiliate_id == affiliate.id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_commission_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateCommissionRuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AffiliateCommissionRule> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_MANAGE)?;
        let affiliate = require_affiliate_in_scope(&state, &path, &_scope)?;
        let input = body.into_inner();
        let product_id = require_text(&input.product_id, "product_id", MAX_PRODUCT_ID_LEN)?;
        let value = validate_commission(input.commission_type, input.commission_value)?;
        if state.commission_rules.iter().any(|entry| {
            entry.value().affiliate_id == affiliate.id && entry.value().product_id == product_id
        }) {
            return Err(UnifiedError::conflict(format!(
                "A commission rule already exists for product \"{product_id}\" on this affiliate"
            )));
        }
        let now = Utc::now();
        let rule = AffiliateCommissionRule {
            id: Uuid::new_v4(),
            affiliate_id: affiliate.id,
            product_id,
            commission_type: input.commission_type,
            commission_value: value,
            created_at: now,
            updated_at: now,
        };
        state.commission_rules.insert(rule.id, rule.clone());
        Ok(rule)
    })();
    match result {
        Ok(rule) => created(rule),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_commission_rule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<AffiliatesState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;

        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_AFFILIATES_MANAGE)?;
        let (affiliate_id, rule_id) = path.into_inner();
        let affiliate = require_affiliate_in_scope(&state, &affiliate_id, &_scope)?;
        let rule_uuid = parse_uuid(&rule_id, "commission rule")?;
        let rule = state
            .commission_rules
            .get(&rule_uuid)
            .map(|entry| entry.value().clone())
            .filter(|rule| rule.affiliate_id == affiliate.id)
            .ok_or_else(|| UnifiedError::not_found(format!("Commission rule not found: {rule_id}")))?;
        state.commission_rules.remove(&rule.id);
        Ok(json!({ "deleted": true, "id": rule.id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::affiliates::{AffiliateCommission, AffiliatePayout};

    fn test_affiliate(user_id: &str, status: AffiliateStatus) -> Affiliate {
        Affiliate {
            id: Uuid::new_v4(),
            user_id: user_id.to_string(),
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

    fn test_commission(affiliate_id: Uuid, status: CommissionStatus) -> AffiliateCommission {
        AffiliateCommission {
            id: Uuid::new_v4(),
            affiliate_id,
            conversion_id: Uuid::new_v4(),
            order_id: None,
            product_id: None,
            commission_type: CommissionType::Percent,
            rate_or_amount: 10.0,
            amount: 2000,
            currency: "USD".to_string(),
            status,
            payout_id: None,
            flagged: false,
            fraud_flags: Vec::new(),
            reviewed_by: None,
            review_reason: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn test_payout(affiliate_id: Uuid, status: PayoutStatus) -> AffiliatePayout {
        AffiliatePayout {
            id: Uuid::new_v4(),
            affiliate_id,
            commission_ids: Vec::new(),
            total_amount: 2000,
            currency: "USD".to_string(),
            status,
            method: None,
            reference: None,
            requested_by: "user-1".to_string(),
            reviewed_by: None,
            review_reason: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn reapply_is_allowed_with_no_record_and_for_a_clean_rejected_record() {
        let state = AffiliatesState::default();
        assert!(reapply_blocked(&state, "user-1").is_none());
        let aff = test_affiliate("user-1", AffiliateStatus::Rejected);
        state.affiliates.insert(aff.id, aff.clone());
        assert!(reapply_blocked(&state, "user-1").is_none());
    }

    #[test]
    fn reapply_is_blocked_by_any_live_affiliate_record() {
        for status in [
            AffiliateStatus::Pending,
            AffiliateStatus::Approved,
            AffiliateStatus::Suspended,
        ] {
            let state = AffiliatesState::default();
            let aff = test_affiliate("user-1", status);
            state.affiliates.insert(aff.id, aff.clone());
            assert!(reapply_blocked(&state, "user-1").is_some());
        }
    }

    #[test]
    fn reapply_is_blocked_while_a_rejected_record_has_outstanding_items() {
        let state = AffiliatesState::default();
        let aff = test_affiliate("user-1", AffiliateStatus::Rejected);
        state.affiliates.insert(aff.id, aff.clone());
        let commission = test_commission(aff.id, CommissionStatus::Pending);
        state.commissions.insert(commission.id, commission);
        assert!(reapply_blocked(&state, "user-1").is_some());

        let state = AffiliatesState::default();
        let aff = test_affiliate("user-1", AffiliateStatus::Rejected);
        state.affiliates.insert(aff.id, aff.clone());
        let payout = test_payout(aff.id, PayoutStatus::Requested);
        state.payouts.insert(payout.id, payout);
        assert!(reapply_blocked(&state, "user-1").is_some());
    }

    #[test]
    fn settled_items_no_longer_block_reapplying() {
        let state = AffiliatesState::default();
        let aff = test_affiliate("user-1", AffiliateStatus::Rejected);
        state.affiliates.insert(aff.id, aff.clone());
        let commission = test_commission(aff.id, CommissionStatus::Void);
        state.commissions.insert(commission.id, commission);
        let payout = test_payout(aff.id, PayoutStatus::Rejected);
        state.payouts.insert(payout.id, payout);
        assert!(reapply_blocked(&state, "user-1").is_none());
    }
}
