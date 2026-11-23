// Events Manager — Reminder handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::*;
use super::types::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::service::*;
use super::handlers_events::*;
use super::handlers_tickets::*;
use super::handlers_registrations::*;
use super::handlers_public::*;
use super::hooks::*;
use super::module::*;

pub(crate) async fn list_reminders(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EVENTS_VIEW)?;
        let event = load_event(&state, &path.into_inner())?;
        require_record_in_company_scope(&event.company_id, &_scope)?;
        let params = query.into_inner();
        let mut items: Vec<EventReminder> = state
            .reminders
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|reminder| reminder.event_id == event.id)
            .collect();
        items.sort_by_key(|reminder| reminder.offset_minutes);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_reminder(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateReminderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<EventReminder> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        let event = load_event(&state, &path.into_inner())?;
        require_record_in_company_scope(&event.company_id, &_scope)?;
        can_manage_event(&ctx, &caller.role, &event, &caller.actor)?;
        let input = body.into_inner();
        if input.offset_minutes < 0 || input.offset_minutes > 60 * 24 * 365 {
            return Err(UnifiedError::bad_request(
                "offset_minutes must be between 0 and 525600 (one year)",
            ));
        }
        let occurrence_id = match input.occurrence_id {
            Some(id) => {
                load_occurrence(&state, event.id, &id.to_string())?;
                Some(id)
            }
            None => None,
        };
        let title_template = match &input.title_template {
            Some(raw) => Some(require_text(raw, "title_template", 500)?),
            None => None,
        };
        let body_template = match &input.body_template {
            Some(raw) => Some(require_text(raw, "body_template", 2000)?),
            None => None,
        };
        let now = Utc::now();
        let mut reminder = EventReminder {
            id: Uuid::now_v7(),
            event_id: event.id,
            offset_minutes: input.offset_minutes,
            occurrence_id,
            title_template,
            body_template,
            status: "pending".to_string(),
            scheduler_job_id: None,
            scheduler_status: "not_scheduled".to_string(),
            sent_at: None,
            created_at: now,
            updated_at: now,
        };
        schedule_reminder(&state, &mut reminder, &event);
        state.reminders.insert(reminder.id, reminder.clone());
        state.emit(
            "events.reminder.created",
            json!({
                "event_id": event.id.to_string(),
                "reminder_id": reminder.id.to_string(),
                "offset_minutes": reminder.offset_minutes,
                "scheduler_job_id": reminder.scheduler_job_id,
                "scheduler_status": reminder.scheduler_status,
            }),
        );
        Ok(reminder)
    })();
    match result {
        Ok(reminder) => created(reminder),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_reminder(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        let (event_id, reminder_id) = path.into_inner();
        let event = load_event(&state, &event_id)?;
        require_record_in_company_scope(&event.company_id, &_scope)?;
        can_manage_event(&ctx, &caller.role, &event, &caller.actor)?;
        let id = parse_uuid(&reminder_id, "reminder")?;
        let reminder = state
            .reminders
            .get(&id)
            .map(|entry| entry.value().clone())
            .filter(|reminder| reminder.event_id == event.id)
            .ok_or_else(|| UnifiedError::not_found(format!("Reminder not found: {id}")))?;
        if reminder.status != "pending" {
            return Err(UnifiedError::conflict(format!(
                "Only a pending reminder can be deleted (status is \"{}\")",
                reminder.status
            )));
        }
        state.reminders.remove(&id);
        state.emit(
            "events.reminder.deleted",
            json!({
                "event_id": event.id.to_string(),
                "reminder_id": id.to_string(),
            }),
        );
        Ok(json!({ "deleted": true, "reminder_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn send_reminder_now(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        let (event_id, reminder_id) = path.into_inner();
        let event = load_event(&state, &event_id)?;
        require_record_in_company_scope(&event.company_id, &_scope)?;
        can_manage_event(&ctx, &caller.role, &event, &caller.actor)?;
        let id = parse_uuid(&reminder_id, "reminder")?;
        let reminder = state
            .reminders
            .get(&id)
            .map(|entry| entry.value().clone())
            .filter(|reminder| reminder.event_id == event.id)
            .ok_or_else(|| UnifiedError::not_found(format!("Reminder not found: {id}")))?;
        if reminder.status != "pending" {
            return Err(UnifiedError::conflict(format!(
                "Only a pending reminder can be sent (status is \"{}\")",
                reminder.status
            )));
        }
        let sent = send_reminder(&state, &reminder)?;
        Ok(json!({
            "sent": true,
            "reminder_id": id.to_string(),
            "recipients": sent,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
