// Activity Timeline module.
//
// A unified activity feed that combines the records store, the audit
// trail, and task / comment events into one newest-first feed. This
// module does NOT duplicate the records store and does NOT read it
// directly: it keeps a lightweight in-memory timeline cache fed by
// subscribing to the same event names the records module subscribes
// to (verified by reading `src/modules/records.rs`), plus the task
// events and the verified failure events listed below. Each entry is
// derived from the event payload with the same field conventions the
// audit and records modules use (`actor_id` / `actor` / `user_id` for
// the actor, `entity_type` / `entity_id` / `summary`, `resource` as
// `type:id`, `ip` when present).
//
// Event sources (all names verified in the tree):
//   - The audit / records event set: the well-known constants in
//     `crate::core::hooks::events` (`user.created`, `user.updated`,
//     `role.created`, `license.issued`, `license.revoked`,
//     `module.enabled`, `module.disabled`, `ssl.renew`, `auth.failed`,
//     `geo.lookup`) plus the plain string events `order.created`,
//     `invoice.paid`, `cloudflare.dns.changed`,
//     `cloudflare.cache.purged`, `cloudflare.connected`,
//     `google.sso.completed`, `google.drive.link`,
//     `google.drive.unlink`, `user.deleted`.
//   - The task events emitted by the tasks module: `task.created`,
//     `task.assigned`, `task.status.changed`, `task.completed`,
//     `task.comment.added`.
//   - Verified failure events: `security.auth.failed`
//     (`core::hardening::security_events::AUTH_FAILED`),
//     `security.bruteforce.locked`, `cluster.sync.failed`
//     (`modules::clusters::CLUSTER_SYNC_FAILED`), `payment.failed`
//     (`modules::payments::PAYMENT_FAILED`).
//
// The cache is bounded (see `MAX_ENTRIES`): the oldest entries are
// dropped when the cap is hit, so the feed is a rolling window, not a
// durable log — the audit log and the records store remain the durable
// sources of truth.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus, HookContext, HookHandler};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Types.
// ---------------------------------------------------------------------------

/// Maximum entries kept in the timeline cache. The oldest entries are
/// evicted first; the durable audit / records stores are unaffected.
pub const MAX_ENTRIES: usize = 5_000;

#[derive(Debug, Clone, Serialize)]
pub struct TimelineEntry {
    pub id: String,
    pub seq: u64,
    pub actor: String,
    /// The hook event name that produced this entry.
    pub action: String,
    pub entity_type: String,
    pub entity_id: String,
    pub summary: String,
    /// Which source family produced the entry: `record`, `audit` or
    /// `task`. Failure events report `security`, `cluster` or
    /// `payment`.
    pub source: String,
    pub details: Value,
    pub ip: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CountEntry {
    pub key: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TimelineCounts {
    pub total: u64,
    pub by_action: Vec<CountEntry>,
    pub by_entity_type: Vec<CountEntry>,
    pub by_day: Vec<CountEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DayGroup {
    pub date: String,
    pub count: u64,
}

// ---------------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------------

/// Events this module subscribes to: the exact records event set,
/// then the verified failure events.
const TIMELINE_EVENTS: &[&str] = &[
    events::USER_CREATED,
    events::USER_UPDATED,
    events::ROLE_CREATED,
    events::LICENSE_ISSUED,
    events::LICENSE_REVOKED,
    events::MODULE_ENABLED,
    events::MODULE_DISABLED,
    "order.created",
    "invoice.paid",
    events::SSL_RENEW,
    events::AUTH_FAILED,
    events::GEO_LOOKUP,
    "cloudflare.dns.changed",
    "cloudflare.cache.purged",
    "cloudflare.connected",
    "google.sso.completed",
    "google.drive.link",
    "google.drive.unlink",
    "user.deleted",
    "task.created",
    "task.assigned",
    "task.status.changed",
    "task.completed",
    "task.comment.added",
    "security.auth.failed",
    "security.bruteforce.locked",
    "cluster.sync.failed",
    "payment.failed",
];

pub struct TimelineState {
    entries: DashMap<String, TimelineEntry>,
    seq: AtomicU64,
}

impl TimelineState {
    fn new() -> Self {
        Self {
            entries: DashMap::new(),
            seq: AtomicU64::new(0),
        }
    }

    /// Append one entry derived from a hook event payload. This is the
    /// only way entries are created.
    fn record(&self, event: &str, data: &Value) -> TimelineEntry {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let actor = data
            .get("actor_id")
            .or_else(|| data.get("actor"))
            .or_else(|| data.get("user_id"))
            .or_else(|| data.get("account"))
            .and_then(|value| value.as_str())
            .unwrap_or("system")
            .to_string();
        let (entity_type, entity_id) = derive_entity(event, data);
        let summary = data
            .get("summary")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("{event} on {entity_type}:{entity_id}"));
        let ip = data
            .get("ip")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let entry = TimelineEntry {
            id: Uuid::new_v4().to_string(),
            seq,
            actor,
            action: event.to_string(),
            entity_type,
            entity_id,
            summary,
            source: source_for(event),
            details: data.clone(),
            ip,
            created_at: Utc::now(),
        };
        self.entries.insert(entry.id.clone(), entry.clone());
        self.evict_oldest_if_over_cap();
        entry
    }

    /// Drop the oldest entries while the cache is over the cap.
    fn evict_oldest_if_over_cap(&self) {
        if self.entries.len() <= MAX_ENTRIES {
            return;
        }
        let mut seqs: Vec<(String, u64)> = self
            .entries
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().seq))
            .collect();
        seqs.sort_by(|a, b| a.1.cmp(&b.1));
        let excess = self.entries.len() - MAX_ENTRIES;
        for (key, _) in seqs.into_iter().take(excess) {
            self.entries.remove(&key);
        }
    }

    fn all_entries(&self) -> Vec<TimelineEntry> {
        self.entries
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }
}

/// Source family for an event name, for the `source` field.
fn source_for(event: &str) -> String {
    if event.starts_with("task.") {
        "task".to_string()
    } else if event.starts_with("security.") {
        "security".to_string()
    } else if event.starts_with("cluster.") {
        "cluster".to_string()
    } else if event.starts_with("payment.") {
        "payment".to_string()
    } else if event.starts_with("auth.") {
        "audit".to_string()
    } else {
        "record".to_string()
    }
}

/// Derive the entity type and id from the event payload, using the
/// same precedence the records module uses: explicit `entity_type` /
/// `entity_id` first, then a `resource` value of the form `type:id`,
/// then an `id` key paired with the event name prefix, and finally
/// the whole event name as the type with an empty id.
fn derive_entity(event: &str, data: &Value) -> (String, String) {
    if let Some(entity_type) = data.get("entity_type").and_then(|v| v.as_str()) {
        let entity_id = data
            .get("entity_id")
            .and_then(|v| v.as_str())
            .or_else(|| data.get("id").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();
        return (entity_type.to_string(), entity_id);
    }
    if let Some(resource) = data.get("resource").and_then(|v| v.as_str()) {
        if let Some((kind, id)) = resource.split_once(':') {
            if !kind.is_empty() && !id.is_empty() {
                return (kind.to_string(), id.to_string());
            }
        }
        return (event.to_string(), resource.to_string());
    }
    let entity_type = event.split('.').next().unwrap_or(event).to_string();
    let entity_id = data
        .get("id")
        .and_then(|v| v.as_str())
        .or_else(|| data.get("order_id").and_then(|v| v.as_str()))
        .or_else(|| data.get("cluster_id").and_then(|v| v.as_str()))
        .or_else(|| data.get("plan_id").and_then(|v| v.as_str()))
        .or_else(|| data.get("module_id").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();
    (entity_type, entity_id)
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

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

/// Parse a `from` / `to` date filter (`YYYY-MM-DD` or RFC 3339).
/// Returns the error to report when the value does not parse.
fn parse_date_filter(raw: &str, label: &str) -> UnifiedResult<DateTime<Utc>> {
    if let Ok(parsed) = raw.parse::<DateTime<Utc>>() {
        return Ok(parsed);
    }
    if let Ok(date) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        let naive = date.and_hms_opt(0, 0, 0).unwrap_or_default();
        return Ok(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc));
    }
    Err(UnifiedError::bad_request(format!(
        "Filter {label} must be a date (YYYY-MM-DD) or an RFC 3339 timestamp"
    )))
}

fn count_by(entries: &[TimelineEntry], key_fn: impl Fn(&TimelineEntry) -> String) -> Vec<CountEntry> {
    let mut counts: HashMap<String, u64> = HashMap::new();
    for entry in entries {
        *counts.entry(key_fn(entry)).or_insert(0) += 1;
    }
    let mut out: Vec<CountEntry> = counts
        .into_iter()
        .map(|(key, count)| CountEntry { key, count })
        .collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then(a.key.cmp(&b.key)));
    out
}

/// Shared filter step for the feed and the entity timeline: actor,
/// action, entity type/id, source, and an inclusive date range.
fn apply_filters(
    mut items: Vec<TimelineEntry>,
    params: &PaginationParams,
) -> UnifiedResult<Vec<TimelineEntry>> {
    let from = match params.filters.get("from") {
        Some(raw) => Some(parse_date_filter(raw, "from")?),
        None => None,
    };
    let to = match params.filters.get("to") {
        Some(raw) => Some(parse_date_filter(raw, "to")?),
        None => None,
    };
    let date = params.filters.get("date").cloned();
    items.retain(|entry| {
        matches_search(
            &format!("{} {} {} {}", entry.action, entry.actor, entry.summary, entry.entity_id),
            params,
        ) && params
            .filters
            .get("actor")
            .is_none_or(|actor| entry.actor == *actor)
            && params.filters.get("action").is_none_or(|action| entry.action == *action)
            && params
                .filters
                .get("entity_type")
                .is_none_or(|entity_type| entry.entity_type == *entity_type)
            && params
                .filters
                .get("entity_id")
                .is_none_or(|entity_id| entry.entity_id == *entity_id)
            && params
                .filters
                .get("source")
                .is_none_or(|source| entry.source == *source)
            && from.is_none_or(|from| entry.created_at >= from)
            && to.is_none_or(|to| entry.created_at <= to)
            && date.as_ref().is_none_or(|date| {
                entry.created_at.format("%Y-%m-%d").to_string() == *date
            })
    });
    // Newest first by insertion sequence; `order=asc` reverses.
    items.sort_by(|a, b| b.seq.cmp(&a.seq));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    Ok(items)
}

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

async fn list_feed(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<TimelineState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let actor = params.filters.get("actor").cloned();
    let result: UnifiedResult<PageResult<TimelineEntry>> =
        apply_filters(state.all_entries(), &params).map(|items| paginate(items, &params));
    match result {
        Ok(page) => {
            // Low-priority analytics signal: the feed was viewed.
            ctx.hooks.emit(
                "timeline.viewed",
                json!({
                    "actor": actor,
                    "total": page.pagination.total,
                    "viewed_at": Utc::now().to_rfc3339(),
                }),
            );
            json::success_response(page, 200)
        }
        Err(error) => json::error_response(&error, debug),
    }
}

async fn entity_timeline(
    state: web::Data<Arc<TimelineState>>,
    path: web::Path<(String, String)>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let (entity_type, entity_id) = path.into_inner();
    let params = query.into_inner();
    let mut items: Vec<TimelineEntry> = state
        .all_entries()
        .into_iter()
        .filter(|entry| entry.entity_type == entity_type && entry.entity_id == entity_id)
        .collect();
    items.sort_by(|a, b| b.seq.cmp(&a.seq));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn counts(state: web::Data<Arc<TimelineState>>) -> HttpResponse {
    let entries = state.all_entries();
    json::success_response(
        TimelineCounts {
            total: entries.len() as u64,
            by_action: count_by(&entries, |entry| entry.action.clone()),
            by_entity_type: count_by(&entries, |entry| entry.entity_type.clone()),
            by_day: count_by(&entries, |entry| {
                entry.created_at.format("%Y-%m-%d").to_string()
            }),
        },
        200,
    )
}

async fn by_day(
    state: web::Data<Arc<TimelineState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let entries = state.all_entries();
    let mut groups: Vec<DayGroup> = count_by(&entries, |entry| {
        entry.created_at.format("%Y-%m-%d").to_string()
    })
    .into_iter()
    .map(|count| DayGroup {
        date: count.key,
        count: count.count,
    })
    .collect();
    // Newest day first.
    groups.sort_by(|a, b| b.date.cmp(&a.date));
    let page = paginate(groups, &params);
    json::success_response(page, 200)
}

// ---------------------------------------------------------------------------
// Module.
// ---------------------------------------------------------------------------

pub struct TimelineModule {
    state: Arc<TimelineState>,
}

impl TimelineModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(TimelineState::new()),
        }
    }
}

impl Default for TimelineModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for TimelineModule {
    fn id(&self) -> &'static str {
        "timeline"
    }

    fn name(&self) -> &'static str {
        "Activity Timeline"
    }

    fn description(&self) -> &'static str {
        "Unified newest-first activity feed combining records, audit and task events, with per-user and per-entity filters and grouping by day."
    }

    fn register_hooks(&self, bus: &HookBus) {
        for event in TIMELINE_EVENTS {
            let event_name = (*event).to_string();
            let state = self.state.clone();
            let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
                state.record(&event_name, &ctx.data.clone());
            });
            bus.register(event, "timeline", handler);
        }
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/timeline")
                .route("", web::get().to(list_feed))
                .route("/counts", web::get().to(counts))
                .route("/by-day", web::get().to(by_day))
                .route("/entity/{entity_type}/{entity_id}", web::get().to(entity_timeline)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/timeline", owner: "timeline", description: "Unified activity feed, newest first, paginated with actor/entity/date filters." },
            RouteInfo { method: "GET", path: "/api/v1/timeline/counts", owner: "timeline", description: "Timeline counts by action, entity type and day." },
            RouteInfo { method: "GET", path: "/api/v1/timeline/by-day", owner: "timeline", description: "Timeline entries grouped by day, newest day first." },
            RouteInfo { method: "GET", path: "/api/v1/timeline/entity/{entity_type}/{entity_id}", owner: "timeline", description: "One entity's timeline, newest first." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(TimelineModule::new()));
}
