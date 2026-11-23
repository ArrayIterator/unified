// Events Manager — Event and occurrence handlers.

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
use super::handlers_tickets::*;
use super::handlers_registrations::*;
use super::handlers_reminders::*;
use super::handlers_public::*;
use super::hooks::*;
use super::module::*;

// ---------------------------------------------------------------------------
// Default reminders
// ---------------------------------------------------------------------------

/// Create the default 24h / 1h reminders for an event, unless
/// reminders already exist for it. Called on event creation.
pub(crate) fn create_default_reminders(state: &EventsState, event: &Event) {
    if state
        .reminders
        .iter()
        .any(|entry| entry.value().event_id == event.id)
    {
        return;
    }
    let now = Utc::now();
    for offset in DEFAULT_REMINDER_OFFSETS_MIN {
        let mut reminder = EventReminder {
            id: Uuid::now_v7(),
            event_id: event.id,
            offset_minutes: offset,
            occurrence_id: None,
            title_template: None,
            body_template: None,
            status: "pending".to_string(),
            scheduler_job_id: None,
            scheduler_status: "not_scheduled".to_string(),
            sent_at: None,
            created_at: now,
            updated_at: now,
        };
        schedule_reminder(state, &mut reminder, event);
        state.reminders.insert(reminder.id, reminder);
    }
}

pub(crate) fn can_manage_event(ctx: &AppContext, role: &str, event: &Event, actor: &str) -> UnifiedResult<()> {
    if event.organizer_id == actor {
        return Ok(());
    }
    require_permission(ctx, role, PERM_EVENTS_MANAGE)
}

// ---------------------------------------------------------------------------
// Event handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_events(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EVENTS_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let visibility_filter = params.filters.get("visibility").cloned();
        let category_filter = params.filters.get("category").cloned();
        let organizer_filter = params.filters.get("organizer_id").cloned();
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
                if !in_company_scope(&event.company_id, &_scope) {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if effective_status(event, now).as_str() != status.as_str() {
                        return false;
                    }
                }
                if let Some(visibility) = &visibility_filter {
                    if event.visibility.as_str() != visibility.as_str() {
                        return false;
                    }
                }
                if let Some(category) = &category_filter {
                    if !event.category.iter().any(|entry| entry == category) {
                        return false;
                    }
                }
                if let Some(organizer) = &organizer_filter {
                    if &event.organizer_id != organizer {
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
        let sort = params.allowed_sort(&["title", "starts_at", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "title" => a
                    .get("title")
                    .and_then(Value::as_str)
                    .cmp(&b.get("title").and_then(Value::as_str)),
                "starts_at" => a
                    .get("starts_at")
                    .and_then(Value::as_str)
                    .cmp(&b.get("starts_at").and_then(Value::as_str)),
                _ => a
                    .get("created_at")
                    .and_then(Value::as_str)
                    .cmp(&b.get("created_at").and_then(Value::as_str)),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_event(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    body: web::Json<CreateEventRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Event> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EVENTS_CREATE)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let slug = match &input.slug {
            Some(raw) => {
                let slug = raw.trim().to_lowercase();
                if !valid_slug(&slug) {
                    return Err(UnifiedError::bad_request(
                        "slug must be lowercase letters, digits, and single hyphens only (no leading/trailing hyphen, max 128 characters)",
                    ));
                }
                slug
            }
            None => slugify(&title),
        };
        if slug_in_use(&state, &slug, None) {
            return Err(UnifiedError::conflict(format!("slug is already in use: {slug}")));
        }
        let mut category: Vec<String> = Vec::new();
        for raw in &input.category {
            category.push(require_text(raw, "category", MAX_CATEGORY_LEN)?);
        }
        let mut tags: Vec<String> = Vec::new();
        for raw in &input.tags {
            if tags.len() >= MAX_TAGS {
                return Err(UnifiedError::bad_request(format!(
                    "at most {MAX_TAGS} tags are allowed"
                )));
            }
            tags.push(require_text(raw, "tag", MAX_TAG_LEN)?);
        }
        let venue = match &input.venue {
            Some(venue) => {
                validate_venue(venue)?;
                Some(venue.clone())
            }
            None => None,
        };
        if input.ends_at <= input.starts_at {
            return Err(UnifiedError::bad_request("ends_at must be after starts_at"));
        }
        let timezone = match &input.timezone {
            Some(raw) => require_text(raw, "timezone", MAX_TIMEZONE_LEN)?,
            None => "UTC".to_string(),
        };
        if !valid_timezone(&timezone) {
            return Err(UnifiedError::bad_request(format!(
                "Invalid timezone: {timezone}. Use an IANA name such as \"Asia/Jakarta\" or \"UTC\""
            )));
        }
        let recurrence = match &input.recurrence {
            Some(rule) => {
                validate_recurrence(rule, input.starts_at)?;
                Some(rule.clone())
            }
            None => None,
        };
        let company_id = require_company_match(
            &_tenant,
            &req,
            &match &input.company_id {
                Some(raw) => Some(require_text(raw, "company_id", 128)?),
                None => None,
            },
        )?;
        let cover_storage_target = match &input.cover_storage_target {
            Some(raw) => Some(require_text(raw, "cover_storage_target", MAX_COVER_KEY_LEN)?),
            None => None,
        };
        let cover_storage_key = match &input.cover_storage_key {
            Some(raw) => Some(require_text(raw, "cover_storage_key", MAX_COVER_KEY_LEN)?),
            None => None,
        };
        if cover_storage_key.is_some() != cover_storage_target.is_some() {
            return Err(UnifiedError::bad_request(
                "cover_storage_target and cover_storage_key must be given together (storage module reference)"
            ));
        }
        let now = Utc::now();
        let event = Event {
            id: Uuid::now_v7(),
            title,
            description,
            slug,
            category,
            tags,
            venue,
            starts_at: input.starts_at,
            ends_at: input.ends_at,
            timezone,
            visibility: input.visibility.unwrap_or_default(),
            status: EventStatus::Draft,
            organizer_id: caller.actor.clone(),
            company_id,
            cover_storage_target,
            cover_storage_key,
            recurrence,
            created_at: now,
            updated_at: now,
        };
        state.events.insert(event.id, event.clone());
        materialise_occurrences(&state, &event);
        create_default_reminders(&state, &event);
        state.emit(
            "events.created",
            json!({
                "event_id": event.id.to_string(),
                "slug": event.slug,
                "title": event.title,
                "organizer_id": event.organizer_id,
                "starts_at": event.starts_at,
                "ends_at": event.ends_at,
                "status": "draft",
            }),
        );
        Ok(event)
    })();
    match result {
        Ok(event) => created(event),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_event(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EVENTS_VIEW)?;
        let event = load_event(&state, &path.into_inner())?;
        require_record_in_company_scope(&event.company_id, &_scope)?;
        Ok(public_event_view(&state, &event))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_event(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateEventRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Event> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        let mut event = load_event(&state, &path.into_inner())?;
        require_record_in_company_scope(&event.company_id, &_scope)?;
        can_manage_event(&ctx, &caller.role, &event, &caller.actor)?;
        if !matches!(event.status, EventStatus::Draft | EventStatus::Published) {
            return Err(UnifiedError::conflict(format!(
                "Only draft or published events can be updated (status is \"{}\")",
                event.status.as_str()
            )));
        }
        let input = body.into_inner();
        let mut times_changed = false;
        if let Some(title) = &input.title {
            event.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = &input.description {
            event.description = match description {
                Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(slug) = &input.slug {
            let slug = slug.trim().to_lowercase();
            if !valid_slug(&slug) {
                return Err(UnifiedError::bad_request(
                    "slug must be lowercase letters, digits, and single hyphens only",
                ));
            }
            if slug_in_use(&state, &slug, Some(event.id)) {
                return Err(UnifiedError::conflict(format!("slug is already in use: {slug}")));
            }
            event.slug = slug;
        }
        if let Some(category) = &input.category {
            let mut checked: Vec<String> = Vec::new();
            for raw in category {
                checked.push(require_text(raw, "category", MAX_CATEGORY_LEN)?);
            }
            event.category = checked;
        }
        if let Some(tags) = &input.tags {
            if tags.len() > MAX_TAGS {
                return Err(UnifiedError::bad_request(format!(
                    "at most {MAX_TAGS} tags are allowed"
                )));
            }
            let mut checked: Vec<String> = Vec::new();
            for raw in tags {
                checked.push(require_text(raw, "tag", MAX_TAG_LEN)?);
            }
            event.tags = checked;
        }
        if let Some(venue) = &input.venue {
            match venue {
                Some(value) => {
                    validate_venue(value)?;
                    event.venue = Some(value.clone());
                }
                None => event.venue = None,
            }
        }
        if let Some(starts_at) = input.starts_at {
            event.starts_at = starts_at;
            times_changed = true;
        }
        if let Some(ends_at) = input.ends_at {
            event.ends_at = ends_at;
            times_changed = true;
        }
        if event.ends_at <= event.starts_at {
            return Err(UnifiedError::bad_request("ends_at must be after starts_at"));
        }
        if let Some(timezone) = &input.timezone {
            let timezone = require_text(timezone, "timezone", MAX_TIMEZONE_LEN)?;
            if !valid_timezone(&timezone) {
                return Err(UnifiedError::bad_request(format!(
                    "Invalid timezone: {timezone}"
                )));
            }
            event.timezone = timezone;
        }
        if let Some(visibility) = input.visibility {
            event.visibility = visibility;
        }
        if let Some(company_id) = &input.company_id {
            let checked = match company_id {
                Some(raw) => Some(require_text(raw, "company_id", 128)?),
                None => None,
            };
            // A newly named company must match the caller scope;
            // an explicit clear stays a clear and is never
            // re-stamped with the caller scope.
            event.company_id = require_company_change_for(&_scope, &checked)?;
        }
        if let Some(target) = &input.cover_storage_target {
            event.cover_storage_target = match target {
                Some(raw) => Some(require_text(raw, "cover_storage_target", MAX_COVER_KEY_LEN)?),
                None => None,
            };
        }
        if let Some(key) = &input.cover_storage_key {
            event.cover_storage_key = match key {
                Some(raw) => Some(require_text(raw, "cover_storage_key", MAX_COVER_KEY_LEN)?),
                None => None,
            };
        }
        if event.cover_storage_key.is_some() != event.cover_storage_target.is_some() {
            return Err(UnifiedError::bad_request(
                "cover_storage_target and cover_storage_key must be given together",
            ));
        }
        if let Some(recurrence) = &input.recurrence {
            match recurrence {
                Some(rule) => {
                    validate_recurrence(rule, event.starts_at)?;
                    event.recurrence = Some(rule.clone());
                }
                None => event.recurrence = None,
            }
            times_changed = true;
        }
        event.updated_at = Utc::now();
        state.events.insert(event.id, event.clone());
        if times_changed {
            materialise_occurrences(&state, &event);
        }
        state.emit(
            "events.updated",
            json!({
                "event_id": event.id.to_string(),
                "slug": event.slug,
                "status": event.status.as_str(),
            }),
        );
        Ok(event)
    })();
    match result {
        Ok(event) => ok(event),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_event(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        let event = load_event(&state, &path.into_inner())?;
        require_record_in_company_scope(&event.company_id, &_scope)?;
        can_manage_event(&ctx, &caller.role, &event, &caller.actor)?;
        if !matches!(event.status, EventStatus::Draft | EventStatus::Cancelled) {
            return Err(UnifiedError::conflict(format!(
                "Only draft or cancelled events can be deleted (status is \"{}\"); cancel it first",
                event.status.as_str()
            )));
        }
        let has_registrations = state
            .registrations
            .iter()
            .any(|entry| entry.value().event_id == event.id);
        if has_registrations {
            return Err(UnifiedError::conflict(
                "An event with registrations cannot be deleted; cancel it instead",
            ));
        }
        let id = event.id;
        state.events.remove(&id);
        let stale_occurrences: Vec<Uuid> = state
            .occurrences
            .iter()
            .filter(|entry| entry.value().event_id == id)
            .map(|entry| *entry.key())
            .collect();
        for key in stale_occurrences {
            state.occurrences.remove(&key);
        }
        let stale_reminders: Vec<Uuid> = state
            .reminders
            .iter()
            .filter(|entry| entry.value().event_id == id)
            .map(|entry| *entry.key())
            .collect();
        for key in stale_reminders {
            state.reminders.remove(&key);
        }
        let stale_tickets: Vec<Uuid> = state
            .ticket_types
            .iter()
            .filter(|entry| entry.value().event_id == id)
            .map(|entry| *entry.key())
            .collect();
        for key in stale_tickets {
            state.ticket_types.remove(&key);
        }
        let stale_waitlist: Vec<Uuid> = state
            .waitlist
            .iter()
            .filter(|entry| entry.value().event_id == id)
            .map(|entry| *entry.key())
            .collect();
        for key in stale_waitlist {
            state.waitlist.remove(&key);
        }
        state.emit(
            "events.deleted",
            json!({ "event_id": id.to_string(), "slug": event.slug }),
        );
        Ok(json!({ "deleted": true, "event_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) fn transition_event(
    ctx: &AppContext,
    state: &EventsState,
    req: &HttpRequest,
    raw_id: &str,
    from: &[EventStatus],
    to: EventStatus,
    event_name: &str,
) -> UnifiedResult<Event> {
    // Tenant edge guard (C3 pilot 3): this shared helper is the
    // single choke point for publish / cancel / complete, so the
    // gate lives here once instead of in each wrapper.
    let _tenant = require_tenant(req)?;
    let _scope = caller_company(&_tenant, req)?;
    let caller = caller(req)?;
    let mut event = load_event(state, raw_id)?;
    require_record_in_company_scope(&event.company_id, &_scope)?;
    can_manage_event(ctx, &caller.role, &event, &caller.actor)?;
    if !from.contains(&event.status) {
        return Err(UnifiedError::conflict(format!(
            "Only {} can move to \"{}\" (status is \"{}\")",
            from.iter()
                .map(|status| status.as_str())
                .collect::<Vec<_>>()
                .join(" or "),
            to.as_str(),
            event.status.as_str()
        )));
    }
    event.status = to;
    event.updated_at = Utc::now();
    state.events.insert(event.id, event.clone());
    state.emit(
        event_name,
        json!({
            "event_id": event.id.to_string(),
            "slug": event.slug,
            "status": to.as_str(),
        }),
    );
    Ok(event)
}

pub(crate) async fn publish_event(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = transition_event(
        &ctx,
        &state,
        &req,
        &path.into_inner(),
        &[EventStatus::Draft],
        EventStatus::Published,
        "events.published",
    );
    match result {
        Ok(event) => ok(event),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn cancel_event(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = transition_event(
        &ctx,
        &state,
        &req,
        &path.into_inner(),
        &[EventStatus::Draft, EventStatus::Published, EventStatus::Ongoing],
        EventStatus::Cancelled,
        "events.cancelled",
    );
    match result {
        Ok(event) => ok(event),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn complete_event(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = transition_event(
        &ctx,
        &state,
        &req,
        &path.into_inner(),
        &[EventStatus::Published, EventStatus::Ongoing],
        EventStatus::Completed,
        "events.completed",
    );
    match result {
        Ok(event) => ok(event),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Occurrence handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_occurrences(
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
        let status_filter = params.filters.get("status").cloned();
        let mut items = occurrences_for(&state, event.id);
        if let Some(status) = &status_filter {
            items.retain(|occurrence| &occurrence.status == status);
        }
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn cancel_occurrence(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<EventOccurrence> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        let (event_id, occurrence_id) = path.into_inner();
        let event = load_event(&state, &event_id)?;
        require_record_in_company_scope(&event.company_id, &_scope)?;
        can_manage_event(&ctx, &caller.role, &event, &caller.actor)?;
        let mut occurrence = load_occurrence(&state, event.id, &occurrence_id)?;
        if occurrence.status == "cancelled" {
            return Err(UnifiedError::conflict("This occurrence is already cancelled"));
        }
        occurrence.status = "cancelled".to_string();
        occurrence.overridden = true;
        occurrence.updated_at = Utc::now();
        state.occurrences.insert(occurrence.id, occurrence.clone());
        state.emit(
            "events.occurrence.cancelled",
            json!({
                "event_id": event.id.to_string(),
                "occurrence_id": occurrence.id.to_string(),
                "sequence": occurrence.sequence,
                "starts_at": occurrence.starts_at,
            }),
        );
        Ok(occurrence)
    })();
    match result {
        Ok(occurrence) => ok(occurrence),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn override_occurrence(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<EventsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<OverrideOccurrenceRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<EventOccurrence> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        let (event_id, occurrence_id) = path.into_inner();
        let event = load_event(&state, &event_id)?;
        require_record_in_company_scope(&event.company_id, &_scope)?;
        can_manage_event(&ctx, &caller.role, &event, &caller.actor)?;
        let mut occurrence = load_occurrence(&state, event.id, &occurrence_id)?;
        let input = body.into_inner();
        if input.cancel {
            occurrence.status = "cancelled".to_string();
            occurrence.overridden = true;
            occurrence.updated_at = Utc::now();
            state.occurrences.insert(occurrence.id, occurrence.clone());
            state.emit(
                "events.occurrence.cancelled",
                json!({
                    "event_id": event.id.to_string(),
                    "occurrence_id": occurrence.id.to_string(),
                    "sequence": occurrence.sequence,
                }),
            );
            return Ok(occurrence);
        }
        if occurrence.status == "cancelled" {
            return Err(UnifiedError::conflict(
                "A cancelled occurrence cannot be overridden; it stays cancelled",
            ));
        }
        if let Some(starts_at) = input.starts_at {
            occurrence.starts_at = starts_at;
        }
        if let Some(ends_at) = input.ends_at {
            occurrence.ends_at = ends_at;
        }
        if occurrence.ends_at <= occurrence.starts_at {
            return Err(UnifiedError::bad_request("ends_at must be after starts_at"));
        }
        if let Some(venue) = &input.venue {
            validate_venue(venue)?;
            occurrence.venue_override = Some(venue.clone());
        }
        occurrence.overridden = true;
        occurrence.updated_at = Utc::now();
        state.occurrences.insert(occurrence.id, occurrence.clone());
        state.emit(
            "events.occurrence.overridden",
            json!({
                "event_id": event.id.to_string(),
                "occurrence_id": occurrence.id.to_string(),
                "sequence": occurrence.sequence,
                "starts_at": occurrence.starts_at,
                "ends_at": occurrence.ends_at,
            }),
        );
        Ok(occurrence)
    })();
    match result {
        Ok(occurrence) => ok(occurrence),
        Err(error) => err(error, debug),
    }
}
