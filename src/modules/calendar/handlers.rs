//! calendar module - HTTP handlers for views, items, settings, sources and feeds.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use uuid::Uuid;

use super::aggregation::aggregate_items;
use super::aggregation::conflict_hints;
use super::aggregation::default_range;
use super::aggregation::effective_settings;
use super::aggregation::non_working_days;
use super::constants::CALENDAR_VIEWS;
use super::constants::DEFAULT_CREATE_LINK_TEMPLATES;
use super::constants::DEFAULT_FEED_TTL_DAYS;
use super::constants::EVENT_CREATE_INTENT_REQUESTED;
use super::constants::EVENT_CREATE_REQUESTED;
use super::constants::EVENT_VIEW_REQUESTED;
use super::constants::MAX_FEED_LABEL_LEN;
use super::constants::MAX_FEED_TTL_DAYS;
use super::constants::MAX_FEEDS_PER_USER;
use super::constants::MAX_ITEMS_RANGE_DAYS;
use super::constants::MAX_MODULE_FILTER;
use super::constants::MAX_SOURCES_PER_USER;
use super::constants::MAX_VIEW_DAYS;
use super::constants::MIN_FEED_TTL_DAYS;
use super::constants::PERM_CALENDAR_MANAGE;
use super::constants::PERM_CALENDAR_READ;
use super::helpers::Caller;
use super::helpers::caller;
use super::helpers::constant_time_eq;
use super::helpers::created;
use super::helpers::err;
use super::helpers::ok;
use super::helpers::paginate;
use super::helpers::require_permission;
use super::helpers::resolve_tenant;
use super::ics::ics_calendar;
use super::ics::ics_response;
use super::state::CalendarState;
use super::types::CalendarFeed;
use super::types::CalendarItem;
use super::types::CalendarItemsQuery;
use super::types::CalendarSettings;
use super::types::CalendarSource;
use super::types::CalendarViewQuery;
use super::types::CreateFeedRequest;
use super::types::CreateIntentRequest;
use super::types::UpdateSettingsRequest;
use super::types::UpdateSourcesRequest;
use super::validation::parse_module_filter;
use super::validation::parse_time;
use super::validation::parse_timezone;
use super::validation::url_encode;
use super::validation::validate_country;
use super::validation::validate_create_template;
use super::validation::validate_create_url;
use super::validation::validate_module_id;
use super::validation::validate_weekend_days;
use super::validation::validate_working_hours;

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

/// GET /api/v1/calendar — month/week/day/agenda view: aggregated
/// items (with viewer-local times), non-working days, and conflict
/// hints. Empty when no module answers; never an error. Returns
/// every item in the range by default; pass `?page=` / `?per_page=`
/// for the standard paginated shape instead (same `PaginationParams`
/// / `PageResult` as `GET /api/v1/calendar/items`).
pub(crate) async fn get_calendar_view(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CalendarState>,
    req: HttpRequest,
    query: web::Query<CalendarViewQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALENDAR_READ)?;
        let settings = effective_settings(&state, &caller);
        let view = query
            .view
            .clone()
            .unwrap_or_else(|| settings.default_view.clone());
        if !CALENDAR_VIEWS.contains(&view.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "view must be one of {}",
                CALENDAR_VIEWS.join(", ")
            )));
        }
        let offset = parse_timezone(
            query
                .timezone
                .as_deref()
                .or(Some(settings.timezone.as_str())),
        )?;
        let now = Utc::now();
        let (default_from, default_to) = default_range(&view, now);
        let from = match &query.from {
            Some(raw) => parse_time(raw, "from")?,
            None => default_from,
        };
        let to = match &query.to {
            Some(raw) => parse_time(raw, "to")?,
            None => default_to,
        };
        if to <= from {
            return Err(UnifiedError::bad_request("to must be after from"));
        }
        if (to - from).num_days() > MAX_VIEW_DAYS {
            return Err(UnifiedError::bad_request(format!(
                "View range must be at most {MAX_VIEW_DAYS} days"
            )));
        }
        let modules = parse_module_filter(query.modules.as_deref())?;
        let (items, skipped) = aggregate_items(&ctx, &state, &caller, from, to, modules.as_deref());
        let rendered: Vec<Value> = items
            .iter()
            .map(|item| item.with_local_times(offset).to_json())
            .collect();
        let non_working = non_working_days(
            &ctx,
            &caller.tenant_id,
            &settings,
            from.date_naive(),
            to.date_naive(),
        );
        let conflicts = conflict_hints(&items);
        state.emit(
            EVENT_VIEW_REQUESTED,
            json!({
                "tenant_id": caller.tenant_id,
                "user_id": caller.actor,
                "view": view,
                "from": from.to_rfc3339(),
                "to": to.to_rfc3339(),
                "item_count": items.len(),
            }),
        );
        // A view is a date-range aggregation, not a list: the full
        // range is returned unless the caller explicitly asks for a
        // page, in which case the standard pagination shape applies
        // (`items` becomes the page + a `pagination` object).
        let item_count = rendered.len();
        let (rendered, pagination) =
            if query.pagination.page.is_some() || query.pagination.per_page.is_some() {
                let page = paginate(rendered, &query.pagination);
                (page.items, Some(page.pagination))
            } else {
                (rendered, None)
            };
        let mut data = json!({
            "view": view,
            "from": from.to_rfc3339(),
            "to": to.to_rfc3339(),
            "timezone": query.timezone.clone().unwrap_or(settings.timezone.clone()),
            "items": rendered,
            "item_count": item_count,
            "skipped_malformed_items": skipped,
            "non_working_days": non_working,
            "conflicts": conflicts,
            "working_hours": {
                "start": settings.working_hours_start,
                "end": settings.working_hours_end,
            },
            "weekend_days": settings.weekend_days,
        });
        if let Some(pagination) = pagination {
            data["pagination"] = json!(pagination);
        }
        Ok(data)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// GET /api/v1/calendar/items — the same aggregation, paginated
/// and filtered. `from`/`to` default to the current month.
pub(crate) async fn list_calendar_items(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CalendarState>,
    req: HttpRequest,
    query: web::Query<CalendarItemsQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALENDAR_READ)?;
        let settings = effective_settings(&state, &caller);
        let offset = parse_timezone(
            query
                .timezone
                .as_deref()
                .or(Some(settings.timezone.as_str())),
        )?;
        let now = Utc::now();
        let (default_from, default_to) = default_range("month", now);
        let from = match &query.from {
            Some(raw) => parse_time(raw, "from")?,
            None => default_from,
        };
        let to = match &query.to {
            Some(raw) => parse_time(raw, "to")?,
            None => default_to,
        };
        if to <= from {
            return Err(UnifiedError::bad_request("to must be after from"));
        }
        if (to - from).num_days() > MAX_ITEMS_RANGE_DAYS {
            return Err(UnifiedError::bad_request(format!(
                "Items range must be at most {MAX_ITEMS_RANGE_DAYS} days"
            )));
        }
        let modules = parse_module_filter(query.modules.as_deref())?;
        let (items, skipped) = aggregate_items(&ctx, &state, &caller, from, to, modules.as_deref());
        let rendered: Vec<CalendarItem> = items
            .iter()
            .map(|item| item.with_local_times(offset))
            .collect();
        let page = paginate(rendered, &query.pagination);
        let mut data = page.to_json();
        data["skipped_malformed_items"] = json!(skipped);
        Ok(data)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// POST /api/v1/calendar/create-intent — resolve the owning
/// module's create deep link for a date/time. The calendar NEVER
/// creates a record itself.
pub(crate) async fn create_intent(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CalendarState>,
    req: HttpRequest,
    body: web::Json<CreateIntentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALENDAR_READ)?;
        let input = body.into_inner();
        let module = validate_module_id(&input.module)?;
        let start = parse_time(&input.start, "start")?;
        let end = match &input.end {
            Some(raw) => Some(parse_time(raw, "end")?),
            None => None,
        };
        if let Some(end) = end {
            if end <= start {
                return Err(UnifiedError::bad_request("end must be after start"));
            }
        }
        let title = match &input.title {
            Some(title) if title.trim().is_empty() => None,
            Some(title) if title.chars().count() > 200 => {
                return Err(UnifiedError::bad_request(
                    "title must be at most 200 characters",
                ));
            }
            Some(title) => Some(title.trim().to_string()),
            None => None,
        };
        // Only same-site URLs are ever returned: a hook answer or
        // a template that resolves to a `javascript:`/`data:` URL,
        // or to another host, is skipped (hook) or refused (see
        // the template branch below) instead of served to the
        // client. The allowed host is the host this request
        // arrived on; with no Host header only relative paths pass.
        let site_host = req.connection_info().host().to_string();
        let allowed_hosts: Vec<&str> = if site_host.is_empty() {
            Vec::new()
        } else {
            vec![site_host.as_str()]
        };
        let answer = ctx.hooks.emit(
            EVENT_CREATE_REQUESTED,
            json!({
                "tenant_id": caller.tenant_id,
                "module": module,
                "start": start.to_rfc3339(),
                "end": end.map(|value| value.to_rfc3339()),
                "title": title,
                "user_id": caller.actor,
                "company_id": caller.company_id,
            }),
        );
        for result in &answer.results {
            if let Some(url) = result.get("url").and_then(Value::as_str) {
                let Ok(url) = validate_create_url(url, &allowed_hosts) else {
                    continue;
                };
                state.emit(
                    EVENT_CREATE_INTENT_REQUESTED,
                    json!({
                        "tenant_id": caller.tenant_id,
                        "module": module,
                        "url": &url,
                        "user_id": caller.actor,
                    }),
                );
                return Ok(json!({
                    "module": module,
                    "url": url,
                    "start": start.to_rfc3339(),
                    "end": end.map(|value| value.to_rfc3339()),
                    "source": "module_hook",
                }));
            }
        }
        // Fallback: a configurable deep-link template (admin-set
        // templates win over the built-in defaults; the hook answer
        // above always wins over both).
        let settings = effective_settings(&state, &caller);
        let template = settings
            .create_link_templates
            .get(&module)
            .cloned()
            .or_else(|| {
                DEFAULT_CREATE_LINK_TEMPLATES
                    .iter()
                    .find(|(name, _)| *name == module)
                    .map(|(_, template)| (*template).to_string())
            });
        match template {
            Some(template) => {
                let url = template
                    .replace("{start}", &url_encode(&start.to_rfc3339()))
                    .replace(
                        "{end}",
                        &url_encode(
                            &end.unwrap_or(start + chrono::Duration::hours(1))
                                .to_rfc3339(),
                        ),
                    )
                    .replace("{title}", &url_encode(title.as_deref().unwrap_or("")));
                // The rendered template is validated like a hook
                // answer: an admin-set template pointing off-site
                // (or at a script scheme) is a configuration
                // error, answered 503 rather than served.
                let url = validate_create_url(&url, &allowed_hosts).map_err(|_| {
                    UnifiedError::new(
                        format!(
                            "Create-link template for module \"{module}\" does not resolve to a same-site http(s) URL"
                        ),
                        503,
                    )
                })?;
                Ok(json!({
                    "module": module,
                    "url": url,
                    "start": start.to_rfc3339(),
                    "end": end.map(|value| value.to_rfc3339()),
                    "source": "template",
                }))
            }
            None => Err(UnifiedError::new(
                format!(
                    "No owning module answered calendar.create_requested for module \"{module}\" and no create-link template is configured for it"
                ),
                503,
            )),
        }
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// GET /api/v1/calendar/settings — effective settings (company
/// defaults merged with the caller's overrides).
pub(crate) async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CalendarState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CalendarSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALENDAR_READ)?;
        Ok(effective_settings(&state, &caller))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// PUT /api/v1/calendar/settings — save settings. `scope` is
/// "user" (default) or "company"; both are tenant-scoped.
pub(crate) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CalendarState>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CalendarSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALENDAR_MANAGE)?;
        let input = body.into_inner();
        let scope = input.scope.as_deref().unwrap_or("user");
        if scope != "user" && scope != "company" {
            return Err(UnifiedError::bad_request(
                "scope must be \"user\" or \"company\"",
            ));
        }
        // Company scope needs a company up front: rejecting here
        // (instead of loading defaults and failing at insert time)
        // keeps the error at the validation stage, before any
        // other field is processed.
        let company_id = if scope == "company" {
            Some(caller.company_id.clone().ok_or_else(|| {
                UnifiedError::bad_request("Company scope needs a company on the tenant context")
            })?)
        } else {
            None
        };
        let mut settings = if scope == "company" {
            let company_id = company_id.as_ref().expect("company scope has a company id");
            let key = format!("{}:{company_id}", caller.tenant_id);
            state
                .company_settings
                .get(&key)
                .map(|entry| entry.clone())
                .unwrap_or_default()
        } else {
            let key = format!("{}:{}", caller.tenant_id, caller.actor);
            state
                .user_settings
                .get(&key)
                .map(|entry| entry.clone())
                .unwrap_or_default()
        };
        if let Some(days) = &input.weekend_days {
            settings.weekend_days = validate_weekend_days(days)?;
        }
        // Working hours are validated as a pair against the merged
        // settings, so a start supplied on its own is still
        // checked against the stored end (and vice versa): the
        // window must be non-empty, start strictly before end.
        if input.working_hours_start.is_some() || input.working_hours_end.is_some() {
            let (start, end) = validate_working_hours(
                input
                    .working_hours_start
                    .as_deref()
                    .unwrap_or(&settings.working_hours_start),
                input
                    .working_hours_end
                    .as_deref()
                    .unwrap_or(&settings.working_hours_end),
            )?;
            settings.working_hours_start = start;
            settings.working_hours_end = end;
        }
        if let Some(view) = &input.default_view {
            if !CALENDAR_VIEWS.contains(&view.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "default_view must be one of {}",
                    CALENDAR_VIEWS.join(", ")
                )));
            }
            settings.default_view = view.clone();
        }
        if let Some(timezone) = &input.timezone {
            // Validated only; the parsed offset is not stored.
            parse_timezone(Some(timezone))?;
            settings.timezone = timezone.trim().to_string();
        }
        if let Some(country) = &input.holiday_country {
            settings.holiday_country = validate_country(country)?;
        }
        if let Some(templates) = &input.create_link_templates {
            if templates.len() > MAX_MODULE_FILTER {
                return Err(UnifiedError::bad_request(format!(
                    "At most {MAX_MODULE_FILTER} create-link templates may be stored"
                )));
            }
            let mut cleaned: HashMap<String, String> = HashMap::new();
            for (module, template) in templates {
                let module = validate_module_id(module)?;
                if template.len() > 512 {
                    return Err(UnifiedError::bad_request(
                        "A create-link template must be at most 512 characters",
                    ));
                }
                // A template that could never render into a safe
                // same-site URL is refused at save time instead of
                // failing every create-intent call later.
                validate_create_template(template)?;
                cleaned.insert(module, template.clone());
            }
            settings.create_link_templates = cleaned;
        }
        if scope == "company" {
            let company_id = company_id.expect("company scope has a company id");
            let key = format!("{}:{company_id}", caller.tenant_id);
            state.company_settings.insert(key, settings.clone());
        } else {
            let key = format!("{}:{}", caller.tenant_id, caller.actor);
            state.user_settings.insert(key, settings.clone());
        }
        state.emit(
            "calendar.settings.updated",
            json!({
                "tenant_id": caller.tenant_id,
                "user_id": caller.actor,
                "scope": scope,
            }),
        );
        Ok(settings)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// GET /api/v1/calendar/sources — the caller's per-module
/// visibility toggles (absent means visible).
pub(crate) async fn get_sources(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CalendarState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALENDAR_READ)?;
        let mut sources: Vec<CalendarSource> = state
            .sources
            .iter()
            .filter(|entry| {
                entry.value().tenant_id == caller.tenant_id && entry.value().user_id == caller.actor
            })
            .map(|entry| entry.value().clone())
            .collect();
        sources.sort_by(|left, right| left.module.cmp(&right.module));
        Ok(json!({ "sources": sources }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// PUT /api/v1/calendar/sources — replace the caller's toggles
/// (full set; a module absent from the list is visible).
pub(crate) async fn update_sources(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CalendarState>,
    req: HttpRequest,
    body: web::Json<UpdateSourcesRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALENDAR_MANAGE)?;
        let input = body.into_inner();
        if input.sources.len() > MAX_SOURCES_PER_USER {
            return Err(UnifiedError::bad_request(format!(
                "At most {MAX_SOURCES_PER_USER} source toggles may be stored"
            )));
        }
        let mut cleaned: Vec<CalendarSource> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for toggle in &input.sources {
            let module = validate_module_id(&toggle.module)?;
            if !seen.insert(module.clone()) {
                return Err(UnifiedError::bad_request(format!(
                    "Duplicate source toggle for module \"{module}\""
                )));
            }
            cleaned.push(CalendarSource {
                tenant_id: caller.tenant_id.clone(),
                user_id: caller.actor.clone(),
                module,
                visible: toggle.visible,
                updated_at: Utc::now(),
            });
        }
        let prefix = format!("{}:{}", caller.tenant_id, caller.actor);
        state.sources.retain(|key, _| !key.starts_with(&prefix));
        for source in &cleaned {
            let key = format!("{}:{}:{}", source.tenant_id, source.user_id, source.module);
            state.sources.insert(key, source.clone());
        }
        state.emit(
            "calendar.sources.updated",
            json!({
                "tenant_id": caller.tenant_id,
                "user_id": caller.actor,
                "source_count": cleaned.len(),
            }),
        );
        Ok(json!({ "sources": cleaned }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// POST /api/v1/calendar/feeds — issue a revocable iCal
/// subscription token. The raw token is returned exactly once;
/// only its SHA-256 hash is stored.
pub(crate) async fn create_feed(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CalendarState>,
    req: HttpRequest,
    body: web::Json<CreateFeedRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALENDAR_MANAGE)?;
        let input = body.into_inner();
        let mut modules: Vec<String> = Vec::new();
        if let Some(requested) = &input.modules {
            if requested.len() > MAX_MODULE_FILTER {
                return Err(UnifiedError::bad_request(format!(
                    "At most {MAX_MODULE_FILTER} modules may be scoped to one feed"
                )));
            }
            for module in requested {
                let module = validate_module_id(module)?;
                if !modules.contains(&module) {
                    modules.push(module);
                }
            }
        }
        let ttl_days = input.expires_in_days.unwrap_or(DEFAULT_FEED_TTL_DAYS);
        if !(MIN_FEED_TTL_DAYS..=MAX_FEED_TTL_DAYS).contains(&ttl_days) {
            return Err(UnifiedError::bad_request(format!(
                "expires_in_days must be between {MIN_FEED_TTL_DAYS} and {MAX_FEED_TTL_DAYS}"
            )));
        }
        let label = match &input.label {
            Some(label) if label.trim().is_empty() => None,
            Some(label) if label.chars().count() > MAX_FEED_LABEL_LEN => {
                return Err(UnifiedError::bad_request(format!(
                    "label must be at most {MAX_FEED_LABEL_LEN} characters"
                )));
            }
            Some(label) => Some(label.trim().to_string()),
            None => None,
        };
        let feed_id = Uuid::new_v4();
        let (token, expires_at) = state.issue_token(feed_id, ttl_days * 86_400);
        let feed = CalendarFeed {
            id: feed_id,
            tenant_id: caller.tenant_id.clone(),
            company_id: caller.company_id.clone(),
            user_id: caller.actor.clone(),
            token_hash: CalendarState::hash_token(&token),
            modules: modules.clone(),
            label,
            expires_at,
            revoked: false,
            created_at: Utc::now(),
            last_used_at: None,
        };
        // Cap check and insert are atomic under the owner's
        // feed-creation lock (see `insert_feed_if_under_cap`):
        // counting first and inserting afterwards would let two
        // concurrent creates both pass and exceed the cap.
        if !state.insert_feed_if_under_cap(feed.clone()) {
            return Err(UnifiedError::bad_request(format!(
                "At most {MAX_FEEDS_PER_USER} active feeds per user"
            )));
        }
        state.emit(
            "calendar.feed.created",
            json!({
                "tenant_id": caller.tenant_id,
                "feed_id": feed_id.to_string(),
                "user_id": caller.actor,
                "expires_at": expires_at.to_rfc3339(),
            }),
        );
        Ok(json!({
            "id": feed.id,
            "tenant_id": feed.tenant_id,
            "modules": feed.modules,
            "label": feed.label,
            "expires_at": feed.expires_at.to_rfc3339(),
            "revoked": feed.revoked,
            "created_at": feed.created_at.to_rfc3339(),
            "feed_url": format!("/api/v1/calendar/feed/{token}"),
            "token": token,
        }))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

/// GET /api/v1/calendar/feeds — the caller's own feed tokens
/// (metadata only; raw tokens are never returned again).
pub(crate) async fn list_feeds(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CalendarState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALENDAR_MANAGE)?;
        let mut feeds: Vec<CalendarFeed> = state
            .feeds
            .iter()
            .filter(|entry| {
                entry.value().tenant_id == caller.tenant_id && entry.value().user_id == caller.actor
            })
            .map(|entry| entry.value().clone())
            .collect();
        feeds.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        let page = paginate(feeds, &query);
        Ok(page.to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// DELETE /api/v1/calendar/feeds/{id} — revoke a feed token.
/// A feed from another tenant (or a missing one) answers 404.
pub(crate) async fn revoke_feed(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CalendarState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_CALENDAR_MANAGE)?;
        let feed_id = Uuid::parse_str(path.as_str())
            .map_err(|_| UnifiedError::not_found("Feed not found"))?;
        let mut feed = state
            .feeds
            .get_mut(&feed_id)
            .filter(|entry| entry.tenant_id == caller.tenant_id)
            .ok_or_else(|| UnifiedError::not_found("Feed not found"))?;
        if feed.user_id != caller.actor {
            return Err(UnifiedError::not_found("Feed not found"));
        }
        feed.revoked = true;
        let revoked_id = feed.id;
        drop(feed);
        state.emit(
            "calendar.feed.revoked",
            json!({
                "tenant_id": caller.tenant_id,
                "feed_id": revoked_id.to_string(),
                "user_id": caller.actor,
            }),
        );
        Ok(json!({ "id": revoked_id, "revoked": true }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Verify a feed token. Every failure answers the same 404 so
/// token existence never leaks — including a token whose feed
/// belongs to another tenant. The resolved tenant must match the
/// feed's tenant.
pub(crate) fn verify_feed_token(
    state: &CalendarState,
    tenant_id: &str,
    raw_token: &str,
) -> Option<CalendarFeed> {
    let token = raw_token.strip_suffix(".ics").unwrap_or(raw_token);
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let feed_id = Uuid::parse_str(parts[0]).ok()?;
    let expiry: i64 = parts[1].parse().ok()?;
    if expiry <= Utc::now().timestamp() {
        return None;
    }
    let expected = state.sign_token(&feed_id.to_string(), expiry);
    if !constant_time_eq(&expected, parts[2]) {
        return None;
    }
    let mut feed = state.feeds.get_mut(&feed_id)?;
    if feed.revoked || feed.expires_at <= Utc::now() {
        return None;
    }
    if feed.tenant_id != tenant_id {
        return None;
    }
    if CalendarState::hash_token(token) != feed.token_hash {
        return None;
    }
    feed.last_used_at = Some(Utc::now());
    let verified = (*feed).clone();
    Some(verified)
}

/// GET /api/v1/calendar/feed/{token} — public iCal subscription
/// feed. No `X-User-Id` header: the signed token is the
/// credential, and the request must still resolve to the feed's
/// tenant (no tenant context, or a foreign tenant, answers 404).
/// A ".ics" suffix on the token is accepted.
pub(crate) async fn get_feed_ics(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<CalendarState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match resolve_tenant(&req) {
        Ok(tenant) => tenant,
        Err(_) => {
            return err(UnifiedError::not_found("Feed not found"), debug);
        }
    };
    let feed = match verify_feed_token(&state, &tenant.tenant_id.to_string(), path.as_str()) {
        Some(feed) => feed,
        None => return err(UnifiedError::not_found("Feed not found"), debug),
    };
    let owner = Caller {
        actor: feed.user_id.clone(),
        role: String::new(),
        tenant_id: feed.tenant_id.clone(),
        schema_name: tenant.schema_name.clone(),
        company_id: feed.company_id.clone(),
    };
    let now = Utc::now();
    let from = now - chrono::Duration::days(30);
    let to = now + chrono::Duration::days(340);
    let module_filter = if feed.modules.is_empty() {
        None
    } else {
        Some(feed.modules.clone())
    };
    let (items, _) = aggregate_items(&ctx, &state, &owner, from, to, module_filter.as_deref());
    let name = feed
        .label
        .clone()
        .unwrap_or_else(|| "Unified Calendar".to_string());
    ics_response(ics_calendar(&items, &name))
}
