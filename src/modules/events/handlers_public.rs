// Events Manager - Public feed and calendar handlers.

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
use super::handlers_reminders::*;
use super::hooks::*;
use super::module::*;

// Calendar feed handlers (authenticated)

pub(crate) fn feed_events(state: &EventsState, public_only: bool) -> Vec<(Event, Option<EventOccurrence>)> {
    let now = Utc::now();
    let mut out: Vec<(Event, Option<EventOccurrence>)> = Vec::new();
    let mut events: Vec<Event> = state
        .events
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|event| {
            if event.status == EventStatus::Cancelled {
                return false;
            }
            if public_only {
                if event.visibility != EventVisibility::Public {
                    return false;
                }
                if !matches!(
                    effective_status(event, now),
                    EventStatus::Published | EventStatus::Ongoing | EventStatus::Completed
                ) {
                    return false;
                }
            }
            true
        })
        .collect();
    events.sort_by_key(|event| event.starts_at);
    for event in events {
        let occurrences = occurrences_for(state, event.id);
        if occurrences.is_empty() {
            out.push((event, None));
        } else {
            for occurrence in occurrences {
                if occurrence.status == "cancelled" {
                    continue;
                }
                out.push((event.clone(), Some(occurrence)));
            }
        }
    }
    out
}

/// Scope-filtered feed for the authenticated calendar handlers:
/// the same rows as `feed_events(state, false)`, restricted to
/// events in the caller scope. The public feed (`public_only`) is
/// a separate anonymous surface and is never scope-filtered.
pub(crate) fn feed_events_for_scope(
    state: &EventsState,
    scope: &Option<String>,
) -> Vec<(Event, Option<EventOccurrence>)> {
    feed_events(state, false)
        .into_iter()
        .filter(|(event, _)| in_company_scope(&event.company_id, scope))
        .collect()
}

pub(crate) fn calendar_json_payload(state: &EventsState, public_only: bool) -> Value {
    calendar_json_payload_for_feed(feed_events(state, public_only))
}

/// Scope-filtered calendar JSON payload for the authenticated
/// calendar handler.
pub(crate) fn calendar_json_payload_for_scope(
    state: &EventsState,
    scope: &Option<String>,
) -> Value {
    calendar_json_payload_for_feed(feed_events_for_scope(state, scope))
}

fn calendar_json_payload_for_feed(
    feed: Vec<(Event, Option<EventOccurrence>)>,
) -> Value {
    let items: Vec<Value> = feed
        .into_iter()
        .map(|(event, occurrence)| {
            let (starts_at, ends_at, location) = match &occurrence {
                Some(occ) => (
                    occ.starts_at,
                    occ.ends_at,
                    occ.venue_override
                        .as_ref()
                        .or(event.venue.as_ref())
                        .map(|venue| venue.name.clone()),
                ),
                None => (
                    event.starts_at,
                    event.ends_at,
                    event.venue.as_ref().map(|venue| venue.name.clone()),
                ),
            };
            json!({
                "event_id": event.id.to_string(),
                "occurrence_id": occurrence.map(|occ| occ.id.to_string()),
                "title": event.title,
                "description": event.description,
                "slug": event.slug,
                "starts_at": starts_at.to_rfc3339(),
                "ends_at": ends_at.to_rfc3339(),
                "timezone": event.timezone,
                "status": event.status.as_str(),
                "visibility": event.visibility.as_str(),
                "location": location,
                "url": format!("/api/v1/events/public/{}", event.slug),
            })
        })
        .collect();
    json!({ "events": items, "count": items.len() })
}

pub(crate) async fn event_ics(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err(error, debug),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_EVENTS_VIEW) {
        return err(error, debug);
    }
    let event = match load_event(&state, &path.into_inner()) {
        Ok(event) => event,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_record_in_company_scope(&event.company_id, &_scope) {
        return err(error, debug);
    }
    let ics = build_ics(&[(event.clone(), None)], &event.title);
    HttpResponse::Ok()
        .content_type("text/calendar; charset=utf-8")
        .insert_header((
            "Content-Disposition",
            format!("attachment; filename=\"{}.ics\"", event.slug),
        ))
        .body(ics)
}

pub(crate) async fn calendar_ics(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err(error, debug),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_EVENTS_VIEW) {
        return err(error, debug);
    }
    let ics = build_ics(&feed_events_for_scope(&state, &_scope), "Unified Events");
    HttpResponse::Ok()
        .content_type("text/calendar; charset=utf-8")
        .insert_header(("Content-Disposition", "attachment; filename=\"events.ics\""))
        .body(ics)
}

pub(crate) async fn calendar_json(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err(error, debug),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_EVENTS_VIEW) {
        return err(error, debug);
    }
    ok(calendar_json_payload_for_scope(&state, &_scope))
}

// Public handlers (no auth - gated by the API Manager's ApiAccessRule
// for each path; default deny. See the module header.)

pub(crate) async fn list_public_events(
    state: web::Data<EventsState>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let category_filter = params.filters.get("category").cloned();
    let from_filter = params
        .filters
        .get("from")
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|value| value.with_timezone(&Utc));
    let to_filter = params
        .filters
        .get("to")
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|value| value.with_timezone(&Utc));
    let search = params.search.clone().unwrap_or_default().to_lowercase();
    let now = Utc::now();
    let mut items: Vec<Value> = state
        .events
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|event| {
            if event.visibility != EventVisibility::Public {
                return false;
            }
            if !matches!(
                effective_status(event, now),
                EventStatus::Published | EventStatus::Ongoing | EventStatus::Completed
            ) {
                return false;
            }
            if let Some(category) = &category_filter {
                if !event.category.iter().any(|entry| entry == category) {
                    return false;
                }
            }
            if let Some(from) = from_filter {
                if event.ends_at < from {
                    return false;
                }
            }
            if let Some(to) = to_filter {
                if event.starts_at > to {
                    return false;
                }
            }
            if !search.is_empty() {
                let haystack = format!(
                    "{} {} {}",
                    event.title,
                    event.description.clone().unwrap_or_default(),
                    event.slug
                )
                .to_lowercase();
                if !haystack.contains(&search) {
                    return false;
                }
            }
            true
        })
        .map(|event| public_event_view(&state, &event))
        .collect();
    items.sort_by(|a, b| {
        a.get("starts_at")
            .and_then(Value::as_str)
            .cmp(&b.get("starts_at").and_then(Value::as_str))
    });
    ok(paginate(items, &params))
}

pub(crate) async fn get_public_event(
    state: web::Data<EventsState>,
    path: web::Path<String>,
) -> HttpResponse {
    let slug = path.into_inner();
    match load_event_by_slug(&state, &slug) {
        Ok(event) => {
            // Private and draft-visibility events never leak through
            // a public lookup: they answer 404, the same as unknown.
            if event.visibility != EventVisibility::Public {
                return err(UnifiedError::not_found(format!("Event not found: {slug}")), false);
            }
            if !matches!(
                effective_status(&event, Utc::now()),
                EventStatus::Published | EventStatus::Ongoing | EventStatus::Completed
            ) {
                return err(UnifiedError::not_found(format!("Event not found: {slug}")), false);
            }
            ok(public_event_view(&state, &event))
        }
        Err(error) => err(error, false),
    }
}

pub(crate) async fn public_event_ics(state: web::Data<EventsState>, path: web::Path<String>) -> HttpResponse {
    let slug = path.into_inner();
    let event = match load_event_by_slug(&state, &slug) {
        Ok(event) => event,
        Err(error) => return err(error, false),
    };
    if event.visibility != EventVisibility::Public
        || !matches!(
            effective_status(&event, Utc::now()),
            EventStatus::Published | EventStatus::Ongoing | EventStatus::Completed
        )
    {
        return err(UnifiedError::not_found(format!("Event not found: {slug}")), false);
    }
    let ics = build_ics(&[(event.clone(), None)], &event.title);
    HttpResponse::Ok()
        .content_type("text/calendar; charset=utf-8")
        .insert_header((
            "Content-Disposition",
            format!("attachment; filename=\"{}.ics\"", event.slug),
        ))
        .body(ics)
}

pub(crate) async fn public_calendar_ics(state: web::Data<EventsState>) -> HttpResponse {
    let ics = build_ics(&feed_events(&state, true), "Unified Events");
    HttpResponse::Ok()
        .content_type("text/calendar; charset=utf-8")
        .insert_header(("Content-Disposition", "attachment; filename=\"events.ics\""))
        .body(ics)
}

pub(crate) async fn public_calendar_json(state: web::Data<EventsState>) -> HttpResponse {
    ok(calendar_json_payload(&state, true))
}
