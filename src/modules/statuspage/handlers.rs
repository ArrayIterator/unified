//! HTTP handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

/// PUBLIC: overall status summary as JSON.
pub(crate) async fn public_summary(
    state: web::Data<Arc<StatusPageState>>,
    ctx: web::Data<Arc<AppContext>>,
) -> HttpResponse {
    let overall = state.refresh_overall(&ctx);
    let components = sorted_components(&state);
    let open = open_incidents(&state);
    json::success_response(
        json!({
            "status": overall,
            "maintenance": ctx.is_maintenance(),
            "components": components,
            "open_incidents": open,
            "open_incident_count": open.len(),
        }),
        200,
    )
}

/// PUBLIC: the status page as HTML.
pub(crate) async fn public_page(
    state: web::Data<Arc<StatusPageState>>,
    ctx: web::Data<Arc<AppContext>>,
) -> HttpResponse {
    let overall = state.refresh_overall(&ctx);
    let components = sorted_components(&state);
    let open = open_incidents(&state);
    let mut recent: Vec<Incident> = state
        .incidents
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    recent.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    recent.truncate(10);
    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(render_status_html(&overall, &components, &open, &recent))
}

/// PUBLIC: component list with per-component uptime.
pub(crate) async fn list_components(
    state: web::Data<Arc<StatusPageState>>,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    state.refresh_overall(&ctx);
    let params = query.into_inner();
    let mut items: Vec<ComponentStatusView> = sorted_components(&state)
        .into_iter()
        .filter(|view| {
            matches_search(
                &format!("{} {}", view.component.name, view.component.description),
                &params,
            ) && params.filters.get("status").map_or(true, |status| {
                view.component.status == *status
            }) && params.filters.get("group").map_or(true, |group| {
                view.component.group.as_deref() == Some(group.as_str())
            })
        })
        .collect();
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

/// PUBLIC: daily uptime history (per component, or all components).
pub(crate) async fn uptime_history(
    state: web::Data<Arc<StatusPageState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let days: i64 = params
        .filters
        .get("days")
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(30)
        .clamp(1, 90);
    let today = Utc::now().date_naive();
    let start = today - chrono::Duration::days(days - 1);
    let mut items: Vec<DailyUptime> = state
        .uptime
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|record| {
            let Ok(date) = parse_date(&record.date) else {
                return false;
            };
            date >= start
                && params.filters.get("component_id").map_or(true, |id| {
                    record.component_id.to_string() == *id
                })
        })
        .collect();
    items.sort_by(|a, b| {
        b.date
            .cmp(&a.date)
            .then_with(|| a.component_id.cmp(&b.component_id))
    });
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

/// PUBLIC: incident list (paginated history).
pub(crate) async fn list_incidents(
    state: web::Data<Arc<StatusPageState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<Incident> = state
        .incidents
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|incident| {
            matches_search(
                &format!("{} {}", incident.title, incident.description),
                &params,
            ) && params.filters.get("status").map_or(true, |status| {
                incident.status == *status
            }) && params.filters.get("severity").map_or(true, |severity| {
                incident.severity == *severity
            })
        })
        .collect();
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

pub(crate) async fn get_incident(
    state: web::Data<Arc<StatusPageState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Incident> = (|| {
        let id = parse_uuid(path.as_str(), "incident")?;
        state
            .incidents
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Incident not found"))
    })();
    json::respond(result, false)
}

pub(crate) async fn list_incident_updates(
    state: web::Data<Arc<StatusPageState>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let result: UnifiedResult<PageResult<IncidentUpdate>> = (|| {
        let id = parse_uuid(path.as_str(), "incident")?;
        if !state.incidents.contains_key(&id) {
            return Err(UnifiedError::not_found("Incident not found"));
        }
        let mut items: Vec<IncidentUpdate> = state
            .updates
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|update| update.incident_id == id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, false)
}

/// Derive one consistent (uptime_percent, minutes_down) pair for a
/// day: `minutes_down` is authoritative when given, otherwise it is
/// derived from `uptime_percent`, and the percent is always
/// recomputed from the minutes — so a stored aggregate can never
/// claim 100% uptime with 1440 minutes down, and the value agrees
/// with `add_downtime`, which recomputes the same way.
fn derive_uptime(uptime_percent: f64, minutes_down: Option<i64>) -> (f64, i64) {
    let minutes = match minutes_down {
        Some(minutes) => minutes,
        None => ((100.0 - uptime_percent) / 100.0 * 1440.0).round() as i64,
    };
    let minutes = minutes.clamp(0, 1440);
    (
        round_percent(100.0 * (1440 - minutes) as f64 / 1440.0),
        minutes,
    )
}

// ---------------------------------------------------------------------------
// Admin handlers (components, incidents)
// ---------------------------------------------------------------------------

pub(crate) async fn create_component(
    state: web::Data<Arc<StatusPageState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateComponentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<StatusComponent> = (|| {
        require_statuspage_manage(&ctx, &req)?;
        let input = body.into_inner();
        validate_component_name(&input.name)?;
        let description = validate_description(&input.description)?;
        let status = input.status.clone().unwrap_or_else(|| COMPONENT_OPERATIONAL.to_string());
        if !valid_component_status(&status) {
            return Err(UnifiedError::bad_request(format!(
                "Invalid component status \"{status}\": expected one of operational, degraded, partial_outage, major_outage, maintenance"
            )));
        }
        let now = Utc::now();
        let component = StatusComponent {
            id: Uuid::now_v7(),
            name: input.name.trim().to_string(),
            description,
            group: input.group.map(|group| group.trim().to_string()).filter(|group| !group.is_empty()),
            status,
            sort_order: input.sort_order.unwrap_or(0),
            created_at: now,
            updated_at: now,
        };
        state.components.insert(component.id, component.clone());
        state.refresh_overall(&ctx);
        Ok(component)
    })();
    match result {
        Ok(component) => json::success_response(component, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_component(
    state: web::Data<Arc<StatusPageState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<ComponentStatusView> = (|| {
        let id = parse_uuid(path.as_str(), "component")?;
        let component = state
            .components
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Component not found"))?;
        Ok(state.component_view(&component))
    })();
    json::respond(result, false)
}

pub(crate) async fn update_component(
    state: web::Data<Arc<StatusPageState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateComponentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<StatusComponent> = (|| {
        require_statuspage_manage(&ctx, &req)?;
        let id = parse_uuid(path.as_str(), "component")?;
        let input = body.into_inner();
        let mut component = state
            .components
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Component not found"))?;
        if let Some(name) = input.name {
            validate_component_name(&name)?;
            component.name = name.trim().to_string();
        }
        if let Some(description) = input.description {
            component.description = validate_description(&description)?;
        }
        if let Some(group) = input.group {
            component.group = if group.trim().is_empty() {
                None
            } else {
                Some(group.trim().to_string())
            };
        }
        if let Some(status) = input.status {
            if !valid_component_status(&status) {
                return Err(UnifiedError::bad_request(format!(
                    "Invalid component status \"{status}\""
                )));
            }
            component.status = status;
        }
        if let Some(sort_order) = input.sort_order {
            component.sort_order = sort_order;
        }
        component.updated_at = Utc::now();
        let snapshot = component.clone();
        drop(component);
        state.refresh_overall(&ctx);
        Ok(snapshot)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_component(
    state: web::Data<Arc<StatusPageState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        require_statuspage_manage(&ctx, &req)?;
        let id = parse_uuid(path.as_str(), "component")?;
        if !state.components.contains_key(&id) {
            return Err(UnifiedError::not_found("Component not found"));
        }
        state.components.remove(&id);
        state.uptime.retain(|_, record| record.component_id != id);
        // Deleting a component can change the derived overall
        // status (for example removing the only major_outage
        // component ends the outage), so refresh it now instead of
        // leaving `last_overall` stale until the next public read.
        state.refresh_overall(&ctx);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn record_uptime(
    state: web::Data<Arc<StatusPageState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RecordUptimeRequest>,
) -> HttpResponse {
    let result: UnifiedResult<DailyUptime> = (|| {
        require_statuspage_manage(&ctx, &req)?;
        let id = parse_uuid(path.as_str(), "component")?;
        if !state.components.contains_key(&id) {
            return Err(UnifiedError::not_found("Component not found"));
        }
        let input = body.into_inner();
        if !(0.0..=100.0).contains(&input.uptime_percent) {
            return Err(UnifiedError::bad_request(
                "uptime_percent must be between 0 and 100",
            ));
        }
        if let Some(minutes) = input.minutes_down {
            if minutes < 0 || minutes > 1440 {
                return Err(UnifiedError::bad_request(
                    "minutes_down must be between 0 and 1440",
                ));
            }
        }
        let today = Utc::now().date_naive();
        let date = match &input.date {
            Some(raw) => parse_date(raw)?,
            None => today,
        };
        if date > today {
            return Err(UnifiedError::bad_request(
                "Uptime cannot be recorded for a future date",
            ));
        }
        if date < today - chrono::Duration::days(365) {
            return Err(UnifiedError::bad_request(
                "Uptime cannot be recorded more than 365 days in the past",
            ));
        }
        let key = uptime_key(id, &date);
        // A day's aggregate is written once: a second record for
        // the same component and day conflicts instead of silently
        // overwriting the first one, unless the caller explicitly
        // asks for a replacement.
        if state.uptime.contains_key(&key) && !input.overwrite {
            return Err(UnifiedError::conflict(
                "An uptime aggregate already exists for this component and date; pass \"overwrite\": true to replace it",
            ));
        }
        // One field is derived from the other, so the stored pair
        // is always consistent.
        let (uptime_percent, minutes_down) =
            derive_uptime(input.uptime_percent, input.minutes_down);
        let record = DailyUptime {
            component_id: id,
            date: date.format("%Y-%m-%d").to_string(),
            uptime_percent,
            minutes_down,
            total_minutes: 1440,
            updated_at: Utc::now(),
        };
        state.uptime.insert(key, record.clone());
        Ok(record)
    })();
    json::respond(result, ctx.is_debug())
}

pub(crate) async fn create_incident(
    state: web::Data<Arc<StatusPageState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateIncidentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Incident> = (|| {
        require_statuspage_manage(&ctx, &req)?;
        let input = body.into_inner();
        let title = validate_incident_title(&input.title)?;
        let description = validate_description(&input.description)?;
        let severity = input.severity.clone().unwrap_or_else(|| "minor".to_string());
        if !valid_severity(&severity) {
            return Err(UnifiedError::bad_request(format!(
                "Invalid severity \"{severity}\": expected minor, major, or critical"
            )));
        }
        let status = input.status.clone().unwrap_or_else(|| INCIDENT_INVESTIGATING.to_string());
        if !valid_incident_status(&status) {
            return Err(UnifiedError::bad_request(format!(
                "Invalid incident status \"{status}\""
            )));
        }
        for component_id in &input.affected_components {
            if !state.components.contains_key(component_id) {
                return Err(UnifiedError::not_found(format!(
                    "Component \"{component_id}\" not found"
                )));
            }
        }
        let now = Utc::now();
        let incident = Incident {
            id: Uuid::now_v7(),
            title,
            description,
            status: status.clone(),
            severity,
            affected_components: input.affected_components,
            created_at: now,
            updated_at: now,
            resolved_at: if status == INCIDENT_RESOLVED { Some(now) } else { None },
        };
        state.incidents.insert(incident.id, incident.clone());
        let update = IncidentUpdate {
            id: Uuid::now_v7(),
            incident_id: incident.id,
            status: status.clone(),
            message: format!("Incident created: {}", incident.title),
            created_at: now,
        };
        state.updates.insert(update.id, update);
        state.emit(
            "statuspage.incident.created",
            json!({
                "incident_id": incident.id,
                "title": incident.title.clone(),
                "status": incident.status.clone(),
                "severity": incident.severity.clone(),
                "affected_components": incident.affected_components.clone(),
            }),
        );
        state.refresh_overall(&ctx);
        Ok(incident)
    })();
    match result {
        Ok(incident) => json::success_response(incident, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn update_incident(
    state: web::Data<Arc<StatusPageState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateIncidentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Incident> = (|| {
        require_statuspage_manage(&ctx, &req)?;
        let id = parse_uuid(path.as_str(), "incident")?;
        let input = body.into_inner();
        let mut incident = state
            .incidents
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Incident not found"))?;
        let was_resolved = incident.status == INCIDENT_RESOLVED;
        let old_status = incident.status.clone();
        if let Some(title) = input.title {
            incident.title = validate_incident_title(&title)?;
        }
        if let Some(description) = input.description {
            incident.description = validate_description(&description)?;
        }
        if let Some(status) = input.status {
            if !valid_incident_status(&status) {
                return Err(UnifiedError::bad_request(format!(
                    "Invalid incident status \"{status}\""
                )));
            }
            incident.status = status;
        }
        if let Some(severity) = input.severity {
            if !valid_severity(&severity) {
                return Err(UnifiedError::bad_request(format!(
                    "Invalid severity \"{severity}\""
                )));
            }
            incident.severity = severity;
        }
        if let Some(components) = input.affected_components {
            for component_id in &components {
                if !state.components.contains_key(component_id) {
                    return Err(UnifiedError::not_found(format!(
                        "Component \"{component_id}\" not found"
                    )));
                }
            }
            incident.affected_components = components;
        }
        let now = Utc::now();
        incident.updated_at = now;
        if incident.status == INCIDENT_RESOLVED && !was_resolved {
            incident.resolved_at = Some(now);
        } else if incident.status != INCIDENT_RESOLVED {
            incident.resolved_at = None;
        }
        let snapshot = incident.clone();
        drop(incident);
        // Every status change leaves a timeline entry, exactly like
        // resolve and add_incident_update do: without it, a status
        // change made here vanishes from the incident timeline.
        if snapshot.status != old_status {
            let update = IncidentUpdate {
                id: Uuid::now_v7(),
                incident_id: id,
                status: snapshot.status.clone(),
                message: format!(
                    "Incident status changed to {}: {}",
                    snapshot.status, snapshot.title
                ),
                created_at: now,
            };
            state.updates.insert(update.id, update);
        }
        if snapshot.status == INCIDENT_RESOLVED && !was_resolved {
            state.record_incident_downtime(&snapshot, now);
            state.emit(
                "statuspage.incident.resolved",
                json!({
                    "incident_id": snapshot.id,
                    "title": snapshot.title.clone(),
                    "resolved_at": snapshot.resolved_at,
                }),
            );
        } else {
            state.emit(
                "statuspage.incident.updated",
                json!({
                    "incident_id": snapshot.id,
                    "title": snapshot.title.clone(),
                    "status": snapshot.status.clone(),
                    "severity": snapshot.severity.clone(),
                }),
            );
        }
        state.refresh_overall(&ctx);
        Ok(snapshot)
    })();
    json::respond(result, debug)
}

pub(crate) async fn resolve_incident(
    state: web::Data<Arc<StatusPageState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Incident> = (|| {
        require_statuspage_manage(&ctx, &req)?;
        let id = parse_uuid(path.as_str(), "incident")?;
        let mut incident = state
            .incidents
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Incident not found"))?;
        if incident.status == INCIDENT_RESOLVED {
            return Err(UnifiedError::conflict("Incident is already resolved"));
        }
        let now = Utc::now();
        incident.status = INCIDENT_RESOLVED.to_string();
        incident.resolved_at = Some(now);
        incident.updated_at = now;
        let snapshot = incident.clone();
        drop(incident);
        let update = IncidentUpdate {
            id: Uuid::now_v7(),
            incident_id: id,
            status: INCIDENT_RESOLVED.to_string(),
            message: format!("Incident resolved: {}", snapshot.title),
            created_at: now,
        };
        state.updates.insert(update.id, update);
        state.record_incident_downtime(&snapshot, now);
        state.emit(
            "statuspage.incident.resolved",
            json!({
                "incident_id": snapshot.id,
                "title": snapshot.title.clone(),
                "resolved_at": snapshot.resolved_at,
            }),
        );
        state.refresh_overall(&ctx);
        Ok(snapshot)
    })();
    json::respond(result, debug)
}

pub(crate) async fn add_incident_update(
    state: web::Data<Arc<StatusPageState>>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AddIncidentUpdateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<IncidentUpdate> = (|| {
        require_statuspage_manage(&ctx, &req)?;
        let id = parse_uuid(path.as_str(), "incident")?;
        let input = body.into_inner();
        if input.message.trim().is_empty() {
            return Err(UnifiedError::bad_request("message must not be empty"));
        }
        if input.message.chars().count() > 2000 {
            return Err(UnifiedError::bad_request(
                "message must be at most 2000 characters",
            ));
        }
        let status = match input.status {
            Some(status) => {
                if !valid_incident_status(&status) {
                    return Err(UnifiedError::bad_request(format!(
                        "Invalid incident status \"{status}\""
                    )));
                }
                status
            }
            None => state
                .incidents
                .get(&id)
                .map(|entry| entry.value().status.clone())
                .ok_or_else(|| UnifiedError::not_found("Incident not found"))?,
        };
        let mut incident = state
            .incidents
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Incident not found"))?;
        let was_resolved = incident.status == INCIDENT_RESOLVED;
        let now = Utc::now();
        incident.status = status.clone();
        incident.updated_at = now;
        if status == INCIDENT_RESOLVED && !was_resolved {
            incident.resolved_at = Some(now);
        } else if status != INCIDENT_RESOLVED {
            incident.resolved_at = None;
        }
        let snapshot = incident.clone();
        drop(incident);
        let update = IncidentUpdate {
            id: Uuid::now_v7(),
            incident_id: id,
            status: status.clone(),
            message: input.message.trim().to_string(),
            created_at: now,
        };
        state.updates.insert(update.id, update.clone());
        if status == INCIDENT_RESOLVED && !was_resolved {
            state.record_incident_downtime(&snapshot, now);
            state.emit(
                "statuspage.incident.resolved",
                json!({
                    "incident_id": snapshot.id,
                    "title": snapshot.title,
                    "resolved_at": snapshot.resolved_at,
                }),
            );
        } else {
            state.emit(
                "statuspage.incident.updated",
                json!({
                    "incident_id": snapshot.id,
                    "title": snapshot.title,
                    "status": snapshot.status,
                    "severity": snapshot.severity,
                }),
            );
        }
        state.refresh_overall(&ctx);
        Ok(update)
    })();
    match result {
        Ok(update) => json::success_response(update, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    use super::*;
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookContext;
    use unified_core::modules::ModuleRegistry;
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;

    /// App context whose `role.check` hook allows exactly the
    /// given permissions (all other permissions are denied).
    fn test_ctx(allowed: &[&str]) -> web::Data<Arc<AppContext>> {
        let hooks = Arc::new(HookBus::new());
        let allowed: Vec<String> = allowed.iter().map(|permission| permission.to_string()).collect();
        hooks.register(
            "role.check",
            "test",
            Arc::new(move |ctx: &mut HookContext| {
                let permission = ctx
                    .get("permission")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                ctx.set(
                    "allowed",
                    serde_json::json!(allowed.iter().any(|held| held == permission)),
                );
            }),
        );
        let ctx = AppContext::new(
            AppConfig::default(),
            hooks,
            Arc::new(ModuleRegistry::new()),
        );
        web::Data::new(Arc::new(ctx))
    }

    fn admin_request() -> HttpRequest {
        verified_request("admin-1", "admin")
    }

    fn new_state() -> (web::Data<Arc<StatusPageState>>, Arc<StatusPageState>) {
        let inner = Arc::new(StatusPageState::new());
        (web::Data::new(Arc::clone(&inner)), inner)
    }

    fn seed_component(state: &StatusPageState, status: &str) -> StatusComponent {
        let now = Utc::now();
        let component = StatusComponent {
            id: Uuid::now_v7(),
            name: "API".to_string(),
            description: String::new(),
            group: None,
            status: status.to_string(),
            sort_order: 0,
            created_at: now,
            updated_at: now,
        };
        state.components.insert(component.id, component.clone());
        component
    }

    fn seed_incident(state: &StatusPageState, status: &str) -> Incident {
        let now = Utc::now();
        let incident = Incident {
            id: Uuid::now_v7(),
            title: "Outage".to_string(),
            description: "Something broke".to_string(),
            status: status.to_string(),
            severity: "minor".to_string(),
            affected_components: Vec::new(),
            created_at: now,
            updated_at: now,
            resolved_at: None,
        };
        state.incidents.insert(incident.id, incident.clone());
        incident
    }

    fn incident_updates(state: &StatusPageState, incident_id: Uuid) -> Vec<IncidentUpdate> {
        state
            .updates
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|update| update.incident_id == incident_id)
            .collect()
    }

    #[test]
    fn derive_uptime_keeps_percent_and_minutes_consistent() {
        assert_eq!(derive_uptime(100.0, Some(1440)), (0.0, 1440));
        assert_eq!(derive_uptime(50.0, Some(720)), (50.0, 720));
        assert_eq!(derive_uptime(100.0, Some(0)), (100.0, 0));
        // Minutes absent: derived from the percent, then the
        // percent is recomputed from the minutes, so the pair
        // agrees by construction.
        let (percent, minutes) = derive_uptime(99.0, None);
        assert_eq!(minutes, 14);
        assert_eq!(percent, round_percent(100.0 * (1440 - 14) as f64 / 1440.0));
        assert_eq!(derive_uptime(100.0, None), (100.0, 0));
        assert_eq!(derive_uptime(0.0, None), (0.0, 1440));
    }

    #[tokio::test]
    async fn create_component_requires_manage_permission() {
        let (state, _inner) = new_state();
        let body = || {
            web::Json(CreateComponentRequest {
                name: "API".to_string(),
                description: String::new(),
                group: None,
                status: None,
                sort_order: None,
            })
        };
        let anonymous = create_component(
            state.clone(),
            test_ctx(&[PERM_STATUSPAGE_MANAGE]),
            TestRequest::default().to_http_request(),
            body(),
        )
        .await;
        assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
        let denied = create_component(
            state.clone(),
            test_ctx(&["statuspage.view"]),
            admin_request(),
            body(),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        let allowed = create_component(
            state.clone(),
            test_ctx(&[PERM_STATUSPAGE_MANAGE]),
            admin_request(),
            body(),
        )
        .await;
        assert_eq!(allowed.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn record_uptime_derives_percent_and_rejects_duplicate_day() {
        let (state, inner) = new_state();
        let ctx = test_ctx(&[PERM_STATUSPAGE_MANAGE]);
        let component = seed_component(&inner, COMPONENT_OPERATIONAL);
        let path = || web::Path::from(component.id.to_string());
        let body = |uptime_percent: f64, minutes_down: Option<i64>, overwrite: bool| {
            web::Json(RecordUptimeRequest {
                date: None,
                uptime_percent,
                minutes_down,
                overwrite,
            })
        };
        let first = record_uptime(
            state.clone(),
            ctx.clone(),
            admin_request(),
            path(),
            body(100.0, Some(720), false),
        )
        .await;
        assert_eq!(first.status(), StatusCode::OK);
        let today = Utc::now().date_naive();
        let stored = inner
            .uptime
            .get(&uptime_key(component.id, &today))
            .expect("aggregate stored")
            .value()
            .clone();
        assert_eq!(stored.minutes_down, 720);
        assert_eq!(stored.uptime_percent, 50.0);
        let duplicate = record_uptime(
            state.clone(),
            ctx.clone(),
            admin_request(),
            path(),
            body(100.0, Some(0), false),
        )
        .await;
        assert_eq!(duplicate.status(), StatusCode::CONFLICT);
        let replaced = record_uptime(
            state.clone(),
            ctx.clone(),
            admin_request(),
            path(),
            body(100.0, Some(0), true),
        )
        .await;
        assert_eq!(replaced.status(), StatusCode::OK);
        let stored = inner
            .uptime
            .get(&uptime_key(component.id, &today))
            .expect("aggregate stored")
            .value()
            .clone();
        assert_eq!(stored.uptime_percent, 100.0);
        assert_eq!(stored.minutes_down, 0);
    }

    #[tokio::test]
    async fn update_incident_status_change_adds_timeline_update() {
        let (state, inner) = new_state();
        let ctx = test_ctx(&[PERM_STATUSPAGE_MANAGE]);
        let incident = seed_incident(&inner, INCIDENT_INVESTIGATING);
        let path = || web::Path::from(incident.id.to_string());
        let changed = update_incident(
            state.clone(),
            ctx.clone(),
            admin_request(),
            path(),
            web::Json(UpdateIncidentRequest {
                title: None,
                description: None,
                status: Some(INCIDENT_MONITORING.to_string()),
                severity: None,
                affected_components: None,
            }),
        )
        .await;
        assert_eq!(changed.status(), StatusCode::OK);
        let updates = incident_updates(&inner, incident.id);
        assert_eq!(updates.len(), 1, "a status change must leave a timeline entry");
        assert_eq!(updates[0].status, INCIDENT_MONITORING);
        let title_only = update_incident(
            state.clone(),
            ctx.clone(),
            admin_request(),
            path(),
            web::Json(UpdateIncidentRequest {
                title: Some("Outage, renamed".to_string()),
                description: None,
                status: None,
                severity: None,
                affected_components: None,
            }),
        )
        .await;
        assert_eq!(title_only.status(), StatusCode::OK);
        assert_eq!(
            incident_updates(&inner, incident.id).len(),
            1,
            "a title-only update adds no timeline entry"
        );
    }

    #[tokio::test]
    async fn update_incident_enforces_title_and_description_caps() {
        let (state, inner) = new_state();
        let ctx = test_ctx(&[PERM_STATUSPAGE_MANAGE]);
        let incident = seed_incident(&inner, INCIDENT_INVESTIGATING);
        let path = || web::Path::from(incident.id.to_string());
        let long_title = update_incident(
            state.clone(),
            ctx.clone(),
            admin_request(),
            path(),
            web::Json(UpdateIncidentRequest {
                title: Some("x".repeat(201)),
                description: None,
                status: None,
                severity: None,
                affected_components: None,
            }),
        )
        .await;
        assert_eq!(long_title.status(), StatusCode::BAD_REQUEST);
        let long_description = update_incident(
            state.clone(),
            ctx.clone(),
            admin_request(),
            path(),
            web::Json(UpdateIncidentRequest {
                title: None,
                description: Some("x".repeat(2001)),
                status: None,
                severity: None,
                affected_components: None,
            }),
        )
        .await;
        assert_eq!(long_description.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn delete_component_refreshes_overall_status() {
        let (state, inner) = new_state();
        let ctx = test_ctx(&[PERM_STATUSPAGE_MANAGE]);
        let component = seed_component(&inner, COMPONENT_MAJOR_OUTAGE);
        assert_eq!(inner.refresh_overall(&ctx), OVERALL_OUTAGE);
        assert_eq!(inner.last_overall(), OVERALL_OUTAGE);
        let resp = delete_component(
            state.clone(),
            ctx.clone(),
            admin_request(),
            web::Path::from(component.id.to_string()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            inner.last_overall(),
            OVERALL_OPERATIONAL,
            "deleting the only major_outage component must clear the outage immediately"
        );
    }

    #[tokio::test]
    async fn all_write_routes_reject_anonymous_callers() {
        // The auth check is the first statement of every write
        // handler, so an anonymous caller must get 401 from all
        // eight write routes, even for ids that do not exist (never
        // a 404 that would leak whether the id is real).
        let (state, _inner) = new_state();
        let ctx = test_ctx(&[PERM_STATUSPAGE_MANAGE]);
        let anonymous = || TestRequest::default().to_http_request();
        let id = || web::Path::from(Uuid::now_v7().to_string());
        let statuses = [
            (
                "create_component",
                create_component(
                    state.clone(),
                    ctx.clone(),
                    anonymous(),
                    web::Json(CreateComponentRequest {
                        name: "API".to_string(),
                        description: String::new(),
                        group: None,
                        status: None,
                        sort_order: None,
                    }),
                )
                .await
                .status(),
            ),
            (
                "update_component",
                update_component(
                    state.clone(),
                    ctx.clone(),
                    anonymous(),
                    id(),
                    web::Json(UpdateComponentRequest {
                        name: None,
                        description: None,
                        group: None,
                        status: None,
                        sort_order: None,
                    }),
                )
                .await
                .status(),
            ),
            (
                "delete_component",
                delete_component(state.clone(), ctx.clone(), anonymous(), id())
                    .await
                    .status(),
            ),
            (
                "record_uptime",
                record_uptime(
                    state.clone(),
                    ctx.clone(),
                    anonymous(),
                    id(),
                    web::Json(RecordUptimeRequest {
                        date: None,
                        uptime_percent: 100.0,
                        minutes_down: None,
                        overwrite: false,
                    }),
                )
                .await
                .status(),
            ),
            (
                "create_incident",
                create_incident(
                    state.clone(),
                    ctx.clone(),
                    anonymous(),
                    web::Json(CreateIncidentRequest {
                        title: "Outage".to_string(),
                        description: String::new(),
                        severity: None,
                        affected_components: Vec::new(),
                        status: None,
                    }),
                )
                .await
                .status(),
            ),
            (
                "update_incident",
                update_incident(
                    state.clone(),
                    ctx.clone(),
                    anonymous(),
                    id(),
                    web::Json(UpdateIncidentRequest {
                        title: None,
                        description: None,
                        status: None,
                        severity: None,
                        affected_components: None,
                    }),
                )
                .await
                .status(),
            ),
            (
                "resolve_incident",
                resolve_incident(state.clone(), ctx.clone(), anonymous(), id())
                    .await
                    .status(),
            ),
            (
                "add_incident_update",
                add_incident_update(
                    state.clone(),
                    ctx.clone(),
                    anonymous(),
                    id(),
                    web::Json(AddIncidentUpdateRequest {
                        message: "Investigating".to_string(),
                        status: None,
                    }),
                )
                .await
                .status(),
            ),
        ];
        for (route, status) in statuses {
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{route} must refuse anonymous callers");
        }
    }
}
