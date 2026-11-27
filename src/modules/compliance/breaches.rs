//! Compliance Center module - GDPR / ISO support: data export - Breach-log handlers.
//!
//! Split from `src/modules/compliance.rs`; behaviour unchanged.

use super::checklists::*;
use super::consents::*;
use super::constants::*;
use super::deletion_listeners::*;
use super::deletions::*;
use super::exports::*;
use super::helpers::*;
use super::module::*;
use super::records::*;
use super::requests::*;
use super::retention::*;
use super::state::*;
use super::types::*;

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

pub(super) async fn list_breaches(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let notification_filter = params.filters.get("notification_status").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<BreachLog> = state
            .breaches
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|breach| {
                if let Some(status) = &status_filter {
                    if &breach.status != status {
                        return false;
                    }
                }
                if let Some(notification) = &notification_filter {
                    if &breach.notification_status != notification {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !format!("{} {} {}", breach.title, breach.description, breach.affected_scope)
                        .to_lowercase()
                        .contains(&search)
                {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by_key(|breach| std::cmp::Reverse(breach.discovered_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn create_breach(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    body: web::Json<CreateBreachRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BreachLog> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let description = require_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let affected_scope = require_text(&input.affected_scope, "affected_scope", MAX_SCOPE_LEN)?;
        let now = Utc::now();
        let breach = BreachLog {
            id: Uuid::now_v7(),
            title,
            description,
            discovered_at: input.discovered_at,
            affected_scope,
            status: "reported".to_string(),
            notification_status: "not_started".to_string(),
            reported_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.breaches.insert(breach.id, breach.clone());
        // The first timeline entry records the report itself.
        let first_entry = BreachTimelineEntry {
            id: Uuid::now_v7(),
            breach_id: breach.id,
            author_id: caller.actor.clone(),
            entry: format!("Breach reported: {}", breach.title),
            recorded_at: now,
        };
        state.breach_timeline.insert(first_entry.id, first_entry);
        state.emit(
            "compliance.breach.reported",
            json!({
                "breach_id": breach.id.to_string(),
                "title": breach.title,
                "affected_scope": breach.affected_scope,
                "discovered_at": breach.discovered_at.to_rfc3339(),
                "reported_by": breach.reported_by,
            }),
        );
        if let Some(notify_user_id) = &input.notify_user_id {
            let target = require_text(notify_user_id, "notify_user_id", MAX_NAME_LEN)?;
            state.notify(
                "compliance.breach.reported",
                &target,
                &format!("Data breach reported: {}", breach.title),
                &format!(
                    "A data breach was reported affecting {}. See the compliance breach log.",
                    breach.affected_scope
                ),
                Some(format!("/api/v1/compliance/breaches/{}", breach.id)),
            );
        }
        Ok(breach)
    })();
    match result {
        Ok(breach) => created(breach),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn get_breach(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let breach = load_breach(&state, &path.into_inner())?;
        Ok(breach_view(&state, &breach))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) fn breach_view(state: &ComplianceState, breach: &BreachLog) -> Value {
    let mut value = serde_json::to_value(breach).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        let timeline: Vec<BreachTimelineEntry> = {
            let mut rows: Vec<BreachTimelineEntry> = state
                .breach_timeline
                .iter()
                .map(|entry| entry.value().clone())
                .filter(|entry| entry.breach_id == breach.id)
                .collect();
            rows.sort_by_key(|entry| entry.recorded_at);
            rows
        };
        map.insert(
            "timeline".to_string(),
            serde_json::to_value(timeline).unwrap_or(Value::Null),
        );
    }
    value
}

pub(super) async fn update_breach(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateBreachRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let mut breach = load_breach(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(title) = &input.title {
            breach.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = &input.description {
            breach.description = require_text(description, "description", MAX_DESCRIPTION_LEN)?;
        }
        if let Some(scope) = &input.affected_scope {
            breach.affected_scope = require_text(scope, "affected_scope", MAX_SCOPE_LEN)?;
        }
        if let Some(status) = &input.status {
            let checked = status.trim().to_lowercase();
            if !valid_breach_status(&checked) {
                return Err(UnifiedError::bad_request(
                    "status must be \"reported\", \"investigating\", \"contained\", \"notified\", or \"closed\"",
                ));
            }
            breach.status = checked;
        }
        if let Some(notification) = &input.notification_status {
            let checked = notification.trim().to_lowercase();
            if !valid_notification_status(&checked) {
                return Err(UnifiedError::bad_request(
                    "notification_status must be \"not_started\", \"in_progress\", or \"notified\"",
                ));
            }
            breach.notification_status = checked;
        }
        breach.updated_at = Utc::now();
        state.breaches.insert(breach.id, breach.clone());
        state.emit(
            "compliance.breach.updated",
            json!({
                "breach_id": breach.id.to_string(),
                "status": breach.status,
                "notification_status": breach.notification_status,
            }),
        );
        Ok(breach_view(&state, &breach))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn add_breach_timeline(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AddTimelineRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BreachTimelineEntry> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let breach = load_breach(&state, &path.into_inner())?;
        let entry_text = require_text(&body.into_inner().entry, "entry", MAX_DESCRIPTION_LEN)?;
        let entry = BreachTimelineEntry {
            id: Uuid::now_v7(),
            breach_id: breach.id,
            author_id: caller.actor.clone(),
            entry: entry_text,
            recorded_at: Utc::now(),
        };
        state.breach_timeline.insert(entry.id, entry.clone());
        Ok(entry)
    })();
    match result {
        Ok(entry) => created(entry),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn list_breach_timeline(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let breach = load_breach(&state, &path.into_inner())?;
        let params = query.into_inner();
        let mut items: Vec<BreachTimelineEntry> = state
            .breach_timeline
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.breach_id == breach.id)
            .collect();
        items.sort_by_key(|entry| entry.recorded_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
