//! Recruitment - HTTP handlers for offers, and the hire handoff.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::{constants::*, types::*, state::*, helpers::*, module::*, handlers_jobs::*, handlers_applicants::*, handlers_interviews::*, handlers_evaluations::*};

// Offer handlers

pub(crate) async fn list_offers(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_VIEW)?;
        let applicant = load_applicant(&state, &path.into_inner())?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<Offer> = state
            .offers
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|offer| {
                if offer.applicant_id != applicant.id {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if offer.status.as_str() != status.as_str() {
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

pub(crate) async fn create_offer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateOfferRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Offer> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let applicant = load_applicant(&state, &path.into_inner())?;
        if !matches!(applicant.stage, ApplicantStage::Interview | ApplicantStage::Offer) {
            return Err(UnifiedError::conflict(format!(
                "Offers can only be created while the applicant is in interview or offer (now: {})",
                applicant.stage.as_str()
            )));
        }
        let input = body.into_inner();
        let salary = valid_price(input.salary)?;
        let currency = match &input.currency {
            Some(raw) => valid_currency(raw)?,
            None => "USD".to_string(),
        };
        let terms = match &input.terms {
            Some(raw) => Some(require_text(raw, "terms", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let offer = Offer {
            id: Uuid::now_v7(),
            applicant_id: applicant.id,
            job_id: applicant.job_id,
            salary,
            currency,
            start_date: input.start_date,
            terms,
            status: OfferStatus::Draft,
            created_by: caller.actor.clone(),
            sent_at: None,
            decided_at: None,
            created_at: now,
            updated_at: now,
        };
        state.offers.insert(offer.id, offer.clone());
        state.emit(
            "recruitment.offer.created",
            json!({
                "offer_id": offer.id.to_string(),
                "applicant_id": applicant.id.to_string(),
                "job_id": applicant.job_id.to_string(),
                "actor": caller.actor,
            }),
        );
        Ok(offer)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_offer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Offer> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_VIEW)?;
        load_offer(&state, &path.into_inner())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_offer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateOfferRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Offer> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let mut offer = load_offer(&state, &path.into_inner())?;
        if offer.status != OfferStatus::Draft {
            return Err(UnifiedError::conflict(format!(
                "Only a draft offer can be updated (offer is {})",
                offer.status.as_str()
            )));
        }
        let input = body.into_inner();
        if let Some(salary) = input.salary {
            offer.salary = valid_price(salary)?;
        }
        if let Some(raw) = &input.currency {
            offer.currency = valid_currency(raw)?;
        }
        if let Some(raw) = &input.start_date {
            offer.start_date = *raw;
        }
        if let Some(raw) = &input.terms {
            offer.terms = match raw {
                Some(value) => Some(require_text(value, "terms", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        offer.updated_at = Utc::now();
        state.offers.insert(offer.id, offer.clone());
        state.emit(
            "recruitment.offer.updated",
            json!({
                "offer_id": offer.id.to_string(),
                "applicant_id": offer.applicant_id.to_string(),
                "actor": caller.actor,
            }),
        );
        Ok(offer)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Shared offer decision: send / accept / decline / expire. Only
/// the transitions listed in the error message are allowed; send
/// additionally requires the applicant's contact consent.
pub(crate) fn transition_offer(
    state: &RecruitmentState,
    offer: &mut Offer,
    from: OfferStatus,
    to: OfferStatus,
    event: &str,
    actor: &str,
) -> UnifiedResult<()> {
    if offer.status != from {
        return Err(UnifiedError::conflict(format!(
            "Offer is {} and cannot move to {}",
            offer.status.as_str(),
            to.as_str()
        )));
    }
    if to == OfferStatus::Sent {
        let applicant = state
            .applicants
            .get(&offer.applicant_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Applicant not found: {}", offer.applicant_id))
            })?;
        require_consent(&applicant, "sending an offer")?;
        offer.sent_at = Some(Utc::now());
    }
    if matches!(
        to,
        OfferStatus::Accepted | OfferStatus::Declined | OfferStatus::Expired
    ) {
        offer.decided_at = Some(Utc::now());
    }
    offer.status = to;
    offer.updated_at = Utc::now();
    state.offers.insert(offer.id, offer.clone());
    state.emit(
        event,
        json!({
            "offer_id": offer.id.to_string(),
            "applicant_id": offer.applicant_id.to_string(),
            "job_id": offer.job_id.to_string(),
            "status": to.as_str(),
            "actor": actor,
        }),
    );
    Ok(())
}

pub(crate) async fn send_offer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Offer> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let mut offer = load_offer(&state, &path.into_inner())?;
        transition_offer(
            &state,
            &mut offer,
            OfferStatus::Draft,
            OfferStatus::Sent,
            "recruitment.offer.sent",
            &caller.actor,
        )?;
        Ok(offer)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn accept_offer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Offer> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let mut offer = load_offer(&state, &path.into_inner())?;
        transition_offer(
            &state,
            &mut offer,
            OfferStatus::Sent,
            OfferStatus::Accepted,
            "recruitment.offer.accepted",
            &caller.actor,
        )?;
        Ok(offer)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn decline_offer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Offer> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let mut offer = load_offer(&state, &path.into_inner())?;
        transition_offer(
            &state,
            &mut offer,
            OfferStatus::Sent,
            OfferStatus::Declined,
            "recruitment.offer.declined",
            &caller.actor,
        )?;
        Ok(offer)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn expire_offer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Offer> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let mut offer = load_offer(&state, &path.into_inner())?;
        transition_offer(
            &state,
            &mut offer,
            OfferStatus::Sent,
            OfferStatus::Expired,
            "recruitment.offer.expired",
            &caller.actor,
        )?;
        Ok(offer)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// Hire handler (HR onboarding handoff)

pub(crate) async fn hire_applicant(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let mut applicant = load_applicant(&state, &path.into_inner())?;
        if applicant.stage != ApplicantStage::Offer {
            return Err(UnifiedError::conflict(format!(
                "Only an applicant in the offer stage can be hired (now: {})",
                applicant.stage.as_str()
            )));
        }
        let accepted_offer = state
            .offers
            .iter()
            .map(|entry| entry.value().clone())
            .find(|offer| {
                offer.applicant_id == applicant.id && offer.status == OfferStatus::Accepted
            })
            .ok_or_else(|| {
                UnifiedError::conflict(
                    "Hiring requires an accepted offer for this applicant",
                )
            })?;
        let from_stage = applicant.stage;
        applicant.stage = ApplicantStage::Hired;
        applicant.updated_at = Utc::now();
        state.applicants.insert(applicant.id, applicant.clone());
        record_stage_move(
            &state,
            &applicant,
            Some(from_stage),
            ApplicantStage::Hired,
            &caller.actor,
            None,
        );
        state.emit(
            "recruitment.hired",
            json!({
                "applicant_id": applicant.id.to_string(),
                "job_id": applicant.job_id.to_string(),
                "offer_id": accepted_offer.id.to_string(),
                "actor": caller.actor,
            }),
        );
        // Hire handoff: the HR module owns the onboarding record.
        // This module creates nothing on the HR side -- it only
        // emits the event with everything HR needs to start.
        let job = state
            .jobs
            .get(&applicant.job_id)
            .map(|entry| entry.value().clone());
        state.emit(
            "hr.onboarding.started",
            json!({
                "applicant_id": applicant.id.to_string(),
                "job_id": applicant.job_id.to_string(),
                "user_id": applicant.email,
                "name": applicant.name,
                "email": applicant.email,
                "start_date": accepted_offer.start_date.map(|date| date.to_string()),
                "salary": round_money(accepted_offer.salary),
                "currency": accepted_offer.currency,
                "department_id": job.and_then(|job| job.department_id),
            }),
        );
        Ok(applicant_view(&state, &applicant))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
