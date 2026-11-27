//! SaaS Domains - reservation handlers.
//!
//! Part of the `saas_domains` module split; see `src/modules/saas_domains.rs`.

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::error::UnifiedResult;
use unified_core::json;
use unified_core::pagination::PaginationParams;
use crate::modules::company::company_exists;
use actix_web::HttpRequest;
use actix_web::HttpResponse;
use actix_web::web;
use chrono::Utc;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

use super::caller::authorize_company;
use super::caller::authorize_list;
use super::caller::caller_from;
use super::caller::conflict_error;
use super::caller::domain_claimed;
use super::caller::require_permission;
use super::events::CAP_MANAGE;
use super::events::EVENT_RESERVATION_CREATED;
use super::events::EVENT_RESERVATION_EXPIRED;
use super::events::EVENT_RESERVATION_PROVISIONED;
use super::events::EVENT_RESERVATION_RELEASED;
use super::events::EVENT_SITE_PROVISIONED;
use super::requests::CreateReservationRequest;
use super::requests::ProvisionReservationRequest;
use super::resolve::expire_stale_reservations;
use super::store::paginate;
use super::store::reservations_store;
use super::store::settings;
use super::types::DomainReservation;
use super::validation::normalize_domain;

// Reservation handlers.

pub(crate) async fn create_reservation(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateReservationRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DomainReservation> = (|| {
        let caller = caller_from(&req)?;
        authorize_company(&ctx, &caller, body.company_id)?;
        let domain = normalize_domain(&body.domain)?;
        if !company_exists(&ctx, body.company_id) {
            return Err(UnifiedError::not_found("Company not found"));
        }
        if domain_claimed(&ctx, &domain, None, None, None) {
            return Err(conflict_error(&domain));
        }
        let cfg = settings();
        let now = Utc::now();
        let reserved_until = match body.reserved_until {
            Some(until) if until > now => until,
            Some(_) => {
                return Err(UnifiedError::bad_request(
                    "reserved_until must be in the future",
                ));
            }
            None => now + chrono::Duration::hours(i64::from(cfg.default_reservation_ttl_hours)),
        };
        let reservation = DomainReservation {
            id: Uuid::now_v7(),
            domain: domain.clone(),
            company_id: body.company_id,
            reserved_by: caller.actor.clone(),
            reserved_until,
            status: "reserved".to_string(),
            site_id: body.site_id,
            created_at: now,
            updated_at: now,
        };
        reservations_store().insert(reservation.id, reservation.clone());
        ctx.hooks.emit(
            EVENT_RESERVATION_CREATED,
            json!({
                "reservation_id": reservation.id,
                "domain": reservation.domain,
                "company_id": reservation.company_id,
                "reserved_by": reservation.reserved_by,
                "reserved_until": reservation.reserved_until,
            }),
        );
        Ok(reservation)
    })();
    match result {
        Ok(row) => json::success_response(row, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_reservations(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller_from(&req)?;
        let scope = authorize_list(&ctx, &caller)?;
        let mut items: Vec<DomainReservation> = reservations_store()
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
                        "Cannot list another company's reservations",
                    ));
                }
                items.retain(|row| row.company_id == wanted);
            }
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|row| row.status == *status);
        }
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|row| row.domain.to_lowercase().contains(&needle));
        }
        let sort = params.allowed_sort(
            &["domain", "status", "reserved_until", "created_at"],
            "created_at",
        );
        match sort.as_str() {
            "domain" => items.sort_by(|a, b| a.domain.cmp(&b.domain)),
            "status" => items.sort_by(|a, b| a.status.cmp(&b.status)),
            "reserved_until" => items.sort_by(|a, b| a.reserved_until.cmp(&b.reserved_until)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_reservation(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DomainReservation> = (|| {
        let caller = caller_from(&req)?;
        let row = reservations_store()
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Reservation not found"))?;
        authorize_company(&ctx, &caller, row.company_id)?;
        Ok(row)
    })();
    json::respond(result, debug)
}

pub(crate) async fn release_reservation(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DomainReservation> = (|| {
        let caller = caller_from(&req)?;
        let id = id.into_inner();
        let mut row = reservations_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Reservation not found"))?;
        authorize_company(&ctx, &caller, row.company_id)?;
        if row.status != "reserved" {
            return Err(UnifiedError::conflict(format!(
                "Only a reserved reservation can be released (current status: {})",
                row.status
            )));
        }
        row.status = "released".to_string();
        row.updated_at = Utc::now();
        reservations_store().insert(row.id, row.clone());
        ctx.hooks.emit(
            EVENT_RESERVATION_RELEASED,
            json!({
                "reservation_id": row.id,
                "domain": row.domain,
                "company_id": row.company_id,
            }),
        );
        Ok(row)
    })();
    json::respond(result, debug)
}

pub(crate) async fn provision_reservation(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<ProvisionReservationRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller_from(&req)?;
        let id = id.into_inner();
        let mut row = reservations_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Reservation not found"))?;
        authorize_company(&ctx, &caller, row.company_id)?;
        if body.company_id != row.company_id {
            return Err(UnifiedError::forbidden(
                "A reservation can only be provisioned for the tenant it was reserved for",
            ));
        }
        if row.status != "reserved" {
            return Err(UnifiedError::conflict(format!(
                "Only a reserved reservation can be provisioned (current status: {})",
                row.status
            )));
        }
        if row.reserved_until <= Utc::now() {
            row.status = "expired".to_string();
            row.updated_at = Utc::now();
            reservations_store().insert(row.id, row.clone());
            ctx.hooks.emit(
                EVENT_RESERVATION_EXPIRED,
                json!({
                    "reservation_id": row.id,
                    "domain": row.domain,
                    "company_id": row.company_id,
                }),
            );
            return Err(UnifiedError::conflict(
                "Reservation has expired; reserve the domain again before provisioning",
            ));
        }
        if let Some(site_id) = body.site_id {
            if !crate::modules::company::site_exists(&ctx, site_id) {
                return Err(UnifiedError::not_found("Company site not found"));
            }
            row.site_id = Some(site_id);
        }
        let site_id = body.site_id.unwrap_or_else(Uuid::now_v7);
        row.status = "active".to_string();
        row.updated_at = Utc::now();
        reservations_store().insert(row.id, row.clone());
        ctx.hooks.emit(
            EVENT_RESERVATION_PROVISIONED,
            json!({
                "reservation_id": row.id,
                "domain": row.domain,
                "company_id": row.company_id,
                "site_id": site_id,
            }),
        );
        // Hand the site itself to the SaaS module, which owns site
        // rows; provisioning directly without a reservation keeps
        // working through POST /api/v1/saas/sites as before.
        ctx.hooks.emit(
            EVENT_SITE_PROVISIONED,
            json!({
                "site_id": site_id,
                "company_id": row.company_id,
                "domain": row.domain,
                "theme_id": body.theme_id.clone().unwrap_or_default(),
                "name": body.name.clone().unwrap_or_default(),
                "reservation_id": row.id,
            }),
        );
        Ok(json!({ "reservation": row, "site_id": site_id }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn expire_reservations_endpoint(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller_from(&req)?;
        require_permission(&ctx, &caller.role, CAP_MANAGE)?;
        let expired = expire_stale_reservations(&ctx);
        Ok(json!({ "expired": expired.len(), "ids": expired }))
    })();
    json::respond(result, debug)
}
