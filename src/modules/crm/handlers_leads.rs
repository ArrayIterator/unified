// crm - handlers leads.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::types::*;
use super::store::*;
use super::common::*;
use super::handlers_contacts::*;
use super::handlers_activities::*;
use super::module::*;

// Lead handlers

pub(crate) async fn list_leads(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CrmState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _company = match caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_CRM_READ) {
        return json::error_response(&error, ctx.is_debug());
    }
    let params = query.into_inner();
    let mut items: Vec<Lead> = state
        .leads
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|lead| {
            matches_search(
                &format!(
                    "{} {} {}",
                    lead.name,
                    lead.email,
                    lead.company.clone().unwrap_or_default()
                ),
                &params,
            ) && params
                .filters
                .get("stage")
                .is_none_or(|stage| lead.stage == *stage)
                && params
                    .filters
                    .get("source")
                    .is_none_or(|source| lead.source.as_deref() == Some(source.as_str()))
                && params.filters.get("company").is_none_or(|company| {
                    lead.company.as_deref() == Some(company.as_str())
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

pub(crate) async fn create_lead(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CrmState>>,
    req: HttpRequest,
    body: web::Json<CreateLead>,
) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _company = match require_caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_CRM_MANAGE) {
        return json::error_response(&error, ctx.is_debug());
    }
    let result: UnifiedResult<Lead> = (|| {
        let input = body.into_inner();
        if input.name.trim().is_empty() {
            return Err(UnifiedError::bad_request("Lead name must not be empty"));
        }
        if !valid_email(&input.email) {
            return Err(UnifiedError::bad_request("Lead email is not valid"));
        }
        let stage = input.stage.unwrap_or_else(|| "new".to_string());
        if !valid_stage(&stage) {
            return Err(UnifiedError::bad_request(
                "Stage must be one of: new, contacted, qualified, negotiation, won, lost, converted",
            ));
        }
        let lead = Lead {
            id: new_id(),
            name: input.name.trim().to_string(),
            email: input.email.trim().to_string(),
            company: clean_optional(input.company),
            source: clean_optional(input.source),
            stage,
            created_at: now(),
            updated_at: now(),
        };
        state.leads.insert(lead.id.clone(), lead.clone());
        Ok(lead)
    })();
    respond_created(result)
}

pub(crate) async fn get_lead(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CrmState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _company = match caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_CRM_READ) {
        return json::error_response(&error, ctx.is_debug());
    }
    let result: UnifiedResult<Lead> = state
        .leads
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Lead not found"));
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn update_lead(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CrmState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateLead>,
) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _company = match require_caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_CRM_MANAGE) {
        return json::error_response(&error, ctx.is_debug());
    }
    let result: UnifiedResult<Lead> = (|| {
        let input = body.into_inner();
        let mut lead = state
            .leads
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Lead not found"))?;
        if let Some(name) = input.name {
            if name.trim().is_empty() {
                return Err(UnifiedError::bad_request("Lead name must not be empty"));
            }
            lead.name = name.trim().to_string();
        }
        if let Some(email) = input.email {
            if !valid_email(&email) {
                return Err(UnifiedError::bad_request("Lead email is not valid"));
            }
            lead.email = email.trim().to_string();
        }
        if let Some(company) = input.company {
            // Empty/whitespace-only clears the field back to `None`.
            lead.company = clean_optional(Some(company));
        }
        if let Some(source) = input.source {
            lead.source = clean_optional(Some(source));
        }
        if let Some(stage) = input.stage {
            if !valid_stage(&stage) {
                return Err(UnifiedError::bad_request(
                    "Stage must be one of: new, contacted, qualified, negotiation, won, lost, converted",
                ));
            }
            if lead.stage == "converted" && stage != "converted" {
                return Err(UnifiedError::unprocessable(
                    "A converted lead cannot move back to another stage",
                ));
            }
            lead.stage = stage;
        }
        lead.updated_at = now();
        Ok(lead.clone())
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn delete_lead(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CrmState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _company = match require_caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_CRM_MANAGE) {
        return json::error_response(&error, ctx.is_debug());
    }
    let result: UnifiedResult<Value> = (|| {
        state
            .leads
            .remove(path.as_str())
            .map(|_| {
                // Cascade: no orphaned activities on a deleted record.
                delete_activities_for(&state, path.as_str());
                json!({ "deleted": true, "id": path.as_str() })
            })
            .ok_or_else(|| UnifiedError::not_found("Lead not found"))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn get_pipeline(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CrmState>>,
    req: HttpRequest,
) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _company = match caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_CRM_READ) {
        return json::error_response(&error, ctx.is_debug());
    }
    let mut stages: Vec<Value> = Vec::new();
    let mut total_leads = 0u64;
    for stage in PIPELINE_STAGES {
        let count = state
            .leads
            .iter()
            .filter(|entry| entry.value().stage == *stage)
            .count() as u64;
        total_leads += count;
        stages.push(json!({ "stage": stage, "count": count }));
    }
    json::success_response(
        json!({ "stages": stages, "total_leads": total_leads }),
        200,
    )
}

/// Convert a lead into a contact. The stage check and the mark as
/// converted happen under the lead's `DashMap` entry guard, so two
/// concurrent converts cannot both pass: the second sees
/// `converted` and fails with 409, and no duplicate contact or
/// duplicate downstream events are created. Only a live pipeline
/// stage (or `won`) may convert; a `lost` lead is rejected.
pub(crate) fn convert_lead_in_store(
    state: &CrmState,
    lead_id: &str,
) -> UnifiedResult<(Lead, Contact)> {
    let lead = {
        let mut entry = state
            .leads
            .get_mut(lead_id)
            .ok_or_else(|| UnifiedError::not_found("Lead not found"))?;
        if entry.stage == "converted" {
            return Err(UnifiedError::conflict("Lead is already converted"));
        }
        if !matches!(
            entry.stage.as_str(),
            "new" | "contacted" | "qualified" | "negotiation" | "won"
        ) {
            return Err(UnifiedError::unprocessable(
                "Only a new, contacted, qualified, negotiation, or won lead can be converted",
            ));
        }
        entry.stage = "converted".to_string();
        entry.updated_at = now();
        entry.clone()
    };
    let contact = Contact {
        id: new_id(),
        name: lead.name.clone(),
        email: lead.email.clone(),
        company: lead.company.clone(),
        phone: None,
        created_at: now(),
        updated_at: now(),
    };
    state.contacts.insert(contact.id.clone(), contact.clone());
    if let Some(company) = &lead.company {
        state.emit(
            "company.create_requested",
            json!({ "name": company, "lead_id": lead.id, "contact_id": contact.id }),
        );
    }
    state.emit(
        "user.create_requested",
        json!({
            "name": lead.name,
            "email": lead.email,
            "company": lead.company,
            "lead_id": lead.id,
            "contact_id": contact.id,
        }),
    );
    Ok((lead, contact))
}

pub(crate) async fn convert_lead(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<CrmState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    let _company = match require_caller_company(&tenant, &req) {
        Ok(company) => company,
        Err(error) => return json::error_response(&error, ctx.is_debug()),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_CRM_MANAGE) {
        return json::error_response(&error, ctx.is_debug());
    }
    let result: UnifiedResult<Value> = (|| {
        let (lead, contact) = convert_lead_in_store(&state, path.as_str())?;
        Ok(json!({ "lead": lead, "contact": contact }))
    })();
    json::respond(result, ctx.is_debug())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lead_with_stage(id: &str, stage: &str) -> Lead {
        Lead {
            id: id.to_string(),
            name: "Ada Lovelace".to_string(),
            email: "ada@example.com".to_string(),
            company: Some("Analytical".to_string()),
            source: None,
            stage: stage.to_string(),
            created_at: now(),
            updated_at: now(),
        }
    }

    #[test]
    fn concurrent_converts_create_exactly_one_contact() {
        let state = Arc::new(CrmState::new());
        state.leads.insert("lead-1".to_string(), lead_with_stage("lead-1", "new"));
        let first = Arc::clone(&state);
        let second = Arc::clone(&state);
        let (first_result, second_result) = std::thread::scope(|scope| {
            let a = scope.spawn(move || convert_lead_in_store(&first, "lead-1"));
            let b = scope.spawn(move || convert_lead_in_store(&second, "lead-1"));
            (
                a.join().expect("first convert thread"),
                b.join().expect("second convert thread"),
            )
        });
        // Exactly one convert succeeds; the other sees `converted`.
        assert_eq!(
            [first_result.is_ok(), second_result.is_ok()]
                .iter()
                .filter(|ok| **ok)
                .count(),
            1
        );
        assert_eq!(state.contacts.len(), 1);
        let stored = state.leads.get("lead-1").expect("lead still present");
        assert_eq!(stored.stage, "converted");
        // A later convert of the same lead fails with a conflict and
        // creates no second contact.
        assert!(convert_lead_in_store(&state, "lead-1").is_err());
        assert_eq!(state.contacts.len(), 1);
    }

    #[test]
    fn lost_lead_cannot_be_converted_but_won_can() {
        let state = CrmState::new();
        state
            .leads
            .insert("lead-lost".to_string(), lead_with_stage("lead-lost", "lost"));
        assert!(convert_lead_in_store(&state, "lead-lost").is_err());
        assert!(state.contacts.is_empty());
        assert_eq!(state.leads.get("lead-lost").unwrap().stage, "lost");

        state
            .leads
            .insert("lead-won".to_string(), lead_with_stage("lead-won", "won"));
        let (lead, contact) = convert_lead_in_store(&state, "lead-won").unwrap();
        assert_eq!(lead.stage, "converted");
        assert_eq!(contact.email, "ada@example.com");
    }
}
