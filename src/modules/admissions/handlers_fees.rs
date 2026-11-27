// admissions module - Fee, result, decision, enrolment handlers.
//
// Split from the former single-file module `src/modules/admissions.rs`.
// The module root (`admissions.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
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

use super::constants::{KEY_DECISIONS, MAX_TEXT, PERM_ADMISSIONS_MANAGE, PERM_ADMISSIONS_READ, PERM_ADMISSIONS_READ_ALL};
use super::types::{AdmissionResult, Application, DecideApplication, FeeAction, WaitlistEntry};
use super::state::AdmissionsState;
use super::helpers::{
    caller, caller_company, check_capability, load_application_for_read, new_id, now,
    optional_text, require_id, require_permission, require_tenant,
};

// Handlers: fee, result, decision, enrolment

pub(crate) async fn request_fee_invoice(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<FeeAction>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Application> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let application_id = path.as_str().to_string();
        let mut application = state.application(&application_id)?;
        let input = body.into_inner();
        match input.action.as_str() {
            "request" => {
                let amount = match application.fee_amount {
                    Some(amount) => amount,
                    None => {
                        return Err(UnifiedError::conflict(
                            "This application has no registration fee configured",
                        ));
                    }
                };
                if application.fee_invoice_status == "paid" {
                    return Err(UnifiedError::conflict("The fee is already paid"));
                }
                if application.fee_invoice_status == "waived" {
                    return Err(UnifiedError::conflict("The fee was waived"));
                }
                if application.fee_invoice_status == "requested" {
                    return Err(UnifiedError::conflict(
                        "The fee invoice was already requested",
                    ));
                }
                application.fee_invoice_status = "requested".to_string();
                application.updated_at = now();
                state
                    .applications
                    .insert(application.id.clone(), application.clone());
                // The reference lets billing echo the application back:
                // billing sets the created invoice's `order_id` to the
                // application id, so `invoice.paid` can mark it paid.
                let reference = format!("admissions:{}", application.id);
                state.emit(
                    "admissions.fee_invoice_requested",
                    json!({
                        "application_id": application.id,
                        "period_id": application.period_id,
                        "amount": amount,
                        "currency": application.fee_currency,
                        "reference": reference,
                    }),
                );
                state.emit(
                    "billing.invoice.requested",
                    json!({
                        "application_id": application.id,
                        "period_id": application.period_id,
                        "customer_user_id": application.applicant_user_id,
                        "customer_name": application.full_name,
                        "customer_email": application.email,
                        "amount": amount,
                        "currency": application.fee_currency,
                        "description": format!("Registration fee for application {}", application.id),
                        "reference": reference,
                    }),
                );
                state.request_notification(
                    &application.applicant_user_id,
                    "Registration fee invoice requested",
                    "Your registration fee invoice was requested. Please complete the payment to continue enrolment.",
                    "admissions.fee_invoice_requested",
                );
            }
            "waive" => {
                if application.fee_invoice_status == "paid" {
                    return Err(UnifiedError::conflict("The fee is already paid"));
                }
                application.fee_invoice_status = "waived".to_string();
                application.updated_at = now();
                state
                    .applications
                    .insert(application.id.clone(), application.clone());
            }
            "mark_paid" => {
                let invoice_id = match input.invoice_id {
                    Some(value) if !value.trim().is_empty() => {
                        require_id(&value, "invoice_id")?
                    }
                    _ => {
                        return Err(UnifiedError::bad_request(
                            "invoice_id is required for action mark_paid",
                        ));
                    }
                };
                application.fee_invoice_status = "paid".to_string();
                application.fee_invoice_id = Some(invoice_id.clone());
                application.updated_at = now();
                state
                    .applications
                    .insert(application.id.clone(), application.clone());
                state.emit(
                    "admissions.fee_paid",
                    json!({
                        "application_id": application.id,
                        "period_id": application.period_id,
                        "invoice_id": invoice_id,
                    }),
                );
            }
            _ => {
                return Err(UnifiedError::bad_request(
                    "action must be one of: request, waive, mark_paid",
                ));
            }
        }
        Ok(application)
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_application_result(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AdmissionResult> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let application_id = path.as_str().to_string();
        let application = load_application_for_read(&state, &ctx, &caller, &application_id)?;
        let result = state
            .results
            .iter()
            .find(|entry| entry.value().application_id == application_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Result not found"))?;
        // An applicant sees their own result only once it is
        // published (and only when the visibility default allows it);
        // staff with read-all always see it. Unpublished results are
        // hidden from applicants as 404, so the decision cannot be
        // probed early.
        let is_staff = check_capability(&ctx, &caller.role, PERM_ADMISSIONS_READ_ALL);
        if !is_staff {
            if !result.published || !state.results_visible_to_applicants() {
                return Err(UnifiedError::not_found("Result not found"));
            }
        }
        Ok(result)
    })();
    json::respond(result, debug)
}

pub(crate) async fn decide_application(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DecideApplication>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let application_id = path.as_str().to_string();
        let mut application = state.application(&application_id)?;
        let input = body.into_inner();
        let decisions = state.vocabulary(KEY_DECISIONS, &["accepted", "rejected", "waitlisted"]);
        if !decisions.iter().any(|decision| decision == &input.decision) {
            return Err(UnifiedError::bad_request(format!(
                "decision must be one of: {}",
                decisions.join(", ")
            )));
        }
        if !matches!(
            application.status.as_str(),
            "submitted" | "under_review" | "waitlisted" | "accepted" | "rejected"
        ) {
            return Err(UnifiedError::conflict(format!(
                "An application in status \"{}\" cannot be decided",
                application.status
            )));
        }
        let note = optional_text(&input.note, "note", MAX_TEXT)?;
        let was_accepted = application.status == "accepted";
        let period_id = application.period_id.clone();
        let mut waitlist_entry: Option<WaitlistEntry> = None;
        match input.decision.as_str() {
            "accepted" => {
                let total = state.compute_total(&application.id);
                if !total.all_required_stages_passed {
                    return Err(UnifiedError::conflict(
                        "Not every required selection stage has a passing score yet",
                    ));
                }
                if !state.seat_available(&application)? {
                    return Err(UnifiedError::conflict(
                        "No seat is free for this program and level; place the application on the waitlist instead",
                    ));
                }
            }
            "waitlisted" => {
                if state.waitlist.iter().any(|entry| {
                    entry.value().application_id == application_id
                        && entry.value().status == "waiting"
                }) {
                    return Err(UnifiedError::conflict(
                        "Application is already on the waitlist",
                    ));
                }
            }
            _ => {}
        }
        // Record the result before mutating the application: a
        // published result locks its decision, so a refusal here
        // must happen before any status change is stored.
        let result = state.upsert_result(
            &application,
            &input.decision,
            &caller.actor,
            note,
        )?;
        match input.decision.as_str() {
            "accepted" => {
                application.status = "accepted".to_string();
            }
            "rejected" => {
                application.status = "rejected".to_string();
                // A rejected application frees its seat when it was
                // accepted; a previously waitlisted one leaves the
                // waitlist.
                for mut entry in state.waitlist.iter_mut() {
                    if entry.value().application_id == application_id
                        && entry.value().status == "waiting"
                    {
                        entry.status = "expired".to_string();
                        entry.updated_at = now();
                    }
                }
            }
            "waitlisted" => {
                application.status = "waitlisted".to_string();
                let position = state
                    .waitlist
                    .iter()
                    .filter(|entry| {
                        entry.value().period_id == application.period_id
                            && entry.value().status == "waiting"
                    })
                    .count() as u32
                    + 1;
                let entry = WaitlistEntry {
                    id: new_id(),
                    period_id: application.period_id.clone(),
                    application_id: application.id.clone(),
                    position,
                    status: "waiting".to_string(),
                    promoted_at: None,
                    created_at: now(),
                    updated_at: now(),
                };
                state.waitlist.insert(entry.id.clone(), entry.clone());
                waitlist_entry = Some(entry);
            }
            _ => {}
        }
        application.updated_at = now();
        state
            .applications
            .insert(application.id.clone(), application.clone());
        state.emit(
            "admissions.decision_made",
            json!({
                "application_id": application.id,
                "period_id": application.period_id,
                "decision": input.decision,
            }),
        );
        // Rejecting an accepted application frees a seat: promote
        // the top waitlisted application, as withdrawal does.
        let mut promoted: Option<Application> = None;
        if input.decision == "rejected" && was_accepted {
            promoted = state.promote_top_waitlisted(&period_id)?;
        }
        Ok(json!({
            "application": application,
            "result": result,
            "waitlist_entry": waitlist_entry,
            "waitlist_promoted": promoted,
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn enroll_application(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Application> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let application_id = path.as_str().to_string();
        let mut application = state.application(&application_id)?;
        if application.status != "accepted" {
            return Err(UnifiedError::conflict(
                "Only an accepted application can be enrolled",
            ));
        }
        if application.fee_amount.is_some()
            && !matches!(
                application.fee_invoice_status.as_str(),
                "paid" | "waived"
            )
        {
            return Err(UnifiedError::conflict(
                "The registration fee must be paid or waived before enrolment",
            ));
        }
        if !state.checklist_complete(&application.id) {
            return Err(UnifiedError::conflict(
                "Not every required document is verified yet",
            ));
        }
        let period = state.period(&application.period_id)?;
        application.status = "enrolled".to_string();
        application.updated_at = now();
        state
            .applications
            .insert(application.id.clone(), application.clone());
        // The academic system owns the student record: this event
        // asks it to create one from the admitted application. The
        // `academic.student.created` answer links the student id
        // back onto this application. `enrollment_year` is the
        // leading year of the period's free-text academic year
        // (for example "2026/2027" -> 2026); the academic module
        // derives it the same way when the value is absent.
        let enrollment_year = period
            .academic_year
            .trim()
            .chars()
            .take(4)
            .collect::<String>()
            .parse::<i32>()
            .ok()
            .filter(|year| (1990..=2100).contains(year));
        let payload = json!({
            "application_id": application.id,
            "period_id": application.period_id,
            "applicant_user_id": application.applicant_user_id,
            "full_name": application.full_name,
            "email": application.email,
            "phone": application.phone,
            "program_id": application.program_id,
            "level": application.level,
            "academic_year": period.academic_year,
            "term": period.term,
            "enrollment_year": enrollment_year,
        });
        state.emit("academic.student.create_requested", payload.clone());
        state.emit("admissions.enrollment_requested", payload);
        Ok(application)
    })();
    json::respond(result, debug)
}
