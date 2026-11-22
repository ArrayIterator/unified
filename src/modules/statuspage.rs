// Status Page module.
//
// A PUBLIC status page: no authentication is required on any route in
// this module (the application has no global auth gate on module
// routes, and these routes are additionally marked public here and in
// the route catalog so a future auth layer can exempt them by path).
//
// Overall status is derived, in priority order, from:
//   1. the global maintenance flag (`AppContext::is_maintenance`,
//      set by the Maintenance Windows module while a window is
//      active) -> `maintenance`;
//   2. open incidents: a critical incident -> `outage`, a major or
//      minor open incident -> `degraded`;
//   3. component states: a `major_outage` component -> `outage`, a
//      `partial_outage` or `degraded` component -> `degraded`;
//   4. otherwise `operational`.
//
// Uptime percentages come from daily aggregates recorded per
// component. Aggregates are recorded through
// `POST /components/{id}/uptime`; open incidents also add downtime
// minutes to their affected components automatically when they are
// created and resolved, so uptime reflects real incident time even
// when nobody posts aggregates by hand. A component with no recorded
// days in the range reports 100.0 with `data_points: 0`.
//
// The HTML page (`GET /status`) is a clean standalone page rendered
// by this module. The Handlebars theme engine does not fit cleanly
// here: no `statuspage` template exists in the embedded default
// theme, the engine is not reachable from `AppContext`, and adding a
// template would require editing theme files outside this module's
// scope. The JSON endpoint carries the same data, so a theme can
// render it later without changing this module.
//
// Events emitted on the hook bus:
//   - `statuspage.incident.created`
//   - `statuspage.incident.updated`
//   - `statuspage.incident.resolved`
//   - `statuspage.status.changed` (when the derived overall status changes)

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::HookBus;
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

pub const OVERALL_OPERATIONAL: &str = "operational";
pub const OVERALL_DEGRADED: &str = "degraded";
pub const OVERALL_OUTAGE: &str = "outage";
pub const OVERALL_MAINTENANCE: &str = "maintenance";

pub const COMPONENT_OPERATIONAL: &str = "operational";
pub const COMPONENT_DEGRADED: &str = "degraded";
pub const COMPONENT_PARTIAL_OUTAGE: &str = "partial_outage";
pub const COMPONENT_MAJOR_OUTAGE: &str = "major_outage";
pub const COMPONENT_MAINTENANCE: &str = "maintenance";

fn valid_component_status(status: &str) -> bool {
    matches!(
        status,
        COMPONENT_OPERATIONAL
            | COMPONENT_DEGRADED
            | COMPONENT_PARTIAL_OUTAGE
            | COMPONENT_MAJOR_OUTAGE
            | COMPONENT_MAINTENANCE
    )
}

pub const INCIDENT_INVESTIGATING: &str = "investigating";
pub const INCIDENT_IDENTIFIED: &str = "identified";
pub const INCIDENT_MONITORING: &str = "monitoring";
pub const INCIDENT_RESOLVED: &str = "resolved";

fn valid_incident_status(status: &str) -> bool {
    matches!(
        status,
        INCIDENT_INVESTIGATING | INCIDENT_IDENTIFIED | INCIDENT_MONITORING | INCIDENT_RESOLVED
    )
}

fn is_open_incident(status: &str) -> bool {
    status != INCIDENT_RESOLVED
}

fn valid_severity(severity: &str) -> bool {
    matches!(severity, "minor" | "major" | "critical")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusComponent {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub group: Option<String>,
    pub status: String,
    pub sort_order: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyUptime {
    pub component_id: Uuid,
    /// Calendar day in `YYYY-MM-DD` (UTC).
    pub date: String,
    pub uptime_percent: f64,
    pub minutes_down: i64,
    pub total_minutes: i64,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Incident {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub status: String,
    pub severity: String,
    pub affected_components: Vec<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncidentUpdate {
    pub id: Uuid,
    pub incident_id: Uuid,
    /// Incident status after this update.
    pub status: String,
    pub message: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateComponentRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub sort_order: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateComponentRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub sort_order: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RecordUptimeRequest {
    /// Calendar day in `YYYY-MM-DD` (UTC). Defaults to today.
    #[serde(default)]
    pub date: Option<String>,
    pub uptime_percent: f64,
    #[serde(default)]
    pub minutes_down: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateIncidentRequest {
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub affected_components: Vec<Uuid>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateIncidentRequest {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub affected_components: Option<Vec<Uuid>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddIncidentUpdateRequest {
    pub message: String,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComponentStatusView {
    #[serde(flatten)]
    pub component: StatusComponent,
    pub uptime_30d: f64,
    pub uptime_90d: f64,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct StatusPageState {
    hooks: RwLock<Arc<HookBus>>,
    components: DashMap<Uuid, StatusComponent>,
    /// Keyed by `{component_id}:{date}`.
    uptime: DashMap<String, DailyUptime>,
    incidents: DashMap<Uuid, Incident>,
    updates: DashMap<Uuid, IncidentUpdate>,
    /// Last derived overall status, used to emit
    /// `statuspage.status.changed` only on real changes.
    last_overall: RwLock<String>,
}

impl StatusPageState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            components: DashMap::new(),
            uptime: DashMap::new(),
            incidents: DashMap::new(),
            updates: DashMap::new(),
            last_overall: RwLock::new(OVERALL_OPERATIONAL.to_string()),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// Derive the overall status from the maintenance flag, open
    /// incidents, and component states.
    fn derive_overall(&self, ctx: &AppContext) -> String {
        if ctx.is_maintenance() {
            return OVERALL_MAINTENANCE.to_string();
        }
        let mut worst = OVERALL_OPERATIONAL.to_string();
        for entry in self.incidents.iter() {
            let incident = entry.value();
            if !is_open_incident(&incident.status) {
                continue;
            }
            match incident.severity.as_str() {
                "critical" => return OVERALL_OUTAGE.to_string(),
                _ => worst = OVERALL_DEGRADED.to_string(),
            }
        }
        for entry in self.components.iter() {
            match entry.value().status.as_str() {
                COMPONENT_MAJOR_OUTAGE => return OVERALL_OUTAGE.to_string(),
                COMPONENT_PARTIAL_OUTAGE | COMPONENT_DEGRADED => {
                    worst = OVERALL_DEGRADED.to_string();
                }
                _ => {}
            }
        }
        worst
    }

    /// Recompute the overall status and emit
    /// `statuspage.status.changed` when it moved.
    fn refresh_overall(&self, ctx: &AppContext) -> String {
        let overall = self.derive_overall(ctx);
        let mut last = self.last_overall.write().expect("overall lock poisoned");
        if *last != overall {
            let previous = last.clone();
            *last = overall.clone();
            drop(last);
            self.emit(
                "statuspage.status.changed",
                json!({ "previous": previous, "current": overall }),
            );
        }
        overall
    }

    fn uptime_for_range(&self, component_id: Uuid, days: i64) -> (f64, u64) {
        let today = Utc::now().date_naive();
        let mut sum = 0f64;
        let mut points = 0u64;
        for offset in 0..days {
            let date = today - chrono::Duration::days(offset);
            let key = uptime_key(component_id, &date);
            if let Some(entry) = self.uptime.get(&key) {
                sum += entry.value().uptime_percent;
                points += 1;
            }
        }
        if points == 0 {
            (100.0, 0)
        } else {
            (sum / points as f64, points)
        }
    }

    fn component_view(&self, component: &StatusComponent) -> ComponentStatusView {
        let (uptime_30d, _) = self.uptime_for_range(component.id, 30);
        let (uptime_90d, _) = self.uptime_for_range(component.id, 90);
        ComponentStatusView {
            component: component.clone(),
            uptime_30d: round_percent(uptime_30d),
            uptime_90d: round_percent(uptime_90d),
        }
    }

    /// Add downtime minutes to a component's aggregate for one day,
    /// recomputing its uptime percentage from the minutes.
    fn add_downtime(&self, component_id: Uuid, date: NaiveDate, minutes_down: i64) {
        if minutes_down <= 0 {
            return;
        }
        let minutes_down = minutes_down.min(1440);
        let key = uptime_key(component_id, &date);
        let mut entry = self.uptime.entry(key).or_insert_with(|| DailyUptime {
            component_id,
            date: date.format("%Y-%m-%d").to_string(),
            uptime_percent: 100.0,
            minutes_down: 0,
            total_minutes: 1440,
            updated_at: Utc::now(),
        });
        entry.minutes_down = (entry.minutes_down + minutes_down).min(1440);
        entry.uptime_percent =
            round_percent(100.0 * (1440 - entry.minutes_down) as f64 / 1440.0);
        entry.updated_at = Utc::now();
    }

    /// Spread an incident's open time across the days it covers as
    /// downtime for every affected component.
    fn record_incident_downtime(&self, incident: &Incident, until: DateTime<Utc>) {
        if incident.affected_components.is_empty() {
            return;
        }
        let start = incident.created_at;
        if until <= start {
            return;
        }
        let mut day = start.date_naive();
        let last_day = until.date_naive();
        loop {
            let day_start = day.and_hms_opt(0, 0, 0).expect("valid midnight");
            let day_end = day_start + chrono::Duration::days(1);
            let from = start.max(day_start.and_utc());
            let to = until.min(day_end.and_utc());
            let minutes = (to - from).num_minutes().max(0);
            for component_id in &incident.affected_components {
                self.add_downtime(*component_id, day, minutes);
            }
            if day >= last_day {
                break;
            }
            day += chrono::Duration::days(1);
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn uptime_key(component_id: Uuid, date: &NaiveDate) -> String {
    format!("{}:{}", component_id, date.format("%Y-%m-%d"))
}

fn round_percent(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw)
        .map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id \"{raw}\"")))
}

fn parse_date(raw: &str) -> UnifiedResult<NaiveDate> {
    NaiveDate::parse_from_str(raw, "%Y-%m-%d").map_err(|_| {
        UnifiedError::bad_request(format!("Invalid date \"{raw}\": expected YYYY-MM-DD"))
    })
}

fn validate_component_name(name: &str) -> UnifiedResult<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(UnifiedError::bad_request("name must not be empty"));
    }
    if name.chars().count() > 120 {
        return Err(UnifiedError::bad_request(
            "name must be at most 120 characters",
        ));
    }
    Ok(())
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

/// Minimal HTML escaping for values rendered into the public page.
fn escape_html(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn status_label(status: &str) -> &'static str {
    match status {
        OVERALL_OPERATIONAL => "All Systems Operational",
        OVERALL_DEGRADED => "Degraded Performance",
        OVERALL_OUTAGE => "Service Outage",
        OVERALL_MAINTENANCE => "Under Maintenance",
        _ => "Unknown Status",
    }
}

fn status_color(status: &str) -> &'static str {
    match status {
        OVERALL_OPERATIONAL => "#15803d",
        OVERALL_DEGRADED => "#b45309",
        OVERALL_OUTAGE => "#b91c1c",
        OVERALL_MAINTENANCE => "#1d4ed8",
        _ => "#475569",
    }
}

fn render_status_html(
    overall: &str,
    components: &[ComponentStatusView],
    open_incidents: &[Incident],
    recent_incidents: &[Incident],
) -> String {
    let mut component_rows = String::new();
    for view in components {
        component_rows.push_str(&format!(
            "<li><span>{}</span><span class=\"pill\" style=\"background:{}20;color:{}\">{}</span><span class=\"uptime\">30d {}% &middot; 90d {}%</span></li>",
            escape_html(&view.component.name),
            status_color(&view.component.status),
            status_color(&view.component.status),
            escape_html(&view.component.status),
            view.uptime_30d,
            view.uptime_90d,
        ));
    }
    if component_rows.is_empty() {
        component_rows.push_str("<li>No components registered yet.</li>");
    }
    let mut incident_rows = String::new();
    for incident in open_incidents {
        incident_rows.push_str(&format!(
            "<li><strong>{}</strong> <span class=\"pill\">{}</span> <span class=\"pill\">{}</span><p>{}</p></li>",
            escape_html(&incident.title),
            escape_html(&incident.severity),
            escape_html(&incident.status),
            escape_html(&incident.description),
        ));
    }
    if incident_rows.is_empty() {
        incident_rows.push_str("<li>No open incidents.</li>");
    }
    let mut history_rows = String::new();
    for incident in recent_incidents {
        history_rows.push_str(&format!(
            "<li><strong>{}</strong> <span class=\"pill\">{}</span> <span>{}</span></li>",
            escape_html(&incident.title),
            escape_html(&incident.status),
            incident.created_at.format("%Y-%m-%d %H:%M UTC"),
        ));
    }
    if history_rows.is_empty() {
        history_rows.push_str("<li>No incidents recorded yet.</li>");
    }
    format!(
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">\
        <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
        <title>System Status</title><style>\
        body{{font-family:system-ui,-apple-system,sans-serif;max-width:760px;margin:2rem auto;padding:0 1rem;color:#0f172a}}\
        .banner{{background:{};color:#fff;padding:1.25rem 1.5rem;border-radius:12px}}\
        .banner h1{{margin:0;font-size:1.5rem}} .banner p{{margin:.25rem 0 0;opacity:.9}}\
        h2{{margin-top:2rem}} ul{{list-style:none;padding:0}} li{{border:1px solid #e2e8f0;border-radius:10px;padding:.75rem 1rem;margin:.5rem 0}}\
        .pill{{display:inline-block;border-radius:999px;padding:.1rem .6rem;font-size:.8rem;margin-left:.5rem}}\
        .uptime{{float:right;color:#475569;font-size:.85rem}} p{{margin:.4rem 0 0;color:#334155}}\
        footer{{margin-top:2rem;color:#64748b;font-size:.85rem}}</style></head><body>\
        <div class=\"banner\"><h1>{}</h1><p>Overall status: {}</p></div>\
        <h2>Components</h2><ul>{}</ul>\
        <h2>Open Incidents</h2><ul>{}</ul>\
        <h2>Recent Incidents</h2><ul>{}</ul>\
        <footer>Data is also available as JSON at <code>/api/v1/statuspage</code>. Times are UTC.</footer>\
        </body></html>",
        status_color(overall),
        escape_html(status_label(overall)),
        escape_html(overall),
        component_rows,
        incident_rows,
        history_rows,
    )
}

// ---------------------------------------------------------------------------
// Public handlers
// ---------------------------------------------------------------------------

fn sorted_components(state: &StatusPageState) -> Vec<ComponentStatusView> {
    let mut components: Vec<StatusComponent> = state
        .components
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    components.sort_by(|a, b| {
        a.sort_order
            .cmp(&b.sort_order)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    components
        .iter()
        .map(|component| state.component_view(component))
        .collect()
}

fn open_incidents(state: &StatusPageState) -> Vec<Incident> {
    let mut incidents: Vec<Incident> = state
        .incidents
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|incident| is_open_incident(&incident.status))
        .collect();
    incidents.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    incidents
}

/// PUBLIC: overall status summary as JSON.
async fn public_summary(
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
async fn public_page(
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
async fn list_components(
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
async fn uptime_history(
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
async fn list_incidents(
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

async fn get_incident(
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

async fn list_incident_updates(
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

// ---------------------------------------------------------------------------
// Admin handlers (components, incidents)
// ---------------------------------------------------------------------------

async fn create_component(
    state: web::Data<Arc<StatusPageState>>,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateComponentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<StatusComponent> = (|| {
        let input = body.into_inner();
        validate_component_name(&input.name)?;
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
            description: input.description.trim().to_string(),
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

async fn get_component(
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

async fn update_component(
    state: web::Data<Arc<StatusPageState>>,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<UpdateComponentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<StatusComponent> = (|| {
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
            component.description = description.trim().to_string();
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

async fn delete_component(
    state: web::Data<Arc<StatusPageState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let id = parse_uuid(path.as_str(), "component")?;
        if !state.components.contains_key(&id) {
            return Err(UnifiedError::not_found("Component not found"));
        }
        state.components.remove(&id);
        state.uptime.retain(|_, record| record.component_id != id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, false)
}

async fn record_uptime(
    state: web::Data<Arc<StatusPageState>>,
    path: web::Path<String>,
    body: web::Json<RecordUptimeRequest>,
) -> HttpResponse {
    let result: UnifiedResult<DailyUptime> = (|| {
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
        if input.minutes_down < 0 || input.minutes_down > 1440 {
            return Err(UnifiedError::bad_request(
                "minutes_down must be between 0 and 1440",
            ));
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
        let record = DailyUptime {
            component_id: id,
            date: date.format("%Y-%m-%d").to_string(),
            uptime_percent: round_percent(input.uptime_percent),
            minutes_down: input.minutes_down,
            total_minutes: 1440,
            updated_at: Utc::now(),
        };
        state.uptime.insert(key, record.clone());
        Ok(record)
    })();
    json::respond(result, false)
}

async fn create_incident(
    state: web::Data<Arc<StatusPageState>>,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateIncidentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Incident> = (|| {
        let input = body.into_inner();
        let title = input.title.trim();
        if title.is_empty() {
            return Err(UnifiedError::bad_request("title must not be empty"));
        }
        if title.chars().count() > 200 {
            return Err(UnifiedError::bad_request(
                "title must be at most 200 characters",
            ));
        }
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
            title: title.to_string(),
            description: input.description.trim().to_string(),
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
                "title": incident.title,
                "status": incident.status,
                "severity": incident.severity,
                "affected_components": incident.affected_components,
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

async fn update_incident(
    state: web::Data<Arc<StatusPageState>>,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<UpdateIncidentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Incident> = (|| {
        let id = parse_uuid(path.as_str(), "incident")?;
        let input = body.into_inner();
        let mut incident = state
            .incidents
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Incident not found"))?;
        let was_resolved = incident.status == INCIDENT_RESOLVED;
        if let Some(title) = input.title {
            if title.trim().is_empty() {
                return Err(UnifiedError::bad_request("title must not be empty"));
            }
            incident.title = title.trim().to_string();
        }
        if let Some(description) = input.description {
            incident.description = description.trim().to_string();
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
        if snapshot.status == INCIDENT_RESOLVED && !was_resolved {
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
        Ok(snapshot)
    })();
    json::respond(result, debug)
}

async fn resolve_incident(
    state: web::Data<Arc<StatusPageState>>,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Incident> = (|| {
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
                "title": snapshot.title,
                "resolved_at": snapshot.resolved_at,
            }),
        );
        state.refresh_overall(&ctx);
        Ok(snapshot)
    })();
    json::respond(result, debug)
}

async fn add_incident_update(
    state: web::Data<Arc<StatusPageState>>,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<AddIncidentUpdateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<IncidentUpdate> = (|| {
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

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------
//
// Note: this module registers no hook handlers. The overall status is
// recomputed with the application context on every request
// (`refresh_overall`), because a hook handler receives no AppContext
// and therefore cannot read the maintenance flag. Registering a
// no-op handler for `app.maintenance` would only pretend to react.

pub struct StatusPageModule {
    state: Arc<StatusPageState>,
}

impl StatusPageModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(StatusPageState::new()),
        }
    }
}

impl Default for StatusPageModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for StatusPageModule {
    fn id(&self) -> &'static str {
        "statuspage"
    }

    fn name(&self) -> &'static str {
        "Status Page"
    }

    fn description(&self) -> &'static str {
        "Public status page: derived overall status, components with 30/90-day uptime, incidents with timeline updates, and daily uptime history."
    }

    fn requires(&self) -> &[&'static str] {
        &["service-monitor", "maintenance"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        // PUBLIC: the HTML status page. Registered outside /api so it
        // stays reachable (and bookmarkable) during maintenance; the
        // server integrator must exempt /status and
        // /api/v1/statuspage from the maintenance 503 block (see the
        // coordinator report snippet).
        cfg.route("/status", web::get().to(public_page));
        cfg.service(
            web::scope("/api/v1/statuspage")
                .route("", web::get().to(public_summary))
                .route("/components", web::get().to(list_components))
                .route("/components", web::post().to(create_component))
                .route("/components/{id}", web::get().to(get_component))
                .route("/components/{id}", web::put().to(update_component))
                .route("/components/{id}", web::delete().to(delete_component))
                .route("/components/{id}/uptime", web::post().to(record_uptime))
                .route("/uptime", web::get().to(uptime_history))
                .route("/incidents", web::get().to(list_incidents))
                .route("/incidents", web::post().to(create_incident))
                .route("/incidents/{id}", web::get().to(get_incident))
                .route("/incidents/{id}", web::put().to(update_incident))
                .route("/incidents/{id}/resolve", web::post().to(resolve_incident))
                .route("/incidents/{id}/updates", web::get().to(list_incident_updates))
                .route("/incidents/{id}/updates", web::post().to(add_incident_update)),
        );
    }

    fn route_info(&self) -> Vec<crate::core::modules::RouteInfo> {
        use crate::core::modules::RouteInfo;
        vec![
            RouteInfo { method: "GET", path: "/status", owner: "statuspage", description: "PUBLIC: status page as HTML." },
            RouteInfo { method: "GET", path: "/api/v1/statuspage", owner: "statuspage", description: "PUBLIC: overall status summary as JSON." },
            RouteInfo { method: "GET", path: "/api/v1/statuspage/components", owner: "statuspage", description: "PUBLIC: components with 30/90-day uptime (paginated)." },
            RouteInfo { method: "POST", path: "/api/v1/statuspage/components", owner: "statuspage", description: "Create a component." },
            RouteInfo { method: "GET", path: "/api/v1/statuspage/components/{id}", owner: "statuspage", description: "Get one component with uptime." },
            RouteInfo { method: "PUT", path: "/api/v1/statuspage/components/{id}", owner: "statuspage", description: "Update a component." },
            RouteInfo { method: "DELETE", path: "/api/v1/statuspage/components/{id}", owner: "statuspage", description: "Delete a component and its uptime records." },
            RouteInfo { method: "POST", path: "/api/v1/statuspage/components/{id}/uptime", owner: "statuspage", description: "Record a daily uptime aggregate for a component." },
            RouteInfo { method: "GET", path: "/api/v1/statuspage/uptime", owner: "statuspage", description: "PUBLIC: daily uptime history (paginated)." },
            RouteInfo { method: "GET", path: "/api/v1/statuspage/incidents", owner: "statuspage", description: "PUBLIC: incident history (paginated)." },
            RouteInfo { method: "POST", path: "/api/v1/statuspage/incidents", owner: "statuspage", description: "Create an incident." },
            RouteInfo { method: "GET", path: "/api/v1/statuspage/incidents/{id}", owner: "statuspage", description: "Get one incident." },
            RouteInfo { method: "PUT", path: "/api/v1/statuspage/incidents/{id}", owner: "statuspage", description: "Update an incident." },
            RouteInfo { method: "POST", path: "/api/v1/statuspage/incidents/{id}/resolve", owner: "statuspage", description: "Resolve an incident." },
            RouteInfo { method: "GET", path: "/api/v1/statuspage/incidents/{id}/updates", owner: "statuspage", description: "PUBLIC: incident timeline updates (paginated)." },
            RouteInfo { method: "POST", path: "/api/v1/statuspage/incidents/{id}/updates", owner: "statuspage", description: "Add a timeline update to an incident." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(StatusPageModule::new()));
}
