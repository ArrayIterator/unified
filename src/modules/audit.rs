// Audit Log module.
//
// Append-only log of major events. Entries are created by this module
// itself from hook subscriptions — there is no public create endpoint —
// and are immutable: no update or delete endpoints exist. Interconnection
// happens only through the hook bus.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus, HookContext, HookHandler};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, RwLock,
};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: String,
    pub seq: u64,
    pub actor: String,
    pub action: String,
    pub resource: String,
    pub metadata: Value,
    pub ip: Option<String>,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Events the audit log subscribes to. Well-known core events use the
/// constants from `crate::core::hooks::events`; the rest are the
/// cross-module event names used across the application.
const AUDITED_EVENTS: &[&str] = &[
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
];

pub struct AuditState {
    entries: DashMap<String, AuditEntry>,
    seq: AtomicU64,
}

impl AuditState {
    fn new() -> Self {
        Self {
            entries: DashMap::new(),
            seq: AtomicU64::new(0),
        }
    }

    /// Append one entry. This is the only way entries are created.
    fn record(&self, event: &str, data: &Value) -> AuditEntry {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let actor = data
            .get("actor_id")
            .or_else(|| data.get("actor"))
            .or_else(|| data.get("user_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("system")
            .to_string();
        let resource = data
            .get("resource")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| {
                data.get("id")
                    .and_then(|v| v.as_str())
                    .map(|id| format!("{event}:{id}"))
            })
            .or_else(|| {
                data.get("order_id")
                    .and_then(|v| v.as_str())
                    .map(|id| format!("order:{id}"))
            })
            .or_else(|| {
                data.get("module_id")
                    .and_then(|v| v.as_str())
                    .map(|id| format!("module:{id}"))
            })
            .unwrap_or_else(|| event.to_string());
        let ip = data
            .get("ip")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let entry = AuditEntry {
            id: Uuid::new_v4().to_string(),
            seq,
            actor,
            action: event.to_string(),
            resource,
            metadata: data.clone(),
            ip,
            created_at: Utc::now(),
        };
        self.entries.insert(entry.id.clone(), entry.clone());
        entry
    }
}

// ---------------------------------------------------------------------------
// Helpers
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

// ---------------------------------------------------------------------------
// Handlers (read-only: entries are immutable)
// ---------------------------------------------------------------------------

async fn list_entries(
    state: web::Data<Arc<AuditState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let mut items: Vec<AuditEntry> = state
        .entries
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|entry| {
            matches_search(
                &format!("{} {} {}", entry.action, entry.actor, entry.resource),
                &params,
            ) && params
                .filters
                .get("actor")
                .is_none_or(|actor| entry.actor == *actor)
                && params
                    .filters
                    .get("action")
                    .is_none_or(|action| entry.action == *action)
                && params
                    .filters
                    .get("resource")
                    .is_none_or(|resource| entry.resource == *resource)
        })
        .collect();
    // Newest first by insertion sequence.
    items.sort_by(|a, b| b.seq.cmp(&a.seq));
    if params.normalized().order == "asc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

async fn get_entry(
    state: web::Data<Arc<AuditState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<AuditEntry> = state
        .entries
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Audit entry not found"));
    json::respond(result, false)
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct AuditModule {
    state: Arc<AuditState>,
}

impl AuditModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(AuditState::new()),
        }
    }
}

impl Default for AuditModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for AuditModule {
    fn id(&self) -> &'static str {
        "audit"
    }

    fn name(&self) -> &'static str {
        "Audit Log"
    }

    fn description(&self) -> &'static str {
        "Append-only audit log of major events across the application."
    }

    fn init(&self, _ctx: &AppContext) -> UnifiedResult<()> {
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        for event in AUDITED_EVENTS {
            let event_name = (*event).to_string();
            let state = self.state.clone();
            let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
                state.record(&event_name, &ctx.data.clone());
            });
            bus.register(event, "audit", handler);
        }
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/audit")
                .route("/entries", web::get().to(list_entries))
                .route("/entries/{id}", web::get().to(get_entry)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(AuditModule::new()));
}
