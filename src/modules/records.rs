// Records module.
//
// A queryable system-records store that sits behind the audit trail:
// who did what, when, on which entity. Records are created only by
// listening to events on the hook bus - there is no public create
// endpoint - and they are append-only: no update or delete endpoints
// exist, only a retention purge that requires explicit confirmation.
//
// Event sources (verified by reading `src/modules/audit.rs` and
// `src/core/hooks.rs`):
//
//   - Every event the audit module subscribes to is subscribed here
//     too, using the same names: the well-known constants in
//     `crate::core::hooks::events` (`user.created`, `user.updated`,
//     `role.created`, `license.issued`, `license.revoked`,
//     `module.enabled`, `module.disabled`, `ssl.renew`, `auth.failed`,
//     `geo.lookup`) plus the plain string events the audit module
//     lists (`order.created`, `invoice.paid`, `cloudflare.dns.changed`,
//     `cloudflare.cache.purged`, `cloudflare.connected`,
//     `google.sso.completed`, `google.drive.link`,
//     `google.drive.unlink`).
//   - The task events emitted by the tasks module (`task.created`,
//     `task.assigned`, `task.status.changed`, `task.completed`,
//     `task.comment.added`) and `user.deleted` (emitted by the users
//     module as a plain string event).
//
// The audit module itself is NOT edited: this module registers its
// own listeners for the same event names. A record is derived from
// the event payload with the same field conventions the audit module
// uses (`actor_id` / `actor` / `user_id` for the actor, `resource` as
// `type:id` when present, `ip` when present), plus the explicit
// `entity_type` / `entity_id` / `summary` keys the tasks module
// emits. Anything the audit module records therefore also lands here,
// in queryable form.
//
// Persistence: records live in an in-memory store (the same approach
// the audit module uses). The Diesel table for durable persistence
// does not exist in `src/db/schema.rs` yet; the migration SQL and the
// schema / model entries are supplied to the integrator separately.
// Nothing in this file references a table that does not exist yet, so
// it compiles against the current schema.
//
// Retention: records older than the configured retention are removed
// only by the explicit purge endpoint. The retention comes from the
// `records` config section (`AppConfig.records.retention_days`,
// default 365 days); `DEFAULT_RETENTION_DAYS` below is only the
// fallback when the configured value is absent (zero).

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus, HookContext, HookHandler};
use crate::core::json::{error_response, success_response};
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use uuid::Uuid;

/// Default retention in days, used when the `records` config
/// section carries no value (zero). Config validation rejects zero,
/// so this only guards against a hand-built config that skipped
/// validation.
pub const DEFAULT_RETENTION_DAYS: u32 = 365;

/// Retention in days from the `records` config section
/// (`AppConfig.records.retention_days`), falling back to
/// `DEFAULT_RETENTION_DAYS` when the configured value is zero.
fn retention_days(ctx: &AppContext) -> u32 {
    let configured = ctx
        .config
        .read()
        .expect("config lock poisoned")
        .records
        .retention_days;
    if configured == 0 {
        DEFAULT_RETENTION_DAYS
    } else {
        configured
    }
}

// ---------------------------------------------------------------------------
// Types.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct SystemRecord {
    pub id: String,
    pub seq: u64,
    pub actor: String,
    pub action: String,
    pub entity_type: String,
    pub entity_id: String,
    pub summary: String,
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
pub struct RetentionView {
    pub retention_days: u32,
    pub configured: bool,
    pub total_records: u64,
    pub oldest_record_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PurgeInput {
    /// Must be exactly `true`; anything else is rejected with 422 so
    /// a purge can never happen by accident.
    pub confirm: bool,
    /// Records older than this many days are removed. When omitted,
    /// the default retention (365 days) is used. Must be at least 1.
    #[serde(default)]
    pub keep_days: Option<u32>,
}

// ---------------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------------

/// Events this module subscribes to. The audit-subscribed events are
/// listed first, then the task events and `user.deleted`.
const RECORDED_EVENTS: &[&str] = &[
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
];

pub struct RecordsState {
    records: DashMap<String, SystemRecord>,
    seq: AtomicU64,
}

impl RecordsState {
    fn new() -> Self {
        Self {
            records: DashMap::new(),
            seq: AtomicU64::new(0),
        }
    }

    /// Append one record derived from a hook event payload. This is
    /// the only way records are created.
    fn record(&self, event: &str, data: &Value) -> SystemRecord {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let actor = data
            .get("actor_id")
            .or_else(|| data.get("actor"))
            .or_else(|| data.get("user_id"))
            .and_then(|value| value.as_str())
            .unwrap_or("system")
            .to_string();
        let resource = data
            .get("resource")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let (entity_type, entity_id) = derive_entity(event, data, resource.as_deref());
        let summary = data
            .get("summary")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("{event} on {entity_type}:{entity_id}"));
        let ip = data
            .get("ip")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let record = SystemRecord {
            id: Uuid::new_v4().to_string(),
            seq,
            actor,
            action: event.to_string(),
            entity_type,
            entity_id,
            summary,
            details: data.clone(),
            ip,
            created_at: Utc::now(),
        };
        self.records.insert(record.id.clone(), record.clone());
        record
    }
}

/// Derive the entity type and id for a record from the event payload.
///
/// Precedence: explicit `entity_type` / `entity_id` keys first, then
/// a `resource` value of the form `type:id`, then an `id` key paired
/// with the event name prefix (`user.created` -> `user`), and finally
/// the whole event name as the type with an empty id.
fn derive_entity(event: &str, data: &Value, resource: Option<&str>) -> (String, String) {
    if let (Some(entity_type), Some(entity_id)) = (
        data.get("entity_type").and_then(|value| value.as_str()),
        data.get("entity_id").and_then(|value| value.as_str()),
    ) {
        return (entity_type.to_string(), entity_id.to_string());
    }
    if let Some(entity_type) = data.get("entity_type").and_then(|value| value.as_str()) {
        let entity_id = data
            .get("entity_id")
            .and_then(|value| value.as_str())
            .or_else(|| data.get("id").and_then(|value| value.as_str()))
            .unwrap_or("")
            .to_string();
        return (entity_type.to_string(), entity_id);
    }
    if let Some(resource) = resource {
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
        .and_then(|value| value.as_str())
        .or_else(|| {
            data.get("order_id")
                .and_then(|value| value.as_str())
        })
        .or_else(|| {
            data.get("module_id")
                .and_then(|value| value.as_str())
        })
        .unwrap_or("")
        .to_string();
    (entity_type, entity_id)
}

fn count_by(records: &[SystemRecord], key_fn: impl Fn(&SystemRecord) -> &str) -> Vec<CountEntry> {
    let mut counts: HashMap<String, u64> = HashMap::new();
    for record in records {
        *counts.entry(key_fn(record).to_string()).or_insert(0) += 1;
    }
    let mut entries: Vec<CountEntry> = counts
        .into_iter()
        .map(|(key, count)| CountEntry { key, count })
        .collect();
    entries.sort_by(|a, b| b.count.cmp(&a.count).then(a.key.cmp(&b.key)));
    entries
}

// ---------------------------------------------------------------------------
// Handlers (read-only except the retention purge).
// ---------------------------------------------------------------------------

async fn list_records(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<RecordsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    let pagination = params.normalized();
    let descending = pagination.order == "desc";
    let search = pagination
        .search
        .clone()
        .map(|value| value.to_lowercase());
    let actor_filter = params.filters.get("actor").cloned();
    let action_filter = params.filters.get("action").cloned();
    let entity_type_filter = params.filters.get("entity_type").cloned();
    let entity_id_filter = params.filters.get("entity_id").cloned();
    let from_filter = match params.filters.get("from").map(|raw| {
        raw.parse::<DateTime<Utc>>().map_err(|_| {
            UnifiedError::bad_request(format!(
                "Filter \"from\" must be an RFC 3339 date-time, got \"{raw}\""
            ))
        })
    }) {
        Some(Ok(value)) => Some(value),
        Some(Err(error)) => return error_response(&error, debug),
        None => None,
    };
    let to_filter = match params.filters.get("to").map(|raw| {
        raw.parse::<DateTime<Utc>>().map_err(|_| {
            UnifiedError::bad_request(format!(
                "Filter \"to\" must be an RFC 3339 date-time, got \"{raw}\""
            ))
        })
    }) {
        Some(Ok(value)) => Some(value),
        Some(Err(error)) => return error_response(&error, debug),
        None => None,
    };

    let mut items: Vec<SystemRecord> = state
        .records
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|record| {
            if let Some(actor) = &actor_filter {
                if record.actor != *actor {
                    return false;
                }
            }
            if let Some(action) = &action_filter {
                if record.action != *action {
                    return false;
                }
            }
            if let Some(entity_type) = &entity_type_filter {
                if record.entity_type != *entity_type {
                    return false;
                }
            }
            if let Some(entity_id) = &entity_id_filter {
                if record.entity_id != *entity_id {
                    return false;
                }
            }
            if let Some(from) = from_filter {
                if record.created_at < from {
                    return false;
                }
            }
            if let Some(to) = to_filter {
                if record.created_at > to {
                    return false;
                }
            }
            if let Some(needle) = &search {
                let haystack = format!(
                    "{} {} {} {} {}",
                    record.action, record.actor, record.entity_type, record.entity_id, record.summary
                )
                .to_lowercase();
                if !haystack.contains(needle.as_str()) {
                    return false;
                }
            }
            true
        })
        .collect();
    items.sort_by(|a, b| b.seq.cmp(&a.seq));
    if !descending {
        items.reverse();
    }
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<SystemRecord> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    success_response(
        PageResult::new(page_items, total, &pagination).to_json(),
        200,
    )
}

async fn get_record(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<RecordsState>>,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let record_id = id.into_inner();
    match state.records.get(record_id.as_str()) {
        Some(record) => success_response(record.value().clone(), 200),
        None => error_response(
            &UnifiedError::not_found(format!("Record {record_id} not found")),
            debug,
        ),
    }
}

async fn counts_by_entity(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<RecordsState>>,
) -> HttpResponse {
    let _debug = ctx.is_debug();
    let records: Vec<SystemRecord> = state
        .records
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    success_response(count_by(&records, |record| record.entity_type.as_str()), 200)
}

async fn counts_by_action(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<RecordsState>>,
) -> HttpResponse {
    let _debug = ctx.is_debug();
    let records: Vec<SystemRecord> = state
        .records
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    success_response(count_by(&records, |record| record.action.as_str()), 200)
}

async fn export_records(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<RecordsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let params = query.into_inner();
    // Reuse the list filters; export is capped at 10 000 records so a
    // single download cannot exhaust memory.
    let mut items: Vec<SystemRecord> = state
        .records
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|record| {
            params
                .filters
                .get("actor")
                .is_none_or(|actor| record.actor == *actor)
                && params
                    .filters
                    .get("action")
                    .is_none_or(|action| record.action == *action)
                && params
                    .filters
                    .get("entity_type")
                    .is_none_or(|entity_type| record.entity_type == *entity_type)
                && params
                    .filters
                    .get("entity_id")
                    .is_none_or(|entity_id| record.entity_id == *entity_id)
        })
        .collect();
    items.sort_by(|a, b| b.seq.cmp(&a.seq));
    items.truncate(10_000);
    let filename = format!("records-{}.json", Utc::now().format("%Y%m%dT%H%M%SZ"));
    let body = crate::core::json::success(serde_json::json!({
        "exported_at": Utc::now(),
        "count": items.len(),
        "records": items,
    }));
    match actix_web::http::StatusCode::from_u16(200) {
        Ok(status) => HttpResponse::build(status)
            .insert_header((
                "Content-Disposition",
                format!("attachment; filename=\"{filename}\""),
            ))
            .insert_header(("Content-Type", "application/json"))
            .json(body),
        Err(_) => error_response(&UnifiedError::internal("Export failed"), debug),
    }
}

async fn retention_view(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<RecordsState>>,
) -> HttpResponse {
    let _debug = ctx.is_debug();
    let oldest = state
        .records
        .iter()
        .map(|entry| entry.value().created_at)
        .min();
    success_response(
        RetentionView {
            retention_days: retention_days(&ctx),
            configured: true,
            total_records: state.records.len() as u64,
            oldest_record_at: oldest,
        },
        200,
    )
}

async fn purge_records(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<RecordsState>>,
    body: web::Json<PurgeInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = body.into_inner();
    if !input.confirm {
        return error_response(
            &UnifiedError::unprocessable(
                "Retention purge requires {\"confirm\": true}",
            )
            .with_metadata("confirm", Value::Bool(false)),
            debug,
        );
    }
    let keep_days = input.keep_days.unwrap_or_else(|| retention_days(&ctx));
    if keep_days == 0 {
        return error_response(
            &UnifiedError::bad_request("keep_days must be at least 1"),
            debug,
        );
    }
    let cutoff = Utc::now() - chrono::Duration::days(i64::from(keep_days));
    let mut purged: u64 = 0;
    state
        .records
        .retain(|_, record| {
            if record.created_at < cutoff {
                purged += 1;
                false
            } else {
                true
            }
        });
    success_response(
        serde_json::json!({
            "purged": purged,
            "keep_days": keep_days,
            "cutoff": cutoff,
        }),
        200,
    )
}

// ---------------------------------------------------------------------------
// Module.
// ---------------------------------------------------------------------------

pub struct RecordsModule {
    state: Arc<RecordsState>,
}

impl RecordsModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(RecordsState::new()),
        }
    }
}

impl Default for RecordsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for RecordsModule {
    fn id(&self) -> &'static str {
        "records"
    }

    fn name(&self) -> &'static str {
        "Records"
    }

    fn description(&self) -> &'static str {
        "Queryable system-records store behind the audit trail: who did what, when, on which entity. Append-only; records are created from hook events and removed only by a confirmed retention purge."
    }

    fn requires(&self) -> &[&'static str] {
        &["audit", "tasks"]
    }

    fn register_hooks(&self, bus: &HookBus) {
        for event in RECORDED_EVENTS {
            let event_name = (*event).to_string();
            let state = self.state.clone();
            let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
                state.record(&event_name, &ctx.data.clone());
            });
            bus.register(event, "records", handler);
        }
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/records", owner: "records", description: "List records with pagination and filters." },
            RouteInfo { method: "GET", path: "/api/v1/records/{id}", owner: "records", description: "Get one record." },
            RouteInfo { method: "GET", path: "/api/v1/records/counts/by-entity", owner: "records", description: "Record counts grouped by entity type." },
            RouteInfo { method: "GET", path: "/api/v1/records/counts/by-action", owner: "records", description: "Record counts grouped by action." },
            RouteInfo { method: "GET", path: "/api/v1/records/export", owner: "records", description: "Export records as a JSON download." },
            RouteInfo { method: "GET", path: "/api/v1/records/retention", owner: "records", description: "Show the retention setting and store size." },
            RouteInfo { method: "POST", path: "/api/v1/records/purge", owner: "records", description: "Purge records older than keep_days (requires confirm=true)." },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/records")
                // Static routes first: they must win over `/{id}`.
                .route("", web::get().to(list_records))
                .route("/counts/by-entity", web::get().to(counts_by_entity))
                .route("/counts/by-action", web::get().to(counts_by_action))
                .route("/export", web::get().to(export_records))
                .route("/retention", web::get().to(retention_view))
                .route("/purge", web::post().to(purge_records))
                .route("/{id}", web::get().to(get_record)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(RecordsModule::new()));
}
